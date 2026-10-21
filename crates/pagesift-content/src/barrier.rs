//! 拦截页识别：反爬挑战页、验证码页、登录墙、WAF 拦截页。只识别，不绕过。
//!
//! 判断结合三样东西：特征命中、可见文字量、HTTP 状态码。
//! 厂商脚本和验证码控件经常出现在正常页面里，所以任何特征都要求页面可见文字很少；
//! 能嵌进正常页面的特征（[`Signal::Hint`]）还要求同时命中拦截提示语或错误状态码。

use std::sync::OnceLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind};
use pagesift_html::Html;

use crate::dom::{NodeR, element, is_hidden, is_non_content_tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BarrierKind {
    /// JS 挑战或浏览器检查（"Just a moment..."）。
    Challenge,
    /// 需要人工完成的验证码。
    Captcha,
    /// 要求登录才能查看。
    LoginWall,
    /// WAF 或访问控制直接拒绝。
    Block,
}

impl BarrierKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Challenge => "challenge",
            Self::Captcha => "captcha",
            Self::LoginWall => "login_wall",
            Self::Block => "block",
        }
    }
}

/// 识别出的拦截页。`vendor` 是厂商名（`cloudflare`、`datadome`……），无法归属厂商时为 `generic`。
#[derive(Debug, Clone, PartialEq)]
pub struct Barrier {
    pub vendor: &'static str,
    pub kind: BarrierKind,
    /// 0 到 1。
    pub confidence: f32,
    /// 决定结果的那条规则（`barrier.rs` 里规则表的 id），或者 `status.401`。
    pub rule_id: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// 拦截页模板专有的标记。
    Marker,
    /// 拦截提示语（"verify you are human"、"请先登录"）。
    Phrase,
    /// 也会出现在正常页面里的组件：验证码控件、密码框、厂商传感器脚本。
    Hint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    /// 原始 HTML（含脚本、属性），按 ASCII 大小写不敏感匹配。
    Html,
    /// 标题加可见文字（含 `<noscript>` 里的提示），按 ASCII 大小写不敏感匹配。
    Text,
}

struct Rule {
    id: &'static str,
    vendor: &'static str,
    kind: BarrierKind,
    signal: Signal,
    field: Field,
    patterns: &'static [&'static str],
}

use BarrierKind::{Block, Captcha, Challenge, LoginWall};
use Field::{Html as H, Text as T};
use Signal::{Hint, Marker, Phrase};

macro_rules! rules {
    ($($id:literal, $vendor:literal, $kind:ident, $signal:ident, $field:ident, [$($p:literal),+ $(,)?];)*) => {
        &[$(Rule { id: $id, vendor: $vendor, kind: $kind, signal: $signal, field: $field, patterns: &[$($p),+] }),*]
    };
}

