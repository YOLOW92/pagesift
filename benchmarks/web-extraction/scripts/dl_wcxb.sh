#!/bin/bash
cd ~/pagesift
benchmarks/common/vendor/.venv/bin/python - <<'PY'
from huggingface_hub import snapshot_download
p = snapshot_download(
    repo_id="murrough-foley/web-content-extraction-benchmark",
    repo_type="dataset",
    local_dir="/home/yolow92/pagesift/benchmarks/data/wcxb",
    max_workers=16,
)
print("downloaded", p)
PY
echo "WCXB_DL_DONE"
du -sh ~/pagesift/benchmarks/data/wcxb
