<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/docs/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.81.4

**Web ページ、PDF、Office ファイルを LLM と AI エージェント向けのクリーンな Markdown に変換します。**  
セルフホストできる単一の Rust バイナリ：CLI、Firecrawl 互換 HTTP API、Jina Reader エンドポイント、MCP サーバー、Python・Node.js パッケージ。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · [简体中文](README.zh-CN.md) · **日本語** · [Español](README.es.md)

</div>

<!-- readme-template: v1 -->
<!-- section: overview -->
## 概要

pagesift は URL を取得する（JavaScript のレンダリングはページに必要な場合だけ行います）か、ローカルファイルを読み込み、
本文を残して、Markdown とメタデータを返します。単一ページ、バッチ、サイト全体の取得に対応し、
コマンド、ライブラリ、HTTP サービス、MCP サーバーとして動作します。

置き換え、または併用できるツール：**Firecrawl**（同じ v1/v2 API、セルフホスト）、**Jina Reader**（`/r/<url>`）、
**Crawl4AI**、**trafilatura** / **Readability**、**pymupdf4llm** / **pypdf**、**Kreuzberg**。

<!-- section: facts -->
## 早見表

- **入力：** URL（HTTP、必要な場合はヘッドレスブラウザ）、HTML、PDF、Word（.docx）、Excel（.xlsx）、PowerPoint（.pptx）、EPUB、CSV
- **出力：** Markdown、HTML、生 HTML、リンク、画像、メタデータ（タイトル、公開日など）、JSON。結果が不完全な場合は警告
- **インターフェース：** CLI · Firecrawl v1/v2 HTTP API · Jina Reader `/r/` · MCP（stdio または HTTP）· Python · Node.js / TypeScript · Rust crate
- **プラットフォーム：** Linux x86_64 / aarch64（glibc と musl）、macOS x86_64 / arm64、Windows x86_64。Docker イメージ `ghcr.io/yolow92/pagesift`
- **クロール：** 既定で robots.txt とホストごとのレート制限、サイトマップ、クエリによるフォーカスクロール、有界メモリ
- **メモリ：** 0.81 の各ベンチマークでワーカーあたりピーク 14–380 MB（16 ワーカーで HTML・PDF を処理するときが最小）。容量は cgroup のメモリ上限から自動で決まります（768 MiB で OOM なし）
- **向かない用途：** スキャン PDF や画像の OCR。pagesift はテキスト層のみを読みます（下の「向いている場面、向いていない場面」を参照）
- **ライセンス：** AGPL-3.0、または[商用ライセンス](COMMERCIAL.md)
- **最新版：** 0.81.4 · [リリースノート](releases/0.81.4.md)

<!-- section: install -->
## インストールと使い方

| | インストール | 最初の呼び出し |
|---|---|---|
| CLI | `cargo install pagesift --locked` | `pagesift scrape https://example.com` |
| Python | `pip install pagesift` | `pagesift.scrape("https://example.com").markdown` |
| Node.js | `npm install @pagesift/node` | `(await scrape("https://example.com")).markdown` |
| サーバー | `docker run -p 3002:3002 ghcr.io/yolow92/pagesift:0.81.4` | `curl localhost:3002/r/https://example.com` |
| MCP | — | `claude mcp add pagesift -- pagesift mcp` |

**CLI**

```bash
pagesift scrape "https://news.ycombinator.com"              # Markdown を標準出力へ
pagesift scrape report.pdf                                  # ローカルの PDF、Office、EPUB、CSV、HTML ファイル
pagesift crawl "https://docs.rs" --limit 500 --out ./data/  # 1 ページにつき 1 つの Markdown ファイル
pagesift crawl "https://www.example.edu" --query "graduate admission" --adaptive
pagesift map "https://docs.rs"                              # サイトの URL を一覧表示
pagesift doctor                                             # ブラウザと環境をチェック
```

**Python**

```python
import pagesift

page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

**Node.js / TypeScript**

```typescript
import { scrape, scrapeMany } from '@pagesift/node'

const page = await scrape('https://example.com', { formats: ['markdown', 'links'] })
console.log(page.markdown)

