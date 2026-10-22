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

## 0.30.0

0.30.0 ではコアアーキテクチャのサブモジュール分割（全ソースファイルを 600 行以内に集約、公開 API は 100% 互換）、スクリプト関数畳み込みによる制御されたナビゲーション発見（13/13 満点）、高速メタデータ抽出と公開日判定（F1 0.950、0.37 ms/ページ）、RAG 検索フィルタの 788 倍高速化（1.35–2.11 ms）、および 12 GiB メモリ上限下での 50,000 ページクロール完走（ピーク 378.8 MB、他社は OOM 敗退）を実現しました。

[Release / migration](releases/0.30.0.md) · [Documentation](README.md) · [Architecture](architecture.md) · [0.30.0 評価レポート](../../bench/fresh/results-0.30.0.md)

![pagesift 0.30.0](assets/overview.svg)

pagesift は、AI パイプライン、RAG、エージェント向けのセルフホスト型スクレイピング・クローリングエンジンです。ページを取得し、必要な場合にだけ JavaScript をレンダリングし、本文を抽出して、Markdown、HTML、リンク、メタデータを返します。インターフェースは [Firecrawl v2](https://docs.firecrawl.dev) 互換の HTTP API、MCP サーバー、CLI、ネイティブの Python パッケージと Node パッケージを用意しています。

サービスとしての運用を前提に設計されています。メモリ使用量の上限、バックプレッシャー、ジョブの永続化、webhook、キーごとの制限、メトリクスを備えています。また、ルールも守ります。robots.txt に従い、ホストごとにレート制限をかけ、正直な User-Agent を名乗り、ボット対策の回避は一切行いません。

## pagesift を選ぶ理由

| | |
|---|---|
| ⚡ **高速** | 8,854 件のランダム実サイトで本文抽出は 1 ページあたり **1.51 ms**（p50、シングルスレッド）。crw の 13 倍、Crawl4AI の 40 倍、Defuddle の 733 倍の速さです。50,000 ページの HTTP クロールでは **2,556 ページ/秒** を達成します。 |
| 🪶 **軽量** | 厳格な 12 GiB メモリ上限下で、50,000 ページのクロールをピークメモリわずか **378.8 MB** で完走。Spider や Colly は 12 GiB を突破しカーネル OOM Killer により強制終了。20,000 ページでは **234 MB**（Spider は 18 倍の 4.3 GB 消費）。 |
| 🎯 **高精度** | AEB 181 本文記事で F1 **0.956**（1位獲得）。Firecrawl 597 ページで F1 **0.706**、ノイズ混入率はわずか **0.047**（競品の半分以下）。公開日抽出 F1 **0.950**・処理時間 **0.37 ms**（htmldate extensive より高精度かつ 14 倍高速）。148 件の非 UTF-8 ページで 99.3% 正確デコード。 |
| 🧠 **スマートなレンダリング** | レンダリングが必要なページだけを、対応できる最も軽量なエンジンで処理します：Lightpanda → moli → Chrome。ブラウザを使っても結果が変わらないサイトはサイトごとに学習します。 |
| 🔌 **そのまま置き換え可能** | Firecrawl v2 API（`/v2/scrape`、`/crawl`、`/map`、`/search`、`/extract`、`/batch/scrape`）に対応しているため、既存の Firecrawl SDK を変更なしで使えます。エージェント向けの MCP サーバーも備えています。 |
| 🛡️ **デフォルトで安全** | すべてのリクエスト、リダイレクト、ブラウザのサブリクエスト、webhook に SSRF 対策を適用し、DNS リバインディングもチェックします。ブラウザのサンドボックスを無効にすることはありません。 |

詳細で再現可能な数値：**[docs/benchmarks.md](benchmarks.md)**

## クイックスタート

### Docker

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=change-me ghcr.io/yolow92/pagesift:0.30.0

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

### MCP

```bash
pagesift mcp            # stdio; tools: pagesift_scrape, pagesift_batch_scrape, pagesift_crawl, pagesift_crawl_status, pagesift_map
```

## 機能

**スクレイピングとクローリング**
- 出力形式：`markdown`、`html`（クリーン化済み）、`rawHtml`、`links`、`images`。OpenAI 互換の任意の LLM と組み合わせて `summary` と `json`（スキーマに基づく抽出）も利用できます。`changeTracking`（前回の訪問との差分。キャッシュが必要です）。
- インクルード/エクスクルードするパス、深さと件数の上限、サイトマップ、robots.txt を指定してクロールできます。**クエリ指向クロール**（`query`、`adaptive`）では、関連性の高いページから順に訪問し、新しいページが役に立たなくなった時点で停止します。
- 忠実な Markdown：表、入れ子のリスト、言語付きのコードブロック、強制改行、数式（KaTeX、MathJax、MathML、Wikipedia の数式は `$…$` の TeX に変換）。本文中の Markdown 記号は、書式として誤って解釈される場合にだけエスケープします。ランダム往復テストで 1.000（次点は 0.937）。
- **実際のサイトでも上限を無駄にしないクロール：** トラッキング用パラメータやセッション ID を除去し、バイナリや無限に続くパスを読み飛ばし、同じページの別名 URL を自動で学習し、カレンダーやページ送りの罠を後回しにし、一時的な失敗は後で再試行し、429 が返ると自動で速度を落とします。雑多なサイトを模したテストでは、同じページ上限で本文ページの 71% を見つけます（次点は 49%、ベンチマーク参照）。
- `queryFilter` はクエリに関連する部分だけを残します。ページを各セクション内で約 100 トークンの単位に分け、見出しパスと一緒に BM25 で採点し、最高点の半分以上の単位を本文の順に見出し付きで残します。ラベル付きの RAG 評価セットでは、約 600 語で回答テキストの 93% を残しました（Crawl4AI の BM25 フィルターは 13,500 語で 67%、速度は 70 分の 1）。`maxTokens`、`topK`、絶対しきい値 `threshold` も指定できます。
- `chunking` は Markdown を RAG 用のチャンクに分け、各チャンクに見出しパスと位置を付けます。`recursive`（サイズ上限付きで最も意味のある境界で分割し、コードブロックと見出しを内容と一緒に保つ。semchunk の Rust 移植で、検索品質は同じ、速度は 3 倍以上）、見出し、段落、文、トークン窓に対応します。
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
| 本文抽出スコア ↑ | **0.716** | 0.670 | — | 0.711 | — |
| Markdown の忠実度（ランダム往復テスト）↑ | **1.000** | 0.784 | — | — | — |
| RAG クエリフィルター：残った回答 / 返す語数 | **93% / 1,150** | 67% / 13,500 | — | — | — |
| 1 ページあたりの抽出時間 p50 ↓ | **0.85 ms** | 50.2 ms | — | 8.93 ms | — |
| HTTP クロール、200 × 100 ページ ↑ | **3,890 p/s** | 17 p/s | — | 48 p/s | 2,090 p/s |
| そのクロールのメモリ使用量 ↓ | **218 MB** | 946 MB | — | 447 MB | 4.1 GB |
| 雑多なサイトのクロール：見つけた本文ページ ↑ | **71.0%** | 49.1% | — | 35.4% | 34.7% |
| 雑多なサイトのクロール：無駄なリクエスト ↓ | **13%** | 44% | — | 45% | 45% |
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
