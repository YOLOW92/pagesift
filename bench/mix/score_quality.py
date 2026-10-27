"""正式质量轮的评分（RULES.md 第二节“评什么”、第四之二节“分数报三样”）。只用标准库；B1 另调 score_olmocr.py。

    python3 -I bench/mix/score_quality.py <子类> <工具>... [--runs $MIX/runs/quality] [--out bench/mix/results]

读 $MIX/runs/quality/<子类>/<工具>/results.jsonl 和每份的 .md，写：
- <out>/quality-<子类>.json：每个工具的完成率、失败分类、三样分数，以及逐份分数；
- <out>/quality-<子类>.md：表（每个数字标明越高越好 / 越低越好）。
三样分数：① 完成率；② 全部题目，没完成的按零分（输出当空串评）；③ 只算参与比较的工具都完成了的题。
用模型的工具（docling*）只跑了 model_subset 的份：它的①②在它跑的份上算，③另出一组“模型子集”上的比较。
评分代码沿用各数据集原有的实现（benchmarks/common/fresh 的 aeb.py、wcxb.py、webmain.py，AEB 自带的
evaluate.py，Firecrawl 集用 pagesift bench --external 的逐页结果，Office 用 benchmarks/office/scripts/score.py）。
"""
import argparse
import functools
import importlib.util
import multiprocessing as mp
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import unicodedata
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
CHECKOUT = Path(os.environ.get("PAGESIFT_CHECKOUT", Path.home() / "pagesift"))
BENCH = CHECKOUT / "benchmarks"
DATA = BENCH / "data"
OFFICE = Path(os.environ.get("MIX_OFFICE", CHECKOUT / "tasks/0.80/fix3/office"))
PAGESIFT_SCORER = os.environ.get("PAGESIFT_SCORER", "pagesift")  # 只用它的 bench --external 评分，不用它的提取
CORES = int(os.environ.get("MIX_SCORE_JOBS", os.cpu_count() or 1))  # 评分并发上限；机器上还有评测在跑时调小


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def rows_tsv(path):
    lines = Path(path).read_text(encoding="utf-8").splitlines()
    cols = lines[0].split("\t")
    return [dict(zip(cols, l.split("\t"))) for l in lines[1:] if l and not l.startswith("#")]


def tool_results(runs, sub, tool):
    d = runs / sub / tool
    res = {}
    p = d / "results.jsonl"
    if p.exists():
        for line in p.read_text(encoding="utf-8").splitlines():
            r = json.loads(line)
            res[r["id"]] = r
    return d, res


def output(d, r):
    if not r or r["status"] != "ok":
        return ""
    p = d / f"{r['id']}.md"
    return p.read_text(encoding="utf-8", errors="replace") if p.exists() else ""


# ---- A1：四套标注集各用自己的评分 ----

