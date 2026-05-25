// qdc-core: Rust → WebAssembly core for the QDC processing web demo.
//
// The pure Python reference implementation is in
// `Initial resources/quasi DC/pkpk_processing.py` and
// `Initial resources/quasi DC/pkpk_plotting.py`.
//
// Modules are added incrementally as functionality is ported.

use wasm_bindgen::prelude::*;

pub mod bindings;
pub mod decimation;
pub mod filter;
pub mod filter_design;
pub mod processing;
pub mod readers;
pub mod tdms;

// Sanity-check entry point used while we wire up the build pipeline.
// Returns a version string to confirm the WASM module loaded and JS
// can call into it.
#[wasm_bindgen]
pub fn qdc_core_version() -> String {
    format!("qdc-core {}", env!("CARGO_PKG_VERSION"))
}
