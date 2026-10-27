"""把 QASPER（allenai，CC BY 4.0）转成和 Chroma chunking_evaluation 一样的格式，作为第二组 RAG 数据。

每篇论文写成一个 Markdown 文档（标题、摘要、各章节和段落），每个问题取第一位给出证据的标注者，
证据段落换算成在文档里的字符区间。无法回答的问题、证据只有图表（FLOAT SELECTED）的问题跳过。

    python3 -I benchmarks/rag/qasper.py <qasper-test-v0.3.json> <输出目录>
输出：<输出目录>/corpora/<论文 id>.md、<输出目录>/questions_df.csv（question, references, corpus_id）
"""
import csv
import json
import sys
from pathlib import Path


def main(src: Path, out: Path) -> None:
    papers = json.load(src.open())
    (out / "corpora").mkdir(parents=True, exist_ok=True)
    rows = []
    for pid, p in sorted(papers.items()):
        parts = [f"# {p['title'].strip()}", "## Abstract", p["abstract"].strip()]
        for sec in p["full_text"]:
            name = (sec.get("section_name") or "").split(":::")[-1].strip()
            if name:
                parts.append(f"## {name}")
            parts.extend(para.strip() for para in sec["paragraphs"] if para.strip())
        doc = "\n\n".join(parts) + "\n"
        (out / "corpora" / f"{pid}.md").write_text(doc, encoding="utf-8")
        for qa in p["qas"]:
            for ans in qa["answers"]:
                a = ans["answer"]
                ev = [e.strip() for e in a.get("evidence", []) if e.strip() and not e.startswith("FLOAT SELECTED")]
                if a.get("unanswerable") or not ev:
                    continue
                refs = []
                for e in ev:
                    at = doc.find(e)
                    if at >= 0:
                        refs.append({"content": e, "start_index": at, "end_index": at + len(e)})
                if refs and len(refs) == len(ev):
                    rows.append({"question": qa["question"].strip(), "references": json.dumps(refs), "corpus_id": pid})
                break
    with (out / "questions_df.csv").open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=["question", "references", "corpus_id"])
        w.writeheader()
        w.writerows(rows)
    words = sum(len(p.read_text().split()) for p in (out / "corpora").glob("*.md"))
    print(f"{len(papers)} papers, {words} words, {len(rows)} questions", file=sys.stderr)


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]))
