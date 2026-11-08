#!/usr/bin/env python3
"""Generate synthetic builds for Vesper end-to-end and patch-size tests."""
from pathlib import Path
import argparse
import random

MIB = 1024 * 1024
LARGE_SIZE = 512 * MIB
INSERT_AT = 256 * MIB
INSERT_SIZE = 64 * 1024
OVERWRITE_SIZE = MIB
OVERWRITE_OFFSETS = (96 * MIB, 320 * MIB, 448 * MIB)
MEDIUM_SIZE = 2 * MIB
MEDIUM_CHANGE_OFFSET = 512 * 1024
MEDIUM_CHANGE_SIZE = 256 * 1024


def random_bytes(rng: random.Random, size: int) -> bytes:
    return rng.randbytes(size)


def write_random(path: Path, size: int, seed: int) -> None:
    rng = random.Random(seed)
    with path.open("wb") as stream:
        remaining = size
        while remaining:
            count = min(remaining, MIB)
            stream.write(random_bytes(rng, count))
            remaining -= count


def copy_source_span(
    source, destination, start: int, end: int, replacements: dict[int, bytes]
) -> None:
    source.seek(start)
    offset = start
    while offset < end:
        count = min(MIB, end - offset)
        data = source.read(count)
        if len(data) != count:
            raise IOError(f"short source read at {offset}")
        destination.write(replacements.get(offset, data))
        offset += count


def mutate_large_two(source_path: Path, destination_path: Path) -> None:
    replacements = {
        offset: random_bytes(random.Random(23 + index), OVERWRITE_SIZE)
        for index, offset in enumerate(OVERWRITE_OFFSETS)
    }
    insertion = random_bytes(random.Random(99), INSERT_SIZE)
    with source_path.open("rb") as source, destination_path.open("wb") as destination:
        copy_source_span(source, destination, 0, INSERT_AT, replacements)
        destination.write(insertion)
        copy_source_span(source, destination, INSERT_AT, LARGE_SIZE, replacements)


def mutate_medium(path: Path) -> None:
    replacement = random_bytes(random.Random(707), MEDIUM_CHANGE_SIZE)
    with path.open("r+b") as stream:
        stream.seek(MEDIUM_CHANGE_OFFSET)
        stream.write(replacement)


def make_build(root: Path, version: int) -> Path:
    build = root / f"v{version}"
    content = build / "Content"
    content.mkdir(parents=True, exist_ok=True)
    write_random(content / "large.pak", LARGE_SIZE, 7)
    if version == 1:
        write_random(content / "large-2.pak", LARGE_SIZE, 19)
    else:
        mutate_large_two(root / "v1" / "Content" / "large-2.pak", content / "large-2.pak")
    for index in range(32):
        (content / f"small-{index:02}.txt").write_text(
            f"stable-{index}\n", encoding="utf-8"
        )
    write_random(content / "medium.bin", MEDIUM_SIZE, 101)
    if version == 2:
        mutate_medium(content / "medium.bin")
        (content / "added.bin").write_bytes(b"added in v2\n")
        (content / "small-03.txt").unlink()
    return build


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    v1 = make_build(args.out, 1)
    v2 = make_build(args.out, 2)
    changed_bytes = INSERT_SIZE + len(OVERWRITE_OFFSETS) * OVERWRITE_SIZE + MEDIUM_CHANGE_SIZE + len(b"added in v2\n")
    print(f"v1={v1}")
    print(f"v2={v2}")
    print(f"expected_actual_changed_bytes={changed_bytes}")
    print(f"v2_large_2_bytes={LARGE_SIZE + INSERT_SIZE}")


if __name__ == "__main__":
    main()
