//! Rolling pk-pk processing — port of `pkpk_processing.run_processing`.
//!
//! The kernel: optionally low-pass filter the input, then compute rolling
//! pk-pk (max − min) over a sliding window of `pkpk_time` seconds, stepping
//! by `fs / pkpk_fs` samples between outputs.
//!
//! Streaming model matches the Python: a `Processor` holds a residual buffer
//! between calls so multi-file inputs are handled identically to single-file
//! inputs (no edge artefacts at file boundaries).
//!
//! Gap detection (`detect_gaps`) is not yet implemented — the loop assumes
//! continuous time across files, which is what the synthetic fixture and
//! the most common field-data captures look like. Real datasets with
//! interleaved silence will need that logic added before they're processed
//! correctly.

use crate::filter::{rc_cascade, sosfiltfilt};
use crate::filter_design::butter_lowpass_sos;

#[derive(Debug, Clone, Copy)]
pub enum FilterKind {
    None,
    Butterworth,
    /// Bessel is not yet ported (filter_design has only butter); selecting
    /// this falls back to no filter and emits a warning at the call site.
    Bessel,
    RcCascade,
}

impl FilterKind {
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "butterworth" | "butter" => FilterKind::Butterworth,
            "bessel" => FilterKind::Bessel,
            "rc_cascade" | "rc" => FilterKind::RcCascade,
            _ => FilterKind::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessingConfig {
    pub lp_freq: f64,
    pub pkpk_time: f64,
    pub pkpk_fs: f64,
    pub filter_kind: FilterKind,
}

#[derive(Debug, Default)]
pub struct ProcessingResults {
    pub times_ns: Vec<i64>,
    pub x_pkpk: Vec<f64>,
    pub y_pkpk: Vec<f64>,
    pub z_pkpk: Vec<f64>,
    pub xy_pkpk: Vec<f64>,
    pub xz_pkpk: Vec<f64>,
    pub yz_pkpk: Vec<f64>,
}

/// One contiguous block of time-aligned samples handed to the processor.
/// Source-format readers convert their inputs to this.
#[derive(Debug)]
pub struct DataBlock {
    pub times_ns: Vec<i64>,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub z: Vec<f64>,
    pub dt: f64,
}

pub struct Processor {
    cfg: ProcessingConfig,
    /// Residual carried between blocks: last n_points − 1 samples so the
    /// next block's sliding window starts from the correct prior state.
    residual_times: Vec<i64>,
    residual_x: Vec<f64>,
    residual_y: Vec<f64>,
    residual_z: Vec<f64>,
    /// Most recently seen dt — used to size the residual buffer.
    last_dt: Option<f64>,
    results: ProcessingResults,
}

impl Processor {
    pub fn new(cfg: ProcessingConfig) -> Self {
        Self {
            cfg,
            residual_times: Vec::new(),
            residual_x: Vec::new(),
            residual_y: Vec::new(),
            residual_z: Vec::new(),
            last_dt: None,
            results: ProcessingResults::default(),
        }
    }

    /// Feed one block of contiguous samples. Internally accumulates with
    /// residual from the prior call before running the kernel.
    pub fn feed(&mut self, block: DataBlock) {
        let dt = block.dt;
        self.last_dt = Some(dt);

        // Concatenate residual + new block.
        let mut times = std::mem::take(&mut self.residual_times);
        let mut xs = std::mem::take(&mut self.residual_x);
        let mut ys = std::mem::take(&mut self.residual_y);
        let mut zs = std::mem::take(&mut self.residual_z);
        times.extend_from_slice(&block.times_ns);
        xs.extend_from_slice(&block.x);
        ys.extend_from_slice(&block.y);
        zs.extend_from_slice(&block.z);

        let fs = 1.0 / dt;
        let n_points = (self.cfg.pkpk_time * fs).ceil() as usize;
        if xs.len() < n_points {
            // Not enough yet; keep accumulating.
            self.residual_times = times;
            self.residual_x = xs;
            self.residual_y = ys;
            self.residual_z = zs;
            return;
        }

        // Run the kernel.
        let kernel_out = processing_kernel(&self.cfg, &times, &xs, &ys, &zs, dt);

        // Append to global output, dropping leading NaN if any.
        self.results.times_ns.extend(kernel_out.times_ns);
        self.results.x_pkpk.extend(kernel_out.x_pkpk);
        self.results.y_pkpk.extend(kernel_out.y_pkpk);
        self.results.z_pkpk.extend(kernel_out.z_pkpk);
        self.results.xy_pkpk.extend(kernel_out.xy_pkpk);
        self.results.xz_pkpk.extend(kernel_out.xz_pkpk);
        self.results.yz_pkpk.extend(kernel_out.yz_pkpk);

        // Carry the last n_points - 1 samples so the next block can pick up
        // mid-window without edge artefacts.
        let carry = n_points.saturating_sub(1);
        if xs.len() > carry {
            let start = xs.len() - carry;
            self.residual_times = times[start..].to_vec();
            self.residual_x = xs[start..].to_vec();
            self.residual_y = ys[start..].to_vec();
            self.residual_z = zs[start..].to_vec();
        } else {
            self.residual_times = times;
            self.residual_x = xs;
            self.residual_y = ys;
            self.residual_z = zs;
        }
    }

