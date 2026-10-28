<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.80.5

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

## 他ツールとの比較

2026-10-27、0.80.0 リリース版バイナリで測定。各領域 3 ツール（Rust 製 1 つ、広く比較される 2 つ）、すべて既定設定、同一マシン（i7-10700、15 GB）。サンプルはルール凍結後に固定シードで抽出し、選別していません。各行は手法・バージョン・件数・生データにリンクしています。

### 大差でリード

| 領域 | 指標（方向） | PageSift 0.80 | 最強の競合 | 根拠 |
| :--- | :--- | :--- | :--- | :--- |
| Office & e-books (docx, xlsx, pptx, epub, csv) | Structure kept, held-out ↑ | **0.988** | MarkItDown 0.717 | [§4](../benchmarks/mix/results/report-2026-10-27.md) |
| PDF, olmOCR-bench born-digital (1,269 pages) | Official score ↑ | **0.626** | pymupdf4llm 0.484 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |
| Hard web pages (166 adversarial) | Completion ↑ / timeouts ↓ | **0.723** / 0 | crw 0.590 / 27 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Office & e-books speed (81 files, 1 worker) | Files/s ↑ | **57.4** | extractous 9.9 | [§4](../benchmarks/mix/results/report-2026-10-27.md) |

### リード

| 領域 | 指標（方向） | PageSift 0.80 | 最強の競合 | 根拠 |
| :--- | :--- | :--- | :--- | :--- |
| Main-content extraction, WCXB | F1 ↑ | **0.866** | Readability 0.848 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction, AEB | F1 ↑ | **0.956** | trafilatura 0.950 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction speed (269 pages, 16 workers) | Pages/s ↑ | **614** | crw 387 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| PDF speed (144 files, 1 worker) | Files/s ↑ | **114** | pdf_oxide 94 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |

### やや劣るが僅差（改善に取り組み中）

| 領域 | 指標（方向） | PageSift 0.80 | 最強の競合 | 根拠 |
| :--- | :--- | :--- | :--- | :--- |
| PDF vs a layout model (144-page subset) | olmOCR score ↑ | 0.650 | Docling (CPU) 0.696 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |
| Random real PDFs (71) | Text completeness ↑ | 0.936 | pdf_oxide 0.962 | [§3](../benchmarks/mix/results/report-2026-10-27.md) |
| Fetching, 269 random pages (local replay) | Completion ↑ | 0.937 | Scrapy 0.981 | [§2 — pagesift refuses captcha/block pages](../benchmarks/mix/results/report-2026-10-27.md) |
| Main-content extraction, Firecrawl set | Truth recall ↑ | 0.477 | crw 0.511 | [§1](../benchmarks/mix/results/report-2026-10-27.md) |
| Hard PDFs (100 pdf.js test files) | Completion ↑ | 0.560 | pymupdf4llm 0.680 | [§3 — the largest gap](../benchmarks/mix/results/report-2026-10-27.md) |

### Behind

| Domain | Metric (direction) | PageSift 0.80 | Strongest competitor | Evidence |
| :--- | :--- | :--- | :--- | :--- |
| Fetching speed (269 pages, 1 worker) | Pages/s ↑ | 2.7 | spider 153 | [§2 — default auto engine, p95 1.5 s](../benchmarks/mix/results/report-2026-10-27.md) |
| PDF speed (144 files, 16 workers) | Files/s ↑ | 142 | pdf_oxide 526 | [§3 — one process per file scales poorly](../benchmarks/mix/results/report-2026-10-27.md) |

それまでの最新の測定結果（方向・バージョン・日付付き）は [benchmarks.md](benchmarks.md)、
領域ごとのスクリプトと生データは [benchmarks/](../benchmarks/) にあります。

---

## 規模：1 台で 2 億リクエスト（0.70）

- **192 vCPU、2 億件の合成負荷試験**：200,000,000/200,000,000 件完了、失敗 0 件、947,611 ページ/秒、所要 211.1 秒、RSS 最大 888.1 MiB。これは合成負荷の値であり、公開 Web の取得速度ではありません。出典：[`0.70_200m_regression.md`](history/0.70_200m_regression.md)。
- **メモリも入力の一つ**：cgroup v2 の `memory.max` を容量制御に反映し、768 MiB 制限下で 380 接続、817,346 ページ/秒、OOM なしを測定しました。出典：[0.70.0 リリースノート](releases/0.70.0.md)。

0.70.0 での測定値で、以降のバージョンでは再測定していません。

---

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
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.80.5
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
