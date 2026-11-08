#!/usr/bin/env python3
"""Estimate directory update sizes with FastCDC, desync, or bita."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from io import BytesIO
from pathlib import Path
from typing import BinaryIO, Iterator


READ_SIZE = 1024 * 1024


def files(root: Path) -> dict[str, Path]:
    return {
        path.relative_to(root).as_posix(): path
        for path in root.rglob("*")
        if path.is_file()
    }


def _gear_table() -> tuple[int, ...]:
    value = 0x9E3779B9
    table = []
    for _ in range(256):
        value = (value * 1664525 + 1013904223) & 0xFFFFFFFF
        table.append(value)
    return tuple(table)


GEAR = _gear_table()


def _append_with_limit(buffer: bytearray, data: bytes, maximum: int) -> Iterator[bytes]:
    while data:
        room = maximum - len(buffer)
        if len(data) <= room:
            buffer.extend(data)
            return
        buffer.extend(data[:room])
        yield bytes(buffer)
        buffer.clear()
        data = data[room:]


def _iter_chunks_python(stream: BinaryIO, target: int) -> Iterator[bytes]:
    minimum = max(64, target // 4)
    maximum = max(minimum, target * 4)
    mask = (1 << max(8, target.bit_length())) - 1
    buffer = bytearray()
    while True:
        part = stream.read(READ_SIZE)
        if not part:
            break
        offset = 0
        for index, byte in enumerate(part):
            if GEAR[byte] & mask or len(buffer) + index + 1 - offset < minimum:
                continue
            yield from _append_with_limit(buffer, part[offset : index + 1], maximum)
            if buffer:
                yield bytes(buffer)
                buffer.clear()
            offset = index + 1
        yield from _append_with_limit(buffer, part[offset:], maximum)
    if buffer:
        yield bytes(buffer)


def _iter_chunks_numpy(stream: BinaryIO, target: int) -> Iterator[bytes]:
    import numpy as np

    minimum = max(64, target // 4)
    maximum = max(minimum, target * 4)
    mask = (1 << max(8, target.bit_length())) - 1
    gear = np.asarray(GEAR, dtype=np.uint64)
    buffer = bytearray()
    while True:
        part = stream.read(READ_SIZE)
        if not part:
            break
        values = gear[np.frombuffer(part, dtype=np.uint8)]
        candidates = np.flatnonzero((values & mask) == 0)
        offset = 0
        for index in candidates:
            boundary = int(index) + 1
            if len(buffer) + boundary - offset < minimum:
                continue
            yield from _append_with_limit(buffer, part[offset:boundary], maximum)
            if buffer:
                yield bytes(buffer)
                buffer.clear()
            offset = boundary
        yield from _append_with_limit(buffer, part[offset:], maximum)
    if buffer:
        yield bytes(buffer)


def iter_chunks(stream: BinaryIO, target: int) -> Iterator[bytes]:
    """Yield FastCDC-like chunks with bounded memory."""
    try:
        import numpy  # noqa: F401
    except ImportError:
        yield from _iter_chunks_python(stream, target)
    else:
        yield from _iter_chunks_numpy(stream, target)


def chunks(data: bytes, target: int) -> list[bytes]:
    return list(iter_chunks(BytesIO(data), target))


def _chunk_ids(path: Path, target: int) -> Iterator[tuple[bytes, int]]:
    with path.open("rb") as stream:
        for chunk in iter_chunks(stream, target):
            yield hashlib.blake2b(chunk, digest_size=16).digest(), len(chunk)


def _same_file(left: Path, right: Path) -> bool:
    if left.stat().st_size != right.stat().st_size:
        return False
    with left.open("rb") as first, right.open("rb") as second:
        while True:
            a = first.read(READ_SIZE)
            b = second.read(READ_SIZE)
            if a != b:
                return False
            if not a:
                return True


def metric(base: Path, new: Path, target: int) -> dict[str, int | str]:
    """Compute an approximate metric without loading whole files."""
    base_files = files(base)
    old_hashes: set[bytes] = set()
    for path in base_files.values():
        old_hashes.update(chunk_id for chunk_id, _ in _chunk_ids(path, target))

    changed_total = 0
    max_file = 0
    install_size = 0
    patch_size = 0
    chunk_count = 0
    for relative, path in files(new).items():
        size = path.stat().st_size
        install_size += size
        max_file = max(max_file, size)
        previous = base_files.get(relative)
        if previous is None or not _same_file(previous, path):
            changed_total += size
        for chunk_id, chunk_size in _chunk_ids(path, target):
            if chunk_id not in old_hashes:
                patch_size += chunk_size
                chunk_count += 1
    return {
        "method": "approx",
        "patch_download_bytes": patch_size,
        "install_size_bytes": install_size,
        "max_single_file_bytes": max_file,
        "changed_files_total_bytes": changed_total,
        "chunk_count": chunk_count,
    }


def _run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, text=True, capture_output=True, check=True)


def _resolve_tool(tool: str, tool_path: str | None) -> str:
    executable = tool_path or shutil.which(tool)
    if not executable:
        raise FileNotFoundError(
            f"{tool} not found; install the official release under experiments/.tools "
            "or put it on PATH, then retry"
        )
    return executable


def _desync_chunks(executable: str, index: Path) -> set[str]:
    result = _run([executable, "list-chunks", str(index)])
    return {
        token.lower()
        for line in result.stdout.splitlines()
        for token in re.findall(r"[0-9a-fA-F]{32,128}", line)
    }


def _desync_metric(executable: str, base: Path, new: Path, target: int) -> dict[str, int | str]:
    with tempfile.TemporaryDirectory(prefix="vesper-desync-") as directory:
        work = Path(directory)
        base_store = work / "base-store"
        new_store = work / "new-store"
        base_index = work / "base.caidx"
        new_index = work / "new.caidx"
        chunk_size = f"{target // 4}:{target}:{target * 4}"
        _run(
            [
                executable,
                "tar",
                "-i",
                "-s",
                str(base_store),
                "--chunk-size",
                chunk_size,
                str(base_index),
                str(base),
            ]
        )
        _run(
            [
                executable,
                "tar",
                "-i",
                "-s",
                str(new_store),
                "--chunk-size",
                chunk_size,
                str(new_index),
                str(new),
            ]
        )
        old_ids = _desync_chunks(executable, base_index)
        new_ids = _desync_chunks(executable, new_index)
        patch_size = sum(
            path.stat().st_size
            for path in new_store.rglob("*")
            if path.is_file() and path.name.lower() in new_ids - old_ids
        )
        result = metric(base, new, target)
        result.update(
            method="desync",
            patch_download_bytes=patch_size,
            chunk_count=len(new_ids - old_ids),
        )
        return result


def _bita_missing_bytes(
    executable: str,
    old: Path,
    new: Path,
    target: int,
) -> int | None:
    result = _run(
        [
            executable,
            "diff",
            "--hash-chunking",
            "RollSum",
            "--avg-chunk-size",
            str(target),
            "--min-chunk-size",
            str(target // 4),
            "--max-chunk-size",
            str(target * 4),
            str(old),
            str(new),
        ]
    )
    output = f"{result.stdout}\n{result.stderr}"
    match = re.search(
        r"(?:missing|different|archive|download)[^0-9]*(\d+)\s*(?:bytes)?",
        output,
        re.IGNORECASE,
    )
    return int(match.group(1)) if match else None


def _bita_metric(executable: str, base: Path, new: Path, target: int) -> dict[str, int | str]:
    """Compress each file, then use bita diff for missing chunk bytes."""
    with tempfile.TemporaryDirectory(prefix="vesper-bita-") as directory:
        work = Path(directory)
        base_files = files(base)
        patch_size = 0
        archive_count = 0
        for relative, path in files(new).items():
            archive = work / (relative.replace("/", "__") + ".cba")
            archive.parent.mkdir(parents=True, exist_ok=True)
            _run(
                [
                    executable,
                    "compress",
                    "-i",
                    str(path),
                    str(archive),
                    "--hash-chunking",
                    "RollSum",
                    "--avg-chunk-size",
                    str(target),
                    "--min-chunk-size",
                    str(target // 4),
                    "--max-chunk-size",
                    str(target * 4),
                ]
            )
            previous = base_files.get(relative)
            missing = _bita_missing_bytes(executable, previous, path, target) if previous else None
            patch_size += missing if missing is not None else archive.stat().st_size
            archive_count += 1
        result = metric(base, new, target)
        result.update(
            method="bita",
            patch_download_bytes=patch_size,
            chunk_count=archive_count,
        )
        return result


def external(
    tool: str,
    base: Path,
    new: Path,
    target: int,
    tool_path: str | None = None,
) -> dict[str, int | str]:
    executable = _resolve_tool(tool, tool_path)
    if tool == "desync":
        return _desync_metric(executable, base, new, target)
    return _bita_metric(executable, base, new, target)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog="numpy is optional: it accelerates --approx; without it the "
        "streaming Python FastCDC fallback is used.",
    )
    parser.add_argument("baseline", type=Path)
    parser.add_argument("builds", nargs="+", type=Path)
    parser.add_argument("--approx", action="store_true")
    parser.add_argument("--tool", choices=("desync", "bita"))
    parser.add_argument("--tool-path")
    args = parser.parse_args()
    if not args.approx and not args.tool:
        parser.error("choose --approx or --tool desync|bita")
    if args.approx and args.tool:
        parser.error("choose exactly one of --approx or --tool")

    output = []
    for build in args.builds:
        for target in (256 * 1024, 1024 * 1024):
            result = (
                metric(args.baseline, build, target)
                if args.approx
                else external(args.tool, args.baseline, build, target, args.tool_path)
            )
            output.append({"build": str(build), "block_size": target, **result})
    print(json.dumps(output, indent=2, sort_keys=True))
    print("\n| Method | Build | Block | Patch | Install | Max file | Changed files | Chunks |")
    print("|---|---|---:|---:|---:|---:|---:|---:|")
    for result in output:
        print(
            f"| {result['method']} | {result['build']} | {result['block_size']} | "
            f"{result['patch_download_bytes']} | {result['install_size_bytes']} | "
            f"{result['max_single_file_bytes']} | {result['changed_files_total_bytes']} | "
            f"{result['chunk_count']} |"
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (FileNotFoundError, subprocess.CalledProcessError) as error:
        print(f"patch_size: {error}", file=sys.stderr)
        raise SystemExit(2)