for await (const p of scrapeMany(['https://a.com', 'https://b.com'], { concurrency: 4 })) {
  console.log(p.url, p.elapsedMs)
}
```

**HTTP サーバー（Firecrawl・Jina Reader 互換）**

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.81.4
```

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")  # 既存の Firecrawl コード、URL だけ変更
print(app.scrape_url("https://example.com", params={"formats": ["markdown"]})["markdown"])
```

```bash
curl -s http://localhost:3002/r/https://example.com -H 'Authorization: Bearer sk-test'   # Jina Reader 形式
curl -s http://localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf -F 'options={"formats":["markdown"]}'                            # ファイルをアップロード
```

エンドポイント：`/v1` と `/v2` の `scrape`、`batch/scrape`、`crawl`、`map`。`/v2/parse`（ファイルアップロード、上限 50 MB、
[詳細](parse-api.md)）。`/r/<url>`（プレーンな Markdown または `text/event-stream`）。

**AI エージェント向け MCP サーバー**

```bash
pagesift mcp                                    # stdio、1 つのエージェント用
pagesift mcp http --port 3003                   # 共有のローカルサービス 1 つ（127.0.0.1 のみ）
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

ツール：`pagesift_scrape`、`pagesift_batch_scrape`、`pagesift_crawl`、`pagesift_crawl_status`、`pagesift_map`。
ブラウザとキャッシュの共有、監査ログ、問題報告ツール：[共有 MCP サービス](mcp-service.md)。

<!-- section: choose -->
## 向いている場面、向いていない場面

**向いている場面**

- Web ページや文書を LLM、RAG インデックス、エージェントに渡す。見出し・リスト・表・リンクを保った Markdown で
- Firecrawl や Jina Reader 形式のサービスを自分のマシンでセルフホストする。ページ単位の料金はかかりません
- 混在した入力：同じ呼び出しで HTML、PDF、Office ファイル、電子書籍を扱えます
- 小さなマシンやコンテナ：メモリは有界で、cgroup の上限から容量を決めます

**別のツールを選ぶべき場合**

- **スキャンした PDF や画像に OCR が必要。** pagesift はテキスト層だけを読みます。スキャン 134 ページでの完了率は 0.112 で、GPU 上の Docling は 1.000 でした。Docling、olmOCR などの OCR パイプラインを使ってください。
- **礼儀正しさが不要で、とにかく最速のクロールがほしい。** pagesift は同じホストへのリクエストの間に待ち時間を入れ、robots.txt に従い、失敗を再試行します。純粋なクロール時間では spider がはるかに速くなります（40 サイト × 60 ページで 1.4 s 対 16.6 s）。
- **AGPL が使えず**、商用ライセンスも選択肢にない場合：寛容なライセンスのツールを使ってください。
- **今すぐマネージドのクラウドサービスが必要。** pagesift はセルフホストです。ホスト型 API はまだ公開していません。

<!-- section: performance -->
## パフォーマンス

<!-- perf-glance -->
↑ 高いほど良い · ↓ 低いほど良い。分野ごとに最も強い 1 項目。方法と出典は完全なレポートに。

| タスク | 指標 | PageSift | 最良の他ツール | 差 |
|---|---|---:|---|---:|
| PDF → Markdown — olmOCR-bench 1,269 pages, per page (single thread) | ms/page ↓ | **2.2** | pypdf 50.0 | **×22.32** |
| HTML → Markdown — 600 generated documents, per page | ms/page ↓ | **0.24** | Turndown 1.9 | **×8.24** |
| Main content — Firecrawl 597 pages, single thread | p50 ms/page ↓ | **1.0** | crw 7.4 | **×7.23** |
| Publication dates — htmldate 1,000 pages, date search | ms/page ↓ | **0.31** | htmldate (fast) 2.0 | **×6.45** |
| Office / e-books — B4 81 files, in-process | files/s ↑ | **94.1** | Kreuzberg 19.5 | **×4.82** |
| RAG chunking — 5 corpora, 1.4 MB | total ms ↓ | **11** | chonkie recursive 35 | **×3.24** |
| RAG query filter — 472 questions, default settings | answer recall ↑ | **0.929** | Crawl4AI BM25 0.673 | **+0.256** |
| Wild crawl — polite, 40 sites × 60 pages, coverage | coverage ↑ | **0.740** | Crawl4AI (HTTP) 0.529 | **+0.211** |

