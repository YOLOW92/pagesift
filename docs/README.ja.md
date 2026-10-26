<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/assets/logo.png" alt="PageSift Logo" width="300" />

# pagesift 0.73.0

**AI エージェントと LLM データ基盤のための、超高速・メモリ有界スクレイピング＆本文抽出エンジン。**  
あらゆる Web サイト、論文、アーカイブを高品質な Markdown と構造化データに変換します。単一の純 Rust バイナリ、ネイティブ SDK、Firecrawl v1/v2 および Jina Reader デュアルプロトコル互換、そして競合のわずか一部のメモリ消費で稼働します。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![2億件の合成負荷](https://img.shields.io/badge/200M%20Synthetic-947%2C611%20p%2Fs-blue.svg)](releases/0.70.0.md)
[![768 MiB cgroup](https://img.shields.io/badge/768%20MiB%20cgroup-Zero%20OOM-success.svg)](releases/0.70.0.md)

[English](../README.md) · [简体中文](README.zh-CN.md) · **日本語** · [Español](README.es.md)

</div>

---

## 0.73.0 の新機能

0.73.0 は、3 系統のコードレビューと約 150 件の異常入力テストで見つかった問題を修正しました。

- **本文抽出が再び全コアを使用**：0.70.0 以降 `scrape`・`crawl`・`serve` の抽出は 1 コアのみでした。API サーバーのスループットは 5〜7 倍（同時 16：約 310 → 最大 2,196 リクエスト/秒、p50 40 ms → 4.6 ms）。
- **クラッシュと DoS 経路を修正**：CJK ページでの feed 判定 panic、UTF-8 ページの文字化け誤補正、属性解析の二乗計算量（4 万属性で 3 秒 → 0.02 秒）、slowloris、RSS 項目の `javascript:` リンク。
- **誤検出するヒューリスティックを修正**：短い Substack 記事が年齢確認ページと判定されていました。著者ウィジェット、cloak のアプリルート、`<template>` の内容、OpenAI キーと誤認された CSS 変数も正しく扱います。
- **API の上限とタイムアウト**を全パラメータに適用し、エラーは統一 JSON 形式で返します。
- **crawl の範囲はページの証拠で決定**（既定 `--scope auto`）：`/docs/python` は `/docs/python/*` のみ。ブログ記事のような末端ページから始めると 所属するセクションまで広げ、その旨を警告で知らせます。

品質は維持または向上（WCXB F1 0.8522 → 0.8533、Firecrawl 0.680 → 0.682、AEB 0.958）、実ページの抽出は 3〜5% 高速化。詳細：[0.73.0 リリースノート](releases/0.73.0.md)。

## 0.72.8 の新機能

0.72.8 は第 4 回監査と第 5〜8 回のフィールドテストを完了し、デフォルト設定の自動検出を追加しました。

- **charset 宣言の検証**：`Content-Type` や `<meta>` が誤っていても本文を再デコードし、修正内容を `metadata.pagesift.warnings` に記録します。
- **`--allow-cidr` を全コマンドに**：`crawl`、`map`、`download`、`session-fetch`、`collect`、`serve` が `scrape` と同じ許可リストを使えます。不正な CIDR はエラーになります。
- **デフォルト設定の検出**：`--config` > `PAGESIFT_CONFIG` > プラットフォーム設定ファイル。`PAGESIFT_CONFIG=` で無効化でき、`doctor` が読み込んだファイルを表示します。
- **偽の成功を廃止**：`map` はシード失敗時にエラーを返し、`crawl` は 0 ページかつエラーありの場合に終了コード 1 を返します。
- **本文の誤削除を修正**：レシピの材料リストなど本文内の `<aside>` を保持し、`quickview`/`teaser` のプレビューを本文と誤認しません。
- **GitHub コードページ**：行番号列を捨て、コード行を 1 つのフェンスにまとめます。
- **著者とメタデータ**：arXiv の著者行を保持し、`citation_author` を `metadata.author` に反映。`jsonLd` をそのまま透過します。
- **正直なシグナル**：ドイツ語/フランス語/スペイン語のブロックページを `ACCESS_BARRIER`、年齢確認を `interstitial`、薄い抽出を `thin_content` として報告します。
- **Feed とメディア**：RSS/Atom を項目リストに変換。画像の `title` を保持し、動画ページは `VideoObject` でタイトルと説明を補います。

詳細：[0.72.8 リリースノート](releases/0.72.8.md)。

---

## 0.70.0 の新機能

0.70 では HTTP データ経路を再構築しました。NUMA ローカル実行器、軽量 HTTP/1.1 reactor、オリジン別の適応的接続制御、有界のシャード処理を導入しています。cgroup v2 の `memory.max` を容量制御に反映し、768 MiB 制限下で 380 接続、817,346 ページ/秒、OOM なしを測定しました。

- **192 vCPU、2 億件の合成負荷試験**：200,000,000/200,000,000 件完了、失敗 0 件、947,611 ページ/秒、所要 211.1 秒、RSS 最大 888.1 MiB。これは合成負荷の値であり、公開 Web の取得速度ではありません。
- **公開ベンチマークでは負けた項目も掲載**：7 領域中 4 領域で首位。本文抽出、動的描画、DOM 自己修復の差も下表に示します。
- **運用上の不具合を修正**：応答ヘッダー待機にも `timeout_secs` を適用し、429 ペナルティの回復、単一 URL の失敗によるホスト停止、低い `ulimit -v` での panic を修正しました。

設計、測定条件、移行情報は [0.70.0 リリースノート](releases/0.70.0.md) を参照してください。

---

## 🏆 7 領域の公開ベンチマーク（0.70）

すべて**同じマシンで全件を実行**し、各ベンチマーク固有の採点器と 10 GiB のメモリ上限を使用しました。検証環境は Intel i7-10700（8C/16T）、RAM 15 GB、Ubuntu 26.04 です。再現スクリプトは [`bench/scripts/`](../bench/scripts/)、個別レポートは [`docs/benchmarks/`](benchmarks/) にあります。

| 領域 | PageSift 0.70 | 最良の比較対象 | 順位 | 最大 RAM |
| :--- | :--- | :--- | :---: | ---: |
| D1 本文抽出（WCXB、2,006 ページ） | 0.6720 F1 | PageSift 0.50：0.6824 | 4 位 | 751 MiB |
| D3 PDF 抽出（olmOCR、1,403 件） | **0.4506** | pymupdf4llm：0.2446 | **1 位** | 271 MiB |
| D5 RAG 分割と BM25（472 問） | **0.9470 R@5** | semchunk：0.9364 | **1 位** | — |
| D6 文字コード判定（3,128 ファイル） | **0.7602** | chardet：0.7113 | **1 位** | 39 MiB |
| D7 構造化データ（2,006 ページ） | **0.7392** | bs4+lxml：0.5488 | **1 位** | **21 MiB** |
| D9 動的描画（7 SPA シナリオ） | 3/7 | Playwright：5/7 | 2 位 | — |
| D4 DOM 自己修復（2,000 変異） | 0.8215 | Scrapling：0.8875 | 2 位 | — |

D1 は精度を優先して再現率が低下しました。D9 はタイマーで遅れて挿入される要素に未対応です。D4 はおとり要素では優位ですが、総合では 2 位です。以下の旧バージョンの測定値は 0.70 の結果ではありません。

---

## ⚔️ 旗艦対決：128GB 本番サーバーにおける 10 万グローバルドメイン頂上決戦

**ハードウェア検証環境**：**AMD EPYC 9B45 本番クラウドサーバー（32 物理コア、128 GB メモリ、1Gbps+ パブリック回線）**  
**対象データセット**：公式 Tranco Top 100,000 グローバルアクティブサイト（768 並行スレッド、接続 3.5 秒 / 合計 10 秒タイムアウト、Blake3 ハッシュ重複排除、純メモリ Markdown 抽出）：

```text
+-----------------------------------------------------------------------------------------+
| PageSift 0.50 vs Spider 2.53.9（32コア 128GB サーバー・10万公網ドメイン実戦比較看板）    |
+-----------------------------------------------------------------------------------------+
| 評価項目                     | PageSift 0.50 (Rust)    | Spider 2.53.9 (Rust)   | 勝敗判定
+------------------------------+-------------------------+------------------------+-------+
| 処理対象サイト総数           | 105,660 サイト          | 100,000 サイト         | 引き分け
| 正常 200 OK レスポンス       | 62,937 ページ           | 72,209 ページ (エラー含)| Spider
| 有効 Markdown 本文 (>100B)   | 60,659 件 (高品質本文)  | 59,803 件              | PageSift (+856件)
| 本文抽出成功率               | 96.38% (極めて高精度)   | 82.82% (17.2% 空洞ドキュ)| PageSift (圧倒的品質)
| ピーク物理メモリ (RSS)       | 188.4 MB (完全平坦直線) | 8,978.9 MB (~9.0 GB!)  | PageSift (47.6倍省メモリ)
| 百万件規模への長期稼働耐性   | 100% 安全 (リーク皆無)  | 🔴 OOM Killer で強制終了| PageSift (産業級)
| メモリリーク・ソケット停止   | 完全ゼロリーク・停止無  | 14 GB でリーク・ハング | PageSift (完全防衛)
+-----------------------------------------------------------------------------------------+
```

> [!IMPORTANT]
> **産業用 AI パイプラインで PageSift が選ばれる理由:**
> - **物理メモリを 47.6 倍削減**：105,660 サイトの激しいトラフィックを経ても、PageSift のピークメモリは **188.4 MB** に完全固定。一方の Spider は開始わずか 2.5 分で **約 9.0 GB (8,978 MB)** まで肥大化し、最終的に 14 GB でハングアップしました。
> - **圧倒的な抽出品質**：Spider は 12,400 件以上の空洞ページ（本文欠落率 17.2%）を出力。対して PageSift は **96.38%** のページから構造化 Markdown の抽出に成功し、**純有効本文数（60,659 件）でも Spider（59,803 件）を完全に逆転**しました。
> - 再現可能な生データとレポートは [`bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md`](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) を参照。

---

## 🏆 総合 10 大領域ベンチマークマトリクス

ベンチマークは **AMD EPYC 本番サーバー（32 物理コア、128 GB メモリ）** およびデスクトップ **Intel Core i7-10700（8C/16T、15.5 GB メモリ、12 GiB メモリ監視下）** にて同条件で計測。検証可能な生データは [`bench/results/final/`](https://github.com/YOLOW92/pagesift/tree/main/bench/results/final/) に保管されています。

| 評価領域 | PageSift 0.50.0 実測値 | 主要競合ツールの実測値 | 性能差 / 高速化倍率 | ピークメモリ（PageSift vs 競合） | 検証レポート |
| :--- | :--- | :--- | :--- | :---: | :--- |
| **01. 本文抽出とマルチスレッド並行** | **3,525 ページ/秒** (0.57s) | `rs-trafilatura`: 241 p/s<br>`trafilatura`: 191 p/s | **14.6× 〜 18.4× 高速** | **137.8 MB** vs 298.3 MB | [D1 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D1_CONTENT_EXTRACTION_050.md) |
| **02. 10万公網巡回と巨大メモリ防衛** | **105,660 サイト (188.4 MB)**<br>Chaos: **2,727 p/s** (447.5 MB) | `Spider 2.53.9`: 8,978 MB 肥大<br>→ **12G 上限抵触で OOM 強制終了** | **完全なメモリリークゼロ**<br>（メモリは他社の 1/47.6） | **188.4 MB** vs 8,978.9 MB (💥 OOM) | [100k レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_100K_PAGESIFT_VS_SPIDER.md) |
| **03. PDF 論文解析と LaTeX 数式復元** | 合格率 **71.0%**、LaTeX 数式 **44.3%**<br>**3.4 ms/ページ** | `pymupdf4llm`: 合格率 64.4%、LaTeX 0.0%<br>320.0 ms/ページ | **94× 高速**<br>独自の TeX 数式復元エンジン | **42.0 MB** vs 280.0 MB | [D3 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D3_HEAVY_PDF_EXTRACTION_050.md) |
| **04. DOM 自己修復と深層変異** | 復旧率 **93.8%**、誤検出 **0.0%**<br>平均遅延 **24.6 µs** | `Scrapling`: 復旧率 46.0%、**誤検出率 51.2%**<br>平均遅延 635.3 µs | **25.8× 高速**、囮要素の誤認ゼロ<br>200階層 DOM 安全性 (928 µs) | **39.4 MB** vs 98.0 MB | [D4 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D4_DOM_SELF_HEALING_050.md) |
| **05. RAG 分割と BM25 検索** | R@1000 = **0.895** (分割 19.7 ms)<br>BM25 フィルタ：**1.35 ms/問** | `semchunk`: 113.4 ms (5.8× 低速)<br>`Crawl4AI`: フィルタ 804.8 ms (**595× 低速**) | **5.8× テキスト分割高速化**<br>**595× BM25 フィルタ高速化** | **45.2 MB** vs 340.0 MB | [D5 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D5_RAG_CHUNKING_050.md) |
| **06. 多言語レガシー文字コード判別** | 正解率 **74.4%**、Unicode **99.7%**<br>**0.45 ms/ファイル** | `chardet 7.7`: 正解率 68.9% (3.05 ms)<br>`charset-normalizer`: 正解率 46.5% (1.05 ms) | **より高い正解率 (+5.5%)**<br>**6.8× 高速** | **18.5 MB** vs 42.0 MB | [D6 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D6_CHARSET_DETECTION_050.md) |
| **07. 深層構造化データ抽出** | 標準マイクロデータ **80.2 µs/ページ**<br>40 層超深ネスト **363 µs** | `extruct 0.18`: 3,508 µs/ページ (43.8× 低速)<br>40 層超深ネスト 15,921 µs | **43.8× 〜 43.9× 高速**<br>スタックオーバーフロー皆無 | **24.0 MB** vs 85.0 MB | [D7 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D7_STRUCTURED_DATA_050.md) |
| **08. 暗黙 JS エンドポイント発見** | **13/13 (100.0%)** 満点回収<br>**非ナビゲーション訪問 0 回**、**0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>非ナビゲーション訪問 4 回 (30.8% 誤認)、48.5 ms | **100% 回収**、囮訪問ゼロ<br>ブラウザサンドボックス負荷ゼロ | **14.2 MB** vs 128.0 MB | [D8 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D8_JS_ENDPOINTS_050.md) |
| **09. 動的ヘッドレス描画 (CDP)** | **158.4 ms/ページ** (常駐ウォームプール)<br>**100%** Canvas/SVG 描画パス | `Lightpanda`: 82 ms (SPA スクリプト失敗)<br>`Playwright`: 842 ms (5.3× 低速) | **Playwright より 5.3× 高速**<br>100% DOM 描画完全性 | **188.0 MB** vs 1,840.0 MB | [D9 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D9_DYNAMIC_HEADLESS_050.md) |
| **10. AI API ゲートウェイ並行性** | **2,450 req/s**、P99 = **1.82 ms**<br>自動 429 バックオフ＆フォールバック | `Portkey`: 480 req/s、P99 = 12.4 ms<br>`LiteLLM`: 340 req/s、P99 = 18.9 ms | **5.1× 〜 7.2× 高スループット**<br>ゲートウェイタイムアウト皆無 | **38.0 MB** vs 420.0 MB | [D10 レポート](https://github.com/YOLOW92/pagesift/blob/main/bench/results/final/BENCHMARK_D10_AI_API_GATEWAYS_050.md) |

> [!TIP]
> **再現可能な検証資料**：全 10 領域の詳細白書は [`docs/releases/0.50.0.md`](releases/0.50.0.md) を、測定生データは [`bench/results/final/`](https://github.com/YOLOW92/pagesift/tree/main/bench/results/final/) を参照してください。

---

## 0.50.0 の新機能

- 🧠 **動的自適応並行＆タイムアウト調律エンジン (`pagesift::adaptive`)**：
  - **ボトルネック拘束モデル ($\min$ 原理)**：並行数は CPU コア数、物理空きメモリ（接続あたり 2MB）、OS ファイルディスクリプタ（`ulimit -n`）の積集合により厳格に制約：
    $$\text{Concurrency} = \min\big(C_{cpu},\; M_{mem},\; FD_{safe}\big)$$
  - **自動ハードウェアプロファイル**：Micro (1C1GB)、Small (2C2G〜4C4G)、Mid (8C16G / i7-10700)、High (32C128G / AMD EPYC)、Extreme (64C+ 128GB+ FD>=65k) を自動判別。
  - **デュアル動作モード**：`--tuning-mode adaptive`（ハードウェア飽和スループット）と `--tuning-mode conservative`（CPU < 20% に抑制するオフィス静粛モード）に対応。
  - **国際ネットワーク長 RTT 保全**：基準接続タイムアウトを 3.5 秒、全体タイムアウトを 10.0 秒に校正し、長距離遅延サイトの誤切断を撲滅。
- 🌐 **Firecrawl v1 & v2 デュアルプロトコル完全対応**：公式の Python / Node SDK をコード修正なしでそのまま利用可能。`/v1/scrape`、`/v1/crawl`、`/v1/batch/scrape`、`/v1/map`、および全 `/v2/*` ルートを網羅。
- ⚡ **Jina Reader スタイル極小プロトコル**：`GET /r/{*url}` および `POST /r` による Markdown 直出し、`Accept: text/event-stream` による SSE リアルタイムストリーミング配信に対応。
- 🛡️ **128 GB サーバー 10 万サイト走破・完全ゼロリーク**：105,660 件の実測でもメモリは **188.4 MB** に完全固定。Spider などの 9GB+ メモリ爆発とソケット停止問題を完全終結。
- 🦀 **100% Pure Safe Rust とモジュール品質**：全クレートで `#![forbid(unsafe_code)]` を死守。全ソースコードが厳格に 500 行以内で構成。

---

## クイックスタート

### 1. Docker / セルフホストサーバー

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.73.0
```

#### Firecrawl SDK の差し替え
既存の Firecrawl コードの接続先を変更するだけで即座に稼働：

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
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

---

## なぜ PageSift なのか？

| 機能 | PageSift 0.51.0 | Firecrawl (Self-Hosted) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **コア設計** | 100% 健全な純 Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **API 互換性** | **Firecrawl v1+v2 + Jina + MCP** | Firecrawl API のみ | 独自 Python API | 独自 REST API |
| **抽出スループット** | **3,525 ページ/秒** | 〜150 ページ/秒 | 〜90 ページ/秒 | 〜1,200 ページ/秒 |
| **10万ドメインピークメモリ**| **188.4 MB (超平坦直線)** | > 8 GB (キュー滞留) | > 10 GB (ブラウザ滞留) | **8,978 MB (47.6倍肥大・漏洩)** |
| **本文抽出成功率** | **96.38% (極めて高品位)** | 〜85% | 〜88% | 82.82% (17.2% 空洞ドキュメント) |
| **自律ハードウェア調律** | **$\min(CPU, RAM, FD)$ 完全自動** | 手動並行設定 | 手動セマフォ設定 | 固定セマフォ |
| **DOM 自己修復** | **93.8% (誤認率 0.0%)** | ❌ 非対応 | ❌ 非対応 | ❌ 非対応 |
| **PDF 数式解析** | **71% 合格 / 44.3% LaTeX** | 外部ツール平文のみ | 平文テキストのみ | 平文テキストのみ |
| **RAG 分割と BM25** | **19.7 ms / 1.35 ms BM25** | ❌ 非対応 | Python 処理 (804 ms) | ❌ 非対応 |
| **巡回哲学** | 規範遵守・透明 UA・自動トークンバケット | プロキシ回避主体 | ステルス回避主体 | ステルス回避主体 |

---

## ライセンスと商用利用

- **オープンソース版**：[AGPL-3.0](LICENSE) ライセンスに基づく提供。
- **商用ライセンス**：AGPL 義務の免除やクローズドソースへの組み込みをご希望の場合は、[商用ライセンスのご案内](COMMERCIAL.md) を確認いただくか、`licensing@pagesift.dev` までお問い合わせください。
