//! IIR filters matching scipy.signal exactly.
//!
//! What "matching exactly" means here: the same algorithm (Direct Form II
//! Transposed for cascaded biquads, `sosfilt_zi` steady-state initial
//! conditions, odd-extension padding) in the same arithmetic order, so the
//! only divergence vs SciPy is sub-ULP floating-point noise. The
//! `filter_match_scipy` integration test asserts ≤ 1e-12 relative error
//! against SciPy reference outputs.
//!
//! Why this matters: the Python reference (`pkpk_processing.lowpass_smooth`)
//! treats the filter as a black box producing specific numbers. Any
//! divergence here propagates through the rolling pk-pk and shows up in
//! every plot we draw. So we port the algorithm, not the API.

/// A second-order section, expressed as scipy does: rows of [b0, b1, b2, a0, a1, a2].
/// `a0` is usually 1.0 in SciPy output but we normalise defensively when filtering.
#[derive(Debug, Clone, Copy)]
pub struct Sos {
    pub b: [f64; 3],
    pub a: [f64; 3],
}

impl Sos {
    /// Build a Vec<Sos> from the flat shape SciPy emits: an Nx6 row-major slice.
    pub fn from_flat_rows(rows: &[[f64; 6]]) -> Vec<Sos> {
        rows.iter()
            .map(|r| Sos {
                b: [r[0], r[1], r[2]],
                a: [r[3], r[4], r[5]],
            })
            .collect()
    }
}

/// Per-section steady-state initial conditions, matching scipy.signal.sosfilt_zi
/// bit-for-bit (algorithm; result equality is best-effort).
///
/// For each section: solve `(I - companion(a)^T) z = b[1:] - a[1:]*b[0]`, then
/// scale by the cumulative DC gain of all prior sections. SciPy's docs say
/// "if H(z) = B(z)/A(z) is this section's transfer function, then b.sum()/a.sum()
/// is H(1)" — we replicate that scaling exactly.
/// 2x2 LU solve with partial pivoting — mirrors LAPACK's `dgesv` algorithm
/// step-for-step (used by `numpy.linalg.solve`, which scipy.signal.lfilter_zi
/// invokes). Cramer's rule is algebraically identical but rounds differently;
/// matching dgesv's order is what brings the filter port within 1e-12 of
/// scipy.
fn lu_solve_2x2(mut a: [[f64; 2]; 2], mut b: [f64; 2]) -> [f64; 2] {
    // Partial pivot: choose the row with the larger |a[*, 0]| as pivot.
    if a[1][0].abs() > a[0][0].abs() {
        a.swap(0, 1);
        b.swap(0, 1);
    }
    let m = a[1][0] / a[0][0];
    let a11 = a[1][1] - m * a[0][1];
    let b1 = b[1] - m * b[0];
    // Back-substitute.
    let x1 = b1 / a11;
    let x0 = (b[0] - a[0][1] * x1) / a[0][0];
    [x0, x1]
}

pub fn sosfilt_zi(sections: &[Sos]) -> Vec<[f64; 2]> {
    let mut zi = Vec::with_capacity(sections.len());
    let mut scale = 1.0_f64;
    for sec in sections {
        // Normalise by a0 — SciPy implicitly does this when designing filters
        // (a0 == 1) but we don't assume.
        let a0 = sec.a[0];
        let b0 = sec.b[0] / a0;
        let b1 = sec.b[1] / a0;
        let b2 = sec.b[2] / a0;
        let a1 = sec.a[1] / a0;
        let a2 = sec.a[2] / a0;

        // Solve (I - companion(a)^T) z = b[1:] - a[1:]*b[0] via the same
        // partial-pivot LU algorithm numpy uses.
        let mat = [[1.0 + a1, -1.0], [a2, 1.0]];
        let rhs = [b1 - a1 * b0, b2 - a2 * b0];
        let z = lu_solve_2x2(mat, rhs);

        zi.push([scale * z[0], scale * z[1]]);

        // Cumulative DC gain after this section, used for the next section's scale.
        let sum_b = b0 + b1 + b2;
        let sum_a = 1.0 + a1 + a2;
        scale *= sum_b / sum_a;
    }
    zi
}

