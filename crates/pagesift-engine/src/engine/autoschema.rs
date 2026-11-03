//! 解析一次 → 缓存 → 自愈 → LLM 兜底（0.97）。
//!
//! - **页面快照**：每个有文字或关键属性的元素一行：短 id、角色、可见文字、关键属性。纯 HTTP 页从
//!   pagesift 的 DOM 推出（浏览器的 `Accessibility.getFullAXTree` 路径没有接：这台机器上浏览器不可用）。
//!   快照的 token 上限 = 模型上下文 − 提示 − 输出上限（[`snapshot_budget`]）。
//! - **生成 schema**：`{"type": "extract", "intent": "…"}` 没有 schema 时，意图加快照交给 LLM，要它输出
//!   pagesift 的 schema JSON（不是代码），当场在页面上验证；为空或不合法时把错误反馈给模型，只重试一次。
//!   缓存键是“注册域 + URL 形状 + 意图哈希”，存在 `pagesift-store`。
//! - **语义字段**：字段只写 `describe` 时，第一次用时由 LLM 解析出选择器并写回缓存。
//! - **自愈顺序**：缓存的选择器（精确）→ 0.93 的 `locate` → LLM 重新解析；走到哪一步写进 warnings。
//! - **observe**：给出候选元素（选择器、角色、名称），供 act 用（act 需要浏览器动作）。

use pagesift_api::types::ScrapeOptions;
use pagesift_api::{ApiError, ErrorCode};
use pagesift_html::{Html, NodeRef};
use pagesift_llm::{ChatClient, ChatMessage, ChatRequest, ResponseFormat, Role};
use serde_json::{Value, json};

use super::extract::llm_error;

/// 快照一行里文字的最大字符数：超过的截断（快照按 token 总预算裁剪，这里只防一个元素吃掉全部预算）。
/// [推导] 取总预算的 1/64（下限一句话的量级 80 字符）：64 个元素以上才会触发截断
fn per_node_chars(budget_tokens: usize) -> usize {
    (budget_tokens / 64).max(80) // [推导] 见上
}

/// 快照的 token 预算：上下文长度减去提示和输出上限。
pub fn snapshot_budget(context_tokens: usize, prompt_tokens: usize, max_output: usize) -> usize {
    context_tokens.saturating_sub(prompt_tokens + max_output)
}

fn role(tag: &str) -> &'static str {
    // [协议] 隐式 ARIA 角色（WAI-ARIA in HTML，W3C Recommendation）
    match tag {
        "a" => "link",
        "button" => "button",
        "input" | "textarea" => "textbox",
        "select" => "combobox",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "img" => "img",
        "li" => "listitem",
        "ul" | "ol" => "list",
        "table" => "table",
        "tr" => "row",
        "td" | "th" => "cell",
        "nav" => "navigation",
        "main" => "main",
        "article" => "article",
        "form" => "form",
        "p" => "paragraph",
        _ => "generic",
    }
}

const KEY_ATTRS: &[&str] = &[
    "id",
    "class",
    "href",
    "src",
    "alt",
    "name",
    "type",
    "aria-label",
    "title",
    "itemprop",
    "data-testid",
    "role",
];

/// 快照：每行 `[n] role "文字" attr=value …`，按文档顺序，总 token 不超过 `budget`。
/// 返回快照文字和 id → 元素的对照（observe 用）。
pub fn snapshot<'a>(doc: &'a Html<'a>, budget: usize) -> (String, Vec<NodeRef<'a>>) {
    let cap = per_node_chars(budget);
    let mut out = String::new();
    let mut nodes = Vec::new();
    let mut used = 0usize;
    for n in doc.root().descendants() {
        let Some(el) = n.as_element() else { continue };
        if matches!(
            el.name(),
            "script" | "style" | "noscript" | "template" | "head" | "svg"
        ) {
            continue;
        }
        let own: String = n
            .children()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join(" ");
        let text: String = own.split_whitespace().collect::<Vec<_>>().join(" ");
        let attrs: Vec<String> = KEY_ATTRS
            .iter()
            .filter_map(|k| el.attr(k).map(|v| format!("{k}={}", truncate(v, cap / 2))))
            .collect();
        if text.is_empty() && attrs.is_empty() {
            continue;
        }
        let line = format!(
            "[{}] {} <{}> {:?} {}\n",
            nodes.len(),
            role(el.name()),
            el.name(),
            truncate(&text, cap),
            attrs.join(" ")
        );
        let t = model_tokens(&line);
        if used + t > budget {
            break;
        }
        used += t;
        out.push_str(&line);
        nodes.push(n);
    }
    (out, nodes)
}

