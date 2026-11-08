#!/usr/bin/env python3
"""Generate deterministic, incompressible benchmark packs."""

import argparse
import hashlib
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory")
    parser.add_argument("--gb", type=float, default=1)
    parser.add_argument("--chunk-mib", type=int, default=256)
    args = parser.parse_args()

    output = Path(args.directory)
    output.mkdir(parents=True, exist_ok=True)
    total = int(args.gb * 1024**3)
    pack_size = args.chunk_mib * 1024**2
    seed = hashlib.sha256(b"vesper-testdata-v1").digest()
    generated = 0
    index = 0
    while generated < total:
        size = min(pack_size, total - generated)
        data = bytearray()
        counter = 0
        while len(data) < size:
            data.extend(
                hashlib.sha256(
                    seed
                    + index.to_bytes(8, "little")
                    + counter.to_bytes(8, "little")
                ).digest()
            )
            counter += 1
        (output / f"pack-{index:04d}.bin").write_bytes(bytes(data[:size]))
        generated += size
        index += 1
    print(f"generated {index} file(s), {generated} bytes in {output}")


if __name__ == "__main__":
    main()
