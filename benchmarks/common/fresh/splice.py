"""按页面编号把补跑的结果换进完整结果：python3 -I benchmarks/common/fresh/splice.py <完整.jsonl> <补跑.jsonl>
用在个别输入页修正之后：只重跑这几页，替换完整结果里同编号的行，其余行不动。"""
import json
import sys

full, part = sys.argv[1], sys.argv[2]
new = {json.loads(l)["id"]: l.rstrip("\n") for l in open(part) if l.strip()}
lines = [l.rstrip("\n") for l in open(full) if l.strip()]
out = [new.get(json.loads(l)["id"], l) for l in lines]
open(full, "w").write("\n".join(out) + "\n")
print(f"{full}: replaced {sum(json.loads(l)['id'] in new for l in lines)} of {len(new)}", file=sys.stderr)
