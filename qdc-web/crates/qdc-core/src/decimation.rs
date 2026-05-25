//! Min/max-per-bucket decimation for the raw-data drill-down.
//!
//! Given an array of samples and a time grid (or just sample indices), bucket
//! into `n_buckets` segments and emit (min, max) per bucket. This preserves
//! every peak the user might want to see while keeping the rendered point
//! count bounded — the standard approach for huge time-series plots.

/// Decimate `samples` into `n_buckets` (min, max) pairs.
/// Output layout: pairs of [min, max] for each bucket, flattened.
/// If `samples.len() <= n_buckets * 2`, returns the samples doubled so the
/// caller can plot raw points directly (min==max in each bucket = single point).
pub fn min_max_decimate(samples: &[f64], n_buckets: usize) -> (Vec<f64>, Vec<f64>) {
    let n = samples.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    if n_buckets == 0 || n <= n_buckets {
        // No decimation needed (or asked for); return raw samples in both
        // min and max channels so the caller can plot a single line.
        return (samples.to_vec(), samples.to_vec());
    }

    let mut mins = Vec::with_capacity(n_buckets);
    let mut maxs = Vec::with_capacity(n_buckets);
    for b in 0..n_buckets {
        // Map bucket b → [start, end) sample indices.
        let start = (b * n) / n_buckets;
        let end = ((b + 1) * n) / n_buckets;
        let slice = &samples[start..end];
        if slice.is_empty() {
            continue;
        }
        let mut mn = f64::INFINITY;
        let mut mx = f64::NEG_INFINITY;
        for &v in slice {
            if v < mn {
                mn = v;
            }
            if v > mx {
                mx = v;
            }
        }
        mins.push(mn);
        maxs.push(mx);
    }
    (mins, maxs)
}
