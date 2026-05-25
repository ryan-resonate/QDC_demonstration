"""Dump nptdms reference output for the example TDMS files.

For each example, writes a JSON blob containing:
- file-level properties (with types coerced to JSON-serialisable form)
- per-channel: name, sample count, dt, start_time, unit_string, first/last
  N samples (for quick byte-level sanity checks)
- per-channel: all numeric samples re-encoded as a base64 LE-f64 buffer so the
  Rust test can do an exact comparison without storing 10 MB of JSON.

The Rust integration test loads these and asserts:
- our parser sees the same channels in the same order
- the f64-converted sample buffers match byte-for-byte (or within FP roundoff
  for f32→f64 conversion)
- the key properties (dt, t0/wf_start_time, unit_string, file datetime)
  match
"""

from __future__ import annotations

import base64
import json
import sys
from pathlib import Path
from typing import Any

import numpy as np

try:
    from nptdms import TdmsFile
except ImportError:
    print("nptdms not installed: pip install nptdms")
    sys.exit(2)

OUT_DIR = Path(__file__).parent / "tdms_refs"
SAMPLE_PEEK = 32  # how many leading + trailing samples to encode separately


def encode_prop(v: Any) -> dict[str, Any]:
    """Normalise an nptdms property into JSON-friendly form, retaining type."""
    if isinstance(v, np.datetime64):
        # nptdms returns datetime64[us]. Render as ISO 8601 in UTC.
        return {"kind": "timestamp", "iso": str(v)}
    if isinstance(v, (np.floating, float)):
        return {"kind": "double", "value": float(v)}
    if isinstance(v, (np.integer, int)):
        return {"kind": "int", "value": int(v)}
    if isinstance(v, bool):
        return {"kind": "bool", "value": bool(v)}
    if isinstance(v, str):
        return {"kind": "string", "value": v}
    # Fallback — repr it so the Rust side can at least know something was there.
    return {"kind": "other", "repr": repr(v)}


def encode_samples(arr: np.ndarray) -> str:
    """Encode samples as base64 LE-f64 bytes (always converted to f64)."""
    b = arr.astype(np.float64, copy=False).tobytes(order="C")
    return base64.b64encode(b).decode("ascii")


def dump_file(path: Path) -> dict[str, Any]:
    with TdmsFile.open(path) as f:
        file_props = {k: encode_prop(v) for k, v in f.properties.items()}
        groups_out = []
        for group in f.groups():
            group_props = {k: encode_prop(v) for k, v in group.properties.items()}
            channels_out = []
            for channel in group.channels():
                data = channel[:]
                channels_out.append({
                    "name": channel.name,
                    "dtype": str(data.dtype),
                    "sample_count": int(data.size),
                    "first_samples": [float(v) for v in data[:SAMPLE_PEEK]],
                    "last_samples": [float(v) for v in data[-SAMPLE_PEEK:]],
                    "all_samples_b64f64": encode_samples(data),
                    "properties": {k: encode_prop(v) for k, v in channel.properties.items()},
                })
            groups_out.append({
                "name": group.name,
                "properties": group_props,
                "channels": channels_out,
            })

    return {
        "file_name": path.name,
        "file_size": path.stat().st_size,
        "file_properties": file_props,
        "groups": groups_out,
    }


def main(paths: list[str]) -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    summary = []
    for raw in paths:
        path = Path(raw)
        print(f"Reading {path} …", flush=True)
        ref = dump_file(path)
        out_path = OUT_DIR / (path.stem + ".ref.json")
        with open(out_path, "w") as f:
            json.dump(ref, f)
        size_mb = out_path.stat().st_size / 1e6
        print(f"  -> {out_path.name}  ({size_mb:.1f} MB)")
        summary.append({"source": str(path), "ref": out_path.name, "size_mb": size_mb})

    with open(OUT_DIR / "index.json", "w") as f:
        json.dump(summary, f, indent=2)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("usage: dump_tdms_reference.py <tdms> [<tdms> ...]")
        sys.exit(1)
    main(sys.argv[1:])
