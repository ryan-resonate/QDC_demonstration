// Plotly wiring for the pk-pk results plot.
// Loaded as a CDN <script> in index.html; we access window.Plotly here.

const SERIES_DEFS = [
    { key: "x_pkpk", label: "X" },
    { key: "y_pkpk", label: "Y" },
    { key: "z_pkpk", label: "Z" },
    { key: "xy_pkpk", label: "XY" },
    { key: "xz_pkpk", label: "XZ" },
    { key: "yz_pkpk", label: "ZY" },
    // Combined XZY (= sqrt(x²+y²+z²)) is computed on demand here so we
    // don't bloat the worker payload with a duplicate buffer.
    { key: "xzy_pkpk", label: "XZY", computed: true },
];

function timeArrayFromUs(times_us) {
    // Plotly accepts JS Date objects on the x-axis. Going through Date
    // means we lose sub-millisecond precision, which is fine for pk-pk
    // (the values are aggregated over `pkpk_time` seconds anyway).
    const out = new Array(times_us.length);
    for (let i = 0; i < times_us.length; i++) {
        out[i] = new Date(times_us[i] / 1000);
    }
    return out;
}

function computeXzy(results) {
    const x = results.x_pkpk, y = results.y_pkpk, z = results.z_pkpk;
    const n = x.length;
    const out = new Float64Array(n);
    for (let i = 0; i < n; i++) {
        out[i] = Math.sqrt(x[i] * x[i] + y[i] * y[i] + z[i] * z[i]);
    }
    return out;
}

export function drawPkPkPlot(container, results, visibility, limitLine, pkpkTime) {
    if (!container || !results || !window.Plotly) return;
    const times = timeArrayFromUs(results.times_us);
    const traces = [];
    for (const def of SERIES_DEFS) {
        if (!visibility[def.key]) continue;
        const y = def.computed ? computeXzy(results) : results[def.key];
        if (!y || !y.length) continue;
        traces.push({
            x: times,
            y: Array.from(y),
            name: def.label,
            mode: "lines",
            type: "scatter",
            line: { width: 1.2 },
        });
    }

    const shapes = [];
    if (limitLine !== null && limitLine !== undefined && Number.isFinite(limitLine)) {
        shapes.push({
            type: "line",
            xref: "paper", x0: 0, x1: 1,
            yref: "y", y0: limitLine, y1: limitLine,
            line: { color: "#b71c1c", width: 1.5, dash: "dash" },
        });
    }

    const layout = {
        margin: { l: 70, r: 16, t: 10, b: 50 },
        xaxis: { title: { text: "Time" }, type: "date" },
        yaxis: { title: { text: yAxisLabel(results.unit, pkpkTime) }, rangemode: "tozero" },
        legend: { orientation: "h", y: -0.18 },
        shapes,
        dragmode: "select",
        showlegend: true,
    };

    const config = {
        displayModeBar: true,
        responsive: true,
        // Scroll-wheel zoom on the chart, in addition to Plotly's
        // toolbar zoom controls.
        scrollZoom: true,
        // The Plotly toolbar has its own image-download icon, but it
        // defaults to PNG only and uses an opaque filename. The dedicated
        // Save buttons in app.js give explicit PNG + SVG with the right
        // filename, so this stays the user's affordance.
        toImageButtonOptions: { format: "png", filename: "qdc_pkpk", scale: 2 },
    };
    window.Plotly.react(container, traces, layout, config);
}

function yAxisLabel(unit, pkpkTime) {
    const window = formatPkPkWindow(pkpkTime);
    if (unit) {
        return `Pk-Pk amplitude (${unit})${window ? ` — ${window}` : ""}`;
    }
    return `Pk-Pk amplitude${window ? ` — ${window}` : ""}`;
}
function formatPkPkWindow(s) {
    if (!Number.isFinite(s)) return "";
    if (s < 60) return `${s} s window`;
    const m = s / 60;
    return Number.isInteger(m) ? `${m} min window` : `${m.toFixed(1)} min window`;
}

/// Download the current pk-pk chart as PNG or SVG using Plotly's exporter.
export async function downloadPlot(container, format, baseName = "qdc_pkpk") {
    if (!container || !window.Plotly) return;
    const width = container.clientWidth || 1200;
    const height = container.clientHeight || 480;
    await window.Plotly.downloadImage(container, {
        format,                 // "png" | "svg"
        filename: baseName,
        width, height,
        scale: format === "png" ? 2 : 1,
    });
}

