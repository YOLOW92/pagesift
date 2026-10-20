//! 渲染选项和结果。`Action` 的 JSON 形状对齐 Firecrawl v2 的 `actions`。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::guard::BlockResources;

pub const MAX_ACTIONS: usize = 50;

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
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotOptions {
    #[serde(default)]
    pub full_page: bool,
    /// 0 到 100。给了就输出 JPEG，否则输出 PNG。
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

#[derive(Debug, Clone, Default)]
pub struct Timings {
    /// 排队等到标签页（可能包括启动浏览器）。
    pub acquire: Duration,
    pub navigation: Duration,
    pub actions: Duration,
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
    pub timings: Timings,
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
            if s.quality.is_some_and(|q| q > 100) {
                return Err(Bad("screenshot quality must be 0..=100".into()));
            }
            if let Some(v) = s.viewport
                && (v.width == 0 || v.height == 0 || v.width > 10_000 || v.height > 10_000)
            {
                return Err(Bad("screenshot viewport out of range".into()));
            }
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
