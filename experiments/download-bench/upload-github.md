# GitHub Release upload

Create a release and upload the generated 256 MiB files without changing their bytes:

```bash
gh release create bench-v1 --title "Download bench v1" --notes "Synthetic deterministic data"
gh release upload bench-v1 ./pack-*.bin --clobber
```

The script never uploads. Record the release asset URLs and feed them to `bench.py` with one URL per line.
