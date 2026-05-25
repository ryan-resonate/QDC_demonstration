// Main-thread orchestration for the 3-stage demo workflow.
// All numerics live in worker.js / WASM. This file is glue and DOM only.

import { drawPkPkPlot, attachBoxSelectHandler, drawRawPlot, downloadPlot } from "./plotting.js";

// ===== State =====
const state = {
    fileType: "tdms",
    files: [],          // [File]
    headers: null,      // { kind, headers? | info? } from worker inspect
    channelMap: null,   // chosen channel mapping (see resolveChannelMap)
    rpcSeq: 1,
    results: null,
    busy: false,
    /// True after loading a Processed-CSV — disables raw drill-down since
    /// the underlying samples aren't available.
    processedCsvMode: false,
};

// ===== Element handles =====
const $ = (sel) => document.querySelector(sel);
const els = {
    wasmStatus: $("#wasm-status"),
    versionLine: $("#version-line"),
    inputType: $("#input-type"),
    addFilesBtn: $("#add-files-btn"),
    clearFilesBtn: $("#clear-files-btn"),
    fileInput: $("#file-input"),
    fileList: $("#filelist"),
    fileListEmpty: $("#filelist-empty"),

    lpFreq: $("#lp-freq"),
    filterType: $("#filter-type"),
    pkpkTime: $("#pkpk-time"),
    pkpkFs: $("#pkpk-fs"),
    csvTimeCol: $("#csv-time-col"),

    channelsTable: $("#channels-table"),
    channelsHint: $("#channels-hint"),
    channelsTbody: $("#channels-table tbody"),

    runBtn: $("#run-btn"),
    runHint: $("#run-hint"),

    progress: $("#progress"),
    progressText: $("#progress-text"),
    stopBtn: $("#stop-btn"),
    statusLog: $("#status-log"),

    pkpkPlot: $("#pkpk-plot"),
    rawPanel: $("#raw-panel"),
    rawPlot: $("#raw-plot"),
    rawStatus: $("#raw-status"),
    rawDisabledNote: $("#raw-disabled-note"),
    subtractMean: $("#subtract-mean"),
    meanSummary: $("#mean-summary"),
    limitLine: $("#limit-line"),
    savePngBtn: $("#save-png-btn"),
    saveSvgBtn: $("#save-svg-btn"),
    saveCsvBtn: $("#save-csv-btn"),
};

// ===== Worker =====
const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
const pending = new Map(); // id → { resolve, reject, onProgress }

function rpc(message, transferables = [], onProgress) {
    const id = state.rpcSeq++;
    return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject, onProgress });
        worker.postMessage({ ...message, id }, transferables);
    });
}

worker.onmessage = (ev) => {
    const msg = ev.data || {};
    if (msg.type === "ready") {
        setStatus("ok", `WASM: ready (${msg.version})`);
        els.versionLine.textContent = msg.version;
        return;
    }
    if (msg.type === "fatal") {
        setStatus("err", "WASM: failed");
        els.versionLine.textContent = msg.message;
        console.error("[worker fatal]", msg.message);
        return;
    }
    if (msg.type === "warn") {
        console.warn("[worker]", msg.message);
        return;
    }

    const slot = pending.get(msg.id);
    if (!slot) return;

    if (msg.type === "progress") {
        slot.onProgress?.(msg);
        return;
    }
    pending.delete(msg.id);
    if (msg.type === "error") {
        slot.reject(new Error(msg.message));
    } else if (msg.type === "inspect-reply") {
        slot.resolve(msg.result);
    } else if (msg.type === "done") {
        slot.resolve(msg);
    } else if (msg.type === "raw-window-reply") {
        slot.resolve(msg);
    } else if (msg.type === "debug-state-reply") {
        slot.resolve(msg);
    } else {
        slot.resolve(msg);
    }
};
// Expose the worker to window for diagnostic poking.
window.__qdcWorker = worker;
window.__qdcRpc = rpc;

worker.onerror = (ev) => {
    setStatus("err", "WASM: worker crashed");
    console.error("[worker error]", ev.message || ev);
};

