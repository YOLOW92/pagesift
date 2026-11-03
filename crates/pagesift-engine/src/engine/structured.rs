//! pagesift 扩展格式 `{"type": "extract", "schema": {...}}`：按 CSS schema 抽取，结果放在
//! `document.extract`（不复用 Firecrawl 的 `json` 格式，那个是交给 LLM 的）。
//!
//! 配了存储时选择器有记忆：键是“注册域 + schema id + 字段”，页面改版、选择器失效时用
//! `pagesift_extract::locate` 重定位，写回稳健的新选择器，在 `warnings` 里报 `relocated`。

use pagesift_api::types::{Document, Format, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_extract::locate::Target;
use pagesift_extract::{Schema, SelectorMemory};
use serde_json::Value;

use super::Scraper;
use super::Source;
use super::cpu::cpu_task;

/// [δ] 全局风险水平。
const DELTA: f64 = 0.05;

/// 格式名。
pub const EXTRACT: &str = "extract";

/// 请求里的 extract 格式：解析并校验 schema（写错在抓取前就报）。
pub fn parse_format(opts: &ScrapeOptions) -> Result<Option<(Schema, String)>, ApiError> {
    let Some(f) = opts.formats.iter().find(|f| f.kind == EXTRACT) else {
        return Ok(None);
    };
    let Some(raw) = f.options.get("schema") else {
        // 没有 schema 时要有意图（0.97：由 LLM 生成 schema，见 `autoschema`）
        return match f.options.get("intent").and_then(Value::as_str) {
            Some(_) => Ok(None),
            None => Err(ApiError::invalid(
                "the extract format needs a `schema` object or an `intent` string",
            )),
        };
    };
    // 只写了 describe 的语义字段：类型、选择器要等 LLM 解析后才有，先按 text 校验其余部分
    let mut raw = raw.clone();
    if let Some(fs) = raw["fields"].as_array_mut() {
        for f in fs
            .iter_mut()
            .filter(|f| f.get("describe").is_some() && f.get("type").is_none())
        {
            f["type"] = Value::String("text".into());
        }
    }
    let raw = &raw;
    let schema = Schema::from_json(raw).map_err(|e| ApiError::invalid(format!("schema: {e}")))?;
    pagesift_extract::JsonCssExtractor::new(&schema)
        .map_err(|e| ApiError::invalid(format!("schema: {e}")))?;
    Ok(Some((schema, schema_id(raw))))
}

/// schema 的 id：规范化 JSON（键有序）的 BLAKE3 摘要前 16 个十六进制字符（64 位，同一站点上
/// 碰撞概率约 n²/2⁶⁵，可以忽略）。schema 改了 id 就变，旧记忆不会套到新 schema 上。
pub fn schema_id(raw: &Value) -> String {
    let canonical = serde_json::to_string(raw).unwrap_or_default();
    blake3::hash(canonical.as_bytes()).to_hex()[..16].to_string()
}

struct StoreMemory {
    store: pagesift_store::Store,
    site: String,
    schema: String,
}

impl SelectorMemory for StoreMemory {
    fn recall(&self, field: &str) -> Option<Target> {
        let e = self
            .store
            .selectors()
            .get(&self.site, &self.schema, field)
            .ok()??;
        serde_json::from_slice(&e.value).ok()
    }

    fn remember(&self, field: &str, target: &Target) {
        if let Ok(v) = serde_json::to_vec(target) {
            // 写失败只影响下次能否重定位，不影响这次的结果
            let _ = self
                .store
                .selectors()
                .put(&self.site, &self.schema, field, &v);
        }
    }
}

impl Scraper {
    /// 单页结果的出口：先走正文管线，要了 `extract` 时再在最终 HTML 上抽取。
    pub(super) async fn scrape_document(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let intent = super::autoschema::intent_of(opts);
        let parsed = parse_format(opts)?;
        if parsed.is_none() && intent.is_none() {
            return self.scrape_content(source, opts).await;
        }
        let site = match &source {
            Source::Url(url, _) => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(pagesift_content::site_key)),
            Source::Upload(_) => None,
        };
        let mut inner = opts.clone();
        inner.formats.retain(|f| f.kind != EXTRACT);
        let wants_raw = inner.wants("rawHtml");
        if !wants_raw {
            inner.formats.push(Format::new("rawHtml"));
        }
        let mut doc = self.scrape_content(source, &inner).await?;
        let html = if wants_raw {
            doc.raw_html.clone()
        } else {
            doc.raw_html.take()
        }
        .unwrap_or_default();
        let url = doc.metadata.source_url.clone().unwrap_or_default();
        let mut notes: Vec<String> = Vec::new();
        let mut used = super::autoschema::LlmUse::default();
        let budget = self.snapshot_budget().await;
        // schema：请求里给的，或按意图从缓存取 / 让 LLM 生成
        let (mut raw, cache_key) = match (&parsed, &intent) {
            (Some(_), _) => (
                opts.formats
                    .iter()
                    .find(|f| f.kind == EXTRACT)
                    .and_then(|f| f.options.get("schema").cloned())
                    .unwrap_or_default(),
                None,
            ),
            (None, Some(intent)) => {
                let key = super::autoschema::cache_key(&url, intent);
                let cached = key.as_ref().and_then(|k| self.kv_get(k));
                match cached {
                    Some(v) => {
                        notes.push("extract_schema: cached".into());
                        (v, key)
                    }
                    None => {
                        let llm = self.llm.as_ref().ok_or_else(|| {
                            ApiError::unsupported(
                                "extract with an intent needs an LLM: set [llm] in pagesift.toml",
                            )
                        })?;
                        let v = super::autoschema::generate(
                            llm, intent, &url, &html, budget, &mut used,
                        )
                        .await?;
                        notes.push("extract_schema: generated".into());
                        (v, key)
                    }
                }
            }
            (None, None) => unreachable!("handled above"),
        };
        // 选择器记忆的命名空间在 LLM 修补前定下：按意图时用缓存键，给了 schema 时用请求里那份的 id，
        // 修补后的 schema 不另起一套记忆（否则改版时记下的列表项位置就丢了）
        let mem_id = cache_key.clone().unwrap_or_else(|| schema_id(&raw));
        // 语义字段：只写了 describe 的字段，第一次用时让 LLM 解析选择器
        let semantic: Vec<usize> = raw["fields"]
            .as_array()
            .map(|fs| {
                fs.iter()
                    .enumerate()
                    .filter(|(_, f)| f.get("selector").is_none() && f.get("describe").is_some())
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        if !semantic.is_empty() {
            let llm = self.llm.as_ref().ok_or_else(|| {
                ApiError::unsupported(
                    "fields with only `describe` need an LLM: set [llm] in pagesift.toml",
                )
            })?;
            let item = first_item_html(&html, raw["baseSelector"].as_str().unwrap_or("body"));
            for i in semantic {
                let d = raw["fields"][i]["describe"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let r = super::autoschema::resolve_field(llm, &d, &item, budget, &mut used).await?;
                patch_field(&mut raw["fields"][i], &r);
                notes.push(format!(
                    "selector_resolved: fields.{} via llm",
                    raw["fields"][i]["name"].as_str().unwrap_or_default()
                ));
            }
        }
        let schema =
            Schema::from_json(&raw).map_err(|e| ApiError::invalid(format!("schema: {e}")))?;
        let mut outcome = self
            .extract_with(&html, &schema, site.clone(), &mem_id)
            .await?;
        // LLM 兜底：缓存的选择器和 locate 都没找回的字段，让 LLM 重新解析一次
        if let (Some(llm), false) = (self.llm.as_ref(), outcome.unresolved.is_empty()) {
            let item = first_item_html(&html, &schema.base_selector);
            let mut healed = false;
            for (key, _) in outcome.unresolved.clone() {
                let Some(name) = key.strip_prefix("fields.") else {
                    continue;
                };
                let Some(i) = raw["fields"]
                    .as_array()
                    .and_then(|fs| fs.iter().position(|f| f["name"] == name))
                else {
                    continue;
                };
                let d = raw["fields"][i]["describe"]
                    .as_str()
                    .unwrap_or(name)
                    .to_string();
                let r = super::autoschema::resolve_field(llm, &d, &item, budget, &mut used).await?;
                patch_field(&mut raw["fields"][i], &r);
                notes.push(format!("selector_healed: fields.{name} via llm"));
                healed = true;
            }
            if healed {
                let schema = Schema::from_json(&raw)
                    .map_err(|e| ApiError::invalid(format!("schema: {e}")))?;
                outcome = self
                    .extract_with(&html, &schema, site.clone(), &mem_id)
                    .await?;
            }
        }
        if let Some(k) = &cache_key
            && !outcome.items.is_empty()
        {
            self.kv_put(k, &raw);
        }
        if used.calls > 0 {
            notes.push(format!(
                "llm_usage: {} calls, {} tokens",
                used.calls, used.tokens
            ));
        }
        let mut warnings = notes;
        warnings.extend(outcome.warnings());
        if !warnings.is_empty() {
            doc.metadata
                .pagesift
                .get_or_insert_with(Default::default)
                .warnings
                .extend(warnings);
        }
        doc.extract = Some(Value::Array(outcome.items));
        Ok(doc)
    }
}

/// 第一个列表项的 HTML（找不到时整页）：语义字段和 LLM 兜底在它的快照里找选择器。
fn first_item_html(html: &str, base: &str) -> String {
    let doc = pagesift_html::Html::parse(html);
    pagesift_html::Selector::parse(base)
        .ok()
        .and_then(|s| doc.select(&s).next().map(|n| n.html()))
        .unwrap_or_else(|| html.to_string())
}

/// 把 LLM 给的 `{selector, type, attribute}` 写进字段（保留 name、describe）。
fn patch_field(field: &mut Value, r: &Value) {
    if let Some(sel) = r["selector"].as_str() {
        field["selector"] = Value::String(sel.to_string());
    }
    let kind = r["type"].as_str().unwrap_or("text");
    field["type"] = Value::String(kind.to_string());
    match r["attribute"].as_str() {
        Some(a) if kind == "attribute" => field["attribute"] = Value::String(a.to_string()),
        _ => {
            if let Some(o) = field.as_object_mut() {
                o.remove("attribute");
            }
        }
    }
}

impl Scraper {
    async fn extract_with(
        &self,
        html: &str,
        schema: &Schema,
        site: Option<String>,
        id: &str,
    ) -> Result<pagesift_extract::AdaptiveOutcome, ApiError> {
        let memory = match (&self.store, site) {
            (Some(store), Some(site)) => Some(StoreMemory {
                store: store.clone(),
                site,
                schema: id.to_string(),
            }),
            _ => None,
        };
        let (html, schema) = (html.to_string(), schema.clone());
        cpu_task(move || {
            let parsed = pagesift_html::Html::parse(&html);
            match &memory {
                Some(m) => pagesift_extract::extract_adaptive(&parsed, &schema, m),
                None => pagesift_extract::extract_adaptive(
                    &parsed,
                    &schema,
                    &pagesift_extract::NoMemory,
                ),
            }
        })
        .await?
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("extract: {e}")))
    }

    fn kv_get(&self, key: &str) -> Option<Value> {
        let v = self.store.as_ref()?.tasks().kv_get(key).ok()??;
        serde_json::from_slice(&v).ok()
    }

    fn kv_put(&self, key: &str, v: &Value) {
        if let (Some(store), Ok(bytes)) = (&self.store, serde_json::to_vec(v)) {
            let _ = store.tasks().kv_put(key, &bytes);
        }
    }

    pub(super) async fn snapshot_budget_pub(&self) -> usize {
        self.snapshot_budget().await
    }

    /// 快照的 token 预算：一次请求能用的 token 数取模型上下文长度和每分钟 token 限额（服务在响应头里
    /// 报）中较小的，减去提示本身；输出长度事先不知道（推理型模型的思考也算输出），剩下的输入、输出
    /// 各一半（[推导] 不知道比例时的对称划分），请求的 `max_tokens` 用另一半，总量不超过限额。
    async fn snapshot_budget(&self) -> usize {
        // [策略] 服务不报上下文长度时的先验：8192（OpenAI 兼容服务里常见的最小上下文）
        let (ctx, out) = match &self.llm {
            Some(l) => l.model_limits().await.unwrap_or((8192, None)), // [策略] 见上
            None => (8192, None),                                      // [策略] 见上
        };
        let tpm = match &self.llm {
            Some(l) => l.token_rate_limit().await,
            None => None,
        };
        // 估计有误差（见 `model_tokens`）：只用限额的 1 − δ
        let usable = (tpm.map_or(ctx, |t| t.min(ctx)) as f64 * (1.0 - DELTA)) as usize;
        let prompt = super::autoschema::model_tokens(super::autoschema::SCHEMA_SYSTEM_TEXT);
        let half = usable.saturating_sub(prompt) / 2;
        super::autoschema::snapshot_budget(usable, prompt, out.map_or(half, |o| o.min(half)))
    }
}
