//! 正文完整度：结果可能只有正文的一部分时说出来（只给警告，不改提取结果）。
//!
//! 两种"悄悄丢正文"在 WCXB 开发集上能从页面自身的数字里看出来（tasks/0.80/c/RESULT.md）：
//! - 文章模式选错了区域：输出远少于默认规则下页面上评论区以外的非链接文字；
//! - 讨论串只取到一帖：选中的主体本身在评论（回复）区里，同一区域的其余文字比输出多得多。
//!
//! 页面整体文字不够多时一次额外遍历都不做，大多数页面只多数一遍 Markdown。

use pagesift_html::NodeId;

use crate::dom::NodeR;
use crate::extract::Skipper;

/// 输出不到默认规则正文的这个比例才算"只选中了一部分"。
/// WCXB 开发集上按"召回最大、精度不低于 0.9"在 0.2–0.5 的候选里选定（0.3：13 页全是真丢）。
const PARTIAL_MAX_SHARE: f64 = 0.3;
/// 并且少掉的文字至少这么多（约两段），几百字的短页面差几句不报。
const PARTIAL_MIN_MISSING: usize = 500;
/// 选中的主体在回复区里时，回复区的文字超过输出这么多倍、并且至少这么多字，才报其余回复没取。
const REPLIES_MIN_RATIO: usize = 2;
const REPLIES_MIN_CHARS: usize = 1000;

/// 完整度的中间量，字数都是非空白字符数。
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// 输出 Markdown 的可见文字（链接和图片的地址不算）。只数到判断够用为止。
    out_chars: usize,
    /// 默认规则（非文章模式）下评论区以外、不在链接里的文字和评论区里的文字；
    /// 页面整体文字不够多、不可能触发警告时不算。
    main_chars: Option<usize>,
    comment_chars: Option<usize>,
    /// 文章模式选中的主体在评论（回复）区里。
    root_in_comments: bool,
}

impl Coverage {
    /// 结果可能不完整时的警告：`partial_content`、`replies_omitted`。
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let got = self.out_chars;
        if let Some(main) = self.main_chars
            && (got as f64) < main as f64 * PARTIAL_MAX_SHARE
            && main - got >= PARTIAL_MIN_MISSING
        {
            out.push(format!(
                "partial_content: extracted {got} characters, but the page has about {main} characters of non-link text outside comments; only part of the main content may have been selected"
            ));
        }
        if self.root_in_comments
            && let Some(replies) = self.comment_chars
            && replies > REPLIES_MIN_RATIO * got
            && replies >= REPLIES_MIN_CHARS
        {
            out.push(format!(
                "replies_omitted: the extracted text sits in a comment or reply thread that holds about {replies} characters; the other replies were left out"
            ));
        }
        out
    }
}

/// 文章模式选中的主体是不是在评论（回复）区里。`<body>`、`<html>` 上的类名
/// （WordPress 的 `comments-open` 这类）说的是整页，不算。
pub(crate) fn in_comments(nodes: &[NodeR<'_>], skipper: &Skipper, body: NodeId) -> bool {
    nodes.iter().any(|n| {
        std::iter::once(*n)
            .chain(n.ancestors())
            .take_while(|a| a.id() != body && a.parent().is_some())
            .any(|a| skipper.is_comments(&a))
    })
}

/// 提取结束时算完整度。只取正文时才有意义；`root_in_comments` 来自文章模式。
pub(crate) fn measure(
    body: &NodeR<'_>,
    skipper: &Skipper,
    markdown: &str,
    root_in_comments: bool,
) -> Coverage {
    // 默认规则下的文字不会多于整页非链接文字：整页都不到输出的两倍、或者本来就很少时，
    // 两条警告都不可能成立，不用再遍历
    let page = skipper.content_chars(body);
    let limit = page / REPLIES_MIN_RATIO;
    let got = text_chars_up_to(markdown, limit);
    let mut cov = Coverage {
        out_chars: got,
        root_in_comments,
        ..Default::default()
    };
    if skipper.only_main() && got < limit && page >= PARTIAL_MIN_MISSING {
        let (main, comments) = default_prose(body, skipper);
        cov.main_chars = Some(main);
        cov.comment_chars = Some(comments);
    }
    cov
}

/// Markdown 里可见文字的非空白字符数（链接和图片的地址 `](...)` 不算），数到 `limit` 为止。
fn text_chars_up_to(md: &str, limit: usize) -> usize {
    let mut n = 0;
    let mut rest = md;
    while n < limit {
        let Some(i) = rest.find(']') else {
            return n + crate::dom::count_non_whitespace(rest);
        };
        n += crate::dom::count_non_whitespace(&rest[..i]);
        rest = &rest[i + 1..];
        if rest.starts_with('(') {
            rest = rest.find(')').map_or("", |j| &rest[j + 1..]);
        }
    }
    n
}

/// 按当前跳过规则数子树里不在链接里的文字：（评论区以外的，评论区里的）。
fn default_prose(node: &NodeR<'_>, skipper: &Skipper) -> (usize, usize) {
    fn walk(
        n: &NodeR<'_>,
        skipper: &Skipper,
        in_link: bool,
        comments: bool,
        acc: &mut (usize, usize),
    ) {
        for c in n.children() {
            if let Some(t) = c.as_text() {
                if !in_link {
                    let k = crate::dom::count_non_whitespace(t);
                    if comments {
                        acc.1 += k;
                    } else {
                        acc.0 += k;
                    }
                }
            } else if c.is_element() && !skipper.skip(&c) {
                let link = in_link || c.tag() == Some(pagesift_html::Tag::A);
                walk(&c, skipper, link, comments || skipper.is_comments(&c), acc);
            }
        }
    }
    let mut acc = (0, 0);
    walk(node, skipper, false, false, &mut acc);
    acc
}