/// 特征库。新增规则只需加一行：id、厂商、类型、强度、匹配位置、特征串。
static RULES: &[Rule] = rules![
    // Cloudflare
    "cloudflare.chl_opt", "cloudflare", Challenge, Marker, H, ["_cf_chl_opt", "cf_chl_prog"];
    "cloudflare.challenge_platform", "cloudflare", Challenge, Marker, H, ["/cdn-cgi/challenge-platform/h/", "chl_page"];
    "cloudflare.browser_verification", "cloudflare", Challenge, Marker, H, ["cf-browser-verification"];
    "cloudflare.captcha_kind", "cloudflare", Captcha, Marker, H, ["cf_captcha_kind"];
    "cloudflare.title_just_a_moment", "cloudflare", Challenge, Marker, T, ["just a moment..."];
    "cloudflare.attention_required", "cloudflare", Block, Marker, T, ["attention required! | cloudflare"];
    "cloudflare.blocked", "cloudflare", Block, Marker, T, ["sorry, you have been blocked"];
    "cloudflare.error_page", "cloudflare", Block, Hint, H, ["cf-error-details"];
    "cloudflare.footer", "cloudflare", Challenge, Hint, T, ["performance & security by cloudflare", "由 cloudflare 提供的性能和安全服务"];
    // Cloudflare Turnstile
    "turnstile.widget", "turnstile", Captcha, Hint, H, ["cf-turnstile", "challenges.cloudflare.com/turnstile", "cf-chl-widget"];
    // Akamai
    "akamai.edgesuite", "akamai", Block, Marker, H, ["errors.edgesuite.net"];
    "akamai.reference", "akamai", Block, Marker, H, ["reference&#32;&#35;"];
    "akamai.bot_challenge", "akamai", Challenge, Marker, H, ["/_sec/cp_challenge/", "bm-verify"];
    "akamai.footer", "akamai", Challenge, Hint, T, ["powered and protected by akamai"];
    // DataDome
    "datadome.captcha", "datadome", Captcha, Marker, H, ["captcha-delivery.com"];
    "datadome.tag", "datadome", Captcha, Hint, H, ["datadome"];
    // PerimeterX / HUMAN
    "perimeterx.captcha", "perimeterx", Captcha, Marker, H, ["px-captcha", "captcha.px-cdn.net", "captcha.px-cloud.net"];
    "perimeterx.denied", "perimeterx", Captcha, Marker, T, ["access to this page has been denied"];
    "perimeterx.press_hold", "perimeterx", Captcha, Phrase, T, ["press & hold", "press and hold"];
    "perimeterx.sensor", "perimeterx", Captcha, Hint, H, ["_pxappid", "px-cloud.net"];
    // Imperva / Incapsula（含原 Distil）
    "imperva.incident", "imperva", Block, Marker, T, ["incapsula incident id"];
    "imperva.interruption", "imperva", Challenge, Marker, T, ["pardon our interruption"];
    "imperva.resource", "imperva", Challenge, Hint, H, ["_incapsula_resource"];
    // 阿里云 WAF / 滑块验证
    "aliyun.waf_challenge", "aliyun", Challenge, Marker, H, ["aliyun_waf_aa", "aliyun_waf_bb"];
    "aliyun.waf", "aliyun", Block, Marker, H, ["errors.aliyun.com"];
    "aliyun.punish", "aliyun", Captcha, Marker, H, ["x5secdata"];
    "aliyun.slider", "aliyun", Captcha, Hint, H, ["nc_1_n1z", "awsc/awsc.js", "aliyuncaptcha"];
    // 腾讯云 WAF / 天御验证码
    "tencent_cloud.waf", "tencent_cloud", Block, Marker, H, ["waf.tencent-cloud.com", "waf.tencent.com"];
    "tencent_cloud.waf_text", "tencent_cloud", Block, Marker, T, ["已被网站管理员设置为拦截"];
    "tencent_cloud.captcha", "tencent_cloud", Captcha, Hint, H, ["turing.captcha.qcloud.com", "captcha.qq.com", "tencentcaptcha"];
    // AWS WAF、Sucuri、F5、DDoS-Guard
    "aws_waf.challenge", "aws_waf", Challenge, Marker, H, ["gokuprops"];
    "aws_waf.token", "aws_waf", Challenge, Hint, H, ["awswaf.com", "aws-waf-token"];
    "aws_cloudfront.error", "aws_cloudfront", Block, Hint, T, ["generated by cloudfront"];
    "sucuri.firewall", "sucuri", Block, Marker, T, ["sucuri website firewall"];
    "f5.rejected", "f5", Block, Marker, T, ["the requested url was rejected"];
    "ddos_guard.check", "ddos_guard", Challenge, Hint, H, ["ddos-guard"];
    // Google 的异常流量页
    "google.sorry", "google", Captcha, Marker, T, ["our systems have detected unusual traffic"];
    // 验证码控件
    "recaptcha.widget", "recaptcha", Captcha, Hint, H, ["g-recaptcha", "google.com/recaptcha/", "gstatic.com/recaptcha", "recaptcha/api.js"];
    "hcaptcha.widget", "hcaptcha", Captcha, Hint, H, ["h-captcha", "hcaptcha.com/1/api.js", "js.hcaptcha.com"];
    "geetest.widget", "geetest", Captcha, Hint, H, ["static.geetest.com", "gcaptcha4.geetest.com", "initgeetest", "geetest_"];
    // 通用提示语
    "generic.browser_check", "generic", Challenge, Phrase, T, [
        "checking your browser", "checking if the site connection is secure",
        "verifying you are human", "enable javascript and cookies to continue",
        "please wait while we verify", "正在验证您的浏览器", "正在进行安全验证",
    ];
    "generic.captcha", "generic", Captcha, Phrase, T, [
        "verify you are human", "are you a robot", "prove you are human", "not a robot",
        "complete the security check", "unusual traffic", "human verification",
        "人机验证", "安全验证", "滑动验证", "拖动滑块", "访问验证",
    ];
    "generic.denied", "generic", Block, Phrase, T, [
        "access denied", "you have been blocked", "request blocked", "request rejected",
        "403 forbidden", "you don't have permission to access", "your ip has been blocked",
        "访问被拒绝", "您的访问被阻断", "访问已被拦截",
    ];
    // 登录墙
    "login.phrase", "generic", LoginWall, Phrase, T, [
        "sign in to continue", "log in to continue", "login to continue",
        "sign in to view", "log in to view", "please log in", "please sign in",
        "you must be logged in", "you need to log in", "you need to sign in",
        "join to view", "请先登录", "登录后查看", "登录后才能", "ログインしてください",
    ];
    "login.authwall", "linkedin", LoginWall, Hint, H, ["authwall"];
    "login.password_field", "generic", LoginWall, Hint, H, ["type=\"password\"", "type='password'", "type=password"];
];