function setStatus(state, text) {
    els.wasmStatus.textContent = text;
    els.wasmStatus.classList.remove("ok", "warn", "err");
    if (state) els.wasmStatus.classList.add(state);
}

// ===== Stage tab navigation =====
document.querySelectorAll(".stage-tab").forEach((tab) => {
    tab.addEventListener("click", () => {
        if (tab.disabled) return;
        switchStage(tab.dataset.stage);
    });
});
function switchStage(key) {
    document.querySelectorAll(".stage-tab").forEach((t) => {
        t.setAttribute("aria-selected", t.dataset.stage === key ? "true" : "false");
    });
    document.querySelectorAll(".stage").forEach((s) => {
        s.dataset.active = s.id === `stage-${key}` ? "true" : "false";
    });
    // Plotly needs a re-layout when its container becomes visible.
    if (key === "plot" && state.results) {
        drawPkPkPlot(els.pkpkPlot, state.results, getVisibleSeries(), getLimitLine(), parseFloat(els.pkpkTime.value));
    }
}

// ===== Stage 1: file list & params =====
els.inputType.addEventListener("change", () => {
    state.fileType = els.inputType.value;
    state.files = [];
    state.headers = null;
    state.channelMap = null;
    renderFileList();
    renderChannels();
    updateRunGate();
});

els.addFilesBtn.addEventListener("click", () => els.fileInput.click());
els.clearFilesBtn.addEventListener("click", () => {
    state.files = [];
    state.headers = null;
    state.channelMap = null;
    renderFileList();
    renderChannels();
    updateRunGate();
});

els.fileInput.addEventListener("change", async () => {
    const list = Array.from(els.fileInput.files || []);
    if (!list.length) return;
    state.files.push(...list);
    els.fileInput.value = "";
    renderFileList();
    await rescanHeaders();
    updateRunGate();
});

// Drag and drop on the file list.
const dropTarget = $(".filelist-shell");
["dragenter", "dragover"].forEach((ev) => {
    dropTarget.addEventListener(ev, (e) => {
        e.preventDefault();
        dropTarget.classList.add("dragging");
    });
});
["dragleave", "drop"].forEach((ev) => {
    dropTarget.addEventListener(ev, (e) => {
        e.preventDefault();
        dropTarget.classList.remove("dragging");
    });
});
dropTarget.addEventListener("drop", async (e) => {
    const dropped = Array.from(e.dataTransfer?.files || []);
    if (!dropped.length) return;
    state.files.push(...dropped);
    renderFileList();
    await rescanHeaders();
    updateRunGate();
});

function renderFileList() {
    els.fileList.innerHTML = "";
    if (!state.files.length) {
        els.fileListEmpty.hidden = false;
        return;
    }
    els.fileListEmpty.hidden = true;
    state.files.forEach((f, i) => {
        const li = document.createElement("li");
        const badge = document.createElement("span");
        badge.className = "badge-type";
        badge.textContent = state.fileType.toUpperCase();
        li.appendChild(badge);
        const name = document.createElement("span");
        name.textContent = f.name;
        li.appendChild(name);
        const size = document.createElement("span");
        size.className = "size";
        size.textContent = formatBytes(f.size);
        li.appendChild(size);
        const rm = document.createElement("button");
        rm.textContent = "✕";
        rm.title = "Remove";
        rm.addEventListener("click", () => {
            state.files.splice(i, 1);
            renderFileList();
            rescanHeaders().then(updateRunGate);
        });
        li.appendChild(rm);
        els.fileList.appendChild(li);
    });
}