/// Render the raw-data drill-down chart. Shows X/Y/Z either as a clean
/// per-sample line (when there's roughly 1 sample per bucket — i.e. the
/// user has zoomed in tightly) or as a min/max envelope (when many samples
/// fall in each bucket — the standard down-sampling view).
///
/// `viewState` carries the user's persistent choices across re-renders
/// (legend toggles + "Subtract mean"). `onLegendClick(name, nowVisible)`
/// is invoked when the user toggles a channel via the legend so app.js
/// can update viewState. `onMeanSummary(text)` reports the mean offsets
/// so the caller can show them next to the toggle.
///
/// `onRelayout(startUs, endUs)` is called when the user zooms/pans inside
/// the plot, so the worker can re-request at the new (narrower) resolution.
export function drawRawPlot(container, raw, onRelayout, viewState, onLegendClick, onMeanSummary) {
    if (!container || !window.Plotly) return;
    const nBuckets = raw.t_us?.length ?? 0;
    const total = raw.total_samples ?? 0;
    const samplesPerBucket = nBuckets > 0 ? total / nBuckets : 0;
    // If samples-per-bucket is small (≤ ~2), min==max for most buckets
    // (Rust decimation falls through to raw samples in that regime). In
    // that case bands look weird — draw a single line instead.
    const drawAsLine = samplesPerBucket <= 2;

    const times = new Array(nBuckets);
    for (let i = 0; i < nBuckets; i++) {
        times[i] = new Date(raw.t_us[i] / 1000);
    }

    const channels = [
        { name: "X", min: raw.x_min, max: raw.x_max, color: "#1565c0" },
        { name: "Y", min: raw.y_min, max: raw.y_max, color: "#2e7d32" },
        { name: "Z", min: raw.z_min, max: raw.z_max, color: "#b26a00" },
    ];

    // Per-channel mean (computed from the midpoint of each bucket so it's
    // a sensible approximation of the true sample mean even when we're
    // looking at decimated bands).
    const visibility = viewState?.visible || { X: true, Y: true, Z: true };
    const subtractMean = !!viewState?.subtractMean;
    const means = {};
    const summaryParts = [];
    for (const ch of channels) {
        const n = ch.min?.length ?? 0;
        if (n === 0) { means[ch.name] = 0; continue; }
        let acc = 0;
        for (let i = 0; i < n; i++) acc += (ch.min[i] + ch.max[i]) * 0.5;
        means[ch.name] = acc / n;
        if (subtractMean) {
            summaryParts.push(`${ch.name}: ${means[ch.name].toFixed(2)}`);
        }
    }
    onMeanSummary?.(subtractMean ? `subtracted means → ${summaryParts.join(", ")}` : "");

    // Apply mean subtraction lazily.
    const adjusted = (arr, name) => {
        if (!subtractMean || !arr) return arr;
        const m = means[name];
        const out = new Array(arr.length);
        for (let i = 0; i < arr.length; i++) out[i] = arr[i] - m;
        return out;
    };

    const visFlag = (name) => (visibility[name] === false ? "legendonly" : true);

    const traces = [];
    for (const ch of channels) {
        const minAdj = adjusted(ch.min, ch.name);
        const maxAdj = adjusted(ch.max, ch.name);
        if (drawAsLine) {
            traces.push({
                x: times, y: Array.from(minAdj || []),
                name: ch.name, legendgroup: ch.name,
                mode: "lines", type: "scatter",
                line: { width: 1, color: ch.color },
                visible: visFlag(ch.name),
            });
        } else {
            traces.push({
                x: times, y: Array.from(maxAdj || []),
                name: ch.name, legendgroup: ch.name,
                mode: "lines", type: "scatter",
                line: { width: 1, color: ch.color },
                visible: visFlag(ch.name),
            });
            traces.push({
                x: times, y: Array.from(minAdj || []),
                name: ch.name, legendgroup: ch.name, showlegend: false,
                mode: "lines", type: "scatter",
                line: { width: 1, color: ch.color },
                fill: "tonexty", fillcolor: hexToRgba(ch.color, 0.18),
                visible: visFlag(ch.name),
            });
        }
    }

    const totalNote = total
        ? `${total.toLocaleString()} raw samples → ${nBuckets} ${drawAsLine ? "points" : "buckets"}`
        : "";

    const layout = {
        margin: { l: 70, r: 16, t: 10, b: 50 },
        xaxis: { title: { text: `Time  (${totalNote})` }, type: "date" },
        yaxis: { title: { text: raw.unit ? `Raw signal (${raw.unit})` : "Raw signal" } },
        legend: { orientation: "h", y: -0.18 },
        showlegend: true,
        dragmode: "zoom",
    };
    const config = {
        displayModeBar: true,
        responsive: true,
        scrollZoom: true,
        toImageButtonOptions: { format: "png", filename: "qdc_raw", scale: 2 },
    };
    window.Plotly.react(container, traces, layout, config);

    // Wire the zoom/pan + legend-click callbacks once per container.
    // Plotly's `on` is additive, so we only attach if we haven't already.
    if (onRelayout && !container._qdcRelayoutAttached) {
        container._qdcRelayoutAttached = true;
        container.on("plotly_relayout", (ev) => {
            // The interesting fields are xaxis.range[0] / xaxis.range[1]
            // (set during box-zoom and scroll-zoom). For "Reset axes"
            // Plotly sends xaxis.autorange instead — ignore those.
            const lo = ev?.["xaxis.range[0]"];
            const hi = ev?.["xaxis.range[1]"];
            if (lo == null || hi == null) return;
            const loUs = toUs(lo);
            const hiUs = toUs(hi);
            onRelayout(loUs, hiUs);
        });
    }
    if (onLegendClick && !container._qdcLegendAttached) {
        container._qdcLegendAttached = true;
        container.on("plotly_restyle", (ev) => {
            // Read back visibility per channel after Plotly handled the toggle.
            // `container.data` is Plotly's authoritative live trace state.
            const data = container.data || [];
            const seen = {};
            for (const trace of data) {
                if (!trace.name || seen[trace.name] !== undefined) continue;
                seen[trace.name] = trace.visible === "legendonly" ? false : true;
            }
            for (const name of Object.keys(seen)) {
                onLegendClick(name, seen[name]);
            }
        });
    }
}

