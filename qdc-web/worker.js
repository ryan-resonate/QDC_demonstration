// Web Worker hosting the qdc-core WebAssembly module.
//
// All heavy work — TDMS/CSV/DAT parsing, signal filtering, rolling pk-pk —
// happens in here so the UI thread stays free. The main thread sends
// requests; we reply with progress events and final results.
//
// RPC surface (msg.type → meaning):
//   "ping"            → "pong" reply
//   "inspect-headers" → reply with channel/column names for one file
//   "process"         → stream { type: "progress" } per file, then { type: "done", results }
//   "stop"            → set the cancel flag; the in-flight processing call
//                       checks it between files and stops gracefully

import init, {
    qdc_core_version,
    WasmProcessor,
    tdms_inspect,
    tdms_channel_samples,
    csv_inspect_headers,
    dat_inspect_headers,
    decimate_minmax,
    processed_csv_load,
} from "./pkg/qdc_core.js";

let ready = false;

// Single-flag cooperative cancellation. The processing loop checks this
// between files (not mid-file — see processing.rs notes).
let cancelRequested = false;

// Raw-sample cache built during processing — keyed by file index. Lets the
// drill-down panel ask for any time range without re-reading the original
// files. For files of typical size (5–15 MB) and a handful of files this
// stays under a few hundred MB.
//
// Entry shape: { name, startUs, endUs, dtUs, x, y, z }
let rawCache = [];
let rawCacheUnit = null;

async function boot() {
    await init();
    ready = true;
    self.postMessage({ type: "ready", version: qdc_core_version() });
}
boot().catch((err) => {
    self.postMessage({ type: "fatal", message: `WASM init failed: ${err?.message ?? err}` });
});

self.onmessage = async (ev) => {
    const msg = ev.data || {};
    try {
        switch (msg.type) {
            case "ping":
                self.postMessage({ type: "pong", ready });
                return;
            case "inspect-headers":
                await handleInspect(msg);
                return;
            case "process":
                await handleProcess(msg);
                return;
            case "stop":
                cancelRequested = true;
                return;
            case "raw-window":
                await handleRawWindow(msg);
                return;
            case "debug-state":
                self.postMessage({
                    type: "debug-state-reply",
                    id: msg.id,
                    rawCacheCount: rawCache.length,
                    rawCacheSummary: rawCache.map((e) => ({
                        name: e.name,
                        sampleCount: e.x?.length,
                        startUs: e.startUs,
                        endUs: e.endUs,
                        dtUs: e.dtUs,
                    })),
                });
                return;
            default:
                self.postMessage({ type: "warn", message: `unknown msg type "${msg.type}"` });
        }
    } catch (err) {
        self.postMessage({
            type: "error",
            id: msg.id,
            message: err?.stack || err?.message || String(err),
        });
    }
};

async function handleInspect({ id, fileType, file }) {
    // Read just enough of the file to scan headers. For CSV/DAT, the first
    // few KB are plenty; for TDMS, we need the metadata sections, which can
    // sit late in the file — easiest is to read the whole file.
    const bytes = await readFileBytes(file, fileType === "tdms" ? null : 16 * 1024);
    let result;
    if (fileType === "tdms") {
        const info = tdms_inspect(bytes);
        result = { kind: "tdms", info };
    } else if (fileType === "csv" || fileType === "processed_csv") {
        const headers = csv_inspect_headers(bytes);
        result = { kind: "csv", headers };
    } else if (fileType === "dat") {
        const headers = dat_inspect_headers(bytes);
        result = { kind: "dat", headers };
    } else {
        throw new Error(`unsupported fileType ${fileType}`);
    }
    self.postMessage({ type: "inspect-reply", id, result });
}

