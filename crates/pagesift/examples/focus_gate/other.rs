//! 非大学领域的合成站点，查询也不同：证明打分规则不是只对大学官网和那一个查询有效。
//! 同一套 [`Campus`] 模型（U = 查询要找的页、M = 相邻有用页、X = 无用页），用各自领域的词。
//!
//! - `devdocs`：SaaS 开发者门户，查询 "api authentication rate limit webhook"。文档在 /docs/api/ 下；
//!   博客、更新日志、社区帖子的标题也满是 API、webhook（真实的陷阱）。
//! - `cityhall`：市政服务站，查询 "passport renewal fee appointment"。办事页在 /services/passports/ 下；
//!   新闻、议会纪要按日期分目录，政务集群互相链接。

use super::campus::{Campus, Kind};

/// 目标页正文：标题 + 查询词都出现的两句。
fn target(topic: &str, terms: &[&str; 4]) -> Vec<String> {
    vec![
        format!("{topic}: what you need, how long it takes and what it costs."),
        format!(
            "This page covers {} and {}: {} is explained step by step, and the {} rules are listed at the end.",
            terms[0], terms[1], terms[2], terms[3]
        ),
    ]
}

/// 相邻有用页正文：同一栏目，少量查询词。
fn near(topic: &str, term: &str) -> Vec<String> {
    vec![
        format!("{topic}: an overview of the options and the steps involved."),
        format!(
            "Related pages describe {term} in more detail; most readers start here and follow the links in order."
        ),
    ]
}

/// 无用页正文：偶尔带一个查询词（博客标题、新闻稿常这样）。
fn noise(topic: &str, n: usize, term: &str) -> Vec<String> {
    let mut v = vec![format!(
        "{topic}. Posted by the team; thanks to everyone who joined the discussion this week."
    )];
    if n.is_multiple_of(3) {
        v.push(format!(
            "The post mentions {term} in passing, with a photo and a link to the recording."
        ));
    } else {
        v.push("Read on for the full story, and subscribe to get the next one by email.".into());
    }
    v
}

fn devdocs(port: u16) -> Campus {
    let mut c = Campus::new("devdocs", "SaaS docs: blog/changelog trap", port);
    c.query = "api authentication rate limit webhook";
    let terms = [
        "api authentication",
        "rate limits",
        "the webhook flow",
        "rate limit",
    ];
    c.home(
        "Acme Platform",
        noise("Build faster with the Acme platform", 1, "api"),
    );
    c.nav_main(&[
        ("/product/", "Product"),
        ("/pricing/", "Pricing"),
        ("/docs/", "Docs"),
        ("/blog/", "Blog"),
        ("/community/", "Community"),
        ("/changelog/", "Changelog"),
        ("/login/", "Log in"),
    ]);
    for (p, t) in [
        ("/product/", "Product tour"),
        ("/pricing/", "Pricing and plans"),
        ("/customers/", "Customer stories"),
        ("/careers/", "Careers at Acme"),
        ("/login/", "Log in"),
    ] {
        c.add_main(p, t, Kind::X, noise(t, 2, "api"));
    }
    c.link("/", "/customers/", "Customer stories");
    c.link("/", "/product/", "See the product");
    // 文档树：指南是 M，API 参考是 U
    c.add_main(
        "/docs/",
        "Documentation",
        Kind::M,
        near("Documentation", "the API"),
    );
    for (sub, t) in [
        ("getting-started/", "Getting started"),
        ("sdks/", "SDKs and client libraries"),
        ("cli/", "Command-line tool"),
        ("deploy/", "Deploying your app"),
    ] {
        let p = format!("/docs/{sub}");
        c.add_main(&p, t, Kind::M, near(t, "the API"));
        c.link("/docs/", &p, t);
    }
    c.add_main(
        "/docs/api/",
        "API reference",
        Kind::U,
        target("API reference", &terms),
    );
    c.link("/docs/", "/docs/api/", "API reference");
    for (sub, t, a) in [
        ("authentication/", "API authentication", "Authentication"),
        ("rate-limits/", "Rate limits", "Rate limits"),
        ("webhooks/", "Webhooks", "Webhooks"),
        ("errors/", "Error codes", "Errors"),
        ("pagination/", "Pagination", "Pagination"),
    ] {
        let p = format!("/docs/api/{sub}");
        c.add_main(&p, t, Kind::U, target(t, &terms));
        c.link("/docs/api/", &p, a);
        c.link(&p, "/docs/api/", "Back to the API reference");
    }
    // 博客：按日期分目录，标题里全是 API、webhook
    c.add_main(
        "/blog/",
        "Blog",
        Kind::X,
        noise("Engineering blog", 2, "webhook"),
    );
    for n in 0..20 {
        let p = format!("/blog/2026/04/post-{n}/");
        let t = match n % 4 {
            0 => format!("How we rebuilt our webhook pipeline ({n})"),
            1 => format!("API rate limiting at scale: lessons learned ({n})"),
            2 => format!("Meet the team: platform engineering ({n})"),
            _ => format!("Authentication outage post-mortem ({n})"),
        };
        c.add_main(&p, &t, Kind::X, noise(&t, n, "api"));
        c.link("/blog/", &p, &t);
        c.link("/", &p, &t);
        for k in 1..=3 {
            let o = format!("/blog/2026/04/post-{}/", (n + k) % 20);
            c.link(&p, &o, "Related post");
        }
    }
    // 更新日志：一个页面列 30 条
    c.add_main(
        "/changelog/",
        "Changelog",
        Kind::X,
        noise("Changelog", 1, "api"),
    );
    for n in 0..30 {
        let p = format!("/changelog/2026-05-{:02}/", n + 1);
        let t = format!("API v2.{n} released: webhook retries and rate limit headers");
        c.add_main(&p, &t, Kind::X, noise(&t, n, "webhook"));
        c.link("/changelog/", &p, &t);
    }
    // 社区帖子：标题像文档，内容是问答
    c.add_main(
        "/community/",
        "Community forum",
        Kind::X,
        noise("Community", 2, "api"),
    );
    for n in 0..20 {
        let p = format!("/community/t/{}/", 1000 + n);
        let t = match n % 3 {
            0 => format!("Webhook not firing after upgrade? ({n})"),
            1 => format!("Rate limit 429 on the batch API ({n})"),
            _ => format!("Show and tell: my dashboard ({n})"),
        };
        c.add_main(&p, &t, Kind::X, noise(&t, n, "rate limit"));
        c.link("/community/", &p, &t);
    }
    c.footer();
    c
}

