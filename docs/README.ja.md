<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.81.0

**AI エージェントと LLM データ基盤のための、超高速・メモリ有界スクレイピング＆本文抽出エンジン。**  
あらゆる Web サイト、論文、アーカイブを高品質な Markdown と構造化データに変換します。単一の Rust バイナリ、ネイティブ SDK、Firecrawl v1/v2 および Jina Reader デュアルプロトコル互換。測定した競合よりメモリ消費が少なくなっています（[benchmarks.md 第 9 節](benchmarks.md) 参照）。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![2億件の合成負荷](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](releases/0.70.0.md)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](releases/0.70.0.md)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · [简体中文](README.zh-CN.md) · **日本語** · [Español](README.es.md)

</div>

---

<!-- perf-glance -->
## パフォーマンス概要 (0.81)

PageSift 0.81 を 2026-10-29 に再計測。競合の数値は 2026-10-21/22/27/28 の記録で、再実行していません。1 行 = 1 ワークロード、PageSift は最良の構成。メモリは列で、別の順位にはしません。


### 大幅リード

| 分野 | ワークロード | 指標 | PageSift 0.81 | 最速の競合 | 競合 | 差 | ピークメモリ MB（PageSift / 競合） |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | olmOCR-bench 1,269 pages, per page (single thread) | ms/page | **2.2** | pypdf | 50.0 | **×22.32** | — |
| HTML → Markdown | 600 generated documents, per page | ms/page | **0.24** | Turndown | 1.9 | **×8.24** | — |
| Main content | Firecrawl 597 pages, single thread | p50 ms/page | **1.0** | crw | 7.4 | **×7.23** | — |
| Publication dates | htmldate 1,000 pages, date search | ms/page | **0.31** | htmldate (fast) | 2.0 | **×6.45** | — |
| Office / e-books | B4 81 files, in-process | files/s | **94.1** | Kreuzberg | 19.5 | **×4.82** | 38 / 231 |
| RAG chunking | 5 corpora, 1.4 MB | total ms | **11** | chonkie recursive | 35 | **×3.24** | — |
| PDF → Markdown | B1 144 pages, 1 worker | files/s | **258.5** | pdf_oxide | 100.6 | **×2.57** | 163 / 29 |
| RAG query filter | 472 questions, default settings | answer recall | **0.929** | Crawl4AI BM25 | 0.673 | **+0.256** | — |
| Wild crawl | polite, 40 sites × 60 pages, coverage | coverage | **0.740** | Crawl4AI (HTTP) | 0.529 | **+0.211** | — |
| Wild crawl | fast, 40 sites × 60 pages, coverage | coverage | **0.685** | Crawl4AI (HTTP) | 0.499 | **+0.186** | — |
| Office / e-books | B4 hold-out 39 files, structure kept | structure kept | **0.988** | Kreuzberg | 0.816 | **+0.172** | — |
| PDF → Markdown | olmOCR-bench 1,269 pages | official score | **0.628** | pymupdf4llm | 0.484 | **+0.144** | — |


### リード

| 分野 | ワークロード | 指標 | PageSift 0.81 | 最速の競合 | 競合 | 差 | ピークメモリ MB（PageSift / 競合） |
|---|---|---|---|---|---|---|---|
| PDF → Markdown | B1 144 pages, 16 workers | files/s | **580.6** | pdf_oxide | 501.7 | **×1.16** | 14 / 11 |
| Main content | A2 269 random pages, 16 workers | pages/s | **630.0** | Resiliparse | 559.3 | **×1.13** | 17 / 54 |
| HTML → Markdown | 600 generated documents, fidelity | mean F1 | **1.000** | markdownify | 0.936 | **+0.064** | — |
| Main content | WCXB test split, 150 pages | F1 | **0.866** | Readability + Turndown | 0.848 | **+0.018** | — |


*A2 は 2026-10-29 に再取得したため、絶対時間は 10-28 の A2 と直接比較できません。*


[完全なレポート](../benchmarks/history/0.81/results/20261029-102426/report.md)

<!-- /perf-glance -->

---

## 他ツールとの比較

**まだ後れている項目：** A2 replay, 269 pages, 1 worker 2.4 pages/s vs spider 156.6 pages/s; fast, 40 sites × 60 pages, time 16.6 seconds vs spider 1.4 seconds; B5 134 pages, completion 0.112 vs Docling (GPU) 1.000.

[完全なレポート](../benchmarks/history/0.81/results/20261029-102426/report.md).

---

## 規模：1 台で 2 億リクエスト（0.70）

