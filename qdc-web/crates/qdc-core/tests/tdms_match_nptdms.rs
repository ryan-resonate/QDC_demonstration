//! Validate qdc_core::tdms against nptdms reference dumps.
//!
//! For each example TDMS file, asserts:
//! - same channels (and groups) in the same order
//! - same sample counts
//! - sample buffers match byte-for-byte after f32→f64 widening (which is
//!   exact in IEEE 754)
//! - key properties (dt/wf_increment, t0/wf_start_time, unit_string,
//!   file-level datetime/DateTime) match
//!
//! Reference dumps live in tests/refdata/tdms_refs/. Generate them with
//! tests/refdata/dump_tdms_reference.py against the project's real
//! Example TDMS files.

use std::path::PathBuf;

use base64::Engine;
use qdc_core::tdms::{parse_tdms, PropertyValue};
use serde_json::Value;

fn project_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = qdc-web/crates/qdc-core. Project root is 3 levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn refs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("refdata").join("tdms_refs")
}

#[test]
fn tdms_parser_matches_nptdms_reference() {
    let dir = refs_dir();
    let index_path = dir.join("index.json");
    if !index_path.exists() {
        // Reference files are large and not always present (the python
        // helper has to be run against the example data first). Skip
        // gracefully rather than failing CI when the fixtures aren't here.
        eprintln!(
            "Skipping: {} missing. Generate via dump_tdms_reference.py.",
            index_path.display()
        );
        return;
    }

    let index: Value = serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
    let entries = index.as_array().expect("index.json: expected array");

    assert!(!entries.is_empty(), "index.json contains no entries");

    for entry in entries {
        let source = entry["source"].as_str().unwrap();
        let ref_name = entry["ref"].as_str().unwrap();
        let source_path = project_root().join(source);
        let ref_path = dir.join(ref_name);

        eprintln!("---- {} ----", source);
        let bytes = std::fs::read(&source_path).unwrap_or_else(|e| {
            panic!("can't read TDMS {}: {}", source_path.display(), e)
        });
        let parsed = parse_tdms(&bytes).expect("parse_tdms failed");
        let reference: Value = serde_json::from_slice(&std::fs::read(&ref_path).unwrap()).unwrap();

        compare_file(&parsed, &reference);
    }
}

fn compare_file(parsed: &qdc_core::tdms::TdmsFile, reference: &Value) {
    // File-level properties.
    let ref_file_props = reference["file_properties"].as_object().unwrap();
    for (k, v) in ref_file_props {
        let actual = parsed.properties.get(k).unwrap_or_else(|| {
            panic!("missing file property {k:?}");
        });
        compare_property(k, actual, v);
    }
    // No assertion about extra properties — file-level props are mostly
    // informational and adding a few non-conflicting ones is fine.

    // Groups: must match in name and order.
    let ref_groups = reference["groups"].as_array().unwrap();
    assert_eq!(parsed.groups.len(), ref_groups.len(), "group count mismatch");

    for (g_actual, g_ref) in parsed.groups.iter().zip(ref_groups.iter()) {
        let expected_name = g_ref["name"].as_str().unwrap();
        assert_eq!(g_actual.name, expected_name, "group name mismatch");

        let ref_props = g_ref["properties"].as_object().unwrap();
        for (k, v) in ref_props {
            // Some FlexLogger group props are the same as file-level props.
            // Compare if present; don't require.
            if let Some(actual) = g_actual.properties.get(k) {
                compare_property(&format!("group/{}/{}", g_actual.name, k), actual, v);
            }
        }

        let ref_channels = g_ref["channels"].as_array().unwrap();
        assert_eq!(
            g_actual.channels.len(),
            ref_channels.len(),
            "channel count mismatch in group {:?}",
            g_actual.name
        );

        for (c_actual, c_ref) in g_actual.channels.iter().zip(ref_channels.iter()) {
            let expected_name = c_ref["name"].as_str().unwrap();
            assert_eq!(
                c_actual.name, expected_name,
                "channel name mismatch in group {:?}",
                g_actual.name
            );

            let expected_count = c_ref["sample_count"].as_u64().unwrap() as usize;
            assert_eq!(
                c_actual.data.len(),
                expected_count,
                "sample count for channel {:?}: {} vs {}",
                c_actual.name,
                c_actual.data.len(),
                expected_count,
            );

            // Compare samples — the reference encodes them as base64 LE f64.
            let b64 = c_ref["all_samples_b64f64"].as_str().unwrap();
            let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            assert_eq!(raw.len(), expected_count * 8);
            let mut max_abs: f64 = 0.0;
            for (i, chunk) in raw.chunks_exact(8).enumerate() {
                let mut a = [0u8; 8];
                a.copy_from_slice(chunk);
                let expected = f64::from_le_bytes(a);
                let actual = c_actual.data[i];
                let diff = (actual - expected).abs();
                if diff > max_abs {
                    max_abs = diff;
                    if diff > 0.0 {
                        eprintln!(
                            "  sample diff at {} (channel {:?}): {} vs {} (diff {})",
                            i, c_actual.name, actual, expected, diff
                        );
                    }
                }
            }
            // f64 should be byte-exact; f32 widened to f64 is also byte-exact
            // because the f32→f64 widening just zero-pads the mantissa.
            assert_eq!(
                max_abs, 0.0,
                "channel {:?} samples diverge (max abs {})",
                c_actual.name, max_abs
            );

            // Compare channel properties.
            let ref_props = c_ref["properties"].as_object().unwrap();
            for (k, v) in ref_props {
                let actual = c_actual
                    .properties
                    .get(k)
                    .unwrap_or_else(|| panic!("missing channel prop {:?}::{:?}", c_actual.name, k));
                compare_property(
                    &format!("channel/{}/{}/{}", g_actual.name, c_actual.name, k),
                    actual,
                    v,
                );
            }
        }
    }
}

