"""Dump a TDMS file's structure for designing the Rust reader.

Prints: file properties, groups, channels per group, channel properties,
first/last few samples per channel, raw sample count, sample rate.
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

try:
    from nptdms import TdmsFile
except ImportError:
    print("nptdms not installed: pip install nptdms")
    sys.exit(2)


def main(path_str: str) -> None:
    path = Path(path_str)
    print(f"--- {path.name} ({path.stat().st_size:,} bytes) ---")

    with TdmsFile.open(path) as f:
        print("\nFile-level properties:")
        for k, v in f.properties.items():
            print(f"  {k!r}: {v!r}")

        for g_idx, group in enumerate(f.groups()):
            print(f"\nGroup[{g_idx}] = {group.name!r}")
            for k, v in group.properties.items():
                print(f"  group prop {k!r}: {v!r}")

            for c_idx, channel in enumerate(group.channels()):
                print(f"\n  Channel[{c_idx}] = {channel.name!r}")
                for k, v in channel.properties.items():
                    print(f"    prop {k!r}: {v!r}")
                # Sample peek without loading the full array:
                try:
                    data = channel[:5]
                    print(f"    first 5 samples: {list(data)}")
                except Exception as exc:
                    print(f"    (sample peek failed: {exc})")
                try:
                    total = len(channel)
                    print(f"    total samples: {total:,}")
                except Exception as exc:
                    print(f"    (length unknown: {exc})")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("usage: inspect_tdms.py <path/to/file.tdms>")
        sys.exit(1)
    for arg in sys.argv[1:]:
        main(arg)
        print()
