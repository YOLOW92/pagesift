#!/bin/bash
cd ~/pagesift
OUT=bench/results/final/0.70/common_crawl
mkdir -p "$OUT"
WARC="crawl-data/CC-MAIN-2024-33/segments/1722640353668.0/warc/CC-MAIN-20240802234508-20240803024508-00000.warc.gz"
if [ ! -f "$OUT/seg.warc.gz" ]; then
  echo "=== download ==="
  curl -sSL --retry 5 --max-time 3000 -o "$OUT/seg.warc.gz" "https://data.commoncrawl.org/$WARC"
fi
ls -la "$OUT/seg.warc.gz"
echo "CC_DL_DONE"