/// Cascaded SOS filter via Direct Form II Transposed, accumulating output and
/// updating `state` in-place. Matches scipy.signal.sosfilt with `zi` provided.
///
/// State layout: `state[k]` is the two-element state of section k.
pub fn sosfilt(sections: &[Sos], x: &[f64], state: &mut [[f64; 2]]) -> Vec<f64> {
    debug_assert_eq!(sections.len(), state.len());
    let mut out = Vec::with_capacity(x.len());
    for &sample in x {
        let mut s = sample;
        for (sec, z) in sections.iter().zip(state.iter_mut()) {
            let a0 = sec.a[0];
            let b0 = sec.b[0] / a0;
            let b1 = sec.b[1] / a0;
            let b2 = sec.b[2] / a0;
            let a1 = sec.a[1] / a0;
            let a2 = sec.a[2] / a0;
            let y = b0 * s + z[0];
            // DF2-T update — parenthesisation matches scipy's _sosfilt.pyx
            // exactly so floating-point rounding agrees. Algebraically the
            // expressions below are equivalent to `b1*s + z[1] - a1*y` but
            // not bit-identical, and the test demands the latter.
            z[0] = (b1 * s - a1 * y) + z[1];
            z[1] = b2 * s - a2 * y;
            s = y;
        }
        out.push(s);
    }
    out
}

/// Odd-extension padding (reflection through the boundary value).
/// Matches scipy.signal.filtfilt with `padtype='odd'`.
pub fn odd_extension(x: &[f64], padlen: usize) -> Vec<f64> {
    let n = x.len();
    debug_assert!(n > padlen, "padlen must be < signal length");
    let mut out = Vec::with_capacity(n + 2 * padlen);
    let x0 = x[0];
    let xn = x[n - 1];

    // Start padding: scipy uses `2*x[0] - x[1:padlen+1][::-1]`, i.e.
    //   padded[i] = 2*x[0] - x[padlen - i]  for i in 0..padlen
    for i in 0..padlen {
        out.push(2.0 * x0 - x[padlen - i]);
    }
    out.extend_from_slice(x);
    // End padding: scipy uses `2*x[-1] - x[-2:-padlen-2:-1]`, i.e.
    //   padded[n+padlen + i] = 2*x[-1] - x[n-2-i]  for i in 0..padlen
    for i in 0..padlen {
        out.push(2.0 * xn - x[n - 2 - i]);
    }
    out
}

/// Forward-backward SOS filtering, matching scipy.signal.sosfiltfilt with the
/// default `padtype='odd'`, `padlen=None` (=> `3*(2*n_sections+1)`).
///
/// Algorithm exactly as scipy:
/// 1. Odd-extend the input by `edge` samples on each side.
/// 2. Compute `zi` once, scale by the first padded sample, forward filter.
/// 3. Reverse; scale `zi` by the last forward-output sample; forward filter (= reverse).
/// 4. Reverse and trim the edge padding.
pub fn sosfiltfilt(sections: &[Sos], x: &[f64]) -> Vec<f64> {
    let n_sec = sections.len();
    // scipy.signal.sosfiltfilt adjusts ntaps for sections that are effectively
    // first-order (b2 == 0 AND a2 == 0) — which happens for odd-order filters
    // where one section degenerates. This matters: for order-5 Butterworth the
    // adjustment changes padlen from 21 to 18 and the output drifts noticeably
    // near the edges without it. SciPy uses min(b2==0 count, a2==0 count).
    let b2_zero = sections.iter().filter(|s| s.b[2] == 0.0).count();
    let a2_zero = sections.iter().filter(|s| s.a[2] == 0.0).count();
    let degenerate = b2_zero.min(a2_zero);
    let ntaps = 2 * n_sec + 1 - degenerate;
    let edge = 3 * ntaps;
    assert!(
        x.len() > edge,
        "sosfiltfilt: signal length ({}) must exceed padlen ({})",
        x.len(),
        edge
    );

    let ext = odd_extension(x, edge);
    let zi_template = sosfilt_zi(sections);

    // Forward pass over the padded signal.
    let x0 = ext[0];
    let mut state: Vec<[f64; 2]> = zi_template
        .iter()
        .map(|z| [z[0] * x0, z[1] * x0])
        .collect();
    let y_forward = sosfilt(sections, &ext, &mut state);

    // Reverse, then forward-filter (this is the "backward" pass on the
    // forward result). Refresh zi from the new starting sample.
    let y_rev: Vec<f64> = y_forward.iter().rev().copied().collect();
    let y0 = y_rev[0];
    let mut state: Vec<[f64; 2]> = zi_template
        .iter()
        .map(|z| [z[0] * y0, z[1] * y0])
        .collect();
    let y_back = sosfilt(sections, &y_rev, &mut state);

    // Reverse back, trim padding.
    let mut y: Vec<f64> = y_back.iter().rev().copied().collect();
    y.drain(0..edge);
    y.truncate(x.len());
    y
}