- **192 vCPU、2 億件の合成負荷試験**：200,000,000/200,000,000 件完了、失敗 0 件、947,611 ページ/秒、所要 211.1 秒、RSS 最大 888.1 MiB。これは合成負荷の値であり、公開 Web の取得速度ではありません。出典：[`0.70_200m_regression.md`](history/0.70_200m_regression.md)。
- **メモリも入力の一つ**：cgroup v2 の `memory.max` を容量制御に反映し、768 MiB 制限下で 380 接続、817,346 ページ/秒、OOM なしを測定しました。出典：[0.70.0 リリースノート](releases/0.70.0.md)。

0.70.0 での測定値で、以降のバージョンでは再測定していません。

---

## 0.81.0 の変更

**関連度を「側面」で評価、そして全体を高速化。** フォーカスクロールは、ページがクエリの各側面を
どれだけ「扱っているか」で評価します（クエリ語と、サイト内で句として使われる隣接語の組
（*computer science* など）。タイトル・URL・見出しは本文より重く、各側面はサイト内での希少さで重み付け）。
すべてクロール自身のページから推定します。本文で判定した 1,833 ページで、有用・無用の判別は
検証用大学で AUC 0.79 → 0.86。速度：スレッド・NUMA ノードごとのアロケータキャッシュ、PDF のグリフ文字列の
インライン化、HTML のタイトル照合の線形化、CSV・xlsx の連続テーブル、fat LTO と PGO によるリリースビルド。
出力は 0.80.10 とバイト単位で同一です。詳細：[0.81.0 リリースノート](releases/0.81.0.md)。

## 0.80.10 の変更

**ページ上限が適応的に。** `crawl --query … --adaptive` は `--limit` なしで動きます。関連する新しい内容が
増え続ける間はクロールを続け、増えなくなったら、または関連ページが見つからないときに止まります
（`no_relevant_pages`）。上限は保険です（未指定時 500）。69 大学・5 分野で、クエリにぴったりのページは
適応版 4,208 ページ、0.80.9 の `--limit 12` は 645、`--limit 100` は 1,856。有用ページの割合も
高くなりました（25.7% 対 20.0%・17.2%）。探索：英語クエリで他言語のメインサイトに予算を
取られなくなり、`/about/housing/` は「概要」ページ扱いされません。本文抽出：プレスリリースの要約、
ニュースのサイドバー、アップロード HTML の相対リンク（`/v2/parse` に `url` フィールド）。
詳細：[0.80.10 リリースノート](releases/0.80.10.md)。

## 0.80.9 の変更

**0.80 系の最終版。** 集中クロールを 69 大学・5 方向（留学生、大学院出願、住居、計算機科学科、図書館）で検証：
0.80.0 と比べて、クエリにぴったりのページ 2.6% → 19.0%、関連以上 18.4% → 49.6%。キューのリンクは学習した
語の重みで採点し直し、同じページの別 URL は一度だけ返します。PDF はフォームの入力値、注釈、スタンプ、署名の
文字も取り出します（pdf.js の難しいテストファイル 100 件中 53 → 61 件）。1 ファイル 1 プロセスの `scrape` は起動が軽く
なりました（1 回の CPU 6.3 → 4.3 ms）。`doctor` は各エンジンを実際に起動し、起動できないものを警告します。
PATH 外や Playwright キャッシュのエンジンも見つけ、`crawl`／`map` に `--ignore-robots` を追加。アコーディオンや
FAQ の見出しを本文に残します。全ベンチマークは 0.80.0 比 2% 以内。0.80.6–0.80.8 はリリースしていません。
詳細：[0.80.9 リリースノート](releases/0.80.9.md)。

## 0.80.5 の変更

**正常に見えて実は問題のある結果を、はっきり知らせるようになりました。** 結果が不完全または誤っている可能性が
ある警告（PDF のページや本文の欠落、本文が薄い・一部だけ、出力の切り詰め、シェルだけのレンダリングなど）を
Firecrawl の `data.warning` に入れるので、Firecrawl 形式のクライアントにも見えます。`crawl --out` は
`summary.json` も書きます。大きなサイト枠の中の年齢確認（Steam の `/agecheck/`）と PDF と表示された HTML の
ブロックページは本文ではなく `ACCESS_BARRIER` を返します。Word・Excel・PowerPoint・CSV・EPUB は 16 MB の
テキストで止まり `output_truncated` を出します（408 KB の DOCX が 84 MB の Markdown に展開されていました）。
PDF の損傷警告は文書の実際のページ数で数え、一部だけ失われたページも示します。ネットワークが静かになっても
ページが空のシェルのままなら、レンダリングは最大 1.5 秒待ちます。`--engine`・`--format`・`--sitemap` の
綴り間違いはエラーです。すべてのベンチマークは 0.80.0 から 2% 以内。0.80.4 はリリースしていません。
詳細：[0.80.5 リリースノート](releases/0.80.5.md)。

