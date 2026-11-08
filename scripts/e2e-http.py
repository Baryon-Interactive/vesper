#!/usr/bin/env python3
"""Local Range server and small GitHub Releases REST mock for scripts/e2e.ps1."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import argparse
import json
import shutil
import threading
import time
import urllib.parse


class State:
    def __init__(self, pack_dirs: list[Path], state_dir: Path):
        self.pack_dirs = pack_dirs
        self.state_dir = state_dir
        self.assets_dir = state_dir / "assets"
        self.assets_dir.mkdir(parents=True, exist_ok=True)
        self.releases: dict[str, dict] = {}
        self.bytes_served = 0
        self.requests = 0
        self.active = 0
        self.max_concurrency = 0
        self.pack_cache: dict[str, bytes] = {}
        self.lock = threading.Lock()

    def find_pack(self, name: str) -> Path | None:
        for directory in self.pack_dirs:
            candidate = directory / name
            if candidate.is_file():
                return candidate
        for candidate in self.assets_dir.glob(f"*/{name}"):
            if candidate.is_file():
                return candidate
        return None

    def asset_path(self, tag: str, name: str) -> Path:
        directory = self.assets_dir / tag
        directory.mkdir(parents=True, exist_ok=True)
        return directory / name

    def read_pack(self, name: str) -> bytes | None:
        with self.lock:
            cached = self.pack_cache.get(name)
        if cached is not None:
            return cached
        path = self.find_pack(name)
        if path is None:
            return None
        data = path.read_bytes()
        with self.lock:
            self.pack_cache.setdefault(name, data)
            return self.pack_cache[name]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        return

    def handle_one_request(self):
        with self.state.lock:
            self.state.requests += 1
            self.state.active += 1
            self.state.max_concurrency = max(self.state.max_concurrency, self.state.active)
        try:
            super().handle_one_request()
        finally:
            with self.state.lock:
                self.state.active = max(0, self.state.active - 1)

    @property
    def state(self) -> State:
        return self.server.state  # type: ignore[attr-defined]

    @property
    def base(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length", "0"))
        return self.rfile.read(length)

    def send_bytes(self, body: bytes, status: int = 200, content_type: str = "application/octet-stream") -> None:
        range_header = self.headers.get("Range")
        start, end = 0, len(body) - 1
        if range_header and range_header.startswith("bytes="):
            value = range_header[6:].split(",", 1)[0]
            start_text, end_text = value.split("-", 1)
            start = int(start_text)
            end = int(end_text) if end_text else end
            if start >= len(body):
                self.send_response(416)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            end = min(end, len(body) - 1)
            payload = body[start : end + 1]
            status = 206
        else:
            payload = body
        self.send_response(status)
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{len(body)}")
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(payload)
        with self.state.lock:
            self.state.bytes_served += len(payload)

    def send_json(self, payload: object, status: int = 200) -> None:
        self.send_bytes(json.dumps(payload).encode("utf-8"), status, "application/json")

    def redirect(self, location: str) -> None:
        self.send_response(302)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()

    def do_GET(self):
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        if path == "/stats":
            with self.state.lock:
                served = self.state.bytes_served
                requests = self.state.requests
                max_concurrency = self.state.max_concurrency
            self.send_json({"bytes_served": served, "requests": requests, "max_concurrency": max_concurrency})
            return
        if path.startswith("/repos/") and "/releases/tags/" in path:
            tag = urllib.parse.unquote(path.rsplit("/", 1)[-1])
            release = self.state.releases.get(tag)
            if release is None:
                self.send_bytes(b"", 404)
            else:
                self.send_json(release)
            return
        if path.startswith("/assets/"):
            tag = urllib.parse.unquote(path.split("/", 2)[2])
            directory = self.state.assets_dir / tag
            assets = []
            if directory.is_dir():
                for item in sorted(directory.iterdir()):
                    if item.is_file():
                        assets.append({
                            "id": 1,
                            "name": item.name,
                            "size": item.stat().st_size,
                            "browser_download_url": f"{self.base}/download/{urllib.parse.quote(tag)}/{urllib.parse.quote(item.name)}",
                        })
            self.send_json(assets)
            return
        if path.startswith("/download/"):
            _, _, tag, name = path.split("/", 3)
            self.redirect(f"{self.base}/blob/{urllib.parse.quote(tag)}/{urllib.parse.quote(name)}?X-Amz-Expires=60")
            return
        if path.startswith("/blob/"):
            _, _, tag, name = path.split("/", 3)
            asset = self.state.asset_path(urllib.parse.unquote(tag), urllib.parse.unquote(name))
            if not asset.is_file():
                self.send_bytes(b"", 404)
            else:
                self.send_bytes(asset.read_bytes())
            return
        if path.startswith("/packs/"):
            name = urllib.parse.unquote(path.split("/", 2)[2])
            if self.state.find_pack(name) is None:
                self.send_bytes(b"", 404)
            else:
                self.redirect(f"{self.base}/direct/{urllib.parse.quote(name)}?X-Amz-Expires=60")
            return
        if path.startswith("/direct/"):
            name = urllib.parse.unquote(path.split("/", 2)[2])
            pack = self.state.read_pack(name)
            if pack is None:
                self.send_bytes(b"", 404)
            else:
                # Keep overlapping Range requests observable in the e2e
                # concurrency counter even on a fast local filesystem.
                time.sleep(0.01)
                self.send_bytes(pack)
            return
        self.send_bytes(b"", 404)

    def do_POST(self):
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        if path == "/repos/o/r/releases":
            body = json.loads(self.read_body() or b"{}")
            tag = str(body.get("tag_name", ""))
            release = {
                "id": len(self.state.releases) + 1,
                "tag_name": tag,
                "upload_url": f"{self.base}/upload/{urllib.parse.quote(tag)}{{?name,label}}",
                "assets_url": f"{self.base}/assets/{urllib.parse.quote(tag)}",
            }
            self.state.releases[tag] = release
            self.send_json(release, 201)
            return
        if path.startswith("/upload/"):
            tag = urllib.parse.unquote(path.split("/", 2)[2])
            name = urllib.parse.unquote(urllib.parse.parse_qs(parsed.query).get("name", [""])[0])
            if not name:
                self.send_bytes(b"missing name", 400)
                return
            asset = self.state.asset_path(tag, name)
            asset.write_bytes(self.read_body())
            self.send_json({
                "id": 1,
                "name": name,
                "size": asset.stat().st_size,
                "browser_download_url": f"{self.base}/download/{urllib.parse.quote(tag)}/{urllib.parse.quote(name)}",
            }, 201)
            return
        self.send_bytes(b"", 404)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pack-dir", action="append", type=Path, default=[])
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    state = State(args.pack_dir, args.state_dir)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.state = state  # type: ignore[attr-defined]
    print(f"PORT={server.server_port}", flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()
        shutil.rmtree(args.state_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
