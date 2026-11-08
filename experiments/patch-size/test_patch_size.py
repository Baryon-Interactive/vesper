import pathlib
import tempfile
import unittest

from patch_size import chunks, external, iter_chunks, metric


class PatchTests(unittest.TestCase):
    def test_streaming_and_one_shot_chunking_match(self):
        from io import BytesIO

        data = bytes(range(256)) * 500
        self.assertEqual(chunks(data, 1024), list(iter_chunks(BytesIO(data), 1024)))

    def test_one_byte_insertion_changes_only_nearby_chunks(self):
        base = (b"alpha" * 3000) + (b"beta" * 3000)
        changed = base[:7000] + b"X" + base[7000:]
        old = chunks(base, 1024)
        new = chunks(changed, 1024)
        old_ids = {chunk for chunk in old}
        new_ids = {chunk for chunk in new}
        self.assertLess(len(new_ids - old_ids), max(5, len(new) // 3))

    def test_approx_metrics_stream_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            base = root / "base"
            new = root / "new"
            base.mkdir()
            new.mkdir()
            (base / "a.bin").write_bytes(b"a" * 10000)
            (new / "a.bin").write_bytes(b"a" * 9000 + b"b" * 1000)
            (new / "b.bin").write_bytes(b"x" * 2000)
            result = metric(base, new, 1024)
            self.assertEqual(result["method"], "approx")
            self.assertGreater(result["patch_download_bytes"], 0)
            self.assertEqual(result["max_single_file_bytes"], 10000)
            self.assertEqual(result["changed_files_total_bytes"], 12000)

    def test_missing_external_tool_does_not_fall_back(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            base = root / "base"
            new = root / "new"
            base.mkdir()
            new.mkdir()
            with self.assertRaises(FileNotFoundError):
                external("desync", base, new, 1024, "definitely-missing-desync")


if __name__ == "__main__":
    unittest.main()