fn cityhall(port: u16) -> Campus {
    let mut c = Campus::new("cityhall", "city services: news/minutes clusters", port);
    c.query = "passport renewal fee appointment";
    let terms = ["passport renewal", "fees", "booking an appointment", "fee"];
    c.home(
        "City of Riverton",
        noise("Welcome to the City of Riverton", 1, "fee"),
    );
    c.nav_main(&[
        ("/residents/", "Residents"),
        ("/business/", "Business"),
        ("/government/", "Government"),
        ("/services/", "Services"),
        ("/news/", "News"),
        ("/events/", "Events"),
    ]);
    for (p, t) in [
        ("/residents/", "Residents"),
        ("/business/", "Business"),
        ("/events/", "Events calendar"),
    ] {
        c.add_main(p, t, Kind::X, noise(t, 2, "fee"));
    }
    // 服务目录：其他服务是 M，护照办事是 U
    c.add_main(
        "/services/",
        "City services",
        Kind::M,
        near("City services", "fees"),
    );
    for (sub, t) in [
        ("parking/", "Parking permits"),
        ("permits/", "Building permits"),
        ("taxes/", "Property taxes"),
        ("library/", "Library cards"),
        ("waste/", "Waste collection"),
    ] {
        let p = format!("/services/{sub}");
        c.add_main(&p, t, Kind::M, near(t, "fees"));
        c.link("/services/", &p, t);
    }
    c.add_main(
        "/services/passports/",
        "Passport services",
        Kind::U,
        target("Passport services", &terms),
    );
    c.link("/services/", "/services/passports/", "Passports");
    for (sub, t, a) in [
        ("renewal/", "Renew your passport", "Renewal"),
        ("fees/", "Passport fees", "Fees"),
        (
            "appointments/",
            "Book a passport appointment",
            "Appointments",
        ),
        ("documents/", "Documents you need", "Documents"),
        ("processing-times/", "Processing times", "Processing times"),
    ] {
        let p = format!("/services/passports/{sub}");
        c.add_main(&p, t, Kind::U, target(t, &terms));
        c.link("/services/passports/", &p, a);
    }
    let form = "/services/passports/files/renewal-form.pdf";
    c.add_main(form, "Passport renewal form (PDF)", Kind::U, Vec::new());
    c.link("/services/passports/renewal/", form, "Renewal form (PDF)");
    // 新闻稿和议会纪要：按日期分目录的集群
    c.add_main(
        "/news/",
        "News releases",
        Kind::X,
        noise("News releases", 2, "fee"),
    );
    for n in 0..20 {
        let p = format!("/news/2026/05/release-{n}/");
        let t = match n % 4 {
            0 => format!("Passport office extends appointment hours ({n})"),
            1 => format!("Council approves the new fee schedule ({n})"),
            2 => format!("Summer festival returns to the riverfront ({n})"),
            _ => format!("Road works on Main Street next week ({n})"),
        };
        c.add_main(&p, &t, Kind::X, noise(&t, n, "appointment"));
        c.link("/news/", &p, &t);
        c.link("/", &p, &t);
    }
    c.add_main(
        "/government/",
        "Government",
        Kind::X,
        noise("City government", 1, "fee"),
    );
    for (sub, t) in [
        ("council/", "City council"),
        ("mayor/", "Office of the mayor"),
        ("departments/", "Departments"),
        ("budget/", "Budget and finance"),
        ("elections/", "Elections"),
    ] {
        let p = format!("/government/{sub}");
        c.add_main(&p, t, Kind::X, noise(t, 2, "fee"));
        c.link("/government/", &p, t);
        c.link("/", &p, t);
    }
    for n in 0..15 {
        let p = format!("/government/council/minutes/2026/{:02}/", n + 1);
        let t = format!("Council meeting minutes, session {n}");
        c.add_main(&p, &t, Kind::X, noise(&t, n, "fee"));
        c.link("/government/council/", &p, &t);
    }
    c.footer();
    c
}

pub fn all(port: u16) -> Vec<Campus> {
    vec![devdocs(port), cityhall(port)]
}
