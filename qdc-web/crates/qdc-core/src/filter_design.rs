//! Filter design — port of scipy.signal.butter / bessel for low-pass SOS output.
//!
//! Why this exists: the WASM demo lets the user pick `lp_freq` at runtime, so
//! we need to design the filter coefficients on the fly (cannot precompute).
//! The port matches scipy's pipeline:
//!
//!   analog prototype → lp2lp_zpk → bilinear_zpk → zpk2sos
//!
//! Pairing/ordering follows scipy's "smallest |pole| first" convention so the
//! cascade DC-gain accumulation in `sosfilt_zi` agrees to within FP noise.
//! The end-to-end test (`filter_design_match_scipy`) validates design+apply
//! against scipy's full pipeline output, which is the only metric that
//! ultimately matters for pk-pk equivalence.
//!
//! Currently implements: Butterworth low-pass. Bessel comes next.

use std::f64::consts::PI;

use crate::filter::Sos;

/// Minimal complex-double type, just what filter design needs.
/// Using a hand-rolled type keeps the wasm bundle small (no num-complex dep).
#[derive(Debug, Clone, Copy, PartialEq)]
struct C64 {
    re: f64,
    im: f64,
}

impl C64 {
    const fn new(re: f64, im: f64) -> Self { Self { re, im } }
    const fn one() -> Self { Self { re: 1.0, im: 0.0 } }

