use pagesift_diff::*;
use proptest::prelude::*;

fn doc() -> impl Strategy<Value = String> {
    let line = prop_oneof![
        "[a-z ]{0,20}",
        "# [a-z]{1,8}",
        "- [a-z0-9 ]{1,12}",
        "\\| [a-z0-9]{1,5} \\| [0-9]{1,4} \\|",
        "[\u{4e00}-\u{4e20}]{1,10}",
        "\\$[0-9]{1,4}\\.[0-9]{2} now",
        "",
    ];
    prop::collection::vec(line, 0..30).prop_map(|v| v.join("\n"))
}

proptest! {
    #[test]
    fn self_diff_is_unchanged(a in doc()) {
        let r = diff(&a, &a);
        prop_assert!(!r.changed && !r.substantive && r.blocks.is_empty());
    }

    #[test]
    fn arbitrary_text_self_diff(a in "\\PC{0,200}") {
        prop_assert!(!diff(&a, &a).changed);
    }

    #[test]
    fn stats_are_consistent(a in doc(), b in doc()) {
        let r = diff(&a, &b);
        let s = &r.stats;
        prop_assert_eq!(s.unchanged + s.removed + s.modified + s.moved, s.old_blocks);
        prop_assert_eq!(s.unchanged + s.added + s.modified + s.moved, s.new_blocks);
        prop_assert_eq!(s.noise + s.substantive, r.blocks.len());
        prop_assert_eq!(r.changed, !r.blocks.is_empty());
    }

    #[test]
    fn symmetric_counts(a in doc(), b in doc()) {
        let (x, y) = (diff(&a, &b), diff(&b, &a));
        prop_assert_eq!(x.changed, y.changed);
        prop_assert_eq!(x.stats.moved, y.stats.moved);
    }
}
