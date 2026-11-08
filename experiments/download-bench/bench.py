#!/usr/bin/env python3
"""Benchmark GitHub/HF-style downloads with Range and redirect telemetry."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import re
import statistics
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, newurl):
        return None


OPENER = build_opener(NoRedirect)
SIGNED = re.compile(r"(?:[?&](?:X-Amz-Expires|Expires|se|exp)=([^&]+))", re.I)


def github_assets(owner_repo: str, tag: str, token: str | None = None) -> list[str]:
    headers = {"Accept": "application/vnd.github+json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = Request(
        f"https://api.github.com/repos/{owner_repo}/releases/tags/{tag}",
        headers=headers,
    )
    with urlopen(request, timeout=30) as response:
        data = json.load(response)
    return [asset["browser_download_url"] for asset in data.get("assets", [])]


def get_url(
    url: str,
    start: int,
    end: int,
    timeout: float = 30.0,
) -> tuple[int, bytes, dict[str, str]]:
    request = Request(url, headers={"Range": f"bytes={start}-{end}"})
    try:
        with OPENER.open(request, timeout=timeout) as response:
            headers = {key.lower(): value for key, value in response.headers.items()}
            return response.status, response.read(), headers
    except HTTPError as error:
        headers = {key.lower(): value for key, value in error.headers.items()}
        return error.code, error.read(1024), headers
    except URLError:
        return 0, b"", {}


def _expires(location: str | None) -> str | None:
    if not location:
        return None
    match = SIGNED.search(location)
    return match.group(1) if match else None


def one(
    url: str,
    workers: int,
    seconds: float | None,
    max_bytes: int | None,
) -> dict:
    began = time.perf_counter()
    total = 0
    status_counts: dict[str, int] = {}
    first_byte: float | None = None
    redirects: list[dict[str, str | None]] = []
    samples: list[float] = []
    ranges = [
        (index * 1024 * 1024, (index + 1) * 1024 * 1024 - 1)
        for index in range(max(workers * 4, 1))
    ]

    def task(item: tuple[int, int]) -> tuple[int, bytes, dict[str, str], float]:
        started = time.perf_counter()
        status, data, headers = get_url(url, *item)
        return status, data, headers, time.perf_counter() - started

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        for status, data, headers, elapsed in pool.map(task, ranges):
            status_counts[str(status)] = status_counts.get(str(status), 0) + 1
            location = headers.get("location")
            if status in (301, 302, 303, 307, 308):
                redirects.append({"url": location, "expires": _expires(location)})
            if data and first_byte is None:
                first_byte = time.perf_counter() - began
            if elapsed > 0 and data:
                samples.append(len(data) / elapsed)
            total += len(data)
            elapsed_total = time.perf_counter() - began
            if (max_bytes and total >= max_bytes) or (
                seconds and elapsed_total >= seconds
            ):
                break
    elapsed_total = max(time.perf_counter() - began, 1e-9)
    limited = workers == 1 and not samples
    return {
        "url": url,
        "workers": workers,
        "bytes": min(total, max_bytes) if max_bytes else total,
        "seconds": elapsed_total,
        "throughput_bps": total / elapsed_total,
        "connection_throughput_bps": statistics.mean(samples) if samples else 0.0,
        "first_byte_seconds": first_byte,
        "status_counts": status_counts,
        "redirects": redirects,
        "single_connection_limited": limited,
    }


def _p10(values: list[float]) -> float:
    if len(values) < 2:
        return values[0] if values else 0.0
    return statistics.quantiles(values, n=10, method="inclusive")[0]


def run(args):
    urls: list[str] = []
    if args.urls:
        for line in Path(args.urls).read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                urls.append(line.split(None, 1)[-1])
    if args.github:
        urls.extend(github_assets(args.github, args.tag, args.token))
    if not urls:
        raise SystemExit("provide URL file or --github owner/repo --tag TAG")

    results = []
    for round_no in range(1, args.rounds + 1):
        for url in urls:
            for workers in args.concurrency:
                result = one(url, workers, args.duration, args.max_bytes)
                result["round"] = round_no
                results.append(result)

    summaries = []
    for url in urls:
        for workers in args.concurrency:
            rows = [
                row for row in results if row["url"] == url and row["workers"] == workers
            ]
            values = [row["throughput_bps"] / 1048576 for row in rows]
            first_bytes = [
                row["first_byte_seconds"]
                for row in rows
                if row["first_byte_seconds"] is not None
            ]
            bad = sum(
                value
                for row in rows
                for code, value in row["status_counts"].items()
                if code in {"403", "429"} or code.startswith("5")
            )
            summaries.append(
                {
                    "url": url,
                    "workers": workers,
                    "average_mib_s": statistics.mean(values) if values else 0.0,
                    "p10_mib_s": _p10(values),
                    "first_byte_seconds": statistics.mean(first_bytes)
                    if first_bytes
                    else None,
                    "error_responses": bad,
                    "single_connection_limited": False,
                }
            )

    for url in urls:
        rows = [summary for summary in summaries if summary["url"] == url]
        single = next((row for row in rows if row["workers"] == 1), None)
        fastest = max((row["average_mib_s"] for row in rows), default=0.0)
        if single and single["average_mib_s"] > 0:
            single["single_connection_limited"] = fastest >= single["average_mib_s"] * 1.2

    output = {
        "config": {
            "urls": urls,
            "concurrency": args.concurrency,
            "rounds": args.rounds,
            "duration": args.duration,
            "max_bytes": args.max_bytes,
        },
        "results": results,
        "summary": summaries,
    }
    prefix = Path(args.output)
    prefix.with_suffix(".json").write_text(
        json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    lines = [
        "| URL | Connections | Average MiB/s | p10 MiB/s | First byte (s) | 403/429/5xx |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for summary in summaries:
        first = summary["first_byte_seconds"]
        lines.append(
            f"| {summary['url']} | {summary['workers']} | "
            f"{summary['average_mib_s']:.3f} | {summary['p10_mib_s']:.3f} | "
            f"{first if first is not None else 'n/a'} | {summary['error_responses']} |"
        )
    prefix.with_suffix(".md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("urls", nargs="?")
    parser.add_argument("--github")
    parser.add_argument("--tag")
    parser.add_argument("--token")
    parser.add_argument("--concurrency", nargs="+", type=int, default=[1, 4, 8, 16])
    parser.add_argument("--duration", type=float)
    parser.add_argument("--max-bytes", type=int)
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--output", default="bench")
    run(parser.parse_args())
