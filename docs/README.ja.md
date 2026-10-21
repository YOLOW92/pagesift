<div align="center">

# pagesift

**LLM と AI エージェントのための、高速でマナーを守る Web スクレイピング。**
あらゆる Web サイトをクリーンな Markdown と構造化データに変換します。Rust 製のバイナリ 1 つ、Firecrawl 互換 API、そしてごくわずかなメモリで動作します。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/pagesift.svg)](https://www.npmjs.com/package/pagesift)

[English](../README.md) · [简体中文](README.zh-CN.md) · **日本語** · [Español](README.es.md)

</div>

---

pagesift は、AI パイプライン、RAG、エージェント向けのセルフホスト型スクレイピング・クローリングエンジンです。ページを取得し、必要な場合にだけ JavaScript をレンダリングし、本文を抽出して、Markdown、HTML、リンク、メタデータを返します。インターフェースは [Firecrawl v2](https://docs.firecrawl.dev) 互換の HTTP API、MCP サーバー、CLI、ネイティブの Python パッケージと Node パッケージを用意しています。

サービスとしての運用を前提に設計されています。メモリ使用量の上限、バックプレッシャー、ジョブの永続化、webhook、キーごとの制限、メトリクスを備えています。また、ルールも守ります。robots.txt に従い、ホストごとにレート制限をかけ、正直な User-Agent を名乗り、ボット対策の回避は一切行いません。

## pagesift を選ぶ理由

| | |
|---|---|
| ⚡ **高速** | 本文抽出は 1 ページあたり **0.47 ms**（p50、シングルスレッド）。crw の 16 倍、Crawl4AI の 80 倍の速さです。スロットリングなしの HTTP クロールでは、1 台のマシンで **3,500+ ページ/秒** を達成します。 |
| 🪶 **軽量** | 200 サイト × 100 ページのクロールを **約 300 MB** で実行します。Spider では 5.5 GB が必要です。本番サービスで Crawl4AI を置き換えたところ、ピークメモリが 1.4 GB から **225 MB** に減りました。 |
| 🎯 **高精度** | Firecrawl のラベル付きデータセットで最高の本文抽出スコア **0.733** を記録しました（crw は 0.703、Crawl4AI は 0.666）。ボイラープレートの混入も半分です。公開日の精度は専用ライブラリ htmldate と同等（同ライブラリの評価セットで F1 **0.948**）で、処理時間は 4 ms に対して **0.06 ms** です。 |
| 🧠 **スマートなレンダリング** | レンダリングが必要なページだけを、対応できる最も軽量なエンジンで処理します：Lightpanda → moli → Chrome。ブラウザを使っても結果が変わらないサイトはサイトごとに学習します。 |
| 🔌 **そのまま置き換え可能** | Firecrawl v2 API（`/v2/scrape`、`/crawl`、`/map`、`/search`、`/extract`、`/batch/scrape`）に対応しているため、既存の Firecrawl SDK を変更なしで使えます。エージェント向けの MCP サーバーも備えています。 |
| 🛡️ **デフォルトで安全** | すべてのリクエスト、リダイレクト、ブラウザのサブリクエスト、webhook に SSRF 対策を適用し、DNS リバインディングもチェックします。ブラウザのサンドボックスを無効にすることはありません。 |

詳細で再現可能な数値：**[docs/benchmarks.md](benchmarks.md)**

## クイックスタート

### Docker

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=change-me ghcr.io/yolow92/pagesift:0.12

curl -s http://localhost:3002/v2/scrape \
  -H 'Authorization: Bearer change-me' -H 'Content-Type: application/json' \
  -d '{"url": "https://example.com", "formats": ["markdown", "links"]}'
```

イメージには JavaScript レンダリング用の Lightpanda が含まれています。

### コマンドライン

```bash
# from a release binary (Linux, macOS) or with Cargo
cargo install pagesift

pagesift scrape https://example.com                 # Markdown to stdout
pagesift crawl https://example.com --out ./site     # one Markdown file per page
pagesift map https://example.com                    # list a site's URLs
pagesift serve --port 3002                          # the HTTP API
pagesift doctor                                     # check browsers, network, config
```

### Python

```bash
pip install pagesift
```

```python
import pagesift

doc = pagesift.scrape("https://example.com")
print(doc.markdown)

# many pages at once, on every core
for doc in pagesift.scrape_iter(["https://example.com/a", "https://example.com/b"]):
    print(doc.metadata["title"])
```

### Node.js

```bash
npm install pagesift
```

```js
const { Scraper } = require('pagesift')

const scraper = new Scraper()
const page = await scraper.scrape('https://example.com')
console.log(page.markdown)
```

### MCP（Claude、Cursor などのエージェント）

```bash
pagesift mcp            # stdio; tools: pagesift_scrape, pagesift_batch_scrape, pagesift_crawl, pagesift_crawl_status, pagesift_map
```

## 機能

**スクレイピングとクローリング**
- 出力形式：`markdown`、`html`（クリーン化済み）、`rawHtml`、`links`、`images`。OpenAI 互換の任意の LLM と組み合わせて `summary` と `json`（スキーマに基づく抽出）も利用できます。`changeTracking`（前回の訪問との差分。キャッシュが必要です）。
- インクルード/エクスクルードするパス、深さと件数の上限、サイトマップ、robots.txt を指定してクロールできます。**クエリ指向クロール**（`query`、`adaptive`）では、関連性の高いページから順に訪問し、新しいページが役に立たなくなった時点で停止します。
- **実際のサイトでも上限を無駄にしないクロール：** トラッキング用パラメータやセッション ID を除去し、バイナリや無限に続くパスを読み飛ばし、同じページの別名 URL を自動で学習し、カレンダーやページ送りの罠を後回しにし、一時的な失敗は後で再試行し、429 が返ると自動で速度を落とします。雑多なサイトを模したテストでは、同じページ上限で本文ページの 74% を見つけます（次点は 53%、ベンチマーク参照）。
- `queryFilter` はクエリに関連する段落だけを残し（BM25）、LLM のトークンを節約します。
- PDF のテキスト抽出。メタデータ（タイトル、説明、言語、著者、公開日時、Open Graph）。公開日は htmldate と同じ手順（meta タグ、JSON-LD、日付要素、本文のパターン）で探し、約 20 言語の月名に対応し、ページ自身のタイムゾーンを保持します。
- 条件付きリクエスト（`ifNoneMatch`、`ifModifiedSince`）、コンテンツキャッシュ（`maxAge`）、変更検知用のコンテンツハッシュ。

**レンダリング**
- エンジンの自動選択とフォールバック（Lightpanda、moli、Chrome/Chromium/chrome-headless-shell）。CDP 経由でリモートブラウザも利用できます。
- 実測スループット、CPU、空きメモリからサイズを決める適応型ブラウザプール。同一オリジンの iframe はインライン展開します。
- 広告、トラッカー、フォント、メディアはデフォルトでブロックします。

**サービスとしての運用**
- ジョブの永続化と再起動後の再開、リトライと署名付きの webhook、API キーごとのレート・同時実行数・1 日あたりのクォータ。
- Prometheus メトリクス（`/metrics`）、JSON ログ、グレースフルシャットダウン、ヘルスチェック。
- リクエストごとに指定できる名前付きエグレスプロキシ、内部サービス向けのホスト許可リスト、リクエスト全体の期限、わかりやすいエラーコード（`DNS_FAILURE`、`NETWORK_ERROR`、`BUSY`、`ACCESS_BARRIER`、…）。

## 比較

| | pagesift | Crawl4AI | Firecrawl | crw | Spider |
|---|---|---|---|---|---|
| 言語 | Rust | Python | TypeScript | Rust | Rust |
| セルフホスト可能な単一バイナリ | ✅ | ❌ | ❌ | ✅ | ✅ |
| Firecrawl 互換 API | ✅ | ❌ | ✅ | ✅ | ❌ |
| 本文抽出スコア ↑ | **0.733** | 0.666 | — | 0.703 | — |
| 1 ページあたりの抽出時間 p50 ↓ | **0.47 ms** | 38.6 ms | — | 7.38 ms | — |
| HTTP クロール、200 × 100 ページ ↑ | **3,500+ p/s** | 15 p/s | — | 206 p/s | 2,000–2,700 p/s |
| そのクロールのメモリ使用量 ↓ | **~300 MB** | — | — | 1.4 GB | 5.5 GB |
| 雑多なサイトのクロール：見つけた本文ページ ↑ | **73.6%** | 52.9% | — | 36.7% | 47.0% |
| 雑多なサイトのクロール：無駄なリクエスト ↓ | **12%** | 39% | — | 40% | 44% |
| 軽量 JS エンジン（Lightpanda/moli） | ✅ 両方対応、自動フォールバック | ❌ | ❌ | Lightpanda | — |
| MCP サーバー | ✅ | ✅ | ✅ | ✅ | — |
| ネイティブ Python / Node パッケージ | ✅ / ✅ | ✅ / ❌ | HTTP SDK | — | ✅ / ✅ |
| ライセンス | AGPL-3.0 + 商用 | Apache-2.0 | AGPL-3.0 | AGPL-3.0 | MIT |

「—」は未確認を表します。Firecrawl のセルフホスト版はベンチマークしていません。測定方法と完全な表は [docs/benchmarks.md](benchmarks.md) をご覧ください。

## 設定

デフォルトのままですべて動作します。サーバーとして使う場合は [`pagesift.example.toml`](../pagesift.example.toml) をコピーし、`--config` を渡すか `PAGESIFT_CONFIG` を設定してください。API キーと制限、webhook、User-Agent、レート制限、ブラウザエンジン、キャッシュ、LLM と検索のバックエンド、エグレスプロキシ、アクセスを許可する内部ホストを設定できます。

## 責任あるクローリング

pagesift は robots.txt（`Crawl-delay` を含む）を尊重し、すべてのホストにレート制限をかけ、429 や 5xx を受け取るとバックオフし、正直な User-Agent で身元を明らかにします。サイトがチャレンジページやアクセス拒否ページを表示した場合、pagesift はそれを回避しようとせず、そのまま報告します（`ACCESS_BARRIER`）。ステルス機能、フィンガープリントの偽装、CAPTCHA の突破、プロキシのローテーションを追加するプルリクエストは受け付けません。

## ライセンス

pagesift はデュアルライセンスです：

- **[AGPL-3.0](../LICENSE)**：オープンソースでの利用向けです。セルフホスト、改変、商用利用が可能です。改変したバージョンをネットワーク経由で他者に提供する場合は、変更内容を公開してください。
- **[商用ライセンス](../COMMERCIAL.md)**：AGPL の義務を負わずに pagesift を使いたい場合（クローズドソース製品、プロプライエタリなホスティングサービスなど）向けです。オプションでサポートも提供します。

## コントリビュート

Issue とプルリクエストを歓迎します。まず [CONTRIBUTING.md](../CONTRIBUTING.md) をお読みください。コントリビューターには一度だけ [CLA](../CLA.md) に署名していただきます。セキュリティに関する報告：[SECURITY.md](../SECURITY.md)