async function handleProcess({ id, fileType, files, config, channelMap }) {
    cancelRequested = false;
    rawCache = [];
    rawCacheUnit = null;
    const processor = new WasmProcessor(config);
    const total = files.length;
    let processed = 0;
    let stopped = false;
    // Capture the post-scaling output unit so the plot can label its y-axis
    // correctly. pkpk_processing's logic: mT/uT inputs get scaled to nT;
    // everything else is left as-is. We snapshot from the first file.
    let outputUnit = null;
    for (let i = 0; i < total; i++) {
        if (cancelRequested) {
            stopped = true;
            break;
        }
        const file = files[i];
        self.postMessage({
            type: "progress", id, fileIndex: i, total, stage: "reading", name: file.name,
        });
        const bytes = await readFileBytes(file, null);
        self.postMessage({
            type: "progress", id, fileIndex: i, total, stage: "processing", name: file.name,
        });
        let cacheEntry = null;
        if (fileType === "tdms") {
            // Inspect first so we can pull the three channels' samples out into
            // the raw cache. (We then re-parse below via feed_tdms — wastes one
            // parse but keeps the bindings simple.)
            const info = tdms_inspect(bytes);
            const group = info.groups.find((g) => g.name === channelMap.group);
            const findCh = (name) => group?.channels.find((c) => c.name === name);
            const cx = findCh(channelMap.x);
            const dtUs = cx ? Math.round(cx.dt_seconds * 1_000_000) : 1_000_000;
            const startUs = cx ? Math.round(cx.start_time_us) : 0;
            // Output unit after the Rust-side unit scaling. mT/uT → nT;
            // everything else passes through unchanged.
            if (outputUnit === null && cx?.unit_string) {
                const u = cx.unit_string;
                outputUnit = (u === "mT" || u === "uT" || u === "µT") ? "nT" : u;
            }
            const x = tdms_channel_samples(bytes, channelMap.group, channelMap.x);
            const y = tdms_channel_samples(bytes, channelMap.group, channelMap.y);
            const z = tdms_channel_samples(bytes, channelMap.group, channelMap.z);
            const sampleCount = x.length;
            cacheEntry = {
                name: file.name,
                startUs,
                endUs: startUs + dtUs * (sampleCount - 1),
                dtUs,
                x, y, z,
            };
            processor.feed_tdms(bytes, channelMap.group, channelMap.x, channelMap.y, channelMap.z);
        } else if (fileType === "csv") {
            processor.feed_csv(bytes, channelMap.time ?? 0, channelMap.xIdx, channelMap.yIdx, channelMap.zIdx);
            // CSV raw cache deferred — would need a separate "read_csv → arrays"
            // binding to populate; not blocking the demo for the most common
            // (TDMS) flow.
        } else if (fileType === "dat") {
            processor.feed_dat(bytes);
        } else if (fileType === "processed_csv") {
            // Processed-CSV is already pk-pk output from a prior run. Skip the
            // rolling-pkpk kernel entirely and pass-through directly.
            const loaded = processed_csv_load(bytes);
            self.postMessage({
                type: "progress", id, fileIndex: i, total, stage: "done", name: file.name,
            });
            self.postMessage(
                {
                    type: "done", id,
                    results: loaded,
                    processed: 1,
                    stopped: false,
                    hasRawCache: false,
                    outputUnit: null,
                    processedCsvMode: true,
                },
                [loaded.times_us.buffer, loaded.x_pkpk.buffer, loaded.y_pkpk.buffer, loaded.z_pkpk.buffer,
                 loaded.xy_pkpk.buffer, loaded.xz_pkpk.buffer, loaded.yz_pkpk.buffer],
            );
            return;
        }
        if (cacheEntry) {
            rawCache.push(cacheEntry);
        }
        processed = i + 1;
        self.postMessage({
            type: "progress", id, fileIndex: i, total, stage: "done", name: file.name,
        });
    }
    const results = processor.finish();
    // Attach the captured output unit so the plot can label its y-axis.
    if (outputUnit) {
        results.unit = outputUnit;
        rawCacheUnit = outputUnit;
    }
    self.postMessage(
        {
            type: "done", id, results, processed, stopped,
            hasRawCache: rawCache.length > 0,
            outputUnit,
        },
        transferableArrays(results),
    );
}

async function handleRawWindow({ id, startUs, endUs, nBuckets }) {
    if (!rawCache.length) {
        self.postMessage({ type: "raw-window-reply", id, empty: true });
        return;
    }
    // Find the files that overlap the requested window and concatenate the
    // relevant sample slices. For typical drill-down windows this is one or
    // two files at most.
    const out = { x: [], y: [], z: [], times: [] };
    for (const entry of rawCache) {
        if (entry.endUs < startUs || entry.startUs > endUs) continue;
        // Map time bounds to sample indices in this file.
        const lo = Math.max(startUs, entry.startUs);
        const hi = Math.min(endUs, entry.endUs);
        const startIdx = Math.max(0, Math.floor((lo - entry.startUs) / entry.dtUs));
        const endIdx = Math.min(entry.x.length, Math.ceil((hi - entry.startUs) / entry.dtUs) + 1);
        if (endIdx <= startIdx) continue;
        out.x.push(entry.x.subarray(startIdx, endIdx));
        out.y.push(entry.y.subarray(startIdx, endIdx));
        out.z.push(entry.z.subarray(startIdx, endIdx));
        out.times.push({ baseUs: entry.startUs + startIdx * entry.dtUs, dtUs: entry.dtUs, n: endIdx - startIdx });
    }
    if (!out.x.length) {
        self.postMessage({ type: "raw-window-reply", id, empty: true });
        return;
    }
    // Concatenate.
    const concat = (parts) => {
        const total = parts.reduce((acc, a) => acc + a.length, 0);
        const out = new Float64Array(total);
        let off = 0;
        for (const a of parts) { out.set(a, off); off += a.length; }
        return out;
    };
    const xAll = concat(out.x);
    const yAll = concat(out.y);
    const zAll = concat(out.z);
    // Build a synthetic time axis. (Doesn't handle inter-file gaps; first
    // segment's grid is extended.)
    const tBase = out.times[0].baseUs;
    const dtUs = out.times[0].dtUs;

    // Decimate (or return raw if small enough).
    const x = decimate_minmax(xAll, nBuckets);
    const y = decimate_minmax(yAll, nBuckets);
    const z = decimate_minmax(zAll, nBuckets);
    const total = xAll.length;
    const n = x.mins.length;
    const tUs = new Float64Array(n);
    for (let i = 0; i < n; i++) {
        tUs[i] = tBase + Math.floor((i * total) / n) * dtUs;
    }
    self.postMessage(
        {
            type: "raw-window-reply", id,
            t_us: tUs,
            x_min: x.mins, x_max: x.maxs,
            y_min: y.mins, y_max: y.maxs,
            z_min: z.mins, z_max: z.maxs,
            total_samples: total,
            unit: rawCacheUnit,
        },
        [tUs.buffer, x.mins.buffer, x.maxs.buffer, y.mins.buffer, y.maxs.buffer, z.mins.buffer, z.maxs.buffer],
    );
}

async function readFileBytes(file, maxBytes) {
    const slice = maxBytes ? file.slice(0, maxBytes) : file;
    const buf = await slice.arrayBuffer();
    return new Uint8Array(buf);
}

// Collect the Float64Array .buffer handles for zero-copy transfer back to
// the main thread.
function transferableArrays(results) {
    if (!results) return [];
    const out = [];
    for (const key of [
        "times_us", "x_pkpk", "y_pkpk", "z_pkpk",
        "xy_pkpk", "xz_pkpk", "yz_pkpk",
    ]) {
        const arr = results[key];
        if (arr && arr.buffer) out.push(arr.buffer);
    }
    return out;
}
