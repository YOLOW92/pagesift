//! 渲染选项和结果。`Action` 的 JSON 形状对齐 Firecrawl v2 的 `actions`。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::guard::BlockResources;

pub const MAX_ACTIONS: usize = 50;
/// Firecrawl 的上限：`waitFor` 加上所有 wait action 不超过 60 秒，按选择器等待记 1 秒。
pub const MAX_TOTAL_WAIT: Duration = Duration::from_secs(60);
pub const SUPPORTED_ACTIONS: &[&str] = &[
    "wait",
    "click",
    "write",
    "press",
    "scroll",
    "screenshot",
    "scrape",
    "executeJavascript",
    "pdf",
];

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// 页面加载完成后再等多久（Firecrawl 的 `waitFor`）。
    pub wait_for: Duration,
    /// 整个渲染（含排队等标签页）的总时长上限。
    pub timeout: Duration,
    /// 只模拟移动端视口和触屏，不改 UA。
    pub mobile: bool,
    /// `location.languages`，映射到 Accept-Language 和 `navigator.languages`。
    pub languages: Vec<String>,
    pub actions: Vec<Action>,
    /// 对应 `formats` 里的 `screenshot`。
    pub screenshot: Option<ScreenshotOptions>,
    pub block: BlockResources,
    /// 什么时候算页面就绪，之后才执行 `wait_for`、滚动和 actions。
    pub wait_until: WaitUntil,
    /// 把 iframe 的文档内联进输出 HTML，替换成 `<div data-pagesift-iframe="src">`。
    pub process_iframes: bool,
    /// iframe 嵌套最多展开几层（主文档里的 iframe 是第 1 层）。
    pub max_iframe_depth: usize,
    /// 一页最多内联多少个 iframe。
    pub max_iframes: usize,
    /// 把 open shadow root 按组合树（slot 换成分配进来的节点）序列化进输出。
    pub flatten_shadow_dom: bool,
    /// 逐屏滚到底触发懒加载（Crawl4AI 的 `scan_full_page`）。
    pub scan_full_page: Option<FullPageScan>,
    /// 会回收节点的虚拟列表：滚动时收集出现过的条目（Crawl4AI 的 `VirtualScrollConfig`）。
    pub virtual_scroll: Option<VirtualScroll>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitUntil {
    /// `load` 事件：所有图片、字体、iframe 都加载完。
    Load,
    #[serde(rename = "domcontentloaded")]
    DomContentLoaded,
    /// DOMContentLoaded 之后，脚本、XHR/fetch、文档类请求静默一小段时间。
    /// 不等图片和字体；需要截图时会再等 `load`。
    #[default]
    #[serde(rename = "networkidle")]
    NetworkIdle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FullPageScan {
    /// 每滚一屏后最多等多久让新内容出现（网络静默就提前结束）。
    pub scroll_delay: Duration,
    pub max_steps: usize,
    /// 整个扫描的时长上限，超出后停止滚动、照常返回。
    pub timeout: Duration,
}

impl Default for FullPageScan {
    fn default() -> Self {
        FullPageScan {
            scroll_delay: Duration::from_millis(500),
            max_steps: 50,
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VirtualScroll {
    /// 可滚动的列表容器；它自己不能滚动时滚动窗口。
    pub container_selector: String,
    pub scroll_count: usize,
    pub scroll_by: ScrollBy,
    /// 每次滚动后最多等多久（容器内容变化后静默 50ms 就提前结束）。
    pub wait_after_scroll: Duration,
}

impl VirtualScroll {
    pub fn new(container_selector: impl Into<String>) -> Self {
        VirtualScroll {
            container_selector: container_selector.into(),
            scroll_count: 10,
            scroll_by: ScrollBy::ContainerHeight,
            wait_after_scroll: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollBy {
    ContainerHeight,
    PageHeight,
    Pixels(u32),
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            wait_for: Duration::ZERO,
            timeout: Duration::from_secs(30),
            mobile: false,
            languages: Vec::new(),
            actions: Vec::new(),
            screenshot: None,
            block: BlockResources::default(),
            wait_until: WaitUntil::default(),
            process_iframes: true,
            max_iframe_depth: 3,
            max_iframes: 20,
            flatten_shadow_dom: true,
            scan_full_page: None,
            virtual_scroll: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotOptions {
    #[serde(default)]
    pub full_page: bool,
    /// 1 到 100。给了就输出 JPEG，否则输出 PNG。
    #[serde(default)]
    pub quality: Option<u8>,
    #[serde(default)]
    pub viewport: Option<Viewport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    Wait {
        #[serde(default)]
        milliseconds: Option<u64>,
        #[serde(default)]
        selector: Option<String>,
    },
    Click {
        selector: String,
        #[serde(default)]
        all: bool,
    },
    Write {
        text: String,
    },
    Press {
        key: String,
    },
    Scroll {
        #[serde(default)]
        direction: ScrollDirection,
        #[serde(default)]
        selector: Option<String>,
    },
    Screenshot(ScreenshotOptions),
    Scrape,
    ExecuteJavascript {
        script: String,
    },
    Pdf {
        #[serde(default)]
        format: PdfFormat,
        #[serde(default)]
        landscape: bool,
        #[serde(default)]
        scale: Option<f64>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    Up,
    #[default]
    Down,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum PdfFormat {
    A0,
    A1,
    A2,
    A3,
    A4,
    A5,
    A6,
    #[default]
    Letter,
    Legal,
    Tabloid,
    Ledger,
}

impl PdfFormat {
    /// 纸张宽高（英寸）。
    pub(crate) fn inches(self) -> (f64, f64) {
        match self {
            PdfFormat::A0 => (33.1, 46.8),
            PdfFormat::A1 => (23.4, 33.1),
            PdfFormat::A2 => (16.54, 23.4),
            PdfFormat::A3 => (11.7, 16.54),
            PdfFormat::A4 => (8.27, 11.7),
            PdfFormat::A5 => (5.83, 8.27),
            PdfFormat::A6 => (4.13, 5.83),
            PdfFormat::Letter => (8.5, 11.0),
            PdfFormat::Legal => (8.5, 14.0),
            PdfFormat::Tabloid => (11.0, 17.0),
            PdfFormat::Ledger => (17.0, 11.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Screenshot {
    pub format: ImageFormat,
    /// 原始图片字节；base64 编码由调用方负责。
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ScrapeResult {
    pub url: String,
    pub html: String,
}

/// 对应 Firecrawl 的 `javascriptReturns` 元素。
#[derive(Debug, Clone, PartialEq)]
pub struct JavascriptReturn {
    pub kind: String,
    pub value: Value,
}

/// 按 Firecrawl 的分组存放 actions 的产出，组内保持执行顺序。
#[derive(Debug, Clone, Default)]
pub struct ActionOutputs {
    pub screenshots: Vec<Screenshot>,
    pub scrapes: Vec<ScrapeResult>,
    pub javascript_returns: Vec<JavascriptReturn>,
    pub pdfs: Vec<Vec<u8>>,
}

impl ActionOutputs {
    /// Firecrawl 响应里 `actions` 字段的形状；截图和 PDF 是 base64 的 data URL。
    pub fn to_firecrawl_json(&self) -> Value {
        use base64::Engine as _;
        let b64 = |mime: &str, d: &[u8]| {
            format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(d)
            )
        };
        serde_json::json!({
            "screenshots": self.screenshots.iter().map(|s| b64(s.format.mime(), &s.data)).collect::<Vec<_>>(),
            "scrapes": self.scrapes.iter().map(|s| serde_json::json!({ "url": s.url, "html": s.html })).collect::<Vec<_>>(),
            "javascriptReturns": self.javascript_returns.iter().map(|j| serde_json::json!({ "type": j.kind, "value": j.value })).collect::<Vec<_>>(),
            "pdfs": self.pdfs.iter().map(|p| b64("application/pdf", p)).collect::<Vec<_>>(),
        })
    }
}

/// 各阶段耗时，越低越好。
#[derive(Debug, Clone, Default)]
pub struct Timings {
    /// 排队等到标签页（可能包括启动浏览器）。
    pub acquire: Duration,
    /// 建上下文和 target、附加、开拦截、设视口。
    pub setup: Duration,
    /// `Page.navigate` 到 DOMContentLoaded（`WaitUntil::Load` 时到 load）。
    pub navigation: Duration,
    /// 就绪等待（网络静默）、`wait_for`、整页滚动和虚拟滚动。
    pub wait: Duration,
    pub actions: Duration,
    pub screenshot: Duration,
    /// 序列化 HTML（含 iframe 合并）。
    pub html: Duration,
    /// 其中花在 iframe 发现和合并上的时间。
    pub iframes: Duration,
    pub total: Duration,
}

#[derive(Debug, Clone)]
pub struct BlockedRequest {
    pub url: String,
    pub resource_type: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub final_url: String,
    /// 最后一次主框架文档响应的状态码；非 HTTP 页面为 `None`。
    pub status: Option<u16>,
    pub html: String,
    pub screenshot: Option<Screenshot>,
    pub actions: ActionOutputs,
    pub blocked_requests: Vec<BlockedRequest>,
    /// 内联进 `html` 的 iframe 数，以及因为深度、数量上限或读取失败而保留原样的数。
    pub iframes_inlined: usize,
    pub iframes_skipped: usize,
    /// 虚拟滚动收集到的条目数；没请求或找不到容器时为 `None`。
    pub virtual_scroll_items: Option<usize>,
    pub timings: Timings,
}

/// 解析 Firecrawl v2 的 `actions` 数组，出错时指出是第几个 action、哪里不对。
pub fn parse_actions(v: &Value) -> crate::Result<Vec<Action>> {
    use crate::Error::InvalidOptions as Bad;
    let arr = v
        .as_array()
        .ok_or_else(|| Bad("`actions` must be an array".into()))?;
    arr.iter()
        .enumerate()
        .map(|(i, a)| {
            let ty = a
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| Bad(format!("action #{i}: missing string field `type`")))?;
            if !SUPPORTED_ACTIONS.contains(&ty) {
                return Err(Bad(format!(
                    "action #{i}: unsupported action type `{ty}`; supported: {}",
                    SUPPORTED_ACTIONS.join(", ")
                )));
            }
            Action::deserialize(a).map_err(|e| Bad(format!("action #{i} ({ty}): {e}")))
        })
        .collect()
}

impl RenderOptions {
    pub(crate) fn validate(&self, allow_js: bool) -> crate::Result<()> {
        use crate::Error::InvalidOptions as Bad;
        if self.actions.len() > MAX_ACTIONS {
            return Err(Bad(format!("at most {MAX_ACTIONS} actions are allowed")));
        }
        let shots = self
            .screenshot
            .iter()
            .chain(self.actions.iter().filter_map(|a| match a {
                Action::Screenshot(s) => Some(s),
                _ => None,
            }));
        for s in shots {
            if s.quality.is_some_and(|q| !(1..=100).contains(&q)) {
                return Err(Bad("screenshot quality must be 1..=100".into()));
            }
            if let Some(v) = s.viewport
                && (v.width == 0 || v.height == 0 || v.width > 7680 || v.height > 4320)
            {
                return Err(Bad(
                    "screenshot viewport must be within 1..=7680 x 1..=4320".into(),
                ));
            }
        }
        let waited = self.wait_for
            + self
                .actions
                .iter()
                .map(|a| match a {
                    Action::Wait {
                        milliseconds: Some(ms),
                        ..
                    } => Duration::from_millis(*ms),
                    Action::Wait { .. } => Duration::from_secs(1),
                    _ => Duration::ZERO,
                })
                .sum::<Duration>();
        if waited > MAX_TOTAL_WAIT {
            return Err(Bad(
                "total wait time (waitFor + wait actions) cannot exceed 60 seconds".into(),
            ));
        }
        if let Some(v) = &self.virtual_scroll
            && v.container_selector.trim().is_empty()
        {
            return Err(Bad("virtual scroll needs a container selector".into()));
        }
        for a in &self.actions {
            match a {
                Action::ExecuteJavascript { .. } if !allow_js => {
                    return Err(crate::Error::ExecuteJavascriptDisabled);
                }
                Action::Wait {
                    milliseconds: None,
                    selector: None,
                } => {
                    return Err(Bad("wait needs `milliseconds` or `selector`".into()));
                }
                Action::Wait {
                    milliseconds: Some(0),
                    ..
                } => {
                    return Err(Bad("wait `milliseconds` must be positive".into()));
                }
                Action::Wait {
                    milliseconds: Some(_),
                    selector: Some(_),
                } => {
                    return Err(Bad(
                        "wait takes either `milliseconds` or `selector`, not both".into(),
                    ));
                }
                Action::Pdf { scale: Some(s), .. } if !(0.1..=2.0).contains(s) => {
                    return Err(Bad("pdf scale must be within 0.1..=2".into()));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firecrawl_action_json() {
        let json = r##"[
            {"type":"wait","milliseconds":500},
            {"type":"wait","selector":"#main"},
            {"type":"click","selector":"button.more","all":true},
            {"type":"write","text":"hello"},
            {"type":"press","key":"Enter"},
            {"type":"scroll","direction":"up","selector":"#feed"},
            {"type":"scroll"},
            {"type":"screenshot","fullPage":true,"quality":80,"viewport":{"width":800,"height":600}},
            {"type":"scrape"},
            {"type":"executeJavascript","script":"1+1"},
            {"type":"pdf","format":"A4","landscape":true,"scale":0.5,"unknownField":1}
        ]"##;
        let actions: Vec<Action> = serde_json::from_str(json).unwrap();
        assert_eq!(actions.len(), 11);
        assert_eq!(
            actions[2],
            Action::Click {
                selector: "button.more".into(),
                all: true
            }
        );
        assert_eq!(
            actions[6],
            Action::Scroll {
                direction: ScrollDirection::Down,
                selector: None
            }
        );
        assert_eq!(
            actions[7],
            Action::Screenshot(ScreenshotOptions {
                full_page: true,
                quality: Some(80),
                viewport: Some(Viewport {
                    width: 800,
                    height: 600
                }),
            })
        );
        assert_eq!(
            actions[10],
            Action::Pdf {
                format: PdfFormat::A4,
                landscape: true,
                scale: Some(0.5)
            }
        );
    }

    #[test]
    fn parse_actions_reports_index_and_type() {
        let ok = parse_actions(
            &serde_json::json!([{"type": "scrape"}, {"type": "wait", "milliseconds": 5}]),
        );
        assert_eq!(ok.unwrap().len(), 2);
        let err = |v: Value| match parse_actions(&v) {
            Err(crate::Error::InvalidOptions(m)) => m,
            other => panic!("{other:?}"),
        };
        let m = err(serde_json::json!([{"type": "scrape"}, {"type": "hover", "selector": "a"}]));
        assert!(
            m.contains("action #1") && m.contains("unsupported action type `hover`"),
            "{m}"
        );
        assert!(m.contains("executeJavascript"), "{m}");
        assert!(
            err(serde_json::json!([{"selector": "a"}])).contains("missing string field `type`")
        );
        let m = err(serde_json::json!([{"type": "click"}]));
        assert!(
            m.contains("action #0 (click)") && m.contains("selector"),
            "{m}"
        );
        assert!(err(serde_json::json!({"type": "scrape"})).contains("array"));
    }

    #[test]
    fn firecrawl_output_shape() {
        let out = ActionOutputs {
            screenshots: vec![Screenshot {
                format: ImageFormat::Jpeg,
                data: vec![1, 2, 3],
            }],
            scrapes: vec![ScrapeResult {
                url: "u".into(),
                html: "<p>".into(),
            }],
            javascript_returns: vec![JavascriptReturn {
                kind: "number".into(),
                value: 2.into(),
            }],
            pdfs: vec![b"%PDF".to_vec()],
        };
        assert_eq!(
            out.to_firecrawl_json(),
            serde_json::json!({
                "screenshots": ["data:image/jpeg;base64,AQID"],
                "scrapes": [{"url": "u", "html": "<p>"}],
                "javascriptReturns": [{"type": "number", "value": 2}],
                "pdfs": ["data:application/pdf;base64,JVBERg=="],
            })
        );
    }

    #[test]
    fn firecrawl_limits() {
        let mut o = RenderOptions {
            wait_for: Duration::from_secs(59),
            actions: vec![Action::Wait {
                milliseconds: None,
                selector: Some("#a".into()),
            }],
            ..Default::default()
        };
        assert!(o.validate(true).is_ok());
        o.actions.push(Action::Wait {
            milliseconds: Some(1),
            selector: None,
        });
        assert!(o.validate(true).is_err());
        let shot = |q, w| RenderOptions {
            screenshot: Some(ScreenshotOptions {
                full_page: false,
                quality: q,
                viewport: Some(Viewport {
                    width: w,
                    height: 100,
                }),
            }),
            ..Default::default()
        };
        assert!(shot(Some(1), 7680).validate(true).is_ok());
        assert!(shot(Some(0), 100).validate(true).is_err());
        assert!(shot(None, 7681).validate(true).is_err());
    }

    #[test]
    fn validation() {
        let mut o = RenderOptions {
            actions: vec![Action::ExecuteJavascript { script: "1".into() }],
            ..Default::default()
        };
        assert!(matches!(
            o.validate(false),
            Err(crate::Error::ExecuteJavascriptDisabled)
        ));
        assert!(o.validate(true).is_ok());
        o.actions = vec![Action::Wait {
            milliseconds: None,
            selector: None,
        }];
        assert!(o.validate(true).is_err());
        o.actions = vec![Action::Scrape; MAX_ACTIONS + 1];
        assert!(o.validate(true).is_err());
    }
}
