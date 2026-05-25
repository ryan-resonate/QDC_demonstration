# QDC Field Log Analyser — web demo

Browser-based proof-of-concept of the [PkPkAnalysis](../Initial%20resources/quasi%20DC/) desktop tool. Loads TDMS/CSV/Processed-CSV/DAT field logs, applies the configured low-pass filter, computes rolling pk-pk, and lets you interrogate the result interactively — including drilling into the raw underlying samples for any selected time window.

Runs entirely in the browser. Hosted on GitHub Pages. Your data files never leave your machine.

## How it's built

| Layer | Stack |
| --- | --- |
| Numerics | Rust → WebAssembly (`crates/qdc-core`, built with `wasm-pack`) |
| UI | Vanilla JS + Plotly.js (CDN) |
| File I/O | Browser `File` API, byte-range slices via `File.slice()` |
| Threading | All numerics run in a Web Worker; UI thread stays free |

The Rust core ports `pkpk_processing.py` and the relevant bits of `pkpk_plotting.py`. Numerical results are validated against the Python reference via the synthetic regression checks shipped with the original tool — target equivalence is ≤1e-12 relative error.

## Layout

```
qdc-web/
├── index.html, app.js, worker.js, plotting.js, styles.css   # static site
├── pkg/                                                     # built WASM (committed for Pages)
│   ├── qdc_core_bg.wasm
│   └── qdc_core.js
├── crates/qdc-core/                                         # Rust source
│   ├── Cargo.toml
│   └── src/lib.rs (+ module files added per task)
├── build.ps1, build.sh                                      # wraps wasm-pack
├── .nojekyll, README.md, .gitignore
```

## Building

You only need this if you're hacking on the Rust core. End-users of the deployed page do not.

```powershell
# Windows
pwsh ./build.ps1            # release build
pwsh ./build.ps1 -Dev       # faster dev build
```

```bash
# macOS / Linux
./build.sh
./build.sh --dev
```

Prereqs: [Rust](https://rustup.rs) ≥ 1.95 with the `wasm32-unknown-unknown` target, and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/installer/).

## Running locally

Module workers and the WASM file need to be served over HTTP (`file://` won't work). Any static server is fine:

```bash
# Python
python -m http.server 8000

# or Node
npx http-server -p 8000
```

Then open `http://localhost:8000` from the `qdc-web/` folder.

## GitHub Pages

1. Push the repo with `qdc-web/` at the deployment root.
2. In repo settings → Pages, set source to `main` branch / root (or `/docs`, configured accordingly).
3. The `.nojekyll` file disables Jekyll so `_*` paths in `pkg/` are served verbatim.

## Status

End-to-end working: load TDMS → process → pk-pk plot → box-select for raw drill-down. Verified against a real 7.4 MB cDAQ FlexLogger TDMS file.

- [x] Scaffold + wasm-pack pipeline
- [x] TDMS reader (validated byte-exact vs nptdms on Flexlab / FlexLogger / dataflex flavours)
- [x] CSV + DAT + Processed-CSV readers
- [x] Filter application — sosfiltfilt + filtfilt + RC cascade (matches scipy to ~1e-9, mostly machine epsilon)
- [x] Butterworth filter design (SOS coefficients match scipy to ~3.5e-16)
- [x] Streaming processing loop with residual buffer + monotonic-deque rolling pk-pk
- [x] WASM bindings: `tdms_inspect`, `tdms_channel_samples`, `csv/dat_inspect_headers`, `WasmProcessor`, `decimate_minmax`
- [x] 3-stage UI: Load → Process → Plot
- [x] Stop button (cooperative — between-file granularity)
- [x] Pk-pk Plotly chart with series toggles + limit line
- [x] Raw-data drill-down with min/max-per-bucket decimation
- [ ] Bessel filter design (not yet ported — falls back to no filter)
- [ ] Mid-file cancellation (currently between files only)
- [ ] Save plot / export CSV / export summary CSV buttons (not yet wired)

## Quick smoke test

A sample TDMS file is committed under `sample-data/sample.tdms` (7.4 MB) so the
deployed demo works without needing the user to source their own data:

1. Open the deployed page (or `python -m http.server 8000` locally).
2. *Add files…* → pick `sample-data/sample.tdms` (or drag it in).
3. *Process →* — completes in a few seconds.
4. On the Plot tab, click the box-select tool in the Plotly toolbar and drag
   across a region. The Raw Samples panel appears below with min/max
   envelopes of the underlying field data.
