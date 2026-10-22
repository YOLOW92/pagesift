<div align="center">

# pagesift 0.40.0

**AI エージェントと LLM データ基盤のための、超高速・メモリ有界スクレイピング＆本文抽出エンジン。**  
あらゆる Web サイト、論文、アーカイブを高品質な Markdown と構造化データに変換します。単一の純 Rust バイナリ、ネイティブ SDK、Firecrawl v1/v2 および Jina Reader デュアルプロトコル互換、そして競合のわずか一部のメモリ消費で稼働します。

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](../COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![12GiB メモリ防衛認証](https://img.shields.io/badge/12GiB%20%E3%83%A1%E3%83%A2%E3%83%AA%E9%98%B2%E8%A1%9B-%E5%AE%9F%E6%B8%AC%20OOM%20%E5%85%8D%E7%96%AB-success.svg)](#-公開ベンチマーク対決全8領域)

[English](../README.md) · [简体中文](README.zh-CN.md) · **日本語** · [Español](README.es.md)

</div>

---

## 🏆 公開ベンチマーク対決（全8領域・完全比較）

すべてのベンチマークはベアメタル環境 **Intel Core i7-10700**（8 コア、16 スレッド、15.5 GB RAM）上で、稼働中の **12 GiB メモリ監視プロセス**（`MemoryMax=12288M`）のもと同条件で計測。検証可能な生 CSV データは [`bench/results/final/`](../bench/results/final/) に公開されています。

| 評価領域 | PageSift 0.40.0 実測値 | 主要競合ツールの実測値 | 圧倒的性能差 / 高速化倍率 | ピーク物理メモリ（PageSift vs 競合） |
| :--- | :--- | :--- | :--- | :---: |
| **01. 本文抽出とマルチスレッド並行** | **3,525 ページ/秒** (0.57s) | `rs-trafilatura`: 241 p/s (8.32s)<br>`trafilatura` (Py): 191 p/s (10.51s) | **14.6× 〜 18.4× 高速** | **137.8 MB** vs 298.3 MB |
| **02. 5万ページ巡回と 12G 防衛** | **2,727 ページ/秒** (17.76s)<br>ピーク **447.5 MB** | `Spider 2.53.9`: 11.7 GiB へメモリ爆発<br>→ **12G 上限抵触で OOM 強制終了** | **完全なメモリリークゼロ**<br>（メモリ消費は他社の 1/26） | **447.5 MB** vs 11,724.8 MB (💥 OOM 敗退) |
| **03. PDF 論文解析と数式復元** | 合格率 **71.0%**、LaTeX 数式 **44.3%**<br>**3.4 ms/ページ** | `pymupdf4llm`: 合格率 64.4%、LaTeX 0.0%<br>320.0 ms/ページ | **94× 高速**<br>独自の TeX 数式復元エンジン | **42.0 MB** vs 280.0 MB |
| **04. DOM 自己修復と深層変異** | 復旧率 **93.8% (469/500)**、誤検出 **0.0%**<br>平均遅延 **24.6 µs** | `Scrapling`: 復旧率 46.0%、**誤検出率 51.2%**<br>平均遅延 635.3 µs | **25.8× 高速**、囮要素の誤認ゼロ<br>200階層 DOM 安全性 (928 µs) | **39.4 MB** vs 98.0 MB |
| **05. RAG 分割と BM25 検索** | R@1000 = **0.895** (分割 19.7 ms)<br>BM25 フィルタ：**1.35 ms/問** | `semchunk`: 113.4 ms (5.8× 低速)<br>`Crawl4AI`: フィルタ 804.8 ms/問 (**595× 低速**) | **5.8× テキスト分割高速化**<br>**595× BM25 フィルタ高速化** | **45.2 MB** vs 340.0 MB |
| **06. 多言語レガシー文字コード判別** | 正解率 **74.4% (2336/3138)**、Unicode **99.7%**<br>**0.45 ms/ファイル** | `chardet 7.7`: 正解率 68.9% (3.05 ms)<br>`charset-normalizer`: 正解率 46.5% (1.05 ms) | **より高い正解率 (+5.5%)**<br>**6.8× 高速** | **18.5 MB** vs 42.0 MB |
| **07. 深層構造化データ抽出** | 標準マイクロデータ **80.2 µs/ページ**<br>40 層超深ネスト **363 µs** | `extruct 0.18`: 3,508 µs/ページ (43.8× 低速)<br>40 層超深ネスト 15,921 µs | **43.8× 〜 43.9× 高速**<br>スタックオーバーフロー皆無 | **24.0 MB** vs 85.0 MB |
| **08. 暗黙 JS エンドポイント発見** | **13/13 (100.0%)** 満点回収<br>**非ナビゲーション訪問 0 回**、**0.42 ms** | `Katana 1.8.0`: 10/13 (76.9%)<br>非ナビゲーション訪問 4 回 (30.8% 誤認)、48.5 ms | **100% 回収**、囮訪問ゼロ<br>ブラウザサンドボックス負荷ゼロ | **14.2 MB** vs 128.0 MB |

> [!TIP]
> **再現可能な検証資料**：全 8 領域の詳細白書は [`docs/releases/0.40.0.md`](releases/0.40.0.md) を、測定生データは [`bench/results/final/`](../bench/results/final/) を参照してください。

---

## 0.40.0 の新機能

- 🌐 **Firecrawl v1 & v2 デュアルプロトコル完全対応**：公式の Python / Node SDK（`V1FirecrawlApp` および `FirecrawlApp`）をコード修正なしでそのまま利用可能。`/v1/scrape`、`/v1/crawl`、`/v1/batch/scrape`、`/v1/map`、および全 `/v2/*` ルートを網羅。
- ⚡ **Jina Reader スタイル極小プロトコル**：`GET /r/{*url}` および `POST /r` による Markdown 直出し、`Accept: text/event-stream` による SSE リアルタイムストリーミング配信に対応。
- 🛡️ **12 GiB メモリ防衛と 10 万 Chaos クラスター対抗**：50,000 ページ実測で他社 Spider が 11.7 GiB に膨らみ OOM 停止する中、PageSift は **447.5 MB** に厳格抑制しながら **2,727 ページ/秒** を記録。100,000 URLs の過酷な Chaos クラスター（150 階層ネスト、503 障害、無限ループ罠）を無停止で走破。
- 🦀 **100% Pure Safe Rust とモジュール品質**：`pagesift-api` は `#![forbid(unsafe_code)]` を死守。全ソースファイルが 500 行以内で構成。

---

## クイックスタート

### 1. Docker / セルフホストサーバー

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data \
  -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:0.40.0
```

#### Firecrawl SDK の差し替え
エンドポイント URL をローカル PageSift に向けるだけで即座に稼働します：

```python
from firecrawl import FirecrawlApp

app = FirecrawlApp(api_url="http://localhost:3002", api_key="sk-test")
res = app.scrape_url("https://example.com", params={"formats": ["markdown"]})
print(res["markdown"])
```

#### Jina Reader による直感的取得
LLM エージェントや curl から 1 行で Markdown を取得：

```bash
# Markdown を直接出力
curl -s http://localhost:3002/r/https://ja.wikipedia.org/wiki/Rust_(%E3%83%97%E3%83%AD%E3%82%B0%E3%83%A9%E3%83%9F%E3%83%B3%E3%82%B0%E8%A8%80%E8%AA%9E)

# SSE リアルタイムストリーミング
curl -s -N -H "Accept: text/event-stream" http://localhost:3002/r/https://news.ycombinator.com
```

---

### 2. Python パッケージ

```bash
pip install pagesift
```

```python
import pagesift

# 単一ページの超高速抽出 (< 2ms)
page = pagesift.scrape("https://example.com", formats=["markdown", "metadata"])
print(page.markdown)
print(page.metadata.title, page.metadata.published_date)

# メモリ有界の並行スクレイピング
results = pagesift.scrape_many(["https://site1.com", "https://site2.com"], concurrency=8)
```

---

### 3. Node.js / TypeScript パッケージ

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
  console.log(`完了: ${page.url} (${page.elapsedMs}ms)`)
}
```

---

### 4. コマンドラインツール (CLI)

```bash
cargo install pagesift --locked

# 単一ページ取得
pagesift scrape "https://news.ycombinator.com" --format markdown

# 礼儀正しく高速な並行クロール
pagesift crawl "https://docs.rs" --limit 5000 --concurrency 16 --output ./data/
```

---

## 競合ツールとの総合比較

| 機能・特性 | PageSift 0.40.0 | Firecrawl (セルフホスト) | Crawl4AI | Spider |
| :--- | :---: | :---: | :---: | :---: |
| **コア設計** | 純粋 Safe Rust (`forbid(unsafe)`) | TypeScript / Python / Redis | Python / Playwright | Rust / Tokio |
| **プロトコル対応** | **Firecrawl v1+v2 + Jina + MCP** | Firecrawl API のみ | 独自 Python API | 独自 REST API |
| **本文抽出スループット** | **3,525 ページ/秒** | 約 150 ページ/秒 | 約 90 ページ/秒 | 約 1,200 ページ/秒 |
| **5万ページ時ピークメモリ** | **447.5 MB (厳密に有界)** | > 6 GB (Redis 滞留) | > 8 GB (ブラウザ滞留) | 💥 **OOM 異常終了 (11.7 GB)** |
| **DOM 自己修復** | **93.8% (誤認率 0.0%)** | ❌ 非対応 | ❌ 非対応 | ❌ 非対応 |
| **学術 PDF / 数式抽出** | **71% 合格 / 44.3% LaTeX** | テキストのみ抽出 | テキストのみ抽出 | テキストのみ抽出 |
| **RAG 分割と BM25** | **19.7 ms / 1.35 ms BM25** | ❌ 非対応 | Python 分割 (804 ms) | ❌ 非対応 |
| **クローラー規約と倫理** | robots.txt 遵守、Host 別レート制限 | プロキシ回転による回避 | ステルスによる回避 | ステルスによる回避 |

---

## ライセンスと商用利用

- **オープンソース版**：[AGPL-3.0](LICENSE) ライセンス。
- **商用ライセンス**：AGPL のソース開示義務を伴わない商用プロプライエタリ組み込みや閉域環境での利用については、[商用ライセンス案内](../COMMERCIAL.md) または `licensing@pagesift.dev` までお問い合わせください。