/// 2xx 等正常状态码下，各类特征生效时可见文字的上限（非空白字符数）。
const MARKER_MAX_CHARS: usize = 2000;
const PHRASE_MAX_CHARS: usize = 1000;
const HINT_MAX_CHARS: usize = 1000;
/// 登录提示语没有密码框佐证时的上限：正常页面里常有"Sign in to view"这类局部入口。
const LOGIN_PHRASE_MAX_CHARS: usize = 400;
/// 状态码表明被拒绝时，上限放宽到这个倍数。
const BLOCKED_STATUS_FACTOR: usize = 4;
/// 原始 HTML 只扫开头和结尾这么多字节。拦截页通常只有几 KB 到几十 KB，
/// 文字少而 HTML 很大的多半是单页应用的外壳；扫全文在 MB 级页面上要几毫秒。
const HTML_SCAN_HEAD: usize = 64 * 1024;
const HTML_SCAN_TAIL: usize = 16 * 1024;
const BLOCKED_STATUS_BONUS: f32 = 0.08;
const EXTRA_HIT_BONUS: f32 = 0.03;

fn base_score(s: Signal) -> f32 {
    match s {
        Marker => 0.9,
        Phrase => 0.6,
        Hint => 0.5,
    }
}

fn max_chars(s: Signal) -> usize {
    match s {
        Marker => MARKER_MAX_CHARS,
        Phrase => PHRASE_MAX_CHARS,
        Hint => HINT_MAX_CHARS,
    }
}

/// 拦截页常用的状态码。404、410 和 5xx 源站错误不算（523 这类 Cloudflare 错误页也带厂商标记）。
/// 可见文字超过这个数的页面不可能是拦截页（所有规则的上限），可以不做识别。
pub(crate) fn visible_limit(status: Option<u16>) -> usize {
    let factor = if status.is_some_and(is_blocked_status) {
        BLOCKED_STATUS_FACTOR
    } else {
        1
    };
    MARKER_MAX_CHARS * factor
}

fn is_blocked_status(s: u16) -> bool {
    matches!(s, 401 | 403 | 405 | 406 | 407 | 412 | 418 | 429 | 451 | 503)
}

struct Matcher {
    ac: AhoCorasick,
    /// 每个模式对应的规则下标。
    rule_of: Vec<usize>,
}

fn matcher(field: Field) -> &'static Matcher {
    static HTML: OnceLock<Matcher> = OnceLock::new();
    static TEXT: OnceLock<Matcher> = OnceLock::new();
    let cell = if field == H { &HTML } else { &TEXT };
    cell.get_or_init(|| {
        let mut patterns = Vec::new();
        let mut rule_of = Vec::new();
        for (i, r) in RULES.iter().enumerate().filter(|(_, r)| r.field == field) {
            for p in r.patterns {
                patterns.push(*p);
                rule_of.push(i);
            }
        }
        let ac = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .kind(Some(AhoCorasickKind::DFA))
            .build(&patterns)
            .expect("barrier patterns are valid");
        Matcher { ac, rule_of }
    })
}

fn mark_hits(field: Field, haystack: &str, hits: &mut [bool]) {
    let m = matcher(field);
    for mat in m.ac.find_overlapping_iter(haystack) {
        hits[m.rule_of[mat.pattern().as_usize()]] = true;
    }
}