    pub fn finish(self) -> ProcessingResults {
        self.results
    }
}

fn processing_kernel(
    cfg: &ProcessingConfig,
    times: &[i64],
    x: &[f64],
    y: &[f64],
    z: &[f64],
    dt: f64,
) -> ProcessingResults {
    let fs = 1.0 / dt;

    // Optional low-pass filtering.
    let (x_f, y_f, z_f);
    if cfg.lp_freq > 0.0 && cfg.lp_freq < fs / 2.0 {
        match cfg.filter_kind {
            FilterKind::None => {
                x_f = x.to_vec();
                y_f = y.to_vec();
                z_f = z.to_vec();
            }
            FilterKind::Butterworth => {
                let sos = butter_lowpass_sos(5, cfg.lp_freq, fs);
                x_f = sosfiltfilt(&sos, x);
                y_f = sosfiltfilt(&sos, y);
                z_f = sosfiltfilt(&sos, z);
            }
            FilterKind::Bessel => {
                // Bessel design not ported yet — fall back to no filter.
                x_f = x.to_vec();
                y_f = y.to_vec();
                z_f = z.to_vec();
            }
            FilterKind::RcCascade => {
                x_f = rc_cascade(x, cfg.lp_freq, fs, 8);
                y_f = rc_cascade(y, cfg.lp_freq, fs, 8);
                z_f = rc_cascade(z, cfg.lp_freq, fs, 8);
            }
        }
    } else {
        x_f = x.to_vec();
        y_f = y.to_vec();
        z_f = z.to_vec();
    }

    // Build the magnitude streams the Python emits.
    let xy: Vec<f64> = x_f.iter().zip(&y_f).map(|(a, b)| (a * a + b * b).sqrt()).collect();
    let xz: Vec<f64> = x_f.iter().zip(&z_f).map(|(a, b)| (a * a + b * b).sqrt()).collect();
    let yz: Vec<f64> = y_f.iter().zip(&z_f).map(|(a, b)| (a * a + b * b).sqrt()).collect();

    let n_points = (cfg.pkpk_time * fs).ceil() as usize;
    let step_size: usize = (fs / cfg.pkpk_fs).max(1.0) as usize;

    let x_pkpk = rolling_pkpk(&x_f, n_points, step_size);
    let y_pkpk = rolling_pkpk(&y_f, n_points, step_size);
    let z_pkpk = rolling_pkpk(&z_f, n_points, step_size);
    let xy_pkpk = rolling_pkpk(&xy, n_points, step_size);
    let xz_pkpk = rolling_pkpk(&xz, n_points, step_size);
    let yz_pkpk = rolling_pkpk(&yz, n_points, step_size);

    // Times: Python does `times[n_points - 1:][::step_size][:len(x_pkpk)]`
    let out_len = x_pkpk.len();
    let mut times_out = Vec::with_capacity(out_len);
    let start = n_points.saturating_sub(1);
    let mut idx = start;
    while times_out.len() < out_len && idx < times.len() {
        times_out.push(times[idx]);
        idx += step_size;
    }

    ProcessingResults {
        times_ns: times_out,
        x_pkpk,
        y_pkpk,
        z_pkpk,
        xy_pkpk,
        xz_pkpk,
        yz_pkpk,
    }
}

/// Rolling (max − min) over a window of `n_points`, stepped by `step_size`.
/// Mirrors `pd.Series.rolling(n).max() - .rolling(n).min()`, but in-line.
///
/// O(N) overall using monotonic deques to track running max/min.
fn rolling_pkpk(data: &[f64], n_points: usize, step_size: usize) -> Vec<f64> {
    if data.len() < n_points || n_points == 0 {
        return Vec::new();
    }

    let total = data.len();
    let n_outputs = (total + step_size - 1).saturating_sub(n_points - 1).div_ceil(step_size);
    let mut out = Vec::with_capacity(n_outputs);

    // Monotonic deques: front holds the index of the current extremum.
    let mut max_dq: std::collections::VecDeque<usize> = Default::default();
    let mut min_dq: std::collections::VecDeque<usize> = Default::default();

    for i in 0..total {
        let v = data[i];
        // Pop smaller values from the max deque tail.
        while let Some(&back) = max_dq.back() {
            if data[back] <= v {
                max_dq.pop_back();
            } else {
                break;
            }
        }
        max_dq.push_back(i);
        // Pop larger values from the min deque tail.
        while let Some(&back) = min_dq.back() {
            if data[back] >= v {
                min_dq.pop_back();
            } else {
                break;
            }
        }
        min_dq.push_back(i);
        // Drop indices that fell out of the window.
        if let Some(&front) = max_dq.front() {
            if front + n_points <= i {
                max_dq.pop_front();
            }
        }
        if let Some(&front) = min_dq.front() {
            if front + n_points <= i {
                min_dq.pop_front();
            }
        }

        // Emit a sample once we have a full window AND we're on a step boundary
        // (counting from the first valid window position).
        if i + 1 >= n_points {
            let window_idx = i + 1 - n_points; // position of first valid window
            if window_idx % step_size == 0 {
                let max_v = data[*max_dq.front().unwrap()];
                let min_v = data[*min_dq.front().unwrap()];
                out.push(max_v - min_v);
            }
        }
    }

    out
}
