//! JS-callable surface for the WASM build.
//!
//! Loose, JSON-friendly shapes — wasm-bindgen can pass JsValue containing
//! plain objects / typed arrays back and forth, which keeps the worker.js
//! glue simple. Heavy data (sample buffers) is returned as Float64Array
//! views over the WASM linear memory, so JS gets zero-copy access.

use wasm_bindgen::prelude::*;

use crate::decimation::min_max_decimate;
use crate::processing::{DataBlock, FilterKind, Processor, ProcessingConfig};
use crate::readers;
use crate::tdms;

/// Min/max-per-bucket decimation. Returns a JS object with two Float64Arrays
/// `{ mins, maxs }` of length `n_buckets` (or fewer if `samples.len() < n_buckets`).
#[wasm_bindgen]
pub fn decimate_minmax(samples: &[f64], n_buckets: usize) -> JsValue {
    let (mins, maxs) = min_max_decimate(samples, n_buckets);
    let obj = js_sys::Object::new();
    let put = |k: &str, v: &[f64]| {
        let arr = js_sys::Float64Array::new_with_length(v.len() as u32);
        arr.copy_from(v);
        js_sys::Reflect::set(&obj, &k.into(), &arr).unwrap();
    };
    put("mins", &mins);
    put("maxs", &maxs);
    obj.into()
}

// ===== TDMS =====

/// Parse a TDMS file (entire bytes) and return a JS object describing its
/// structure: `{ properties: {...}, groups: [{ name, properties, channels: [{ name, sample_count, dt_seconds, start_time_us, unit_string }] }] }`.
///
/// Sample data is NOT included here — call `tdms_channel_samples` to fetch
/// the f64 buffer for a specific channel. This lets the UI cheaply scan
/// headers without paying the channel-allocation cost.
#[wasm_bindgen]
pub fn tdms_inspect(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let file = tdms::parse_tdms(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;

    let props = file
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), property_to_js(v)))
        .collect::<Vec<_>>();

    let groups: Vec<JsValue> = file
        .groups
        .iter()
        .map(|g| {
            let channels: Vec<JsValue> = g
                .channels
                .iter()
                .map(|c| {
                    let dt = c
                        .properties
                        .get("wf_increment")
                        .or_else(|| c.properties.get("dt"))
                        .and_then(|p| p.as_f64())
                        .unwrap_or(f64::NAN);
                    let start_us = c
                        .properties
                        .get("wf_start_time")
                        .and_then(|p| p.as_timestamp())
                        .map(|t| t.us_since_epoch as f64)
                        .or_else(|| {
                            c.properties
                                .get("t0")
                                .and_then(|p| p.as_f64())
                                .map(|s| s * 1_000_000.0)
                        })
                        .unwrap_or(f64::NAN);
                    let unit = c
                        .properties
                        .get("unit_string")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();

                    let obj = js_sys::Object::new();
                    js_sys::Reflect::set(&obj, &"name".into(), &c.name.clone().into()).unwrap();
                    js_sys::Reflect::set(&obj, &"sample_count".into(), &(c.data.len() as f64).into()).unwrap();
                    js_sys::Reflect::set(&obj, &"dt_seconds".into(), &dt.into()).unwrap();
                    js_sys::Reflect::set(&obj, &"start_time_us".into(), &start_us.into()).unwrap();
                    js_sys::Reflect::set(&obj, &"unit_string".into(), &unit.into()).unwrap();
                    obj.into()
                })
                .collect();

            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"name".into(), &g.name.clone().into()).unwrap();
            let chs_arr = js_sys::Array::new();
            for c in channels {
                chs_arr.push(&c);
            }
            js_sys::Reflect::set(&obj, &"channels".into(), &chs_arr).unwrap();
            obj.into()
        })
        .collect();

    let root = js_sys::Object::new();
    let props_obj = js_sys::Object::new();
    for (k, v) in props {
        js_sys::Reflect::set(&props_obj, &k.into(), &v).unwrap();
    }
    js_sys::Reflect::set(&root, &"properties".into(), &props_obj).unwrap();
    let groups_arr = js_sys::Array::new();
    for g in groups {
        groups_arr.push(&g);
    }
    js_sys::Reflect::set(&root, &"groups".into(), &groups_arr).unwrap();
    Ok(root.into())
}

