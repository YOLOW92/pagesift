#!/usr/bin/env python3
"""对候选真值的统一修正（留出集；只调用 edits_common.py，没有针对单个文件的修改）。每条都写了原因，见 REVIEW.md。
用法：edits_dev.py <truth 目录>（由 make_truth.sh 调用）"""
import glob, json, os, re, sys, unicodedata
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from edits_common import common
d = sys.argv[1]
for f in sorted(glob.glob(os.path.join(d, "*.json"))):
    t = json.load(open(f))
    common(t, os.path.join(os.path.basename(os.path.normpath(d)), t["format"], t["file"]))
    json.dump(t, open(f, "w"), ensure_ascii=False, indent=1)