function toUs(v) {
    if (typeof v === "number") return v * 1000;
    if (v instanceof Date) return v.getTime() * 1000;
    const parsed = new Date(v);
    return parsed.getTime() * 1000;
}

function hexToRgba(hex, alpha) {
    const r = parseInt(hex.slice(1, 3), 16);
    const g = parseInt(hex.slice(3, 5), 16);
    const b = parseInt(hex.slice(5, 7), 16);
    return `rgba(${r},${g},${b},${alpha})`;
}

/// Box-select callback. Plotly's `plotly_selected` event fires when the
/// user releases a box selection; `range.x` can be numbers (ms), ISO
/// strings, or Date objects depending on the version + axis type, so we
/// normalise carefully.
export function attachBoxSelectHandler(container, onBoxSelect) {
    if (!container || !container.on) return;
    container.on("plotly_selected", (ev) => {
        console.debug("[qdc] plotly_selected fired", ev);
        if (!ev) {
            console.debug("[qdc] plotly_selected: no event payload (likely click without drag)");
            return;
        }
        if (!ev.range || !ev.range.x) {
            console.debug("[qdc] plotly_selected: no range.x — probably lasso mode or click. Switch the Plotly toolbar to Box Select.");
            return;
        }
        const xs = ev.range.x;
        if (xs.length !== 2) {
            console.warn("[qdc] plotly_selected: unexpected range.x shape", xs);
            return;
        }
        const startMs = toMs(xs[0]);
        const endMs = toMs(xs[1]);
        if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) {
            console.warn("[qdc] plotly_selected: couldn't parse range.x to ms", xs);
            return;
        }
        const lo = Math.min(startMs, endMs);
        const hi = Math.max(startMs, endMs);
        onBoxSelect({
            start_us: Math.round(lo * 1000),
            end_us: Math.round(hi * 1000),
        });
    });
}

function toMs(v) {
    if (typeof v === "number") return v;
    if (v instanceof Date) return v.getTime();
    // Strings: ISO 8601 ("2025-03-28T01:38:00.000") or other Date-parsable.
    const parsed = new Date(v);
    return parsed.getTime();
}
