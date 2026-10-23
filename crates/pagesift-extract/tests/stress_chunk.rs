//! 极限压力与对抗性样本测试：针对分块引擎在超大输入、病态语法结构下的健壮性与内存稳定性。

use pagesift_extract::{ChunkStrategy, chunk};

#[test]
fn stress_massive_continuous_text_zero_newlines() {
    // 10MB 无换行超长文本，单行密集拉丁单词
    let word = "supercalifragilisticexpialidocious ";
    let repeats = 10_000_000 / word.len();
    let text = word.repeat(repeats);
    assert!(text.len() >= 9_000_000);

    let fixed = chunk(&text, &ChunkStrategy::FixedTokens { size: 250 }).expect("fixed chunks");
    assert!(!fixed.is_empty());
    assert!(fixed.iter().all(|c| c.tokens <= 250));

    let sliding = chunk(
        &text,
        &ChunkStrategy::SlidingWindow {
            window: 250,
            step: 125,
        },
    )
    .expect("sliding chunks");
    assert!(!sliding.is_empty());
    assert!(sliding.len() > fixed.len());
}

#[test]
fn stress_pathological_cjk_dense_punctuation() {
    // 500,000 字无空白纯中日韩长文本，交替混合句末与逗号标点
    let sentence = "这是一段非常冗长的中文段落测试文本，包含各种符号；比如叹号！还有问号？以及各种复杂的词汇。";
    let text = sentence.repeat(10_000);

    let recursive =
        chunk(&text, &ChunkStrategy::Recursive { size: 100 }).expect("recursive chunks");
    assert!(!recursive.is_empty());
    assert!(recursive.iter().all(|c| c.tokens <= 100));

    let sentences = chunk(&text, &ChunkStrategy::Sentences).expect("sentence chunks");
    assert!(sentences.len() >= 10_000);
}

#[test]
fn stress_unclosed_fenced_code_blocks() {
    // 5,000 个未闭合的围栏代码块嵌套与交替
    let mut text = String::with_capacity(500_000);
    for i in 0..5_000 {
        if i % 2 == 0 {
            text.push_str("```rust\nfn test() { println!(\"unclosed\"); }\n");
        } else {
            text.push_str("~~~sh\necho \"another unclosed fence\"\n");
        }
    }
    let res = chunk(&text, &ChunkStrategy::Paragraphs)
        .expect("should handle unclosed fences without panic");
    assert!(!res.is_empty());
}

#[test]
fn stress_massive_gfm_table() {
    // 10,000 行超长 GFM 表格
    let mut table = String::with_capacity(1_000_000);
    table.push_str("| ID | Name | Role | Status |\n| :--- | :---: | :---: | ---: |\n");
    for i in 0..10_000 {
        table.push_str(&format!("| {i} | User_{i} | Engineer | Active |\n"));
    }
    let res = chunk(&table, &ChunkStrategy::Paragraphs).expect("table chunking");
    // 表格作为整块被完整保护
    assert_eq!(res.len(), 1);
    assert!(res[0].text.starts_with("| ID | Name | Role | Status |"));
    assert!(
        res[0]
            .text
            .ends_with("| 9999 | User_9999 | Engineer | Active |")
    );
}

#[test]
fn stress_deep_adversarial_headings() {
    // 5,000 级深度交替 ATX 标题层级
    let mut md = String::new();
    for i in 0..5_000 {
        let level = (i % 6) + 1;
        let hashes = "#".repeat(level);
        md.push_str(&format!("{hashes} Section {i} [Link](https://test.com/section{i})\n\nContent for section {i}.\n\n"));
    }
    let res = chunk(&md, &ChunkStrategy::Headings { max_level: 6 }).expect("headings chunking");
    assert_eq!(res.len(), 5_000);
    assert!(!res[4999].heading_path.is_empty());
}

#[test]
fn stress_pathological_strategy_options() {
    let text = "Sample text for parameter validation";
    assert!(chunk(text, &ChunkStrategy::FixedTokens { size: 0 }).is_err());
    assert!(
        chunk(
            text,
            &ChunkStrategy::SlidingWindow {
                window: 0,
                step: 10
            }
        )
        .is_err()
    );
    assert!(
        chunk(
            text,
            &ChunkStrategy::SlidingWindow {
                window: 10,
                step: 0
            }
        )
        .is_err()
    );
    assert!(
        chunk(
            text,
            &ChunkStrategy::Overlapping {
                window: 10,
                overlap: 10
            }
        )
        .is_err()
    );
    assert!(
        chunk(
            text,
            &ChunkStrategy::Overlapping {
                window: 10,
                overlap: 20
            }
        )
        .is_err()
    );
    assert!(chunk(text, &ChunkStrategy::Headings { max_level: 0 }).is_err());
    assert!(chunk(text, &ChunkStrategy::Headings { max_level: 7 }).is_err());
    assert!(
        chunk(
            text,
            &ChunkStrategy::Regex {
                patterns: vec!["(?=a)(".into()]
            }
        )
        .is_err()
    );
}
