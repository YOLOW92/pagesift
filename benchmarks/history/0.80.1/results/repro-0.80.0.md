# 0.80.0 复现记录（2026-10-27，本容器：Linux x86_64，4 核，无 IPv6，纯净 main ba1c677 的 release 构建）

夹具由 scratchpad/gen_fixtures.py 生成（合成，确定性）：paper.pdf（40 页，作者行的脚注星号用 CMSY10 字体、8 pt、抬高 3.6 pt）、
paper-untitled.pdf（3 页，/Info 无 /Title）、paper-truncated.pdf（前者截到 19567/29427 字节：xref 丢失，第 3 页对象丢失）、
sjis.csv / gbk.csv（Shift_JIS、GBK 编码的表格）。

- S-3：`pdf_md paper.pdf` 第 3 行：`## Ashish $Vaswani^{\ast}$`；全文 41 处 `\ast`。真实 arXiv 1706.03762 本环境下载不到（出口代理拒绝 arxiv.org），只有本地用例覆盖。
- S-4：`POST /v2/parse paper-untitled.pdf` → metadata 无 title 字段；markdown 首行 `# Attention Is All You Need`。
- D-1：`POST /v2/parse sjis-small.csv`（Content-Type: text/csv; charset=shift_jis）→ 表头 `| ”Ô† | “sŽs | ”—Ê | ”õl |`（乱码），metadata.pagesift 无 warnings。
- D-2：`POST /v2/parse paper-truncated.pdf` → 200，numPages 2，totalPages 2，warnings 无；文件声明 3 页。
- IPv4：`crawl_gate`（scripts/ci.sh 的性能门）在本机绑定 `::1` 失败；`tests/multihost.rs` 同样（Address family not supported）。
  另外 `getent hosts a.root.localhost` 在本机无结果（nsswitch 只有 files dns），0.80.0 把 *.localhost 交给系统解析器。

# 0.80.1 复测（同一容器、同一夹具，release 构建）
- S-3：`pdf_md paper.pdf` 第 3 行 `## Ashish Vaswani* Noam Shazeer* …`；全文 0 处 `\ast`（0.80.0：41 处）。
- S-4：paper-untitled.pdf → metadata.title "Attention Is All You Need"，titleSource "inferred"。
- D-1：sjis-small.csv（charset=shift_jis）→ `| 番号 | 都市 | 数量 | 備考 |`；GBK 无声明按正文探测；声明 utf-8 实为 GBK 时纠正并给 decode: 警告。
- D-2：paper-truncated.pdf → warnings `pdf_repaired: …`、`pages_missing: 1 of 3 pages …; 2 converted`。
- IPv4：multihost 测试通过；crawl_gate 本机能跑（646–701 页/秒；0.80.0 借 /etc/hosts 跑出 578–633）。
性能：perf-0.80.0.tsv、perf-0.80.1.tsv、perf-0.80.1-csv2.tsv、perf-*-crawl.tsv；PDF 指令 +0.59%，ASCII CSV +0.51%，UTF-8 CJK CSV +0.49%。
