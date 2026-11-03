//! 整页的文字体系一致性：识别模型是多语种的（字符表含汉字、假名、谚文），拉丁文扫描页上的
//! 污点、破折号、手写笔画常被认成零星的东亚文字。整页几乎全是拉丁字母时，这些字去掉；
//! 东亚文字的页、东亚与拉丁混排的页原样保留。

use crate::Line;

/// [策略] 东亚文字占全页字母的比例不超过它时算零星噪声：真正的中日韩文页这一比例接近 1，
/// 中英混排的页（夹几个汉字词）也远高于 5%
const STRAY_SHARE: f64 = 0.05;

/// 汉字、假名、谚文及其标点（Unicode 区块范围）。
fn east_asian(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x30FF   // [协议] CJK 标点、平假名、片假名
        | 0x3400..=0x4DBF // [协议] 汉字扩展 A
        | 0x4E00..=0x9FFF // [协议] 中日韩统一表意文字
        | 0xAC00..=0xD7AF // [协议] 谚文音节
        | 0xF900..=0xFAFF // [协议] 兼容表意文字
        | 0xFF00..=0xFFEF) // [协议] 全角与半角形式
}

/// 和数学符号形似的汉字：数学页上的减号、等号、加号常被认成它们。
fn lookalike(c: char) -> Option<char> {
    match c {
        '一' => Some('-'),
        '二' => Some('='),
        '十' => Some('+'),
        _ => None,
    }
}

/// 拉丁文为主的页上去掉零星的东亚文字（形似符号的换回符号），去完变空的行丢掉。
/// 形似符号的字不计入占比：公式多的页上它们可能很多，但不说明这是中文页。
pub(crate) fn drop_stray_east_asian(lines: &mut Vec<Line>) {
    let (mut ea, mut letters) = (0usize, 0usize);
    for c in lines.iter().flat_map(|l| l.text.chars()) {
        if lookalike(c).is_some() {
            continue;
        }
        if east_asian(c) {
            ea += 1;
            letters += 1;
        } else if c.is_alphabetic() {
            letters += 1;
        }
    }
    if ea as f64 > letters as f64 * STRAY_SHARE
        || !lines.iter().any(|l| l.text.chars().any(east_asian))
    {
        return;
    }
    for l in lines.iter_mut() {
        if l.text.chars().any(east_asian) {
            l.text = l
                .text
                .chars()
                .filter_map(|c| lookalike(c).or((!east_asian(c)).then_some(c)))
                .collect();
        }
    }
    lines.retain(|l| l.text.chars().any(char::is_alphanumeric));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: &str) -> Line {
        Line {
            quad: [(0.0, 0.0); 4],
            text: t.into(),
            score: 1.0,
            formula: false,
        }
    }

    fn texts(l: &[Line]) -> Vec<&str> {
        l.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn 拉丁页去掉零星汉字() {
        let mut l = vec![
            line("God then sends at Head quarters that no doubt"),
            line("中"),
            line("2a-x 一 1 x"),
        ];
        drop_stray_east_asian(&mut l);
        assert_eq!(
            texts(&l),
            [
                "God then sends at Head quarters that no doubt",
                "2a-x - 1 x"
            ]
        );
    }

    #[test]
    fn 中文页与混排页不动() {
        let mut zh = vec![line("扫描页上的文字"), line("第二行 PDF")];
        drop_stray_east_asian(&mut zh);
        assert_eq!(texts(&zh), ["扫描页上的文字", "第二行 PDF"]);
        let mut mixed = vec![line("The word 中文 means Chinese")];
        drop_stray_east_asian(&mut mixed);
        assert_eq!(texts(&mixed), ["The word 中文 means Chinese"]);
    }

    #[test]
    fn 数学页的形似字换回符号() {
        let mut l = vec![line("dx dz =一"), line("1+ 二 7 x 2 十 y 一")];
        drop_stray_east_asian(&mut l);
        assert_eq!(texts(&l), ["dx dz =-", "1+ = 7 x 2 + y -"]);
    }
}