## 0.80.3 の変更

**フォーカスクロールを実在の大学サイトで検証しました。** 同じ 38 大学のトップページを 0.80.0 と 0.80.3 で
`crawl --query "international students admission tuition scholarship visa" --adaptive --limit 12`。
両方が到達できた 32 サイトで：有用なページ 29.6% → 66.1%、留学生向けページ 6.3% → 38.9%、サイトあたり
有用ページの中央値 1.5 → 8（URL とタイトルによる自動判定。スクリプトと生データは `benchmarks/history/0.80.3/`）。
クエリ付きクロールはサイト全体を対象にし、失敗した取得はページ予算に戻し、`hreflang` と英語サブドメインから
英語サイトを見つけ、ナビゲーションのメガメニューを一覧扱いしません。切り詰められた PDF でページの本文が
失われた場合はそのページを示します（`content_missing`）。詳細：[0.80.3 リリースノート](releases/0.80.3.md)。

## 0.80.2 の変更

**フォーカスクロールが、求められたページを取ってくるようになりました。** テスターが 159 大学のサイトで
`crawl --query`（*international students admission tuition scholarship visa*、各サイト 12 ページ）を実行したところ、
有用なページは 25.6% でした。リンクの採点・スケジューリング・停止規則を書き直しています：すべてのリンクをアンカーテキストで
採点（ナビゲーションも対象）、検索語をサイト内での希少さで重み付け、ニュースや「大学について」系を減点、セクションを順番に処理、
サブドメインを既定で対象に、エラーページを除外、関連ページだけを根拠にする適応停止。報告の失敗パターンを再現した合成 8 サイトでは
有用ページが 7.3% → 82.6%。**開発環境から実サイトへは接続できないため、この数値は合成サイトのみのものです。**
新機能：`attachments` が関連ページからリンクされた PDF・Office 文書を取得；各結果にクロール経路（`depth`、`parent`、`score`、`attachmentOf`）。
PDF の追加修正：章だけのしおりでもタイトルが空にならない；ページツリーのルートより前で切れた線形化 PDF をエラーにせず変換。
詳細：[0.80.2 リリースノート](releases/0.80.2.md)。

## 0.80.1 の修正

0.80 のレビューで見つかった問題の修正です（HTML 抽出は変更なし。PDF の出力は語の後ろに上付きの脚注記号がある箇所だけ変わります。）：Shift_JIS、GBK、または文字セットが宣言された
CSV を HTML と同じ方法でデコード（以前は警告なしに文字化け）；破損した PDF を部分的にしか復元できないときはその旨を警告
（`pdf_repaired`、`pages_missing`）；著者名の後の脚注記号を数式にしない（`Ashish $Vaswani^{\ast}$` → `Ashish Vaswani*`）；
タイトルが埋め込まれていない PDF は推定したタイトルを `metadata.title` に入れる（`titleSource: inferred`）；
`*.localhost` はどのマシンでもループバックに解決され、テストと性能ゲートが IPv4 のみの環境でも動作；
`NOTICE`（Adobe cmap-resources のライセンスを追加）をすべてのバイナリに同梱。詳細：[0.80.1 リリースノート](releases/0.80.1.md)。

## 0.80.0 の新機能

**PDF の構造を保ち、取りこぼしの可能性を自分で知らせる。**

- **自前の PDF ファイル層**で lopdf を置き換え：xref、オブジェクトストリーム、デコード、暗号化、壊れたファイルの修復を自前で行い、より速く、より省メモリに。
- **PDF の見出しに正しい階層**：書き手が付けた構造ツリーとしおりを優先して読み、ない場合は番号・文字サイズ・太字から推定。AiSeek レポートの 13 件の PDF で（高いほど良い）：再現率 0.595 → 0.947、適合率 0.400 → 0.876、階層一致率 0.819 → 0.957、空の見出し 3 → 0。
- **ヘッダー・フッターと図**：`/Artifact` マーク、ページ番号付きの柱、ベクター図を認識。olmOCR-bench `headers_footers` 0.681 → 0.734（高いほど良い）、13 件の PDF での誤った表 196 → 9（低いほど良い）。
- **CJK text without ToUnicode**: Chinese, Japanese and Korean PDFs whose CID fonts carry no
  `/ToUnicode` map (LaTeX/ctex with Fandol fonts, many others) lost all CJK text; it is now decoded
  through the Adobe-GB1/CNS1/Japan1/Korea1 collections or the embedded TrueType cmap. On 19 diverse
  Chinese PDFs, inferred-heading recall 0.497 → 0.826 (higher is better).