fn compare_property(name: &str, actual: &PropertyValue, ref_v: &Value) {
    let kind = ref_v["kind"].as_str().unwrap();
    match kind {
        "string" => {
            let want = ref_v["value"].as_str().unwrap();
            let got = actual.as_str().unwrap_or_else(|| {
                panic!("prop {name}: expected string, got {:?}", actual)
            });
            assert_eq!(got, want, "prop {name}: string mismatch");
        }
        "double" => {
            let want = ref_v["value"].as_f64().unwrap();
            let got = actual.as_f64().unwrap_or_else(|| {
                panic!("prop {name}: expected numeric, got {:?}", actual)
            });
            assert!(
                (got - want).abs() <= f64::EPSILON.max(want.abs() * f64::EPSILON),
                "prop {name}: {got} vs {want}"
            );
        }
        "int" => {
            let want = ref_v["value"].as_i64().unwrap();
            // Accept any integer-valued PropertyValue here.
            let got = match actual {
                PropertyValue::I8(v) => *v as i64,
                PropertyValue::I16(v) => *v as i64,
                PropertyValue::I32(v) => *v as i64,
                PropertyValue::I64(v) => *v,
                PropertyValue::U8(v) => *v as i64,
                PropertyValue::U16(v) => *v as i64,
                PropertyValue::U32(v) => *v as i64,
                PropertyValue::U64(v) => *v as i64,
                other => panic!("prop {name}: expected int, got {:?}", other),
            };
            assert_eq!(got, want, "prop {name}: int mismatch");
        }
        "bool" => {
            let want = ref_v["value"].as_bool().unwrap();
            if let PropertyValue::Bool(b) = actual {
                assert_eq!(*b, want, "prop {name}: bool mismatch");
            } else {
                panic!("prop {name}: expected bool, got {:?}", actual);
            }
        }
        "timestamp" => {
            // Nptdms emits an ISO 8601 string with microsecond precision.
            let want_str = ref_v["iso"].as_str().unwrap();
            let ts = actual.as_timestamp().unwrap_or_else(|| {
                panic!("prop {name}: expected timestamp, got {:?}", actual)
            });
            let got_str = format_timestamp_iso(ts);
            assert_eq!(got_str, want_str, "prop {name}: timestamp mismatch");
        }
        _ => {
            // Skip other kinds for now.
        }
    }
}

/// Format a TdmsTimestamp as ISO 8601 microseconds matching numpy.datetime64[us]'s
/// `str()` output (no trailing 'Z', e.g. "2025-03-28T01:37:27.549808").
fn format_timestamp_iso(ts: qdc_core::tdms::TdmsTimestamp) -> String {
    // Convert microseconds-since-epoch to (year, month, day, h, m, s, us).
    let us = ts.us_since_epoch;
    let secs_total = us.div_euclid(1_000_000);
    let us_remain = us.rem_euclid(1_000_000) as u32;

    let days = secs_total.div_euclid(86_400);
    let time_secs = secs_total.rem_euclid(86_400) as u32;

    let hour = time_secs / 3600;
    let minute = (time_secs % 3600) / 60;
    let second = time_secs % 60;

    let (year, month, day) = days_to_date(days);

    if us_remain == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
    } else {
        format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{us_remain:06}"
        )
    }
}

/// Convert days since 1970-01-01 (possibly negative) to (year, month, day).
fn days_to_date(days: i64) -> (i64, u32, u32) {
    // Reference algorithm: Howard Hinnant's date-from-days.
    let z = days + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}
