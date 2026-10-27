#!/bin/bash
cd ~/pagesift
python3 - <<'PY'
from huggingface_hub import snapshot_download
p = snapshot_download(repo_id="allenai/olmOCR-bench", repo_type="dataset",
    local_dir="/home/yolow92/pagesift/benchmarks/data/olmocr_1403",
    allow_patterns=["bench_data/**"], max_workers=12)
print("ok", p)
PY