/// 收集标题、可见文字和 `<noscript>` 文字，连同可见文字的非空白字符数；
/// 可见文字超过 `limit` 时提前返回 `None`。
fn page_text(dom: &Html<'_>, title: Option<&str>, limit: usize) -> Option<(String, usize)> {
    let mut text = String::from(title.unwrap_or(""));
    let mut visible = 0usize;
    let mut stack: Vec<(NodeR<'_>, bool)> = vec![(dom.root(), false)];
    while let Some((node, in_noscript)) = stack.pop() {
        if let Some(t) = node.as_text() {
            if !in_noscript {
                visible += crate::dom::count_non_whitespace(t);
                if visible > limit {
                    return None;
                }
            }
            text.push(' ');
            text.push_str(t);
            continue;
        }
        let mut noscript = in_noscript;
        if let Some(el) = element(&node) {
            if el.tag() == pagesift_html::Tag::Noscript {
                noscript = true;
            } else if is_non_content_tag(el.tag()) || is_hidden(&node) {
                continue;
            }
        }
        let start = stack.len();
        stack.extend(node.children().map(|c| (c, noscript)));
        stack[start..].reverse();
    }
    Some((text, visible))
}

pub(crate) fn detect(
    dom: &Html<'_>,
    html: &str,
    title: Option<&str>,
    status: Option<u16>,
) -> Option<Barrier> {
    let blocked = status.is_some_and(is_blocked_status);
    let factor = if blocked { BLOCKED_STATUS_FACTOR } else { 1 };
    // 文字量超过所有规则的上限时直接放行，正常页面只走到这里
    let (text, visible) = page_text(dom, title, MARKER_MAX_CHARS * factor)?;

    let mut hits = vec![false; RULES.len()];
    mark_hits(T, &text, &mut hits);
    if html.len() <= HTML_SCAN_HEAD + HTML_SCAN_TAIL {
        mark_hits(H, html, &mut hits);
    } else {
        mark_hits(H, &html[..floor_boundary(html, HTML_SCAN_HEAD)], &mut hits);
        let tail = floor_boundary(html, html.len() - HTML_SCAN_TAIL);
        mark_hits(H, &html[tail..], &mut hits);
    }

    let password = RULES
        .iter()
        .zip(&hits)
        .any(|(r, &h)| h && r.id == "login.password_field");
    let valid_by_size = |r: &Rule| {
        let max = if r.kind == LoginWall && r.signal == Phrase && !password {
            LOGIN_PHRASE_MAX_CHARS
        } else {
            max_chars(r.signal)
        };
        visible <= max * factor
    };
    let phrase = RULES
        .iter()
        .zip(&hits)
        .any(|(r, &h)| h && r.signal == Phrase && valid_by_size(r));
    let valid: Vec<&Rule> = RULES
        .iter()
        .zip(&hits)
        .filter(|(r, h)| **h && valid_by_size(r) && (r.signal != Hint || phrase || blocked))
        .map(|(r, _)| r)
        .collect();

    let Some(mut primary) = valid.iter().copied().max_by(|a, b| {
        base_score(a.signal)
            .total_cmp(&base_score(b.signal))
            .then((a.vendor != "generic").cmp(&(b.vendor != "generic")))
    }) else {
        // 401 本身就是"需要登录"
        return (status == Some(401) && visible <= PHRASE_MAX_CHARS * factor).then_some(Barrier {
            vendor: "generic",
            kind: LoginWall,
            confidence: 0.6,
            rule_id: "status.401",
        });
    };
    let mut score = base_score(primary.signal);
    // 通用提示语配上厂商特征时，归到那个厂商
    if primary.vendor == "generic" {
        let same_family = |k: BarrierKind| (k == LoginWall) == (primary.kind == LoginWall);
        if let Some(r) = valid
            .iter()
            .find(|r| r.vendor != "generic" && same_family(r.kind))
        {
            let kind = primary.kind;
            primary = r;
            if primary.signal == Hint {
                // 证据仍主要来自提示语，类型跟着提示语走
                return Some(finish(
                    primary.vendor,
                    kind,
                    primary.id,
                    score,
                    &valid,
                    blocked,
                ));
            }
            score = base_score(primary.signal);
        }
    }
    Some(finish(
        primary.vendor,
        primary.kind,
        primary.id,
        score,
        &valid,
        blocked,
    ))
}

fn finish(
    vendor: &'static str,
    kind: BarrierKind,
    rule_id: &'static str,
    score: f32,
    valid: &[&Rule],
    blocked: bool,
) -> Barrier {
    let mut confidence = score + EXTRA_HIT_BONUS * (valid.len() - 1) as f32;
    if blocked {
        confidence += BLOCKED_STATUS_BONUS;
    }
    Barrier {
        vendor,
        kind,
        confidence: confidence.min(0.99),
        rule_id,
    }
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