    fn add(self, o: Self) -> Self { Self::new(self.re + o.re, self.im + o.im) }
    fn sub(self, o: Self) -> Self { Self::new(self.re - o.re, self.im - o.im) }
    fn mul(self, o: Self) -> Self {
        Self::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
    fn scale(self, s: f64) -> Self { Self::new(self.re * s, self.im * s) }
    fn div(self, o: Self) -> Self {
        let d = o.re * o.re + o.im * o.im;
        Self::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
    fn abs(self) -> f64 { (self.re * self.re + self.im * self.im).sqrt() }
    fn conj(self) -> Self { Self::new(self.re, -self.im) }
}

/// Butterworth analog prototype (poles only — no zeros, gain = 1).
/// Matches `scipy.signal.buttap(N)`.
fn buttap_poles(order: usize) -> Vec<C64> {
    let n = order as i32;
    let mut p = Vec::with_capacity(order);
    for k in 0..order {
        let m = -(n - 1) + 2 * (k as i32);
        let angle = PI * (m as f64) / (2.0 * (n as f64));
        // p_k = -exp(i*angle) = -cos(angle) - i*sin(angle)
        p.push(C64::new(-angle.cos(), -angle.sin()));
    }
    p
}

/// Low-pass to low-pass z/p/k transform. Matches `scipy.signal.lp2lp_zpk`.
/// For our case (no zeros), only the pole scaling and gain are updated.
fn lp2lp_zpk(p: &[C64], k: f64, wo: f64) -> (Vec<C64>, f64) {
    let degree = p.len(); // since len(z) == 0
    let p_lp: Vec<C64> = p.iter().map(|&p| p.scale(wo)).collect();
    let k_lp = k * wo.powi(degree as i32);
    (p_lp, k_lp)
}

/// Bilinear transform of z/p/k. Matches `scipy.signal.bilinear_zpk`.
/// For our low-pass case, `z_analog` is empty so the bilinear-of-z step
/// is a no-op; we append `degree` zeros at z = -1 (Nyquist) afterwards.
fn bilinear_zpk(p_lp: &[C64], k_lp: f64, fs_design: f64) -> (Vec<C64>, Vec<C64>, f64) {
    let fs2 = 2.0 * fs_design;
    let fs2_c = C64::new(fs2, 0.0);

    // Transform poles.
    let p_z: Vec<C64> = p_lp.iter().map(|&p| fs2_c.add(p).div(fs2_c.sub(p))).collect();

    // After bilinear, any analog "zero at infinity" becomes a zero at z = -1.
    // For our case len(z_analog) = 0 and degree = len(p_lp), so we get `degree`
    // zeros at z = -1.
    let degree = p_lp.len();
    let z_z: Vec<C64> = (0..degree).map(|_| C64::new(-1.0, 0.0)).collect();

    // Gain: k_z = k_lp * real( prod(fs2 - z_analog) / prod(fs2 - p_analog) )
    // Empty product → 1.
    let prod_p_inv: C64 = p_lp
        .iter()
        .fold(C64::one(), |acc, &p| acc.mul(fs2_c.sub(p)));
    let k_z = k_lp * (C64::one().div(prod_p_inv)).re;

    (z_z, p_z, k_z)
}

/// Split poles into "real" and "complex (top half plane only)" buckets.
/// Conjugates are reconstructed at use time. Matches the spirit of
/// `_cplxreal` in scipy.signal.filter_design.
fn split_real_complex(p: &[C64], tol: f64) -> (Vec<f64>, Vec<C64>) {
    let mut reals = Vec::new();
    let mut complex_top = Vec::new();
    let mut used = vec![false; p.len()];

    for i in 0..p.len() {
        if used[i] {
            continue;
        }
        if p[i].im.abs() < tol {
            reals.push(p[i].re);
            used[i] = true;
            continue;
        }
        // Find the conjugate partner.
        let want = p[i].conj();
        let mut partner: Option<usize> = None;
        for j in (i + 1)..p.len() {
            if used[j] {
                continue;
            }
            if (p[j].re - want.re).abs() < tol && (p[j].im - want.im).abs() < tol {
                partner = Some(j);
                break;
            }
        }
        used[i] = true;
        if let Some(j) = partner {
            used[j] = true;
        }
        // Keep the one with positive imaginary part as the "representative".
        if p[i].im > 0.0 {
            complex_top.push(p[i]);
        } else {
            complex_top.push(p[i].conj());
        }
    }

    (reals, complex_top)
}

/// Specialized `zpk2sos` for our case: a Butterworth low-pass with all zeros
/// at z = -1 (real) and poles consisting of real(s) + complex-conjugate pairs.
///
/// Ordering matches `scipy.signal.zpk2sos(..., pairing='nearest', analog=False)`
/// for this specific filter family: sections sorted by |pole| ascending.
/// Zero distribution: each section takes 2 zeros at z = -1; the FIRST section
/// (smallest |pole|) absorbs the "leftover" structure so that:
///   - if order is odd, section 0 is `1.5-order` (b is 2nd-order in z^-1,
///     a is 1st-order: a2 = 0, b2 != 0). The lone real pole sits here.
///   - the LAST section (highest |pole|) gets only 1 zero (b2 = 0).
///
/// Gain is folded into the first section's b coefficients.
fn zpk2sos_butter_lp(zeros: &[C64], poles: &[C64], gain: f64) -> Vec<Sos> {
    debug_assert_eq!(zeros.len(), poles.len());
    let order = poles.len();
    let n_sections = (order + 1) / 2;

    let (real_poles, complex_pairs) = split_real_complex(poles, 1e-12);
    debug_assert!(real_poles.len() <= 1, "Butterworth low-pass has at most one real pole");

    // Build per-section pole groups, sorted by |pole| ascending (smallest first).
    #[derive(Debug, Clone, Copy)]
    enum PoleGroup {
        Real(f64),
        Pair(C64),
    }

    let mut groups: Vec<(f64, PoleGroup)> = Vec::with_capacity(n_sections);
    for &r in &real_poles {
        groups.push((r.abs(), PoleGroup::Real(r)));
    }
    for &p in &complex_pairs {
        groups.push((p.abs(), PoleGroup::Pair(p)));
    }
    groups.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    debug_assert_eq!(groups.len(), n_sections);

    // Distribute zeros: each section takes 2 zeros normally; the LAST section
    // gets `order mod 2 == 1 ? 1 : 2` zeros (i.e. for odd order, last has 1).
    // This matches scipy's output structure observed empirically.
    let zeros_per_section: Vec<usize> = (0..n_sections)
        .map(|i| {
            if i == n_sections - 1 && order % 2 == 1 {
                1
            } else {
                2
            }
        })
        .collect();
    debug_assert_eq!(zeros_per_section.iter().sum::<usize>(), order);

    // All zeros are at z = -1 (real). Build each section's b polynomial:
    //   2 zeros at -1 → b = [1, 2, 1]
    //   1 zero at -1  → b = [1, 1, 0]
    // ...and a polynomial from the pole group:
    //   Real(r)       → a = [1, -r, 0]
    //   Pair(p)       → a = [1, -2*Re(p), |p|^2]
    let mut sections: Vec<Sos> = Vec::with_capacity(n_sections);
    for (i, (_mag, group)) in groups.iter().enumerate() {
        let n_zeros = zeros_per_section[i];
        let b = match n_zeros {
            2 => [1.0, 2.0, 1.0],
            1 => [1.0, 1.0, 0.0],
            _ => unreachable!("zeros_per_section must be 1 or 2"),
        };
        let a = match group {
            PoleGroup::Real(r) => [1.0, -r, 0.0],
            PoleGroup::Pair(p) => [1.0, -2.0 * p.re, p.re * p.re + p.im * p.im],
        };
        sections.push(Sos { b, a });
    }

    // Distribute the overall gain into the first section's numerator.
    if let Some(first) = sections.first_mut() {
        first.b[0] *= gain;
        first.b[1] *= gain;
        first.b[2] *= gain;
    }

    sections
}

/// Design a Butterworth low-pass digital filter, output as SOS sections.
///
/// Matches `scipy.signal.butter(order, cutoff_hz, fs=fs, btype='low', output='sos')`
/// in algorithm and ordering. End-to-end output (design + sosfiltfilt) matches
/// SciPy within the same ~1e-9 tolerance as the filter-application port.
pub fn butter_lowpass_sos(order: usize, cutoff_hz: f64, fs: f64) -> Vec<Sos> {
    assert!(order >= 1, "filter order must be >= 1");
    assert!(cutoff_hz > 0.0 && cutoff_hz < fs / 2.0, "cutoff must be in (0, fs/2)");

    // Normalise to fraction of Nyquist, then pre-warp for bilinear.
    let wn = 2.0 * cutoff_hz / fs;
    let fs_design = 2.0_f64;
    let warped = 2.0 * fs_design * (PI * wn / fs_design).tan();

    let p_proto = buttap_poles(order);
    let (p_lp, k_lp) = lp2lp_zpk(&p_proto, 1.0, warped);
    let (z_z, p_z, k_z) = bilinear_zpk(&p_lp, k_lp, fs_design);

    zpk2sos_butter_lp(&z_z, &p_z, k_z)
}
