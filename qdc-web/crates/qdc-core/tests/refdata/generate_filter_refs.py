"""Generate SciPy reference outputs used to validate the Rust filter port.

The Rust unit tests load the JSON written here and assert numerical
equivalence to <= 1e-12 relative error. This file is the single source
of truth for what "matches SciPy" means in this project.

Run from the project root:

    python qdc-web/crates/qdc-core/tests/refdata/generate_filter_refs.py
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import numpy as np
from scipy.signal import bessel, butter, filtfilt, sosfiltfilt


OUT_DIR = Path(__file__).parent
OUT_PATH = OUT_DIR / "filter_refs.json"


def make_signals(fs: float, duration_s: float, seed: int) -> dict[str, list[float]]:
    """Set of signals chosen to exercise edge cases.

    - sine: smooth, periodic, easy case
    - step: tests transient + edge handling
    - chirp: nonstationary content
    - noise: full-band, validates passband + stopband
    - dc_plus_noise: rolling pk-pk's actual input character (slow drift + jitter)
    """
    n = int(round(fs * duration_s))
    t = np.arange(n, dtype=np.float64) / fs
    rng = np.random.default_rng(seed)

    sine = np.sin(2 * np.pi * 7.3 * t)
    step = np.where(t > duration_s / 3, 1.0, 0.0)
    chirp = np.sin(2 * np.pi * (1.0 + 30.0 * (t / duration_s)) * t)
    noise = rng.standard_normal(n)
    dc_plus_noise = 50.0 + 2.0 * np.sin(2 * np.pi * 0.5 * t) + 0.3 * rng.standard_normal(n)

    return {
        "sine": sine.tolist(),
        "step": step.tolist(),
        "chirp": chirp.tolist(),
        "noise": noise.tolist(),
        "dc_plus_noise": dc_plus_noise.tolist(),
    }


def butter_sos(order: int, cutoff_hz: float, fs: float) -> np.ndarray:
    return butter(order, cutoff_hz, fs=fs, btype="low", output="sos")


def bessel_sos(order: int, cutoff_hz: float, fs: float) -> np.ndarray:
    # pkpk_processing uses norm="phase" — keep this matched exactly.
    return bessel(
        order, cutoff_hz, btype="low", analog=False, fs=fs, output="sos", norm="phase"
    )


def rc_cascade_ba(cutoff_hz: float, fs: float) -> tuple[np.ndarray, np.ndarray]:
    alpha = np.exp(-2.0 * np.pi * cutoff_hz / fs)
    b = np.array([1.0 - alpha])
    a = np.array([1.0, -alpha])
    return b, a


def main() -> None:
    fs = 1000.0
    duration = 2.0
    signals = make_signals(fs=fs, duration_s=duration, seed=12345)

    # Filter configurations spanning the parameter space pkpk_processing
    # actually uses: lp_freq from < 1 Hz to ~ fs/2.
    # Each config records:
    #   - the design output (sos / b / a) so we can validate the FILTER
    #     APPLICATION step (loads pre-computed coefficients in Rust)
    #   - the source design parameters (order, cutoff_hz, fs) so we can
    #     also validate the FILTER DESIGN step (Rust computes its own
    #     coefficients from these and applies them; result compares to
    #     SciPy's end-to-end output)
    configs = []
    for cutoff in [0.5, 5.0, 50.0, 200.0]:
        configs.append({
            "kind": "butter",
            "order": 5,
            "cutoff_hz": cutoff,
            "fs": fs,
            "sos": butter_sos(5, cutoff, fs).tolist(),
        })
        configs.append({
            "kind": "bessel",
            "order": 8,
            "cutoff_hz": cutoff,
            "fs": fs,
            "sos": bessel_sos(8, cutoff, fs).tolist(),
        })
        b, a = rc_cascade_ba(cutoff, fs)
        configs.append({
            "kind": "rc_cascade",
            "stages": 8,
            "cutoff_hz": cutoff,
            "fs": fs,
            "b": b.tolist(),
            "a": a.tolist(),
        })

    # For each (signal, filter config) compute the SciPy reference output.
    cases = []
    for signal_name, signal in signals.items():
        x = np.asarray(signal, dtype=np.float64)
        for cfg in configs:
            if cfg["kind"] == "rc_cascade":
                b = np.asarray(cfg["b"], dtype=np.float64)
                a = np.asarray(cfg["a"], dtype=np.float64)
                y = x.copy()
                for _ in range(cfg["stages"]):
                    y = filtfilt(b, a, y)
            else:
                sos = np.asarray(cfg["sos"], dtype=np.float64)
                y = sosfiltfilt(sos, x)
            cases.append({
                "signal": signal_name,
                "filter": cfg,
                "y": y.tolist(),
            })

    out = {
        "fs": fs,
        "duration_s": duration,
        "signals": signals,
        "cases": cases,
        "metadata": {
            "scipy_version": __import__("scipy").__version__,
            "numpy_version": np.__version__,
            "purpose": "Rust filter port validates against these outputs",
        },
    }

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT_PATH, "w") as f:
        # No indent — file size matters; jsonl-like blob is fine for tests.
        json.dump(out, f)

    size_mb = OUT_PATH.stat().st_size / 1e6
    print(f"Wrote {OUT_PATH}  ({size_mb:.2f} MB, {len(cases)} cases)")


if __name__ == "__main__":
    main()