/// Single-pole `lfilter_zi`. For `b = [b0]` (or `[b0, b1]`) and `a = [a0, a1]`,
/// returns the scalar steady state for unit DC input. Used by the RC cascade.
pub fn lfilter_zi_order1(b: &[f64], a: &[f64]) -> f64 {
    let a0 = a[0];
    let a1 = a[1] / a0;
    let b0 = b[0] / a0;
    let b1 = if b.len() > 1 { b[1] / a0 } else { 0.0 };
    (b1 - a1 * b0) / (1.0 + a1)
}

/// Single-pole DF2T filter. `b = [b0]` (or `[b0, b1]`), `a = [a0, a1]`.
pub fn lfilter_order1(b: &[f64], a: &[f64], x: &[f64], zi: f64) -> Vec<f64> {
    let a0 = a[0];
    let a1 = a[1] / a0;
    let b0 = b[0] / a0;
    let b1 = if b.len() > 1 { b[1] / a0 } else { 0.0 };

    let mut y = Vec::with_capacity(x.len());
    let mut z = zi;
    for &sample in x {
        let out = b0 * sample + z;
        z = b1 * sample - a1 * out;
        y.push(out);
    }
    y
}

/// Forward-backward single-pole filter, matching scipy.signal.filtfilt
/// defaults for the RC-cascade case used in `pkpk_processing.lowpass_smooth`.
pub fn filtfilt_order1(b: &[f64], a: &[f64], x: &[f64]) -> Vec<f64> {
    let ntaps = b.len().max(a.len());
    let edge = 3 * ntaps;
    assert!(
        x.len() > edge,
        "filtfilt_order1: signal length ({}) must exceed padlen ({})",
        x.len(),
        edge
    );
    let ext = odd_extension(x, edge);
    let zi = lfilter_zi_order1(b, a);

    let x0 = ext[0];
    let y_forward = lfilter_order1(b, a, &ext, zi * x0);

    let y_rev: Vec<f64> = y_forward.iter().rev().copied().collect();
    let y0 = y_rev[0];
    let y_back = lfilter_order1(b, a, &y_rev, zi * y0);

    let mut y: Vec<f64> = y_back.iter().rev().copied().collect();
    y.drain(0..edge);
    y.truncate(x.len());
    y
}

/// RC cascade as `pkpk_processing.lowpass_smooth(method='rc_cascade')`:
/// `alpha = exp(-2*pi*cutoff/fs)`, then `filtfilt` applied `stages` times.
pub fn rc_cascade(x: &[f64], cutoff_hz: f64, fs: f64, stages: usize) -> Vec<f64> {
    let alpha = (-2.0 * std::f64::consts::PI * cutoff_hz / fs).exp();
    let b = [1.0 - alpha];
    let a = [1.0, -alpha];
    let mut y = x.to_vec();
    for _ in 0..stages {
        y = filtfilt_order1(&b, &a, &y);
    }
    y
}
