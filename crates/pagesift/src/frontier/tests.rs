use super::*;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn cleaned(s: &str) -> String {
    let mut x = u(s);
    clean(&mut x);
    x.to_string()
}

#[test]
fn strips_trackers_and_sessions() {
    assert_eq!(
        cleaned("https://a.com/p?utm_source=x&id=7&fbclid=IwAR1&utm_medium=y#top"),
        "https://a.com/p?id=7"
    );
    assert_eq!(
        cleaned("https://a.com/p/4/;jsessionid=ABC123?x=1"),
        "https://a.com/p/4/?x=1"
    );
    assert_eq!(cleaned("https://a.com/p;JSESSIONID=ABC"), "https://a.com/p");
    assert_eq!(
        cleaned("https://a.com/?PHPSESSID=0123456789abcdef0123456789"),
        "https://a.com/"
    );
    assert_eq!(
        cleaned("https://a.com/?sid=8dd3dede98a7626cbf4e1c23217573da"),
        "https://a.com/"
    );
    // 短的 sid、ref、source 可能决定内容，不动
    assert_eq!(
        cleaned("https://a.com/x?sid=3&ref=main&source=rss"),
        "https://a.com/x?sid=3&ref=main&source=rss"
    );
    assert_eq!(cleaned("https://a.com//docs///a"), "https://a.com/docs/a");
    assert_eq!(
        cleaned("https://a.com/a?b=1&a=2"),
        "https://a.com/a?b=1&a=2"
    );
    // 内部链接归因（internal_*）、Eloqua（elqTrackId）也是跟踪参数
    assert_eq!(
        cleaned("https://a.com/x?internal_medium=website&elqTrackId=abc&page=2"),
        "https://a.com/x?page=2"
    );
}

#[test]
fn skips_binaries_and_traps() {
    assert!(skip(&u("https://a.com/media/1.JPG")));
    assert!(skip(&u("https://a.com/files/x.tar.gz")));
    assert!(!skip(&u("https://a.com/doc.pdf")));
    assert!(!skip(&u("https://a.com/v1.2/guide")));
    assert!(!skip(&u("https://a.com/p/5/more/more/")));
    assert!(skip(&u("https://a.com/p/5/more/more/more/")));
    assert!(skip(&u("https://a.com/a/b/a/b/a/b/")));
    assert!(!skip(&u("https://a.com/a/b/a/b/c")));
    let deep = format!(
        "https://a.com/{}",
        (0..25)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join("/")
    );
    assert!(skip(&u(&deep)));
}

#[test]
fn shapes_group_templated_urls() {
    assert_eq!(
        shape(&u("https://a.com/events?month=2026-11")),
        shape(&u("https://a.com/events?month=1999-01"))
    );
    assert_eq!(
        shape(&u("https://a.com/p/12/")),
        shape(&u("https://a.com/p/7/"))
    );
    assert_ne!(
        shape(&u("https://a.com/p/12/")),
        shape(&u("https://a.com/q/12/"))
    );
    assert_eq!(
        shape(&u("https://a.com/list?sort=new&view=grid")),
        shape(&u("https://a.com/list?view=list&sort=az"))
    );
}