def score_a1(items, tools, runs):
    fresh = BENCH / "common/fresh"
    aeb_eval = load(DATA / "fresh/data/aeb/evaluate.py", "aeb_evaluate")
    aeb = load(fresh / "aeb.py", "mix_aeb")
    wcxb = load(fresh / "wcxb.py", "mix_wcxb")
    wmb = load(fresh / "webmain.py", "mix_webmain")
    aeb_truth = json.loads((DATA / "fresh/data/aeb/ground-truth.json").read_text())
    wmb_gold = {}
    with open(DATA / "p4_2_webmainbench/webmainbench.jsonl", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            # 这版 webmainbench.jsonl 的标准答案字段是 convert_main_content（数据集自带的正文 Markdown）；
            # webmain.py prepare 取的 groundtruth_content / content 在这版文件里不存在，留作后备
            wmb_gold[r["track_id"]] = wmb.plain(r.get("convert_main_content") or r.get("groundtruth_content") or r.get("content", ""))
    per = {t: {} for t in tools}
    for t in tools:
        d, res = tool_results(runs, "a1", t)
        fc_rows = []
        for it in items:
            id_, md = it["id"], output(d, res.get(it["id"]))
            ds, key = id_.split("-", 1)
            if ds == "aeb":
                true = aeb_truth[key].get("articleBody", "")
                tp, fp, fn = aeb_eval.string_shingle_matching(true=true, pred=aeb.plain(md))
                per[t][id_] = {"set": "AEB", "tp_fp_fn": [tp, fp, fn], "f1": 2 * tp / (2 * tp + fp + fn) if tp else 0.0}
            elif ds == "wcxb":
                gt = json.loads((DATA / f"fresh/data/wcxb/test/ground-truth/{key}.json").read_text())["ground_truth"]["main_content"]
                p, r, f1 = wcxb.word_f1(wcxb.plain(md), gt)
                per[t][id_] = {"set": "WCXB", "f1": f1, "p": p, "r": r}
            elif ds == "wmb":
                p, r, f1 = wmb.word_f1(wmb.plain(md), wmb_gold.get(key, ""))
                per[t][id_] = {"set": "WebMainBench", "f1": f1, "p": p, "r": r}
            elif ds == "fc":
                fc_rows.append({"id": key, "markdown": md, "ms": 0})
        # Firecrawl 集：pagesift bench --external 的逐页 truth_recall（召回）和 lie_leak（噪声泄漏）
        with tempfile.TemporaryDirectory() as tmp:
            ext, out = Path(tmp) / "ext.jsonl", Path(tmp) / "per.jsonl"
            ext.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in fc_rows), encoding="utf-8")
            subprocess.run([PAGESIFT_SCORER, "bench", str(DATA), "--auto", "--external", str(ext), "--out", str(out)],
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            want = {r["id"] for r in fc_rows}
            for line in out.read_text().splitlines():
                r = json.loads(line)
                if str(r.get("id")) in want:
                    per[t][f"fc-{r['id']}"] = {"set": "Firecrawl", "recall": r.get("truth_recall", 0.0), "leak": r.get("lie_leak"),
                                               "f1": r.get("truth_recall", 0.0)}
    def agg(scores, set_):
        if set_ == "AEB":  # AEB 的 F1 是把逐份归一化的 tp/fp/fn 加总后再算（evaluate.py 的 metrics_from_tp_fp_fns）
            tps = [s["tp_fp_fn"] for s in scores]
            return aeb_eval.metrics_from_tp_fp_fns(tps)["f1"] if tps else None
        return statistics.mean(s["f1"] for s in scores) if scores else None
    return per, agg, {"AEB": "F1（AEB evaluate.py）", "WCXB": "词级 F1（wcxb.py）", "WebMainBench": "词级 F1（webmain.py）",
                      "Firecrawl": "正文召回 truth_recall（pagesift bench）"}


# ---- A2 / A4 / B3：只评自动可判的项目 ----

MOJI = re.compile("[ÃÂ][\u0080-¿]|â€[\u0080-¿™œ\u009d“”]|ï¿½|�")


def garble(md):
    if not md:
        return None
    return len(MOJI.findall(md)) / max(1, len(md))


def score_robust(sub, items, tools, runs):
    per = {t: {} for t in tools}
    for t in tools:
        d, res = tool_results(runs, sub, t)
        for it in items:
            r = res.get(it["id"])
            g = garble(output(d, r)) if sub == "a2" else None
            per[t][it["id"]] = {"set": sub.upper(), "garble": g, "f1": None}
    return per, None, {}


# ---- B2：文字完整度（对 pdftotext 读出的文本层的词覆盖率） ----

WORD = re.compile(r"\w+", re.U)


def words(s):
    return Counter(WORD.findall(unicodedata.normalize("NFKC", s).casefold()))


B2_REF = {}


def b2_refs(items):
    """pdftotext 读文本层，按核数并发调用。"""
    def one(it):
        txt = subprocess.run(["pdftotext", "-enc", "UTF-8", str(it["path"]), "-"], capture_output=True, timeout=600).stdout
        return it["id"], words(txt.decode("utf-8", "replace"))
    with ThreadPoolExecutor(CORES) as ex:
        B2_REF.update(ex.map(one, items))


def score_b2(items, tools, runs):
    ref = B2_REF
    per = {t: {} for t in tools}
    for t in tools:
        d, res = tool_results(runs, "b2", t)
        for it in items:
            rw = ref[it["id"]]
            total = sum(rw.values())
            if total < 50:  # 文本层基本为空（扫描件等），没有可比的参照，不计
                per[t][it["id"]] = {"set": "B2", "f1": None, "no_text_layer": True}
                continue
            ow = words(output(d, res.get(it["id"])))
            per[t][it["id"]] = {"set": "B2", "f1": sum(min(c, ow[w]) for w, c in rw.items()) / total}
    def agg(scores, set_):
        return statistics.mean(s["f1"] for s in scores) if scores else None
    return per, agg, {"B2": "文字完整度（输出对 pdftotext 文本层的词覆盖率，逐份平均）"}


# ---- B1：olmOCR 自己的测试 ----

def score_b1(items, tools, runs):
    bench = DATA / "fresh/tools/olmocr-bench"
    py = DATA / "fresh/tools/py/bin/python"
    bdata = DATA / "olmocr_1403/bench_data"
    per = {t: {} for t in tools}
    for t in tools:
        d, res = tool_results(runs, "b1", t)
        with tempfile.TemporaryDirectory() as tmp:
            cand = Path(tmp) / "cand"
            tdata = Path(tmp) / "data"
            tdata.mkdir()
            for jl in bdata.glob("*.jsonl"):
                if not jl.stem.startswith("old_scans"):
                    (tdata / jl.name).symlink_to(jl)
            for it in items:
                md = output(d, res.get(it["id"]))
                if md:
                    rel = Path(it["location"]).relative_to("olmocr_1403/bench_data/pdfs").with_suffix("")
                    f = cand / f"{rel}_pg1_repeat1.md"
                    f.parent.mkdir(parents=True, exist_ok=True)
                    f.write_text(md, encoding="utf-8")
            k = max(1, CORES // max(1, NTOOLS))  # 各工具同时评，核按工具数分
            outs = [Path(tmp) / f"tests.{i}.jsonl" for i in range(k)]
            procs = [subprocess.Popen([str(py), "-I", str(HERE / "score_olmocr.py"), str(tdata), str(cand), str(o), str(i), str(k)],
                                      cwd=bench, env={**os.environ, "PYTHONPATH": str(bench)}, stderr=subprocess.DEVNULL)
                     for i, o in enumerate(outs)]
            for pr in procs:
                if pr.wait() != 0:
                    raise SystemExit("score_olmocr.py 失败")
            by_pdf = {}
            for o in outs:
                for line in o.read_text().splitlines():
                    r = json.loads(line)
                    by_pdf.setdefault(r["pdf"], []).append(r)
        for it in items:
            rel = str(Path(it["location"]).relative_to("olmocr_1403/bench_data/pdfs"))
            ts = by_pdf.get(rel, [])
            per[t][it["id"]] = {"set": "olmOCR", "tests": [[x["category"], x["passed"]] for x in ts],
                                "f1": sum(x["passed"] for x in ts) / len(ts) if ts else None}

    def agg(scores, set_):
        # olmOCR 的分数：每个类别内按测试算通过率，再对各类别取平均。官方口径（benchmark.py 的 Average Score）
        # 包括它给每个 PDF 加的 baseline 类；另一栏去掉 baseline 类
        cats = {}
        for s in scores:
            for c, ok in s["tests"]:
                if c != "baseline" or set_ == "olmOCR":
                    cats.setdefault(c, []).append(ok)
        return statistics.mean(sum(v) / len(v) for v in cats.values()) if cats else None
    return per, agg, {"olmOCR": "olmOCR-bench 官方总分（各类平均，含 baseline 类）",
                      "olmOCR 去掉 baseline": "各类平均，不含 baseline 类"}


# ---- B4：Office 结构保留率 ----

STRUCT = ["headings", "tables", "cells", "lists", "images", "footnotes", "notes", "order"]  # 同 score.py

def score_b4(items, tools, runs):
    score = BENCH / "office/scripts/score.py"
    per = {t: {} for t in tools}
    for t in tools:
        d, res = tool_results(runs, "b4", t)
        for split in ("dev", "holdout"):
            mine = [it for it in items if it["id"].startswith(f"b4-{split}-")]
            with tempfile.TemporaryDirectory() as tmp:
                tdir, odir = Path(tmp) / "truth", Path(tmp) / "out"
                tdir.mkdir()
                odir.mkdir()
                names = {}
                for it in mine:
                    name = Path(it["location"].split(":", 1)[1]).name
                    names[name] = it["id"]
                    tj = BENCH / f"office/truth/{split}/{name}.json"
                    if tj.exists():
                        (tdir / tj.name).symlink_to(tj)
                    (odir / f"{name}.md").write_text(output(d, res.get(it["id"])), encoding="utf-8")
                js = Path(tmp) / "r.json"
                subprocess.run([sys.executable, "-I", str(score), str(tdir), str(odir), "--json", str(js)], check=True, stdout=subprocess.DEVNULL)
                files = json.loads(js.read_text())["files"]
                for name, f in files.items():  # 每份：{元素类别: [保住, 总数]}；结构保留率只算 STRUCT 里的类别（同 score.py）
                    kept = sum(v[0] for k, v in f.items() if k in STRUCT)
                    total = sum(v[1] for k, v in f.items() if k in STRUCT)
                    per[t][names[name]] = {"set": f"B4 {split}", "kept": kept, "total": total, "f1": kept / total if total else None}

    def agg(scores, set_):
        k, n = sum(s["kept"] for s in scores), sum(s["total"] for s in scores)
        return k / n if n else None
    return per, agg, {"B4 dev": "结构保留率（score.py，开发集）", "B4 holdout": "结构保留率（score.py，留出集）"}


# ---- 汇总 ----

NTOOLS = 1


def score_one(fn, items, tool, runs):
    return fn(items, [tool], runs)[0][tool]

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sub", choices=["a1", "a2", "a4", "b1", "b2", "b3", "b4"])
    ap.add_argument("tools", nargs="+")
    ap.add_argument("--runs", default=str(MIX / "runs/quality"))
    ap.add_argument("--out", default=str(HERE / "results"))
    a = ap.parse_args()
    runs, sub, tools = Path(a.runs), a.sub, a.tools
    samples = {r["id"]: r for r in rows_tsv(HERE / f"samples/{sub}.tsv")}
    man = {l.split("\t")[0]: l.split("\t")[1] for l in (MIX / f"inputs/{sub}.manifest").read_text(encoding="utf-8").splitlines() if l}
    items = [{**samples[i], "path": p} for i, p in man.items()]  # 只算拿到了输入的份（抓不到的已在 fetched.tsv 里记不可用）
    fn = {"a1": score_a1, "b1": score_b1, "b2": score_b2, "b4": score_b4}.get(sub)
    fn = fn or functools.partial(score_robust, sub)
    global NTOOLS
    NTOOLS = len(tools)
    if sub == "b2":
        b2_refs(items)
    _, agg, label = fn(items, [], runs)  # 只取汇总函数和指标名
    # 各工具在 fork 出来的子进程里同时评（multiprocessing，fork 继承已读入的数据）
    with mp.get_context("fork").Pool(len(tools)) as pool:
        per = dict(zip(tools, pool.starmap(score_one, [(fn, items, t, runs) for t in tools])))

    model_ids = {i["id"] for i in items if i.get("model_subset") == "1"}
    assigned = {t: ({i["id"] for i in items} & model_ids if t.startswith("docling") else {i["id"] for i in items}) for t in tools}
    status = {t: {k: v["status"] for k, v in tool_results(runs, sub, t)[1].items()} for t in tools}
    report = {"sub": sub, "tools": {}, "label": label}
    for t in tools:
        ids = sorted(assigned[t])
        c = Counter(status[t].get(i, "not_run") for i in ids)
        ok = [s for s in ids if status[t].get(s) == "ok"]
        rep = {"items": len(ids), "status": dict(c), "completion": len(ok) / len(ids) if ids else None}
        if sub == "a2":
            gs = [per[t][i]["garble"] for i in ok if per[t][i]["garble"] is not None]
            rep["garble_mean"] = statistics.mean(gs) if gs else None
            rep["garble_pages"] = sum(1 for g in gs if g > 0)
        rep["per_item"] = per[t]
        report["tools"][t] = rep
    # 三样分数：按集（数据集）分开算
    if agg:
        sets = sorted({v["set"] for t in tools for v in per[t].values()})
        if sub == "b1":
            sets.append("olmOCR 去掉 baseline")
        base_set = lambda x: "olmOCR" if x == "olmOCR 去掉 baseline" else x
        groups = {"all": [t for t in tools if not t.startswith("docling")], "model_subset": tools}
        report["scores"] = {}
        for gname, gtools in groups.items():
            if not gtools or (gname == "model_subset" and not any(t.startswith("docling") for t in tools)):
                continue
            base = model_ids if gname == "model_subset" else {i["id"] for i in items}
            common = {i for i in base if all(status[t].get(i) == "ok" for t in gtools)}
            for set_ in sets:
                for t in gtools:
                    sc_all = [per[t][i] for i in base if i in per[t] and per[t][i]["set"] == base_set(set_) and per[t][i]["f1"] is not None]
                    sc_com = [per[t][i] for i in common if i in per[t] and per[t][i]["set"] == base_set(set_) and per[t][i]["f1"] is not None]
                    report["scores"].setdefault(gname, {}).setdefault(set_, {})[t] = {
                        "all_zero_fail": agg(sc_all, set_), "n_all": len(sc_all),
                        "common_only": agg(sc_com, set_), "n_common": len(sc_com)}
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / f"quality-{sub}.json").write_text(json.dumps(report, ensure_ascii=False, indent=1))
    with open(out / f"quality-{sub}.md", "w", encoding="utf-8") as f:
        f.write(f"# 质量轮 {sub.upper()}（竞品；pagesift 一栏待最终二进制）\n\n")
        f.write("| 工具 | 份数 | 完成率 ↑ | ok | empty | error | timeout | oom | crash |" + (" 乱码字符比例 ↓ | 有乱码的页 ↓ |" if sub == "a2" else "") + "\n")
        f.write("|---|---|---|---|---|---|---|---|---|" + ("---|---|" if sub == "a2" else "") + "\n")
        for t, r in report["tools"].items():
            s = r["status"]
            row = f"| {t} | {r['items']} | {r['completion']:.3f} | " + " | ".join(str(s.get(k, 0)) for k in ("ok", "empty", "error", "timeout", "oom", "crash")) + " |"
            if sub == "a2":
                row += f" {r['garble_mean']:.5f} | {r['garble_pages']} |" if r["garble_mean"] is not None else " — | — |"
            f.write(row + "\n")
        for gname, sets in report.get("scores", {}).items():
            f.write(f"\n## 分数（{'全部样本' if gname == 'all' else '模型子集 10%（含 Docling）'}）\n\n")
            f.write("| 集 | 指标 | 工具 | 全部题目，没完成按零分 ↑ | 份数 | 只算都完成的题 ↑ | 份数 |\n|---|---|---|---|---|---|---|\n")
            for set_, ts in sets.items():
                for t, v in ts.items():
                    fmt = lambda x: "—" if x is None else f"{x:.3f}"
                    f.write(f"| {set_} | {label.get(set_, '')} | {t} | {fmt(v['all_zero_fail'])} | {v['n_all']} | {fmt(v['common_only'])} | {v['n_common']} |\n")
    print(f"{sub}: 写入 {out / f'quality-{sub}.md'}")


if __name__ == "__main__":
    main()
