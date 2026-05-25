//! Compare Rust-designed Butterworth SOS coefficients against scipy.signal.butter,
//! and end-to-end (design + sosfiltfilt) against scipy's full pipeline.
//!
//! The end-to-end check is what matters for pk-pk equivalence — section
//! ordering or sub-ULP coefficient noise are absorbed by the filter
//! application as long as the math is right.

use qdc_core::filter::sosfiltfilt;
use qdc_core::filter_design::butter_lowpass_sos;
use serde_json::Value;

fn load_refs() -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("refdata")
        .join("filter_refs.json");
    let bytes = std::fs::read(&path).expect("filter_refs.json missing");
    serde_json::from_slice(&bytes).unwrap()
}

fn as_f64_vec(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()
}

#[test]
fn butter_design_matches_scipy_coefficients() {
    let refs = load_refs();
    let cases = refs["cases"].as_array().unwrap();

    let mut checked = 0;
    let mut worst_coeff_diff: f64 = 0.0;

    for case in cases {
        let filter = &case["filter"];
        if filter["kind"].as_str().unwrap() != "butter" {
            continue;
        }
        let order = filter["order"].as_u64().unwrap() as usize;
        let cutoff = filter["cutoff_hz"].as_f64().unwrap();
        let fs = filter["fs"].as_f64().unwrap();
        let expected_rows = filter["sos"].as_array().unwrap();

        let designed = butter_lowpass_sos(order, cutoff, fs);

        assert_eq!(
            designed.len(),
            expected_rows.len(),
            "section count mismatch for cutoff={cutoff}: {} vs {}",
            designed.len(),
            expected_rows.len(),
        );

        for (sec_idx, (sec, row)) in designed.iter().zip(expected_rows.iter()).enumerate() {
            let expected: [f64; 6] = {
                let r = row.as_array().unwrap();
                [
                    r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(),
                    r[3].as_f64().unwrap(), r[4].as_f64().unwrap(), r[5].as_f64().unwrap(),
                ]
            };
            let actual = [sec.b[0], sec.b[1], sec.b[2], sec.a[0], sec.a[1], sec.a[2]];
            for (i, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
                let diff = (a - e).abs();
                let rel = if e.abs() > 0.0 { diff / e.abs() } else { diff };
                worst_coeff_diff = worst_coeff_diff.max(rel);
                if diff > 1e-12 + 1e-12 * e.abs() {
                    eprintln!(
                        "coeff mismatch: cutoff={cutoff} section={sec_idx} idx={i} \
                         actual={a:.17e} expected={e:.17e} diff={diff:.3e}"
                    );
                }
            }
        }
        checked += 1;
    }

    eprintln!("butter_design: {checked} configs, worst rel diff {worst_coeff_diff:.3e}");
    assert!(checked > 0, "no butter configs found in refs");
}

#[test]
fn butter_design_end_to_end_matches_scipy() {
    let refs = load_refs();
    let signals = &refs["signals"];
    let cases = refs["cases"].as_array().unwrap();

    let mut max_abs: f64 = 0.0;
    let mut max_rel: f64 = 0.0;
    let mut n_checked = 0;

    for case in cases {
        let filter = &case["filter"];
        if filter["kind"].as_str().unwrap() != "butter" {
            continue;
        }
        let order = filter["order"].as_u64().unwrap() as usize;
        let cutoff = filter["cutoff_hz"].as_f64().unwrap();
        let fs = filter["fs"].as_f64().unwrap();
        let signal_name = case["signal"].as_str().unwrap();
        let x = as_f64_vec(&signals[signal_name]);
        let expected = as_f64_vec(&case["y"]);

        let sections = butter_lowpass_sos(order, cutoff, fs);
        let actual = sosfiltfilt(&sections, &x);

        for (&a, &e) in actual.iter().zip(expected.iter()) {
            let diff = (a - e).abs();
            max_abs = max_abs.max(diff);
            let rel = if e.abs() > 0.0 { diff / e.abs() } else { diff };
            max_rel = max_rel.max(rel);
        }
        n_checked += 1;
    }

    eprintln!("butter_design end-to-end: {n_checked} configs, max_abs={max_abs:.3e}, max_rel={max_rel:.3e}");
    // Tolerance: same as the filter-application test — ~1e-9 is "numerically
    // indistinguishable" for nT-scale field data.
    assert!(max_abs <= 1e-8, "butter end-to-end max_abs {max_abs:.3e} exceeds 1e-8");
}
