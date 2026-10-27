<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.80.0

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

リードの大きさで 3 段階に分けて示します。0.80 の最終値はリリース版バイナリで各領域の最強ツールと再測定中です。
測定が終わるまで表は空欄のままにし、未測定の数値は書きません。各行には手法・スクリプト・日付付きの結果へのリンクを付けます。

### 大差でリード

| 領域 | 指標（方向） | PageSift 0.80 | 最強の競合 | 根拠 |
| :--- | :--- | :--- | :--- | :--- |
| _最終測定の完了後に記入_ | | | | |

### リード

| 領域 | 指標（方向） | PageSift 0.80 | 最強の競合 | 根拠 |
| :--- | :--- | :--- | :--- | :--- |
| _最終測定の完了後に記入_ | | | | |

### やや劣るが僅差（改善に取り組み中）

| 領域 | 指標（方向） | PageSift 0.80 | 最強の競合 | 根拠 |
| :--- | :--- | :--- | :--- | :--- |
| _最終測定の完了後に記入_ | | | | |

それまでの最新の測定結果（方向・バージョン・日付付き）は [benchmarks.md](benchmarks.md)、
領域ごとのスクリプトと生データは [benchmarks/](../benchmarks/) にあります。

---

## 規模：1 台で 2 億リクエスト（0.70）

- **192 vCPU、2 億件の合成負荷試験**：200,000,000/200,000,000 件完了、失敗 0 件、947,611 ページ/秒、所要 211.1 秒、RSS 最大 888.1 MiB。これは合成負荷の値であり、公開 Web の取得速度ではありません。出典：[`0.70_200m_regression.md`](history/0.70_200m_regression.md)。
- **メモリも入力の一つ**：cgroup v2 の `memory.max` を容量制御に反映し、768 MiB 制限下で 380 接続、817,346 ページ/秒、OOM なしを測定しました。出典：[0.70.0 リリースノート](releases/0.70.0.md)。

0.70.0 での測定値で、以降のバージョンでは再測定していません。

---

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
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.80.0
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
