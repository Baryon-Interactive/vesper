import http.server
import pathlib
import socketserver
import tempfile
import threading
import time
import unittest

from bench import one


class Handler(http.server.BaseHTTPRequestHandler):
    data = b"0123456789" * 200000

    def do_GET(self):
        if self.path.startswith("/redirect"):
            self.send_response(302)
            self.send_header("Location", "/asset?X-Amz-Expires=600")
            self.end_headers()
            return
        if self.path.startswith("/forbidden"):
            self.send_response(403)
            self.end_headers()
            return
        if self.path.startswith("/rate"):
            time.sleep(0.02)
        if self.path.startswith("/server-error"):
            self.send_response(503)
            self.end_headers()
            return
        raw = self.headers.get("Range", "")
        start, end = 0, len(self.data) - 1
        if raw.startswith("bytes="):
            first, last = raw[6:].split("-")
            start = int(first)
            end = min(int(last), len(self.data) - 1)
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{len(self.data)}")
        else:
            self.send_response(200)
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        self.wfile.write(self.data[start : end + 1])

    def log_message(self, *_args):
        pass


class BenchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def test_range_benchmark_and_statuses(self):
        result = one(f"{self.base}/asset", 2, None, 20000)
        self.assertGreater(result["bytes"], 0)
        self.assertIn("206", result["status_counts"])
        self.assertGreater(result["connection_throughput_bps"], 0)
        self.assertFalse(result["single_connection_limited"])

        redirect = one(f"{self.base}/redirect", 1, None, 100)
        self.assertEqual(redirect["status_counts"]["302"], 4)
        self.assertEqual(redirect["redirects"][0]["expires"], "600")

        forbidden = one(f"{self.base}/forbidden", 1, None, 100)
        self.assertEqual(forbidden["status_counts"]["403"], 4)

        server_error = one(f"{self.base}/server-error", 1, None, 100)
        self.assertEqual(server_error["status_counts"]["503"], 4)

    def test_output_json_and_markdown(self):
        from argparse import Namespace
        from bench import run

        with tempfile.TemporaryDirectory() as directory:
            urls_path = pathlib.Path(directory) / "urls.txt"
            urls_path.write_text(f"{self.base}/asset\n", encoding="utf-8")
            output = run(
                Namespace(
                    urls=str(urls_path),
                    github=None,
                    tag=None,
                    token=None,
                    concurrency=[1],
                    duration=None,
                    max_bytes=2000,
                    rounds=2,
                    output=str(pathlib.Path(directory) / "result"),
                )
            )
            self.assertEqual(len(output["results"]), 2)
            self.assertTrue(pathlib.Path(directory, "result.json").exists())
            self.assertTrue(pathlib.Path(directory, "result.md").exists())


if __name__ == "__main__":
    unittest.main()
