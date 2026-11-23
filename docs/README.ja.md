<div align="center">

<img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/docs/assets/logo.png" alt="PageSift Logo" width="180" />

# pagesift 1.7.0

**ローカルファーストな Rust 製 Web・文書取り込みエンジン — HTTP/ブラウザ収集、レイアウト認識 PDF/Office/OCR 解析、継続的同期、検証可能な DataGraph、証拠に基づく AI コンテキスト編成、ライブ結論検証を単一バイナリに統合。**  
セルフホストできる単一の Rust バイナリとして CLI、Firecrawl v1/v2・Jina Reader HTTP API、MCP サーバー、C ABI、Python / Node.js パッケージを提供します。

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](../LICENSE)
[![Commercial license](https://img.shields.io/badge/commercial-license%20available-green.svg)](COMMERCIAL.md)
[![Release](https://img.shields.io/github/v/release/YOLOW92/pagesift?include_prereleases)](https://github.com/YOLOW92/pagesift/releases)
[![crates.io](https://img.shields.io/crates/v/pagesift.svg)](https://crates.io/crates/pagesift)
[![PyPI](https://img.shields.io/pypi/v/pagesift.svg)](https://pypi.org/project/pagesift/)
[![npm](https://img.shields.io/npm/v/@pagesift/node.svg)](https://www.npmjs.com/package/@pagesift/node)
[![PageSift MCP server – quality and maintenance score on Glama](https://glama.ai/mcp/servers/YOLOW92/pagesift/badges/score.svg)](https://glama.ai/mcp/servers/YOLOW92/pagesift)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/esA3wMZVvA)

[English](../README.md) · [简体中文](README.zh-CN.md) · **日本語** · [Español](README.es.md)

</div>

<!-- readme-template: v2 -->
<!-- section: overview -->
## 概要

`pagesift` は、ライブ Web ページ、ローカル文書ファイル、アーカイブ、長期監視対象ソースを、メモリ上限付きの単一 Rust プロセス内でクリーンな Markdown、スキーマ検証済み JSON、来歴追跡付きの `DataGraph` 関係データ、トークン予算・証拠制約付きの `ContextPack`、および増分再検証可能な永続 `Resolve` 結論グラフへ変換します。HTTP 収集、ネイティブ文書パーサ、HTML 抽出、適応的同期状態管理、増分コンテキスト索引、決定論的 7 層クレーム検証、証拠ストレージは Python・JVM・Node ワーカーに依存せず同一プロセスで動作し、ヘッドレスブラウザプールや NVIDIA GPU OCR、利用者所有の LLM エンドポイントは必要時のみ組み込まれます。

置き換えまたは併用できるツール：**Firecrawl**（セルフホスト v1/v2 API）、**Jina Reader**（`/r/` と `/s/`）、**Crawl4AI**、**trafilatura**、**Readability**、**pymupdf4llm**、**Docling**、**Kreuzberg**、**xberg**。

<!-- section: sponsors -->
## スポンサー

<a href="https://proxy-seller.com/?partner=9CCCBD6151334AC0"><img src="https://raw.githubusercontent.com/YOLOW92/pagesift/main/docs/assets/sponsor-proxyseller.jpg" alt="Proxy-Seller: residential and ISP proxies, promo code PAGESIFT" width="720" /></a>

> アフィリエイトリンク：購入によりプロジェクトに報酬が発生する場合があります。

[**Proxy-Seller**](https://proxy-seller.com/?partner=9CCCBD6151334AC0) が pagesift をスポンサーしています。レジデンシャル／ISP／データセンター プロキシ IP（IPv4/IPv6）、220+ ロケーション、24時間365日サポート。プロモコード `PAGESIFT` で ISP / IPv4 / レジデンシャルが 15% オフ。*アフィリエイトリンク：購入によりプロジェクトに報酬が発生する場合があります。*

<!-- section: facts -->
## 早見表

- **階層構成：** エントリ層（`CLI`/`HTTP`/`MCP`/FFI）→ ポリシー・SSRF 保護付きネット層 → 適応取得（`HTTP`/埋め込み JSON/ブラウザ）→ ネイティブ形式パーサ＆抽出 → `DataGraph`・`Context`・`Resolve`・追記型ストア → cgroup ベースの NUMA ワーカープール（`pagesift-flow`）
- **入力：** URL、HTML、PDF（テキスト層＋オプション PP-OCRv6 / 数式認識）、アップロード画像（OCR 有効時）、Office・OpenDocument、RTF/WordPerfect/HWP、電子書籍、メール、書庫（`zip`、`tar`、`7z`）、SQLite/CSV、373 種類のソースコード言語、WARC アーカイブ（[形式一覧](parse-api.md)）
- **出力：** Markdown、HTML、生 HTML、リンク、画像・添付データ、RAG チャンク、スキーマ JSON、`DataGraph`（`graph.jsonl`、引用、クレーム状態、競合、`fact_clusters`、`?asOf=`）、`ContextPack`（`KnowledgeUnit`、`ContextDelta`、`provenanceManifest`）、`Resolve`（`ConclusionRecord`、`CounterSearchAudit`、`ResolveEvent`）、不完全取得時の構造化警告
- **インターフェース：** CLI · Firecrawl v1/v2 HTTP API · Jina Reader（`/r/`、`/s/`）· MCP（stdio / HTTP）· Python · Node.js / TypeScript · Rust crate · C ABI（Go、Java、Kotlin、C#、Ruby、PHP、Lua、Perl、Dart、Swift、Zig）· LangChain / LlamaIndex
- **プラットフォーム：** 単一バイナリ（`serve --role gateway`、`worker`、`standby`）、Linux x86_64/aarch64（glibc ≥ 2.28 / musl）、macOS arm64、Windows x86_64、Docker `ghcr.io/yolow92/pagesift:1.7.0`
- **インデックスと版：** 機械可読インデックス [llms.txt](../llms.txt) · Apache-2.0 または[商用サポート](COMMERCIAL.md) · **1.7.0**（[リリースノート](releases/1.7.0.md)）

<!-- section: install -->
## インストールと使い方

| 入口 | インストール | 最初の呼び出し |
|---|---|---|
| CLI | `cargo install pagesift --locked` | `pagesift scrape https://example.com` |
| Python | `pip install pagesift` | `pagesift.scrape("https://example.com").markdown` |
| Node.js | `npm install @pagesift/node` | `(await new Scraper().scrape("https://example.com")).markdown` |
| サーバー | `docker run -p 3002:3002 ghcr.io/yolow92/pagesift:1.7.0` | `curl localhost:3002/r/https://example.com` |
| MCP | — | `claude mcp add pagesift -- pagesift mcp` |

<details>
<summary>クイックスタート例：CLI、Python、Node.js、HTTP API、MCP</summary>

**CLI**

```bash
pagesift scrape https://example.com                          # クリーンな Markdown を標準出力へ
pagesift scrape --graph https://example.com                  # Markdown + DataGraph 来歴・クレーム出力
pagesift scrape report.pdf                                   # ローカル PDF、Office、電子書籍、メール、書庫、コード、WARC
pagesift --ocr scrape scan.pdf                               # スキャン PDF やアップロード画像に OCR を実行
pagesift crawl https://docs.rs --limit 500 --out ./data/     # サイト全体を取得保存（--graph で graph.jsonl 生成）
pagesift graph inspect ./data/graph.jsonl                    # 引用、系統、クレーム、競合を確認
pagesift sync init --id docs --seed https://example.com      # 永続 Loop ソースを初期化・同期
pagesift context pack --file report.pdf "rate limits"        # 来歴マニフェスト付き ContextPack を予算内で編成
pagesift resolve check --store ./data/store --source-id docs "Plan Pro costs $99/yr." # 結論を検証・永続化
pagesift extract --schema schema.json https://example.com    # セレクタ自動修復付きスキーマ JSON 抽出
pagesift doctor                                              # ローカルのブラウザ・GPU・モデル状態を確認
```

**Python・Node.js**

```python
import pagesift

page = pagesift.scrape("https://example.com", formats=["markdown", "links"])
print(page.markdown, page.metadata["title"], page.links)

with pagesift.Scraper(store_path="./data/store") as s:
    pack = s.context(action="pack", source_ids=["docs"], query="rate limits", budget_tokens=1024)
    verdict = s.resolve(action="check", source_ids=["docs"], answer="Plan Pro costs $99/yr.")
    print(pack["pack"]["compiledContext"], verdict["conclusion"]["state"])
```

```typescript
import { Scraper } from '@pagesift/node'

const scraper = new Scraper({ storePath: './data/store' })
const page = await scraper.scrape('https://example.com', { formats: ['markdown', 'links'] })
const pack = await scraper.context({ action: 'pack', sourceIds: ['docs'], query: 'rate limits', budgetTokens: 1024 })
const verdict = await scraper.resolve({ action: 'check', sourceIds: ['docs'], answer: 'Plan Pro costs $99/yr.' })
console.log(page.markdown, pack.pack, verdict.conclusion?.state)
```

**HTTP サーバー（`Firecrawl v1/v2`、`Jina Reader`、`/v2/sources`、`/v2/graph`、`/v2/context`、`/v2/resolve`）・MCP**

```bash
docker run -d -p 3002:3002 -v pagesift-data:/data -e PAGESIFT_API_KEYS=sk-test ghcr.io/yolow92/pagesift:1.7.0

curl -s localhost:3002/v2/scrape -H 'Authorization: Bearer sk-test' -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","formats":["markdown"]}'
curl -s localhost:3002/r/https://example.com -H 'Authorization: Bearer sk-test'
curl -s localhost:3002/v2/parse -H 'Authorization: Bearer sk-test' -F file=@report.pdf -F 'options={"formats":["markdown"]}'
curl -s localhost:3002/v2/context/pack -H 'Authorization: Bearer sk-test' -H 'Content-Type: application/json' \
  -d '{"sourceIds":["docs"],"query":"rate limits","budgetTokens":1024}'
curl -s localhost:3002/v2/resolve/check -H 'Authorization: Bearer sk-test' -H 'Content-Type: application/json' \
  -d '{"sourceIds":["docs"],"answer":"Plan Pro costs $99/yr."}'

pagesift mcp                                                 # 単一エージェント向け stdio モード
pagesift mcp http --port 3003                                # ローカル共有 HTTP MCP デーモン
```

</details>

MCP ツール：`pagesift_scrape`、`pagesift_batch_scrape`、`pagesift_crawl`、`pagesift_poll_job`、`pagesift_cancel_job`、`pagesift_map`、`pagesift_interact`、`pagesift_interact_stop`、`pagesift_sync`、`pagesift_graph_query`、`pagesift_context`、`pagesift_resolve`、およびモデル設定時の `pagesift_agent` と `pagesift_agent_status`。詳細は[共有 MCP サービス](mcp-service.md)、[Context リファレンス](context.md)、[Resolve リファレンス](resolve.md)を参照してください。

<!-- section: capabilities -->
## できること

アーキテクチャの責務ごとに現在実装済みの機能とリファレンスを整理しています：

| レイヤー | 実装済み機能 | ドキュメント |
|---|---|---|
| **インターフェース互換** | セルフホスト CLI、Firecrawl v1/v2 HTTP API（`/v2/scrape`、`/v2/crawl`、`/v2/parse`、`/v2/monitor`、`/v2/sources`、`/v2/graph`、`/v2/context`、`/v2/resolve`、`/v2/agent`、`/interact`）、Jina Reader（`/r/`、`/s/`）、MCP（stdio/HTTP）、Python、Node.js/TS、Rust ライブラリ、C ABI バインディング | [parse-api.md](parse-api.md) · [context.md](context.md) · [resolve.md](resolve.md) · [mcp-service.md](mcp-service.md) |
| **収集と安全性** | 条件付き HTTP（`ETag`/`Last-Modified`）、SSRF / DNS リバインディング保護、`robots.txt` とホスト単位レート制限、アイデンティティリース、埋め込み JSON 合成（`__NEXT_DATA__`/`__NUXT_DATA__`/JSON-LD）、任意 CDP ブラウザプール（`Lightpanda`/`moli`/Chrome）と `/interact` セッション、オプトイン `[itna]` アクセス障壁回復 | [interact.md](interact.md) · [http-sessions.md](http-sessions.md) · [bot.md](bot.md) |
| **ネイティブ解析と OCR** | コンテナ／フォーマット層からの純 Rust パーサ（`yolowhtml`、`yolowpdf`、`yolowdoc`、`yolowsyntax`、`yolowzstd`、`yolow64`）により Web、Office/ODF、電子書籍、メール、多段圧縮書庫、SQLite/CSV、373 言語コード（32 以上の言語で関数・クラス単位分割）、WARC、PP-OCRv6 / PP-FormulaNet（CPU/GPU）に対応 | [parse-api.md](parse-api.md) |
| **抽出とエージェント** | 見出し・表（`<caption>`・`hidden="until-found"`）・数式・コードを保持する本文 Markdown、RAG `queryFilter` とチャンク分割、DOM 変化に追従するセレクタ記憶抽出、アーカイブ済み証拠と各フィールドを突き合わせる `/v2/agent` | [structured-extract.md](structured-extract.md) · [agent-api.md](agent-api.md) |
| **継続同期 Loop と監視** | 永続 `pagesift sync` / `/v2/sources` ソース（非エイリアス周期検出、階層 Gamma-Poisson 縮小とラグランジュ注水予算配分、Hub 協調前倒し、モダリティ別コスト・タブシャドウプライシング、`DirectBrowser` 回復プローブ、ベイズ予測サプライズ忘却、テナント分離転移と信頼性検閲）、および `/v2/monitor` 実質ブロック差分と Webhook | [monitor.md](monitor.md) · [incremental-collection.md](incremental-collection.md) |
| **DataGraph と来歴** | ページ横断・ソース横断の証拠グラフ（`graph.jsonl`、`graph_summary.json`、`/v2/graph/claims`、`/history`、`/conflicts`、`?asOf=`）：永続化された引用エッジ、リダイレクト／正規URL／添付来歴、文脈通貨判別、`OriginRelation` 独立ソース根と `fact_clusters`、クレーム状態管理（`Active`、`Superseded`、`Withdrawn`、`Unverified`、`EvidenceDegraded`） | [architecture.md](architecture.md) |
| **Context とプロンプト編成** | 版横断で安定した `KnowledgeUnit` ID・曖昧性追跡（`boundedUot`/`greedyDice`/`exactAnchorOnly`）、依存スタンプ駆動のアトミック `ContextDelta` と冪等チェックポイント、語彙・記号・二時間軸（`asOf`+`effectiveAt`）・系統 `Search`、予算と `MustKeep` を守る `ContextPack`（`submodularClosure`/`boundedDpp`/`edgeSalience`）＋`provenanceManifest`、VOI・シャドウ価格に基づく `Action` とドリフト防護 `Learn` | [context.md](context.md) |
| **Resolve とライブ結論検証** | GC 免除 `ns::RESOLVE` に永続化される `ConclusionRecord` グラフ、文脈保持クレーム分解、決定論的 7 層 `Check`（エンティティ分離、二時間軸 `asOf`/`effectiveAt`、通貨・課金周期・単位正規化、文単位極性・限定句保持、因果保守性、`originRoot` ミラー集約、比較反例・同根正誤反証）、オープンワールド `absence_is_not_proof` 監査付き `Counter` 探索、権限・VOI 制約付き `Gap` 補完、引用照合付き任意 `HybridAssisted` 判定、Salsa 型早期打ち切り `Live` 増分調停、および `HardEvidence` 専用 PAV/CUSUM `Learn` | [resolve.md](resolve.md) |

<!-- section: performance -->
## パフォーマンス概要

トップページでは設計とデータフローを中心に示すため、計測条件・マシン構成・競合ツールとの比較表は [benchmarks.md](benchmarks.md) にまとめています：

- **レイテンシ（記録ベンチマーク・単一ワーカー・低いほど良い）：** PDF → Markdown `2.5–2.58 ms/ページ`（1,269 ページ）、HTML → Markdown `0.23–0.24 ms/ページ`（600 文書）、本文抽出 `1.0 ms/ページ`（597 ページ）、Context `BoundedUot` 照合 `63.9 µs`・`ContextPack` 編成 `67–225 µs`（`192`/`384` トークン予算）、Resolve 決定論的 `Check` `p50 96 µs` / `p99 161 µs`（`400` 件）・`Live` 増分調停 `7,504 µs` 対 全量再検証 `33,550 µs`（永続結論 `60` 件で `4.47x` 高速化）。
- **スループット・品質・メモリ（記録ベンチマーク）：** Office インプロセス処理 `81.9–210.0 ファイル/s`（高いほど良い）、HTML → Markdown 平均 F1 `1.000`、WCXB F1 `0.866`、RAG クエリ再現率 `0.929`（高いほど良い）、Context `Search + Pack` `1–16` スレッドで `2,708.8–24,434.4 QPS`（RSS `25.9–28.8 MiB`）、Resolve `Check` `9,448.4 QPS`（RSS `31.25 MiB`、敵対的罠 `64` 件で誤支持率 `0.0000`・支持適合率/再現率 `1.0000`、QPS・精度は高いほど、誤支持率・RSS は低いほど良い）、ブラウザ未使用時の各計測ピークメモリ `17–395 MB/ワーカー`。
- **詳細スコアと評価記録：** [benchmarks.md](benchmarks.md) · [完全ベンチマークレポート](../benchmarks/history/1.4/results/final/new/report.md) · [1.7.0 Resolve 評価記録](releases/1.7.0.md) · [1.6.0 Context・Loop 評価記録](releases/1.6.0.md)

<!-- section: architecture -->
## アーキテクチャと主要フロー

コア全体がコンパイル済み単一 Rust バイナリとして動作し、パース・ストレージ・スケジューリングの背後に外部ランタイムを必要としません。CPU ワーカー、メモリ、ブラウザタブは cgroup 上限を考慮した共通アドミッション制御を通り、内部パラメータはオンライン統計検定（`δ = 0.05`）を満たした場合のみ適応します。

### リクエスト・取り込みパイプライン

```mermaid
flowchart LR
    In["インターフェース<br/>CLI · HTTP · MCP · SDK"]
    Net["取得・レンダリング<br/>ポリシー · HTTP · CDP"]
    Parse["ネイティブ解析<br/>HTML · PDF · Office · OCR"]
    Core["グラフ・コンテキスト<br/>DataGraph · ContextPack"]
    Res["Resolve 検証<br/>Check · Counter · Live"]
    Store[("追記型ストア<br/>Sync · Context · Resolve")]

    In --> Net --> Parse --> Core --> Res
    Core <--> Store
    Res <--> Store
    Store -.->|Loop / Gap| Net
```

### 継続同期 Loop の閉ループ（`Observe → Learn → Decide → Execute → Verify`）

`pagesift sync` や `/v2/sources` で登録された持続的データソースは各ティックで閉ループを実行し、不確実な観測時にも検証済みの既存クレームを保護しつつ、有効化時は `ContextStore` と `ResolveStore` を増分更新します：

```mermaid
flowchart LR
    Obs["観測・計画<br/>周期検出 · 予算配分"]
    Dec["手段決定<br/>304 · HTTP · CDP"]
    Exe["実行・検閲<br/>取得 · 異常除外"]
    Ver["検証・保存<br/>クレーム · Context · Resolve"]

    Obs --> Dec --> Exe --> Ver --> Obs
```

### 証拠検証 `/v2/agent` とマルチサーバー構成

```mermaid
flowchart LR
    subgraph Agent["/v2/agent ループ"]
        Q["質問 + スキーマ"] --> M["利用者モデル"]
        M --> T["収集 / 検索<br/>ポリシー保護"]
        T --> Ev["証拠保存"]
        Ev --> Check{"十分か?"}
        Check -->|続行| M
        Check -->|完了| Verify["検証済み出力"]
    end
```

```mermaid
flowchart LR
    subgraph Fleet["マルチノード構成"]
        C["クライアント"] --> G["Gateway<br/>API · ジョブ"]
        G -->|ホスト割当| W1["Worker"]
        G --> W2["Worker"]
        G -.->|リース| S["Standby"]
    end
```

### ワークスペース Crate マップ

| レイヤー | クレート一覧 |
|---|---|
| **エントリ層と FFI** | `pagesift`（CLI・ライブラリ）、`pagesift-api`（HTTP サーバー・スキーマ）、`pagesift-mcp`（MCP stdio/HTTP）、`pagesift-py`、`pagesift-node`、`pagesift-ffi`（C ABI） |
| **オーケストレーション・戦略・グラフ** | `pagesift-engine`、`pagesift-plan`、`pagesift-hooks`、`pagesift-llm`、`pagesift-search`、`pagesift-graph` |
| **ネットワーク・ポリシー・ブラウザ** | `pagesift-net`、`pagesift-policy`、`pagesift-rustls`、`pagesift-cdp`、`pagesift-media`、`pagesift-itna` |
| **Web 構造抽出とフロンティア** | `pagesift-html`（`yolowhtml-dom`）、`pagesift-content`（`yolowhtml`）、`pagesift-extract`、`pagesift-diff`、`pagesift-frontier` |
| **ネイティブ文書・OCR パーサ** | `pagesift-pdf`（`yolowpdf`）、`pagesift-office`（`yolowdoc`）、`pagesift-syntax`（`yolowsyntax`）、`pagesift-ocr`、`pagesift-infer`、`pagesift-gpu` |
| **ランタイム・オンライン統計・永続化** | `pagesift-flow`（NUMA ワーカー・cgroup 制御）、`pagesift-stat`、`pagesift-base64`（`yolow64`）、`pagesift-compress`（`yolowzstd`）、`pagesift-sys`、`pagesift-store`、`pagesift-sink`、`pagesift-cluster` |

cgroup による容量計算、統計的調整基準（`δ = 0.05` の USL・生存時間・トンプソン抽出）、増分デリバリーのチェックポイント、バイト範囲メディアアーカイブ検証の詳細は[アーキテクチャ仕様](architecture.md)を参照してください。

<!-- section: choose -->
## 向いている場面、向いていない場面

**向いている場面**

- Web ページ、PDF、Office 文書、アーカイブ、ソースコードを外部ワーカープロセスなしで LLM・RAG・エージェント基盤へ供給したい
- 表見出し、否定・限定条件、単位、有効期間、ソース間競合と引用マニフェストを保持した `ContextPack` をトークン予算内で編成したい
- LLM の回答や事実主張を永続 `ConclusionRecord` グラフとして 7 層決定論検証・能動的反例探索・ソース変更時の `Live` 増分再検証まで一貫管理したい
- Firecrawl / Jina Reader 互換 API や共有 MCP サーバーをページ課金なしでセルフホストしたい
- 変更監視や継続同期において、コスト認識型のブラウザ切替、来歴管理、クレーム単位の競合検出が必要

**別のツールを選ぶべき場合**

- **マネージドクラウドサービスが欲しい場合：** `pagesift` はセルフホスト専用です。
- **対話型 CAPTCHA の自動突破が必要な場合：** アクセス障壁は分類され、人間による解決待ちとして報告されます（`[itna]` は任意設定であり CAPTCHA 回避を行いません）。
- **OCR なしで画像ファイルを扱いたい場合：** ローカル／アップロード画像は `--ocr`（`[ocr] enabled = true`）が必要で、画像 URL や OCR オフ時の画像アップロードは `UNSUPPORTED_OPTION` を返します。

<!-- section: license -->
## ライセンス

[Apache-2.0](../LICENSE)。エンタープライズサポート、カスタム SLA、専用導入支援については[商用ライセンス・サポート](COMMERCIAL.md)または `licensing@pagesift.dev` をご確認ください。

<!-- section: changelog -->
## 1.7.0 の新機能

PageSift 1.7.0 は `DataGraph`・`Loop`・`Context` の上に証拠駆動の結論検証・ライブ保守エンジン **Resolve** を導入し、Rust（`Engine::resolve`）、HTTP（`/v2/resolve*`・クラスタゲートウェイ経路・キー別レート制限）、CLI（`pagesift resolve`）、MCP（`pagesift_resolve`）、C ABI（`pagesift_resolve`）、Python（`Scraper.resolve`/`aresolve`）、Node.js（`Scraper.resolve`）に横断対応しました：GC 免除名前空間 `ns::RESOLVE` に `AtomicTx` で履歴付き `ConclusionRecord`・単調 `ResolveEvent`・`unitId`＋`ScopeDependency` 二重転置索引を原子永続化し、文脈保持クレーム分解と決定論的 7 層 `Check`（エンティティ分離、二時間軸 `asOf`/`effectiveAt`、通貨・課金周期・単位正規化、文単位極性・限定句保持、因果保守性、`originRoot` ミラー集約、比較反例・同根正誤反証）、オープンワールド `absence_is_not_proof` 監査付き属性制限 `Counter` 反例探索、明示許可 `allowNetworkAcquisition`/`allowedHosts` と Howard VOI で制御する `Gap` 補完、原文引用照合付き任意 `HybridAssisted` 判定、Salsa 型早期打ち切り `Live` 増分調停（`RelocatedOnly`/`Reconfirmed` 省略、`ObservationLost` 非誤反証、範囲拡張伝播、ABA フォールバック）、および `HardEvidence` のみを受理し CUSUM ドリフト凍結とバージョンリセットを備えた PAV 等張 `Learn` 較正を提供します。

[リリースノート](releases/1.7.0.md) · [1.6.3](releases/1.6.3.md) · [1.6.2](releases/1.6.2.md) · [1.6.1](releases/1.6.1.md) · [1.6.0](releases/1.6.0.md) · [1.5.6](releases/1.5.6.md) · [1.5.5](releases/1.5.5.md) · [すべてのリリース](releases/) · [CHANGELOG](CHANGELOG.md)

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
  note   = {Version 1.7.0}
}
```

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=YOLOW92/pagesift&type=Date)](https://star-history.com/#YOLOW92/pagesift&Date)
