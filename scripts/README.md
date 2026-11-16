# Build and test scripts

These scripts build releases or exercise the CLI; see the [overview](../README.md),
[testing guide](../docs/testing.md) and [release guide](../docs/releases.md).
Run them from the repository root. PowerShell scripts that build native code
require Windows x64 C++ Build Tools and Rust; Python scripts require Python 3.

| Script | Purpose |
| --- | --- |
| [build-vesper-release.ps1](build-vesper-release.ps1) | Build Linux/Windows binaries from committed source; requires `-Version` and `-OutDir`, Docker Linux containers and the pinned Windows Rust toolchain |
| [make-synth-build.py](make-synth-build.py) | `python scripts/make-synth-build.py --out ./tmp/builds` creates deterministic v1/v2 builds with two 512 MiB pak files |
| [e2e.ps1](e2e.ps1) | Synthetic local HTTP publication/install/update/rollback and metadata tampering checks; prints measured transfer sizes |
| [e2e-http.py](e2e-http.py) | GitHub REST/Range mock used by e2e, not a production server |
| [github-smoke.ps1](github-smoke.ps1) | Read-only public release install/update/rollback smoke check; required `-Owner`, `-Repo`, `-Manifest`, `-Release` |
| [check-docs.py](check-docs.py) | `python scripts/check-docs.py` scans tracked UTF-8 text, local Markdown file links and the root README length |
| [third-party-licenses.py](third-party-licenses.py) | `python scripts/third-party-licenses.py` writes [docs/third-party-licenses.md](../docs/third-party-licenses.md) from production Rust and launcher npm dependencies; `--check` fails when it is stale or a license is unreviewed |
| [mem-check.ps1](mem-check.ps1) | Synthetic 64/512 MiB installs and process peak-working-set comparison |

Before `e2e.ps1`, set `VESPER_ONLINE_KEY` to a separate 32-byte Ed25519 seed in
hex for its local index initialization and online signing. Use a disposable test
key, not a production secret. The script sets its own synthetic developer key.
Its large input and memory/concurrency assertions make it a separate measurement
run, not a prerequisite for the small README example.

Release scratch output stays under `target`; other measurement scripts use the
system temporary directory and clean it up. Set `TEMP`/`TMP` to an existing ignored
`tmp` directory to contain those writes in the checkout. Build-tool discovery uses
the standard Visual Studio 2019 Build Tools directory; demo/smoke/memory helpers
also accept `VESPER_VCVARS64` pointing to an alternate developer command file.

Desktop demonstration, screenshot capture and native tour scripts remain in
[launcher/dev](../launcher/README.md#local-demonstration). Measurement tools live
in [experiments](../experiments/README.md). No script here automatically publishes
Vesper binary releases; the manual maintainer step is documented separately.