/// 模型分词器下的 token 数估计：按字节数 / 3。实测（2026-11-03，Groq `openai/gpt-oss-120b`）：
/// quotes.toscrape.com 的快照 6033 字节计 1969 个 token，3.06 字节/token，取整到 3（偏保守）。
/// 快照里多是标签、属性和网址，按词数会严重低估；常说的“4 个字符一个 token”是英文散文的均值，也偏少。
pub fn model_tokens(s: &str) -> usize {
    s.len().div_ceil(3) // [推导] 见上，实测 3.06 字节/token 向下取整
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 生成 schema 的系统提示（预算估算也用它的长度）。
pub const SCHEMA_SYSTEM_TEXT: &str = SCHEMA_SYSTEM;
const SCHEMA_SYSTEM: &str = "You write CSS extraction schemas for pagesift. Reply with one JSON object only: \
{\"baseSelector\": \"<CSS selector of each repeated item, or of the single item>\", \"fields\": [{\"name\": \"<field>\", \
\"selector\": \"<CSS selector relative to the item>\", \"type\": \"text\" | \"attribute\" | \"html\", \"attribute\": \"<attr, when type is attribute>\"}]}. \
Use selectors that exist in the page snapshot (tags, ids, classes, attributes shown there). Prefer stable attributes and \
semantic class names over positions. Do not invent fields that are not on the page.";

/// LLM 调用的计数（重复运行应接近 0）。
#[derive(Debug, Default, Clone, Copy)]
pub struct LlmUse {
    pub calls: usize,
    pub tokens: usize,
}

/// `max_out`：输出上限，等于快照预算（`structured.rs` 的对称划分：输入、输出各一半）。
async fn ask(
    llm: &ChatClient,
    system: &str,
    user: String,
    max_out: usize,
    used: &mut LlmUse,
) -> Result<Value, ApiError> {
    let req = ChatRequest {
        messages: vec![
            ChatMessage::new(Role::System, system),
            ChatMessage::new(Role::User, user.clone()),
        ],
        temperature: Some(0.0),
        max_tokens: Some(max_out.min(u32::MAX as usize) as u32),
        response_format: Some(ResponseFormat::JsonObject),
    };
    used.calls += 1;
    let resp = llm.chat(&req).await.map_err(llm_error)?;
    used.tokens += pagesift_extract::TokenIndex::new(system).len()
        + pagesift_extract::TokenIndex::new(&user).len()
        + pagesift_extract::TokenIndex::new(&resp.content).len();
    let body = resp.content.trim();
    let body = body
        .strip_prefix("```json")
        .or_else(|| body.strip_prefix("```"))
        .map(|b| b.trim_end_matches("```").trim())
        .unwrap_or(body);
    serde_json::from_str(body).map_err(|e| {
        ApiError::new(
            ErrorCode::UpstreamError,
            format!("the LLM did not return JSON: {e}"),
        )
    })
}

/// 在页面上验证 schema：能编译、至少抽出一个列表项、列表项里至少一个字段有值。
pub fn validate_on(doc: &Html<'_>, schema: &Value) -> Result<Vec<Value>, String> {
    let s = pagesift_extract::Schema::from_json(schema).map_err(|e| e.to_string())?;
    let items = pagesift_extract::JsonCssExtractor::new(&s)
        .map_err(|e| e.to_string())?
        .extract_document(doc);
    if items.is_empty() {
        return Err(format!(
            "the schema extracted nothing: baseSelector {:?} matched no element with any field",
            s.base_selector
        ));
    }
    Ok(items)
}

/// 意图 → schema：生成、验证，不合格时把错误反馈给模型重试一次。
pub async fn generate(
    llm: &ChatClient,
    intent: &str,
    url: &str,
    html: &str,
    budget: usize,
    used: &mut LlmUse,
) -> Result<Value, ApiError> {
    let doc = Html::parse(html);
    let (snap, _) = snapshot(&doc, budget);
    let prompt = format!("URL: {url}\nWhat to extract: {intent}\n\nPage snapshot:\n{snap}");
    let first = ask(llm, SCHEMA_SYSTEM, prompt.clone(), budget, used).await?;
    match validate_on(&doc, &first) {
        Ok(_) => Ok(first),
        Err(why) => {
            let retry = format!(
                "{prompt}\n\nYour previous schema was:\n{first}\nIt failed on this page: {why}\nReturn a corrected schema."
            );
            let second = ask(llm, SCHEMA_SYSTEM, retry, budget, used).await?;
            validate_on(&doc, &second).map_err(|why| {
                ApiError::new(
                    ErrorCode::UpstreamError,
                    format!(
                        "the generated schema does not work on the page after one correction: {why}"
                    ),
                )
            })?;
            Ok(second)
        }
    }
}

const FIELD_SYSTEM: &str = "You find CSS selectors for pagesift. Reply with one JSON object only: \
{\"selector\": \"<CSS selector relative to the item element>\", \"type\": \"text\" | \"attribute\" | \"html\", \"attribute\": \"<attr or null>\"}. \
Use only tags, classes and attributes that appear in the snapshot of the item.";

/// 语义字段或失效字段：在第一个列表项的快照里让 LLM 找选择器。
pub async fn resolve_field(
    llm: &ChatClient,
    describe: &str,
    item_html: &str,
    budget: usize,
    used: &mut LlmUse,
) -> Result<Value, ApiError> {
    let doc = Html::parse(item_html);
    let (snap, _) = snapshot(&doc, budget);
    ask(
        llm,
        FIELD_SYSTEM,
        format!("Field: {describe}\n\nItem snapshot:\n{snap}"),
        budget,
        used,
    )
    .await
}

/// observe：意图对应的候选元素（选择器、角色、名称），选择器在页面上验证过恰好命中一个元素。
pub async fn observe(
    llm: &ChatClient,
    instruction: &str,
    html: &str,
    budget: usize,
    used: &mut LlmUse,
) -> Result<Vec<Value>, ApiError> {
    let doc = Html::parse(html);
    let (snap, nodes) = snapshot(&doc, budget);
    let v = ask(
        llm,
        "You pick page elements for an action. Reply with JSON {\"ids\": [<snapshot ids, best first>]}.",
        format!("Instruction: {instruction}\n\nPage snapshot:\n{snap}"),
        budget,
        used,
    )
    .await?;
    let mut out = Vec::new();
    for id in v["ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
    {
        let Some(n) = nodes.get(id as usize) else {
            continue;
        };
        let Some(sel) = pagesift_extract::locate::robust_selector(*n, doc.root()) else {
            continue;
        };
        let el = n.as_element();
        let name = el
            .and_then(|e| e.attr("aria-label").or(e.attr("title")))
            .map(str::to_string)
            .unwrap_or_else(|| n.text().split_whitespace().collect::<Vec<_>>().join(" "));
        out.push(
            json!({ "selector": sel, "role": role(el.map_or("", |e| e.name())), "name": name }),
        );
    }
    Ok(out)
}

/// act 的动作里 `{{secret:NAME}}` 换成本地配置的凭证；替换在发给浏览器之前做，模型看不到值。
pub fn fill_secrets(
    actions: &mut [Value],
    credentials: &std::collections::BTreeMap<String, String>,
) -> Result<(), ApiError> {
    for a in actions {
        let Some(text) = a.get("text").and_then(Value::as_str) else {
            continue;
        };
        let mut out = String::new();
        let mut rest = text;
        while let Some(i) = rest.find("{{secret:") {
            out.push_str(&rest[..i]);
            let after = &rest[i + "{{secret:".len()..];
            let Some(j) = after.find("}}") else {
                return Err(ApiError::invalid("unterminated {{secret:...}} placeholder"));
            };
            let name = &after[..j];
            let v = credentials.get(name).ok_or_else(|| {
                ApiError::invalid(format!("no credential named {name:?} is configured"))
            })?;
            out.push_str(v);
            rest = &after[j + 2..];
        }
        out.push_str(rest);
        a["text"] = json!(out);
    }
    Ok(())
}

/// `intent` 的缓存键：注册域 + URL 形状 + 意图的 BLAKE3 前 16 个十六进制字符。
pub fn cache_key(url: &str, intent: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let site = pagesift_content::site_key(u.host_str()?);
    let shape = pagesift_frontier::focus::url_shape(&u);
    let h = blake3::hash(intent.trim().to_lowercase().as_bytes()).to_hex();
    Some(format!("autoschema/{site}{shape}#{}", &h[..16]))
}

/// 选项里的意图（`{"type": "extract", "intent": "..."}`，没有 `schema` 时）。
pub fn intent_of(opts: &ScrapeOptions) -> Option<String> {
    let f = opts.formats.iter().find(|f| f.kind == "extract")?;
    if f.options.contains_key("schema") {
        return None;
    }
    f.options.get("intent")?.as_str().map(str::to_string)
}

impl super::Scraper {
    /// observe：页面上和指令对应的候选元素（选择器、角色、名称），最合适的在前。
    pub async fn observe(
        &self,
        url: &str,
        instruction: &str,
    ) -> Result<(Vec<Value>, LlmUse), ApiError> {
        let llm = self.llm.as_ref().ok_or_else(|| {
            ApiError::unsupported("observe needs an LLM: set [llm] in pagesift.toml")
        })?;
        let opts: ScrapeOptions = serde_json::from_value(json!({ "formats": ["rawHtml"] }))
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        let doc = self.scrape(url, &opts).await?;
        let html = doc.raw_html.unwrap_or_default();
        let mut used = LlmUse::default();
        let budget = self.snapshot_budget_pub().await;
        let c = observe(llm, instruction, &html, budget, &mut used).await?;
        Ok((c, used))
    }

    /// act：observe 选出元素，再用浏览器动作执行（`text` 给了时输入它，否则点击）。`text` 里的
    /// `{{secret:NAME}}` 在本地换成配置的凭证，模型和日志都看不到值。返回执行后的页面。
    pub async fn act(
        &self,
        url: &str,
        instruction: &str,
        text: Option<&str>,
    ) -> Result<(pagesift_api::types::Document, Value), ApiError> {
        let (cands, used) = self.observe(url, instruction).await?;
        let Some(target) = cands.first() else {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!("no element on the page matches {instruction:?}"),
            ));
        };
        let sel = target["selector"].clone();
        let mut actions = match text {
            Some(t) => vec![
                json!({ "type": "click", "selector": sel }),
                json!({ "type": "write", "text": t }),
            ],
            None => vec![json!({ "type": "click", "selector": sel })],
        };
        fill_secrets(&mut actions, &self.secrets)?;
        actions.push(json!({ "type": "scrape" }));
        let opts: ScrapeOptions =
            serde_json::from_value(json!({ "engine": "browser", "actions": actions }))
                .map_err(|e| ApiError::invalid(e.to_string()))?;
        let doc = self.scrape(url, &opts).await?;
        Ok((
            doc,
            json!({ "element": target, "llmCalls": used.calls, "llmTokens": used.tokens }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_respects_budget_and_secrets_stay_local() {
        let html = "<ul>".to_string()
            + &(0..200)
                .map(|i| format!("<li class='item'><a href='/p/{i}'>Product {i}</a></li>"))
                .collect::<String>()
            + "</ul>";
        let doc = Html::parse(&html);
        let (snap, nodes) = snapshot(&doc, 300);
        assert!(model_tokens(&snap) <= 300);
        assert!(!nodes.is_empty() && nodes.len() < 200);
        assert!(snap.contains("link <a> \"Product 0\" href=/p/0"), "{snap}");
        let mut acts = vec![json!({"type": "write", "text": "user {{secret:PW}}!"})];
        let creds = [("PW".to_string(), "hunter2".to_string())].into();
        fill_secrets(&mut acts, &creds).unwrap();
        assert_eq!(acts[0]["text"], "user hunter2!");
        let mut bad = vec![json!({"type": "write", "text": "{{secret:NOPE}}"})];
        assert!(fill_secrets(&mut bad, &creds).is_err());
        let a = cache_key("https://www.shop.example.co.uk/item/123?ref=x", "Prices").unwrap();
        let b = cache_key("https://a.example.co.uk/item/9?ref=1", " prices ").unwrap();
        assert_eq!(a, b);
        assert!(
            a.starts_with("autoschema/example.co.uk/item/{n}?ref#"),
            "{a}"
        );
    }
}