fn property_to_js(v: &tdms::PropertyValue) -> JsValue {
    use tdms::PropertyValue::*;
    match v {
        Void => JsValue::NULL,
        Bool(b) => JsValue::from_bool(*b),
        I8(x) => JsValue::from_f64(*x as f64),
        I16(x) => JsValue::from_f64(*x as f64),
        I32(x) => JsValue::from_f64(*x as f64),
        I64(x) => JsValue::from_f64(*x as f64),
        U8(x) => JsValue::from_f64(*x as f64),
        U16(x) => JsValue::from_f64(*x as f64),
        U32(x) => JsValue::from_f64(*x as f64),
        U64(x) => JsValue::from_f64(*x as f64),
        F32(x) => JsValue::from_f64(*x as f64),
        F64(x) => JsValue::from_f64(*x),
        String(s) => JsValue::from_str(s),
        Timestamp(t) => {
            // Surface as an object: { kind: 'timestamp', us_since_epoch }.
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"kind".into(), &"timestamp".into()).unwrap();
            js_sys::Reflect::set(&obj, &"us_since_epoch".into(), &(t.us_since_epoch as f64).into()).unwrap();
            obj.into()
        }
    }
}

/// Parse a TDMS file and return all the sample data for the named channel
/// inside the named group, as a `Float64Array`.
#[wasm_bindgen]
pub fn tdms_channel_samples(bytes: &[u8], group: &str, channel: &str) -> Result<js_sys::Float64Array, JsValue> {
    let file = tdms::parse_tdms(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
    let ch = file
        .channel(group, channel)
        .ok_or_else(|| JsValue::from_str(&format!("channel {group}/{channel} not found")))?;
    let arr = js_sys::Float64Array::new_with_length(ch.data.len() as u32);
    arr.copy_from(&ch.data);
    Ok(arr)
}

// ===== CSV / DAT / Processed CSV =====

#[wasm_bindgen]
pub fn csv_inspect_headers(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let headers = readers::inspect_csv_headers(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
    let arr = js_sys::Array::new();
    for h in headers {
        arr.push(&h.into());
    }
    Ok(arr.into())
}

#[wasm_bindgen]
pub fn dat_inspect_headers(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let headers = readers::inspect_dat_headers(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
    let arr = js_sys::Array::new();
    for h in headers {
        arr.push(&h.into());
    }
    Ok(arr.into())
}

/// Load a processed-CSV (already computed pk-pk results from a previous
/// run) and return the columns as Float64Arrays. The UI consumes this
/// directly into the plot stage without going through the rolling-pk-pk
/// pipeline.
#[wasm_bindgen]
pub fn processed_csv_load(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let parsed = readers::read_processed_csv(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
    let obj = js_sys::Object::new();
    let push = |k: &str, v: &[f64]| {
        let arr = js_sys::Float64Array::new_with_length(v.len() as u32);
        arr.copy_from(v);
        js_sys::Reflect::set(&obj, &k.into(), &arr).unwrap();
    };
    // Times: convert ns → us (matches the JS-side times_us convention).
    let times_us: Vec<f64> = parsed.times_ns.iter().map(|&ns| ns as f64 / 1000.0).collect();
    push("times_us", &times_us);
    push("x_pkpk", &parsed.x_pkpk);
    push("y_pkpk", &parsed.y_pkpk);
    push("z_pkpk", &parsed.z_pkpk);
    push("xy_pkpk", &parsed.xy_pkpk);
    push("xz_pkpk", &parsed.xz_pkpk);
    push("yz_pkpk", &parsed.yz_pkpk);
    js_sys::Reflect::set(&obj, &"metadata".into(), &parsed.metadata.into()).unwrap();
    Ok(obj.into())
}

// ===== Streaming processor =====

/// Wraps `Processor` for JS. Use lifecycle: `new_processor(...)`, repeatedly
/// `processor_feed_*`, then `processor_finish` to retrieve the result.
#[wasm_bindgen]
pub struct WasmProcessor {
    inner: Processor,
    n_files_seen: usize,
}

#[wasm_bindgen]
impl WasmProcessor {
    /// Build a processor from a small JS config object:
    ///   { lp_freq, pkpk_time, pkpk_fs, filter_kind }  // filter_kind: "butterworth" | "bessel" | "rc_cascade" | "none"
    #[wasm_bindgen(constructor)]
    pub fn new(config: JsValue) -> Result<WasmProcessor, JsValue> {
        let cfg = parse_config(&config)?;
        Ok(WasmProcessor { inner: Processor::new(cfg), n_files_seen: 0 })
    }

    /// Feed a TDMS file (entire bytes). The processor extracts X/Y/Z from
    /// the named group + channels and runs the kernel.
    #[wasm_bindgen]
    pub fn feed_tdms(
        &mut self,
        bytes: &[u8],
        group: &str,
        ch_x: &str,
        ch_y: &str,
        ch_z: &str,
    ) -> Result<(), JsValue> {
        let file = tdms::parse_tdms(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
        let x = file.channel(group, ch_x).ok_or_else(|| JsValue::from_str("channel X not found"))?;
        let y = file.channel(group, ch_y).ok_or_else(|| JsValue::from_str("channel Y not found"))?;
        let z = file.channel(group, ch_z).ok_or_else(|| JsValue::from_str("channel Z not found"))?;
        let dt = x
            .properties
            .get("wf_increment")
            .or_else(|| x.properties.get("dt"))
            .and_then(|p| p.as_f64())
            .ok_or_else(|| JsValue::from_str("channel X missing wf_increment/dt"))?;
        let start_us = x
            .properties
            .get("wf_start_time")
            .and_then(|p| p.as_timestamp())
            .map(|t| t.us_since_epoch)
            .or_else(|| {
                x.properties
                    .get("t0")
                    .and_then(|p| p.as_f64())
                    .map(|s| (s * 1_000_000.0) as i64)
            })
            .ok_or_else(|| JsValue::from_str("channel X missing start time"))?;

        let dt_ns = (dt * 1e9) as i64;
        let times_ns: Vec<i64> = (0..x.data.len() as i64)
            .map(|i| start_us * 1000 + i * dt_ns)
            .collect();

        // Unit-string scaling: pkpk_processing converts mT→nT (×1e6) and
        // uT→nT (×1e3); native nT stays.
        let scale = x.properties.get("unit_string").and_then(|p| p.as_str())
            .map(unit_scale_factor).unwrap_or(1.0);

        let block = DataBlock {
            times_ns,
            x: maybe_scale(&x.data, scale),
            y: maybe_scale(&y.data, scale),
            z: maybe_scale(&z.data, scale),
            dt,
        };
        self.inner.feed(block);
        self.n_files_seen += 1;
        Ok(())
    }

    #[wasm_bindgen]
    pub fn feed_csv(
        &mut self,
        bytes: &[u8],
        time_col: usize,
        x_col: usize,
        y_col: usize,
        z_col: usize,
    ) -> Result<(), JsValue> {
        let loaded = readers::read_csv(bytes, time_col, x_col, y_col, z_col)
            .map_err(|e| JsValue::from_str(&format!("{e}")))?;
        self.inner.feed(DataBlock {
            times_ns: loaded.times_ns,
            x: loaded.x,
            y: loaded.y,
            z: loaded.z,
            dt: loaded.dt,
        });
        self.n_files_seen += 1;
        Ok(())
    }

    #[wasm_bindgen]
    pub fn feed_dat(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let loaded = readers::read_dat(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
        self.inner.feed(DataBlock {
            times_ns: loaded.times_ns,
            x: loaded.x,
            y: loaded.y,
            z: loaded.z,
            dt: loaded.dt,
        });
        self.n_files_seen += 1;
        Ok(())
    }

    /// Drain the processor and return the results as a JS object containing
    /// Float64Arrays. Subsequent calls panic — call exactly once.
    #[wasm_bindgen]
    pub fn finish(self) -> JsValue {
        let r = self.inner.finish();
        let obj = js_sys::Object::new();
        // Times: store as Float64Array of microseconds (loses sub-microsecond
        // precision but matches JS Date resolution which the UI uses anyway).
        let times_us: Vec<f64> = r.times_ns.iter().map(|&ns| ns as f64 / 1000.0).collect();
        let push = |k: &str, v: &[f64]| {
            let arr = js_sys::Float64Array::new_with_length(v.len() as u32);
            arr.copy_from(v);
            js_sys::Reflect::set(&obj, &k.into(), &arr).unwrap();
        };
        push("times_us", &times_us);
        push("x_pkpk", &r.x_pkpk);
        push("y_pkpk", &r.y_pkpk);
        push("z_pkpk", &r.z_pkpk);
        push("xy_pkpk", &r.xy_pkpk);
        push("xz_pkpk", &r.xz_pkpk);
        push("yz_pkpk", &r.yz_pkpk);
        obj.into()
    }
}

fn parse_config(value: &JsValue) -> Result<ProcessingConfig, JsValue> {
    let get = |k: &str| -> Result<JsValue, JsValue> {
        js_sys::Reflect::get(value, &k.into())
    };
    let lp_freq = get("lp_freq")?.as_f64().unwrap_or(5.0);
    let pkpk_time = get("pkpk_time")?.as_f64().unwrap_or(120.0);
    let pkpk_fs = get("pkpk_fs")?.as_f64().unwrap_or(1.0);
    let filter_kind = get("filter_kind")?.as_string().unwrap_or_else(|| "butterworth".into());
    Ok(ProcessingConfig {
        lp_freq,
        pkpk_time,
        pkpk_fs,
        filter_kind: FilterKind::from_str(&filter_kind),
    })
}

fn unit_scale_factor(unit: &str) -> f64 {
    match unit.trim() {
        "mT" => 1e6,
        "uT" | "µT" => 1e3,
        _ => 1.0, // nT, m/s^2, empty all pass through unchanged
    }
}

fn maybe_scale(v: &[f64], scale: f64) -> Vec<f64> {
    if scale == 1.0 {
        v.to_vec()
    } else {
        v.iter().map(|x| x * scale).collect()
    }
}
