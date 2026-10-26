"""把 Chroma 评测集的问题转成 JSON 行：{id, question, corpus, gold: [[起, 止]...], half}（half 按 sha1 分 tune/hold）。
python -I qf_prepare.py <数据目录> <输出.jsonl>"""
import csv, hashlib, json, os, sys
d, out = sys.argv[1], sys.argv[2]
with open(out, "w") as f:
    for q in csv.DictReader(open(os.path.join(d, "questions_df.csv"), encoding="utf-8")):
        h = hashlib.sha1(q["question"].encode()).hexdigest()
        gold = [[r["start_index"], r["end_index"]] for r in json.loads(q["references"])]
        f.write(json.dumps({"id": h[:16], "question": q["question"], "corpus": q["corpus_id"], "gold": gold,
                            "half": "hold" if int(h, 16) % 2 else "tune"}, ensure_ascii=False) + "\n")