function formatBytes(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
    return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

async function rescanHeaders() {
    if (!state.files.length) {
        state.headers = null;
        renderChannels();
        return;
    }
    try {
        state.headers = await rpc({
            type: "inspect-headers",
            fileType: state.fileType,
            file: state.files[0],
        });
        renderChannels();
    } catch (err) {
        console.error("inspect failed", err);
        state.headers = null;
        els.channelsHint.textContent = `Header scan failed: ${err.message}`;
    }
}

function renderChannels() {
    const tbody = els.channelsTbody;
    tbody.innerHTML = "";
    if (!state.headers) {
        els.channelsTable.hidden = true;
        els.channelsHint.hidden = false;
        els.channelsHint.textContent = "Add files to scan headers.";
        return;
    }
    if (state.fileType === "processed_csv") {
        // Processed CSVs have fixed columns — no channel choice to make.
        els.channelsTable.hidden = true;
        els.channelsHint.hidden = false;
        els.channelsHint.textContent =
            "Processed CSV: Time + X_PkPk/Y_PkPk/Z_PkPk columns will be loaded automatically. Skip directly to processing.";
        return;
    }
    let rows = [];
    if (state.headers.kind === "tdms") {
        // Pick the second group if present, else the first.
        const info = state.headers.info;
        const groups = info?.groups || [];
        const target = groups[1] || groups[0];
        if (!target) {
            els.channelsHint.textContent = "TDMS file has no groups.";
            els.channelsHint.hidden = false;
            els.channelsTable.hidden = true;
            return;
        }
        rows = (target.channels || []).map((c, i) => ({
            idx: i,
            sourceName: c.name,
            tdmsGroup: target.name,
        }));
    } else if (state.headers.kind === "csv") {
        rows = (state.headers.headers || []).map((h, i) => ({ idx: i, sourceName: h }));
    } else if (state.headers.kind === "dat") {
        // DAT always has 3 fixed channels (X/Y/Z in some flavour).
        rows = (state.headers.headers || []).map((h, i) => ({ idx: i, sourceName: h }));
    }

    if (!rows.length) {
        els.channelsHint.textContent = "No channels detected.";
        els.channelsHint.hidden = false;
        els.channelsTable.hidden = true;
        return;
    }

    // Default-tick the first three rows skipping the CSV time column.
    const timeCol = parseInt(els.csvTimeCol.value, 10) || 0;
    let ticked = 0;
    rows.forEach((r) => {
        const tr = document.createElement("tr");
        const cellUse = document.createElement("td");
        const cb = document.createElement("input");
        cb.type = "checkbox";
        const isTime = state.fileType === "csv" && r.idx === timeCol;
        if (!isTime && ticked < 3) {
            cb.checked = true;
            ticked++;
        }
        cb.disabled = isTime;
        cellUse.appendChild(cb);
        tr.appendChild(cellUse);

        const cellIdx = document.createElement("td");
        cellIdx.textContent = String(r.idx);
        tr.appendChild(cellIdx);

        const cellSrc = document.createElement("td");
        cellSrc.textContent = r.sourceName;
        tr.appendChild(cellSrc);

        const cellPlot = document.createElement("td");
        const plotInput = document.createElement("input");
        plotInput.type = "text";
        plotInput.value = r.sourceName;
        cellPlot.appendChild(plotInput);
        tr.appendChild(cellPlot);

        tbody.appendChild(tr);
    });
    els.channelsTable.hidden = false;
    els.channelsHint.hidden = true;
}

function collectChannelMap() {
    const checks = Array.from(els.channelsTbody.querySelectorAll("tr"));
    const selected = checks
        .map((tr, idx) => ({ tr, idx }))
        .filter(({ tr }) => tr.querySelector("input[type=checkbox]")?.checked)
        .slice(0, 3);
    if (selected.length < 3) return null;

    if (state.headers.kind === "tdms") {
        const info = state.headers.info;
        const groups = info.groups || [];
        const target = groups[1] || groups[0];
        const names = target.channels.map((c) => c.name);
        const indices = selected.map(({ tr }) => parseInt(tr.children[1].textContent, 10));
        return {
            group: target.name,
            x: names[indices[0]],
            y: names[indices[1]],
            z: names[indices[2]],
        };
    }
    if (state.headers.kind === "csv") {
        const indices = selected.map(({ tr }) => parseInt(tr.children[1].textContent, 10));
        return {
            time: parseInt(els.csvTimeCol.value, 10) || 0,
            xIdx: indices[0],
            yIdx: indices[1],
            zIdx: indices[2],
        };
    }
    // DAT is positional — channels are already X/Y/Z by file structure.
    return {};
}

function updateRunGate() {
    const ok = state.files.length > 0 && state.headers && !state.busy;
    els.runBtn.disabled = !ok;
    if (state.busy) {
        els.runHint.textContent = "Processing…";
    } else if (!state.files.length) {
        els.runHint.textContent = "Add files and select channels first.";
    } else if (!state.headers) {
        els.runHint.textContent = "Scanning headers…";
    } else {
        els.runHint.textContent = "";
    }
}

// ===== Stage 2: run processing =====
els.runBtn.addEventListener("click", async () => {
    if (state.busy) return;
    // Processed-CSV bypasses channel selection — it's already pk-pk output
    // with fixed columns.
    const isProcessedCsv = state.fileType === "processed_csv";
    let channelMap = null;
    if (!isProcessedCsv) {
        channelMap = collectChannelMap();
        if (channelMap === null && state.fileType !== "dat") {
            els.runHint.textContent = "Tick at least 3 channels in the table.";
            return;
        }
    }
    state.busy = true;
    state.results = null;
    updateRunGate();
    switchStage("process");
    els.statusLog.textContent = "";
    appendStatus("Starting…");
    els.stopBtn.disabled = false;
    els.progress.value = 0;
    els.progressText.textContent = `0 / ${state.files.length}`;

    const config = {
        lp_freq: parseFloat(els.lpFreq.value),
        pkpk_time: parseFloat(els.pkpkTime.value),
        pkpk_fs: parseFloat(els.pkpkFs.value),
        filter_kind: els.filterType.value,
    };

    try {
        const reply = await rpc(
            {
                type: "process",
                fileType: state.fileType,
                files: state.files,
                config,
                channelMap,
            },
            [],
            (p) => {
                els.progress.value = ((p.fileIndex + (p.stage === "done" ? 1 : 0.5)) / p.total) * 100;
                els.progressText.textContent = `${p.fileIndex + (p.stage === "done" ? 1 : 0)} / ${p.total}`;
                appendStatus(`[${p.fileIndex + 1}/${p.total}] ${p.stage}: ${p.name}`);
            },
        );
        els.progress.value = 100;
        appendStatus(reply.stopped ? "Stopped." : "Done.");
        state.results = reply.results;
        state.processedCsvMode = !!reply.processedCsvMode;
        // Pre-show the raw panel in disabled mode for processed-CSV so the
        // user knows drill-down isn't available without trial-and-error.
        if (state.processedCsvMode) {
            els.rawPanel.hidden = false;
            els.rawDisabledNote.hidden = false;
            setRawStatus("");
        } else {
            els.rawDisabledNote.hidden = true;
        }
        // Switch to plot.
        switchStage("plot");
        drawPkPkPlot(els.pkpkPlot, state.results, getVisibleSeries(), getLimitLine(), parseFloat(els.pkpkTime.value));
        attachBoxSelectHandler(els.pkpkPlot, onBoxSelect);
    } catch (err) {
        appendStatus(`Error: ${err.message}`);
        console.error(err);
    } finally {
        state.busy = false;
        els.stopBtn.disabled = true;
        updateRunGate();
    }
});

els.stopBtn.addEventListener("click", () => {
    worker.postMessage({ type: "stop" });
    appendStatus("Stop requested. Finishing current file…");
});

function appendStatus(line) {
    const stamp = new Date().toLocaleTimeString();
    els.statusLog.textContent += `[${stamp}] ${line}\n`;
    els.statusLog.scrollTop = els.statusLog.scrollHeight;
}

// ===== Stage 3: plot interactions =====
document.querySelectorAll(".series-toggles input").forEach((cb) => {
    cb.addEventListener("change", () => {
        if (state.results) drawPkPkPlot(els.pkpkPlot, state.results, getVisibleSeries(), getLimitLine(), parseFloat(els.pkpkTime.value));
    });
});
els.limitLine.addEventListener("input", () => {
    if (state.results) drawPkPkPlot(els.pkpkPlot, state.results, getVisibleSeries(), getLimitLine(), parseFloat(els.pkpkTime.value));
});

els.savePngBtn?.addEventListener("click", async () => {
    if (!state.results) return;
    await downloadPlot(els.pkpkPlot, "png");
});
els.saveSvgBtn?.addEventListener("click", async () => {
    if (!state.results) return;
    await downloadPlot(els.pkpkPlot, "svg");
});
els.saveCsvBtn?.addEventListener("click", () => {
    if (!state.results) return;
    downloadProcessedCsv(state.results, parseFloat(els.lpFreq.value), parseFloat(els.pkpkTime.value));
});

/// Write a Processed-CSV file in the same layout pkpk_processing.save_results_to_csv
/// produces, so the user can round-trip the analysis (export here → load via the
/// "Processed CSV" file type in this tool, or via the original Python tool).
///
/// Format:
///   # pkpkTime: <s>, lp_freq: <Hz>
///   Time,X_PkPk,Y_PkPk,Z_PkPk,XY_PkPk,XZ_PkPk,YZ_PkPk
///   <iso8601>,<x>,<y>,<z>,<xy>,<xz>,<yz>
function downloadProcessedCsv(results, lpFreq, pkpkTime) {
    const lines = [];
    const lpStr = Number.isFinite(lpFreq) && lpFreq > 0 ? lpFreq : "None";
    lines.push(`# pkpkTime: ${pkpkTime}, lp_freq: ${lpStr}`);
    lines.push("Time,X_PkPk,Y_PkPk,Z_PkPk,XY_PkPk,XZ_PkPk,YZ_PkPk");
    const n = results.times_us?.length || 0;
    for (let i = 0; i < n; i++) {
        const tMs = results.times_us[i] / 1000;
        const iso = formatIsoMicros(results.times_us[i]);
        const row = [
            iso,
            fmt(results.x_pkpk?.[i]),
            fmt(results.y_pkpk?.[i]),
            fmt(results.z_pkpk?.[i]),
            fmt(results.xy_pkpk?.[i]),
            fmt(results.xz_pkpk?.[i]),
            fmt(results.yz_pkpk?.[i]),
        ];
        // Skip rows where every value is NaN — same as the Python dropna().
        if (row.slice(1).every((v) => v === "")) continue;
        lines.push(row.join(","));
    }
    const blob = new Blob([lines.join("\n") + "\n"], { type: "text/csv" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "qdc_pkpk_results.csv";
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 5_000);
}

function fmt(v) {
    return Number.isFinite(v) ? v.toFixed(3) : "";
}

/// ISO 8601 with microsecond precision: matches what the original Python
/// pkpk_plotting.save_results_to_csv produces from a datetime64[ns] column
/// (modulo the trailing "Z" — we omit it since the Python output is naive).
///
/// `us` may be fractional (e.g. TDMS at 1024 Hz gives dt_us=976.5625), so we
/// integer-floor at every step or the seconds-fraction comes out malformed.
function formatIsoMicros(us) {
    const usInt = Math.trunc(us);                  // integer microseconds
    const ms = Math.trunc(usInt / 1000);
    const usFrac = ((usInt % 1000) + 1000) % 1000; // handle negative correctly
    const date = new Date(ms);
    if (Number.isNaN(date.getTime())) return "";
    const pad = (n, w) => String(n).padStart(w, "0");
    const sixDigit = pad(date.getUTCMilliseconds() * 1000 + usFrac, 6);
    return (
        `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1, 2)}-${pad(date.getUTCDate(), 2)} ` +
        `${pad(date.getUTCHours(), 2)}:${pad(date.getUTCMinutes(), 2)}:${pad(date.getUTCSeconds(), 2)}` +
        `.${sixDigit}`
    );
}

function getVisibleSeries() {
    const out = {};
    document.querySelectorAll(".series-toggles input").forEach((cb) => {
        out[cb.dataset.series] = cb.checked;
    });
    return out;
}
function getLimitLine() {
    const v = parseFloat(els.limitLine.value);
    return Number.isFinite(v) ? v : null;
}

// Tracks the most recent raw selection so the raw-plot zoom handler can
// re-request at higher resolution as the user zooms in.
let lastRawRange = null;
let rawRpcInFlight = 0;

// Persisted across re-renders so the user's legend toggles + "subtract
// mean" choice survive a zoom-triggered re-request. drawRawPlot reads
// these every time.
const rawViewState = {
    visible: { X: true, Y: true, Z: true },
    subtractMean: false,
};

// Cache the latest raw reply so the Subtract Mean toggle can re-render
// without re-requesting from the worker.
let lastRawReply = null;

async function requestRawWindow(startUs, endUs) {
    // Bump the in-flight counter — late replies from previous requests
    // are discarded so the user never sees stale data flash up.
    const seq = ++rawRpcInFlight;
    const targetBuckets = Math.max(800, Math.min(4000, Math.floor((els.rawPlot.clientWidth || 1200) * 2)));
    setRawStatus("Loading…");
    const timeoutMs = 15_000;
    try {
        const reply = await Promise.race([
            rpc({ type: "raw-window", startUs, endUs, nBuckets: targetBuckets }),
            new Promise((_, reject) =>
                setTimeout(() => reject(new Error(`worker didn't reply within ${timeoutMs / 1000}s`)), timeoutMs)
            ),
        ]);
        if (seq !== rawRpcInFlight) return; // a newer request superseded us
        if (reply.empty) {
            setRawStatus("");
            lastRawReply = null;
            // Reset to a friendly empty state but don't trample the plot div —
            // Plotly.react with empty traces clears the chart cleanly.
            drawRawPlot(els.rawPlot, { t_us: new Float64Array(), x_min: new Float64Array(), x_max: new Float64Array(), y_min: new Float64Array(), y_max: new Float64Array(), z_min: new Float64Array(), z_max: new Float64Array(), total_samples: 0 }, onRawRelayout, rawViewState, onRawLegendClick, updateMeanSummary);
            setRawStatus("(no raw samples in this range)");
            return;
        }
        lastRawReply = reply;
        drawRawPlot(els.rawPlot, reply, onRawRelayout, rawViewState, onRawLegendClick, updateMeanSummary);
        setRawStatus(`${reply.total_samples?.toLocaleString() ?? "?"} samples`);
    } catch (err) {
        if (seq !== rawRpcInFlight) return;
        console.error("[qdc] raw-window failed", err);
        setRawStatus(`failed: ${err.message}`);
    }
}

function setRawStatus(text) {
    if (els.rawStatus) els.rawStatus.textContent = text;
}

/// Plotly invokes this when the user clicks a legend entry; we update
/// rawViewState so the next re-render (after zoom) respects the toggle.
function onRawLegendClick(name, nowVisible) {
    rawViewState.visible[name] = nowVisible;
}

function updateMeanSummary(summary) {
    if (!els.meanSummary) return;
    els.meanSummary.textContent = summary || "";
}

els.subtractMean?.addEventListener("change", () => {
    rawViewState.subtractMean = !!els.subtractMean.checked;
    if (lastRawReply) {
        drawRawPlot(els.rawPlot, lastRawReply, onRawRelayout, rawViewState, onRawLegendClick, updateMeanSummary);
    }
});

async function onBoxSelect(range) {
    console.log("[qdc] onBoxSelect", range);
    if (state.processedCsvMode) {
        // Processed CSV has no underlying raw — show the disabled note instead
        // of dispatching a worker request that would return empty.
        els.rawPanel.hidden = false;
        els.rawDisabledNote.hidden = false;
        setRawStatus("");
        return;
    }
    els.rawPanel.hidden = false;
    els.rawDisabledNote.hidden = true;
    lastRawRange = { startUs: range.start_us, endUs: range.end_us };
    await requestRawWindow(range.start_us, range.end_us);
}

// Called by drawRawPlot when the user zooms the raw chart. We re-request
// with the narrower window so resolution improves as you zoom in.
async function onRawRelayout(zoomedStartUs, zoomedEndUs) {
    if (!Number.isFinite(zoomedStartUs) || !Number.isFinite(zoomedEndUs)) return;
    if (!lastRawRange) return;
    // Clamp the new request to within the original selection so the user
    // can't zoom into samples we don't actually have cached.
    const startUs = Math.max(zoomedStartUs, lastRawRange.startUs);
    const endUs = Math.min(zoomedEndUs, lastRawRange.endUs);
    if (endUs <= startUs) return;
    await requestRawWindow(startUs, endUs);
}
