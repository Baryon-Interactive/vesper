# Measurement experiments

These tools measure patch sizes and download behavior independently of the
runtime. See the [project overview](../README.md), [testing guide](../docs/testing.md)
and [synthetic build generator](../scripts/make-synth-build.py).

| Directory | Tools and local checks |
| --- | --- |
| [patch-size](patch-size/) | `patch_size.py` compares two build trees; `python -m unittest discover -s experiments/patch-size` |
| [download-bench](download-bench/) | `make-testdata.py` generates bytes; `bench.py` measures HTTP transfers; `python -m unittest discover -s experiments/download-bench` |

Run `python experiments/patch-size/patch_size.py --help` and
`python experiments/download-bench/bench.py --help` for arguments. External
comparison modes require `desync` or `bita` on PATH; numpy speeds up approximate
chunking but is optional. Download measurements need a readable remote object.
[GitHub upload preparation](download-bench/upload-github.md) and
[Hugging Face upload preparation](download-bench/upload-hf.md) describe manual
setup; local tests do not upload anything. Keep generated data and reports in
ignored `tmp/` or the component's ignored `results/`/`testdata/` directories.

## Measurement plan

Experiment A needs a real UE5 project: a baseline build, a small Blueprint or code change, a texture swap, and a new level, each packaged under three settings: default compression, pak/IoStore compression off, and compression on with Primary Asset Labels / Chunk ID splitting. Confirm the setting names for the engine version being measured. For each build, get a FastCDC approximation with `patch_size.py --approx` first, then re-measure 256 KiB and 1 MiB chunks with `--tool desync` or `--tool bita`.

Experiment B generates a 256 MiB file with `make-testdata.py`, uploads it to a GitHub Release, and optionally to a Hugging Face dataset. Run `download-bench/bench.py` from home broadband in mainland China and from overseas servers, at 1/4/8/16 connections, at several times of day. Record mean/p10 throughput, time to first byte, 302 direct links and their expiry parameters, 403/429/5xx responses, and per-connection rate limits.

`--approx` streams each file and never loads the whole build into memory. With numpy installed it scans Gear-table boundaries vectorized; without numpy it falls back to a pure Python scan. Local measurement on Windows x64, Python 3.13: a sparse synthetic 1 GiB single file, baseline plus a new version with 1 byte changed, both chunk sizes, full run 47.57 s, peak working set about 56.7 MiB (varies with disk cache and CPU).

External modes really run the tools. desync runs `desync tar -i` on each directory with `min:avg:max = target/4:target:target*4` to produce a `.caidx` and a chunk store, then counts the bytes of chunks missing from the new version using the chunk IDs from `list-chunks`. bita runs `compress` on each file separately with the same avg/min/max chunk sizes and merges the per-file dictionaries for the totals. Both modes set the output `method` field to `desync` or `bita`; if the tool is missing the exit code is non-zero and there is no fallback to `approx`.

Results table:

| Build/source | Settings/region | Connections or chunk | Patch download | Install size | Largest file | Changed files total | Chunks | Mean throughput | p10 | Notes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| | | | | | | | | | | |

Decision rules: if a small change stays under about 1 GB with default compression, keep compression; if turning compression off grows the install size by more than 30%, give both sets of numbers to the developer; if chunk splitting clearly reduces the changed-files total, enable the large-file check; if 256 KiB and 1 MiB are close, use 1 MiB; if every region is acceptable, use GitHub only; if GitHub is slow in mainland China but the HF mirror works, suggest the backup source; for connections, pick the level where throughput stops growing noticeably.

Feed the results back into `vesper` parameters: chunk size goes into the chunking parameters, the changed-file and largest-file results set the large-file threshold for `check`, the throughput curve sets the default connection count, and the source comparison sets the backup-source suggestion in `targets.json`.