#[test]
fn learns_rewrite_rules_from_duplicate_pairs() {
    let mut l = Learner::default();
    let page = |n: usize| {
        format!(
            "Story {n} about {}",
            ["cats", "dogs", "owls", "bees", "rats"][n]
        )
    };
    for n in 0..5 {
        l.observe(&u(&format!("https://a.com/p/{n}/")), &page(n));
    }
    // 一对证据不够
    l.observe(&u("https://a.com/p/0/index.html"), &page(0));
    l.observe(&u("https://a.com/p/0/?print=1"), &page(0));
    l.observe(&u("https://a.com/p/0/?page=1"), &page(0));
    let mut x = u("https://a.com/p/3/index.html?print=1");
    assert!(!l.rewrite(&mut x));
    // 第二对证实后生效
    l.observe(&u("https://a.com/p/1/index.html"), &page(1));
    l.observe(&u("https://a.com/p/1/?print=1"), &page(1));
    l.observe(&u("https://a.com/p/1/?page=1"), &page(1));
    assert!(l.rewrite(&mut x));
    assert_eq!(x.as_str(), "https://a.com/p/3/");
    // 只去掉证实过的取值
    let mut y = u("https://a.com/p/3/?page=2");
    assert!(!l.rewrite(&mut y));
    let mut z = u("https://a.com/p/3/?page=1&q=x");
    assert!(l.rewrite(&mut z));
    assert_eq!(z.as_str(), "https://a.com/p/3/?q=x");
    // 其他主机不受影响
    let mut w = u("https://b.com/p/3/index.html");
    assert!(!l.rewrite(&mut w));
    // 越叠越长的相对链接：规则反复套用
    l.observe(&u("https://a.com/p/2/more/"), &page(2));
    l.observe(&u("https://a.com/p/4/more/"), &page(4));
    let mut m = u("https://a.com/p/9/more/more/index.html");
    assert!(l.rewrite(&mut m));
    assert_eq!(m.as_str(), "https://a.com/p/9/");
    // 反过来先抓到长写法也一样能学
    let mut r = Learner::default();
    r.observe(&u("https://c.com/d/1/index.php"), "one");
    r.observe(&u("https://c.com/d/1/"), "one");
    r.observe(&u("https://c.com/d/2/index.php"), "two");
    r.observe(&u("https://c.com/d/2/"), "two");
    let mut v = u("https://c.com/d/7/index.php");
    assert!(r.rewrite(&mut v));
    assert_eq!(v.as_str(), "https://c.com/d/7/");
}

#[test]
fn hosts_take_turns_and_busy_hosts_are_skipped() {
    let l = Learner::default();
    let mut q = Tiered::default();
    for p in [
        "https://a.com/1",
        "https://a.com/2",
        "https://a.com/3",
        "https://b.com/1",
        "https://c.com/1",
    ] {
        q.push(&l, u(p), 0);
    }
    let order: Vec<String> =
        std::iter::from_fn(|| q.pop(&l, true, |_| false).map(|x| x.0.to_string())).collect();
    assert_eq!(
        order,
        [
            "https://a.com/1",
            "https://b.com/1",
            "https://c.com/1",
            "https://a.com/2",
            "https://a.com/3"
        ]
    );
    for p in ["https://a.com/4", "https://b.com/2"] {
        q.push(&l, u(p), 0);
    }
    assert_eq!(
        q.pop(&l, true, |h| h == "a.com").unwrap().0.as_str(),
        "https://b.com/2"
    );
    assert!(
        q.pop(&l, true, |h| h == "a.com").is_none(),
        "only a busy host is left"
    );
    assert_eq!(q.len(), 1);
    assert_eq!(
        q.pop(&l, true, |_| false).unwrap().0.as_str(),
        "https://a.com/4"
    );
    assert_eq!(q.len(), 0);
}

#[test]
fn learns_traps_from_repeated_content() {
    let mut l = Learner::default();
    let cal = |m: &str| {
        (
            u(&format!("https://a.com/events?month={m}")),
            format!("# Events in {m}\n\nNo events scheduled."),
        )
    };
    for (i, m) in ["2026-10", "2026-11", "2026-12", "2027-01"]
        .iter()
        .enumerate()
    {
        let (url, md) = cal(m);
        assert!(!l.demoted(&url));
        let seen = l.observe(&url, &md);
        assert_eq!(seen == Seen::New, i == 0, "{m}");
    }
    assert!(l.demoted(&cal("2027-02").0));
    // 内容各不相同的模板不降级
    for i in 0..10 {
        let url = u(&format!("https://a.com/p/{i}/"));
        assert_eq!(
            l.observe(
                &url,
                &format!(
                    "Story about topic {}",
                    [
                        "cats", "dogs", "owls", "bees", "rats", "cows", "pigs", "elks", "yaks",
                        "emus"
                    ][i]
                )
            ),
            Seen::New
        );
    }
    assert!(!l.demoted(&u("https://a.com/p/99/")));
    // 全文相同：报告第一次出现的 URL
    assert_eq!(
        l.observe(&u("https://a.com/p/3/?print=1"), "Story about topic bees"),
        Seen::Duplicate("https://a.com/p/3/".into())
    );
}