**まだ後れている項目：** A2 replay, 269 pages, 1 worker 2.4 pages/s vs spider 156.6 pages/s; fast, 40 sites × 60 pages, time 16.6 seconds vs spider 1.4 seconds; B5 134 pages, completion 0.112 vs Docling (GPU) 1.000.

*A2 のスループットは単一のループバックアドレス上のリプレイサーバーに対して測定しており、269 ページすべてが同じホストになります。PageSift は既定で同一ホストへのリクエスト間隔を 1 秒空けますが、spider にはこの制限がありません。この行の差は主にこの間隔によるもので、エンジン速度の差ではありません。*

[完全なレポート](../benchmarks/history/0.81/results/20261029-102426/report.md)

<!-- /perf-glance -->

規模（0.70、合成のローカルオリジン、実際のインターネット上の速度ではありません）：192 vCPU で 200,000,000 リクエスト、失敗 0、
947,611 ページ/秒、ピーク RSS 888 MiB。768 MiB の cgroup 内でも OOM なしで 817,346 ページ/秒を維持しました。
出典：[0.70 の実行](history/0.70_200m_regression.md)、[全ベンチマーク](benchmarks.md)。

<!-- section: features -->
## 機能

- **抽出：** 本文検出。見出し・リスト・表・コード・数式を往復変換できる Markdown。公開日。`includeTags` / `excludeTags` CSS フィルタ。不完全な結果を黙って返す代わりに `partial_content`、`pages_missing`、`output_truncated` などの警告
- **文書：** PDF（構造ツリー、しおり、ヘッダー・フッター、ToUnicode のない CJK フォント、破損ファイル）、Word、Excel、PowerPoint、EPUB、CSV の自前 Rust リーダー
- **レンダリング：** `engine: auto` はまず HTTP で取得し、それで本文が増える場合だけヘッドレスブラウザを開きます。Docker イメージにはブラウザが同梱されています
- **クロール：** ページが関連する内容を増やさなくなった時点で止まるフォーカスクロール（`--query … --adaptive`）。添付ファイル（リンクされた PDF・Office ファイル）。ページごとのクロール経路。サイトマップ。重複 URL とトラップ URL の処理。ホストごとのトークンバケットと 429/503 時のバックオフ
- **運用：** 有界キュー、CPU と cgroup メモリから決める容量、サーバーのジョブ永続化・webhook・キーごとの制限・メトリクス。すべての取得の監査ログ
- **安全性：** 既定でプライベートネットワークと DNS リバインディングを防御（`--allow-cidr` で許可）、ブラウザのサンドボックス化

アーキテクチャ：[architecture.md](architecture.md)。

<!-- section: license -->
## ライセンス

[AGPL-3.0](../LICENSE)。AGPL の義務なしでのプロプライエタリなリンクや非公開デプロイについては、
[商用ライセンス](COMMERCIAL.md)を確認するか、`licensing@pagesift.dev` までご連絡ください。

<!-- section: changelog -->
## 0.81.4 の変更

依存関係のセキュリティ更新：tokio 1.54.1 と zstd 0.14.3（zstd-safe 8.2.0）に上げ、RustSec の 4 件の勧告
（RUSTSEC-2026-0384、-0391、-0398、-0403）を修正しました。API の変更はありません。
[リリースノート](releases/0.81.4.md) · [全リリース](releases/) · [CHANGELOG](../CHANGELOG.md)

---

## コミュニティ

ご質問やフィードバック：[Discord](https://discord.gg/esA3wMZVvA) · [Issues](https://github.com/YOLOW92/pagesift/issues) · [Discussions](https://github.com/YOLOW92/pagesift/discussions)

## 引用

```bibtex
@software{pagesift2026,
  title  = {PageSift: Rust Web and Document Extraction Engine for AI Pipelines},
  author = {YOLOW92},
  year   = {2026},
  url    = {https://github.com/YOLOW92/pagesift},
  note   = {Version 0.81.4}
}
```

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