- **`POST /v2/parse`**：Firecrawl 互換のファイルアップロード解析。PDF・HTML・Word（.docx）・Excel（.xlsx）・PowerPoint（.pptx）・EPUB・CSV に対応。`/v2/scrape` と `pagesift scrape <ファイル>` も同じ形式を変換します。
- **新しい警告 `partial_content`、`replies_omitted`**：抽出した本文が不完全な可能性があるときに通知。保存済みの WCXB 1,842 ページで、本文を失ったのに警告なしの割合 0.842 → 0.667、誤警告 0.174 → 0.130（どちらも低いほど良い）。
- **修正**：バッチ取得で同じサイトの速いページが遅いページを待たなくなりました。旧プロトコルの MCP クライアントも監査ログに名前が残ります。`pagesift doctor` は監査ログに書き込みません。

詳細、数値の出典、ホールドアウトの結果：[0.80.0 リリースノート](releases/0.80.0.md)。

以前のバージョン（0.73、0.72.8、0.71、0.70 など）は [CHANGELOG](../CHANGELOG.md) と[リリースノート](releases/)を参照してください。
トップページから移した節（0.70 の 7 領域表、0.50 の Spider 比較と 10 領域マトリクス、新旧の数値が混在する比較表など）は、
出典とともに [history/homepage-ja.md](history/homepage-ja.md) にそのまま残しています。

---

## クイックスタート

### 1. Docker / セルフホストサーバー

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.81.0
```

#### Firecrawl SDK の差し替え
既存の Firecrawl コードの接続先を変更するだけで即座に稼働：

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### ファイルのアップロード解析（`/v2/parse`）
ローカルや非公開の PDF・HTML・Word・Excel・PowerPoint・EPUB・CSV ファイルを URL に置かずにそのまま解析できます。Firecrawl 互換の multipart 形式、上限 50 MB、応答は `/v2/scrape` と同じです（[詳細](parse-api.md)）。Office・EPUB・CSV の変換は pagesift 独自の Rust 実装です（ZIP と XML の読み取りも独自）。コマンドラインでは `pagesift scrape report.docx` で同じ結果になります：

```bash
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'
```

#### Jina Reader の差し替え
任意の LLM エージェントや curl からダイレクトに呼び出し：

```bash
# Markdown を直接取得
curl -s http://localhost:3002/r/https://en.wikipedia.org/wiki/Rust_(programming_language)

# SSE リアルタイムストリーミング
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Python SDK

```bash
pip install pagesift
```

```python
import pagesift

page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)

results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

---

### 3. Node.js / TypeScript SDK

```bash
npm install @pagesift/node
# または
npm install pagesift
```

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

const page = await scrape('https://example.com', {
  formats: ['markdown', 'links']
})
console.log(page.markdown)

const stream = scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })
for await (const page of stream) {
  console.log(`Scraped: ${page.url} in ${page.elapsedMs}ms`)
}
```

---

### 4. コマンドラインツール (CLI)

```bash
cargo install pagesift --locked

pagesift scrape "https://news.ycombinator.com" --format markdown

pagesift crawl "https://docs.rs" --limit 5000 --tuning-mode adaptive --out ./data/
```

### 5. AI エージェント共用の MCP サーバー

```bash
# マシン上のすべてのエージェントで 1 つのサービスを共有（127.0.0.1 のみ）
pagesift mcp http --port 3003 --audit-log ~/.local/state/pagesift/audit.jsonl
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

ブラウザとキャッシュを共有し、取得ごとに監査ログを 1 行記録、疑わしい結果は HTML スナップショットを保存し、
`pagesift_report_issue` ツールを提供します。詳細は [Running pagesift as a shared MCP service](mcp-service.md)。

---

## コミュニティ

ご質問やフィードバックは [Discord](https://discord.gg/esA3wMZVvA) へどうぞ。まもなく公開予定のホスト型 API のクローズドベータ（未公開）についても、こちらでお知らせします。

---

## ライセンスと商用利用

- **オープンソース版**：[AGPL-3.0](../LICENSE) ライセンスに基づく提供。
- **商用ライセンス**：AGPL 義務の免除やクローズドソースへの組み込みをご希望の場合は、[商用ライセンスのご案内](../COMMERCIAL.md) を確認いただくか、`licensing@pagesift.dev` までお問い合わせください。
