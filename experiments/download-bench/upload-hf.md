# Hugging Face upload

Create a public dataset repository, then upload the same files:

```bash
huggingface-cli repo create vesper-download-bench --type dataset
huggingface-cli upload OWNER/vesper-download-bench ./pack-0000.bin pack-0000.bin --repo-type dataset
```

Repeat for each pack. `hf-mirror.com` is only a URL variant for the same dataset and must be measured separately. The script does not upload or retain credentials.
