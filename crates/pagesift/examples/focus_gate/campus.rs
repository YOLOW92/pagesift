//! 合成的大学站点：复现 159 校基线里的失败形态（受众门户、新闻集群、子域、日文门户稀释、
//! 单子树陷阱、错误页、关于/治理集群，外加一个结构良好的对照校）。
//! 每页按 URL 标好真值：U（国际生专属）、M（一般学习/招生）、X（无用）。
//! 页面是确定性生成的，不依赖网络；导航在 `<header>` 里、正文在 `<main>` 里，和真实站点一样
//! 导航链接不在正文提取结果里。

use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// 国际生专属：国际招生、学费、签证、奖学金、国际生支持。
    U,
    /// 一般学习/招生/学位页。
    M,
    /// 门户壳、家具页、新闻、无关页。
    X,
}

pub struct Page {
    pub title: String,
    pub kind: Kind,
    pub status: u16,
    pub paragraphs: Vec<String>,
    /// 正文里的链接（绝对地址，锚文本）。
    pub links: Vec<(String, String)>,
}

pub struct Campus {
    pub name: &'static str,
    pub pattern: &'static str,
    /// 这个站点用的查询（大学站点都用同一个，其他领域各有各的）。
    pub query: &'static str,
    /// 主站主机名（不含端口）。
    pub host: String,
    /// `(主机, 路径) → 页面`。
    pub pages: BTreeMap<(String, String), Page>,
    /// 导航栏（每页都有）：绝对地址和锚文本。
    pub nav: Vec<(String, String)>,
    /// sitemap.xml 里的 URL（顺序就是文件顺序）；None 表示没有 sitemap。
    pub sitemap: Option<Vec<String>>,
    port: u16,
}

const FOOTER: &[(&str, &str)] = &[
    ("/privacy/", "Privacy"),
    ("/accessibility/", "Accessibility"),
    ("/terms/", "Terms of use"),
    ("/contact/", "Contact us"),
    ("/sitemap/", "Site map"),
];

/// 国际生专属页的正文（含查询词）。
fn u_text(topic: &str) -> Vec<String> {
    vec![
        format!("{topic} for international students: who can apply, what documents the admission office needs, and when decisions are sent."),
        "Tuition fees for international students are listed per programme; a deposit confirms your place and the scholarship office reviews funding applications twice a year.".into(),
        "Visa guidance: after admission you receive the confirmation needed for your student visa application; the international office checks your documents before you travel.".into(),
    ]
}

/// 一般招生/学习页的正文。
fn m_text(topic: &str) -> Vec<String> {
    vec![
        format!("{topic}: entry requirements, how the application works, and the deadlines for each intake."),
        "Our degree programmes combine lectures, seminars and project work; students choose modules in their second year and complete a dissertation.".into(),
        "Admission decisions consider grades, references and the personal statement; the admissions team answers questions on open days.".into(),
    ]
}

/// 无用页的正文：新闻、关于、家具页，偶尔也提到 students / international（真实站点就是这样）。
fn x_text(topic: &str, n: usize) -> Vec<String> {
    let mut v = vec![format!(
        "{topic}. The university community gathered this week; staff, alumni and students took part in the programme of talks and tours."
    )];
    if n.is_multiple_of(3) {
        v.push("The project involves international partners and students from several faculties, and the results were presented at the annual showcase.".into());
    } else {
        v.push("Photographs from the day are available from the press office; the next event is announced on the calendar.".into());
    }
    v.push(
        "Published by the communications team. Share this story by email or on social media."
            .into(),
    );
    v
}

impl Campus {
    pub(super) fn new(name: &'static str, pattern: &'static str, port: u16) -> Self {
        Campus {
            name,
            pattern,
            query: "international students admission tuition scholarship visa",
            host: format!("www.{name}.localhost"),
            pages: BTreeMap::new(),
            nav: Vec::new(),
            sitemap: None,
            port,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}:{}{}", self.host, self.port, path)
    }

    pub fn url_on(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.port)
    }

    fn add(
        &mut self,
        host: &str,
        path: &str,
        title: &str,
        kind: Kind,
        paragraphs: Vec<String>,
    ) -> &mut Page {
        self.pages
            .entry((host.to_string(), path.to_string()))
            .or_insert(Page {
                title: title.to_string(),
                kind,
                status: 200,
                paragraphs,
                links: Vec::new(),
            })
    }

    pub(super) fn add_main(
        &mut self,
        path: &str,
        title: &str,
        kind: Kind,
        paragraphs: Vec<String>,
    ) -> &mut Page {
        let host = self.host.clone();
        self.add(&host, path, title, kind, paragraphs)
    }

    pub(super) fn link(&mut self, from: &str, to: &str, anchor: &str) {
        let (host, url) = (self.host.clone(), self.url(to));
        if let Some(p) = self.pages.get_mut(&(host, from.to_string())) {
            p.links.push((url, anchor.to_string()));
        }
    }

    pub(super) fn footer(&mut self) {
        for (path, title) in FOOTER {
            self.add_main(path, title, Kind::X, x_text(title, 1));
        }
        // 站点地图页列出全站：真实站点上它是个"什么都链"的枢纽
        let all: Vec<(String, String)> = self
            .pages
            .iter()
            .filter(|((h, _), _)| *h == self.host)
            .map(|((_, p), page)| (self.url(p), page.title.clone()))
            .collect();
        let host = self.host.clone();
        if let Some(p) = self.pages.get_mut(&(host, "/sitemap/".to_string())) {
            p.links = all;
        }
    }

    fn news(&mut self, count: usize, link_from: &str) {
        for n in 0..count {
            let path = format!("/news/2026/05/story-{n}/");
            let title = match n % 4 {
                0 => format!("International students celebrate at the spring festival ({n})"),
                1 => format!("Research team wins award for new materials ({n})"),
                2 => format!("Alumni return to campus for reunion weekend ({n})"),
                _ => format!("Student volunteers plant trees along the river ({n})"),
            };
            self.add_main(&path, &title, Kind::X, x_text(&title, n));
            self.link(link_from, &path, &title);
            // 新闻页之间互相链接，集群内部循环
            for k in 1..=4 {
                let other = format!("/news/2026/05/story-{}/", (n + k) % count);
                self.link(&path, &other, &format!("Related story {}", (n + k) % count));
            }
        }
        self.add_main("/news/", "News", Kind::X, x_text("News and media", 2));
        for n in 0..count.min(10) {
            self.link("/news/", &format!("/news/2026/05/story-{n}/"), "Read more");
        }
    }

    fn about_cluster(&mut self, link_from: &str) {
        const ABOUT: &[(&str, &str)] = &[
            ("/about/", "About the university"),
            ("/about/history/", "Our history"),
            ("/about/leadership/", "Leadership"),
            ("/about/governance/", "Governance"),
            ("/about/mission/", "Mission and values"),
            ("/about/brand/", "Brand guidelines"),
            ("/about/strategy/", "Strategy 2030"),
            ("/about/campus/", "Campus and buildings"),
            ("/about/sustainability/", "Sustainability"),
            ("/about/facts/", "Facts and figures"),
            ("/about/rankings/", "Rankings"),
            ("/about/partnerships/", "Partnerships"),
            ("/about/offices/", "Offices and services"),
            ("/about/departments/", "Departments"),
            ("/about/events/", "Events"),
        ];
        for (i, (path, title)) in ABOUT.iter().enumerate() {
            self.add_main(path, title, Kind::X, x_text(title, i));
            self.link(link_from, path, title);
            for (other, t) in ABOUT.iter().skip(1).take(6) {
                if other != path {
                    self.link(path, other, t);
                }
            }
        }
    }

    /// 国际生专区：枢纽 + 5 个专属页。
    fn international_hub(&mut self, hub: &str, hub_title: &str) {
        self.add_main(hub, hub_title, Kind::U, u_text("International admissions"));
        const PAGES: &[(&str, &str, &str)] = &[
            (
                "apply/",
                "How to apply as an international student",
                "Applying",
            ),
            (
                "tuition/",
                "Tuition fees for international students",
                "Tuition fees",
            ),
            ("visa/", "Student visa and immigration", "Visa"),
            (
                "scholarships/",
                "International scholarships",
                "Scholarships",
            ),
            (
                "support/",
                "International student support",
                "Support services",
            ),
        ];
        for (sub, title, anchor) in PAGES {
            let path = format!("{hub}{sub}");
            self.add_main(&path, title, Kind::U, u_text(title));
            self.link(hub, &path, anchor);
            self.link(&path, hub, "Back to international admissions");
        }
        // 学费表和申请指南只以 PDF 挂在页面上（真实站点的常态）
        let fees = format!("{hub}files/tuition-fees-2026.pdf");
        self.add_main(&fees, "Tuition fees 2026 (PDF)", Kind::U, Vec::new());
        self.link(
            &format!("{hub}tuition/"),
            &fees,
            "Tuition fee table 2026 (PDF)",
        );
        let guide = format!("{hub}files/international-application-guide.pdf");
        self.add_main(
            &guide,
            "International application guide (PDF)",
            Kind::U,
            Vec::new(),
        );
        self.link(hub, &guide, "Application guide (PDF)");
    }

    /// 一页文字的 PDF（附件夹具）。
    pub fn pdf(title: &str) -> Vec<u8> {
        let content = format!(
            "BT /F1 18 Tf 72 720 Td ({title}) Tj ET BT /F1 11 Tf 72 690 Td (International students: tuition fees, scholarship deadlines and visa documents for admission.) Tj ET"
        );
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        out
    }

    /// 一般招生/学习树：枢纽 + 4 个 M 页。
    fn study_tree(&mut self, hub: &str, hub_title: &str) {
        self.add_main(hub, hub_title, Kind::M, m_text(hub_title));
        const PAGES: &[(&str, &str)] = &[
            ("undergraduate/", "Undergraduate study"),
            ("graduate/", "Graduate study"),
            ("apply/", "How to apply"),
            ("degrees/", "Degree programmes"),
        ];
        for (sub, title) in PAGES {
            let path = format!("{hub}{sub}");
            self.add_main(&path, title, Kind::M, m_text(title));
            self.link(hub, &path, title);
        }
    }

    pub(super) fn home(&mut self, title: &str, paragraphs: Vec<String>) {
        self.add_main("/", title, Kind::X, paragraphs);
    }

    pub(super) fn nav_main(&mut self, items: &[(&str, &str)]) {
        self.nav = items
            .iter()
            .map(|(p, t)| (self.url(p), t.to_string()))
            .collect();
    }

    /// 受众门户（美式大校）：首页是 prospective/current/alumni 分流页，国际内容在 prospective 下两层。
    fn portal(port: u16) -> Self {
        let mut c = Campus::new("portal", "audience portal", port);
        c.home(
            "Portal University",
            x_text("Welcome to Portal University", 1),
        );
        c.nav_main(&[
            ("/prospective/", "Prospective students"),
            ("/current/", "Current students"),
            ("/alumni/", "Alumni and friends"),
            ("/research/", "Research"),
            ("/news/", "News"),
            ("/about/", "About"),
            ("/giving/", "Giving"),
        ]);
        for (p, t) in [
            ("/prospective/", "Prospective students"),
            ("/current/", "Current students"),
            ("/alumni/", "Alumni and friends"),
            ("/research/", "Research"),
            ("/giving/", "Giving"),
        ] {
            c.add_main(p, t, Kind::X, x_text(t, 2));
            c.link("/", p, t);
        }
        c.news(12, "/");
        c.about_cluster("/about/");
        c.link("/", "/about/", "About");
        c.study_tree("/prospective/undergraduate/", "Undergraduate admissions");
        c.link(
            "/prospective/",
            "/prospective/undergraduate/",
            "Undergraduate",
        );
        c.add_main(
            "/prospective/graduate/",
            "Graduate admissions",
            Kind::M,
            m_text("Graduate admissions"),
        );
        c.link("/prospective/", "/prospective/graduate/", "Graduate");
        c.add_main(
            "/prospective/visit/",
            "Visit campus",
            Kind::X,
            x_text("Visit campus", 2),
        );
        c.link("/prospective/", "/prospective/visit/", "Visit");
        c.international_hub("/prospective/international/", "International admissions");
        c.link(
            "/prospective/",
            "/prospective/international/",
            "International",
        );
        c.footer();
        c
    }

    /// 新闻集群劫持：首页 30 条新鲜新闻占满，国际招生只在导航的 Admissions 下；sitemap 新闻在前。
    fn news_hijack(port: u16) -> Self {
        let mut c = Campus::new("newsy", "news cluster hijack", port);
        c.home("Newsy University", x_text("Newsy University home", 1));
        c.nav_main(&[
            ("/admissions/", "Admissions"),
            ("/study/", "Study"),
            ("/research/", "Research"),
            ("/news/", "News"),
            ("/about/", "About"),
        ]);
        c.news(30, "/");
        c.add_main("/research/", "Research", Kind::X, x_text("Research", 2));
        c.about_cluster("/about/");
        c.study_tree("/admissions/", "Admissions");
        c.study_tree("/study/", "Study with us");
        c.international_hub("/admissions/international/", "International admissions");
        c.link(
            "/admissions/",
            "/admissions/international/",
            "International applicants",
        );
        c.footer();
        let mut sm: Vec<String> = (0..30)
            .map(|n| c.url(&format!("/news/2026/05/story-{n}/")))
            .collect();
        sm.extend(
            c.pages
                .keys()
                .filter(|(h, p)| *h == c.host && !p.starts_with("/news/"))
                .map(|(_, p)| c.url(p)),
        );
        c.sitemap = Some(sm);
        c
    }

    /// 子域：国际生服务在 iss.<校>，主站没有国际内容；导航有 "International Student Services"。
    fn subdomain(port: u16) -> Self {
        let mut c = Campus::new("subdom", "services on a subdomain", port);
        let iss = format!("iss.{}", c.host.trim_start_matches("www."));
        c.home("Subdom University", x_text("Subdom University home", 1));
        c.nav = vec![
            (c.url("/admissions/"), "Admissions".into()),
            (c.url("/study/"), "Study".into()),
            (c.url_on(&iss, "/"), "International Student Services".into()),
            (c.url("/news/"), "News".into()),
            (c.url("/about/"), "About".into()),
        ];
        c.news(12, "/");
        c.about_cluster("/about/");
        c.study_tree("/admissions/", "Admissions");
        c.study_tree("/study/", "Study");
        const ISS: &[(&str, &str)] = &[
            ("/", "International Student Services"),
            (
                "/visa/",
                "Visa and immigration advice for international students",
            ),
            ("/tuition/", "Tuition and fees for international students"),
            ("/scholarships/", "Scholarships for international students"),
            ("/orientation/", "International student orientation"),
            ("/insurance/", "Health insurance for international students"),
            ("/work/", "Working while you study"),
            ("/housing/", "Housing for international students"),
        ];
        for (p, t) in ISS {
            c.add(&iss, p, t, Kind::U, u_text(t));
        }
        for (p, t) in ISS.iter().skip(1) {
            let (from, to) = ((iss.clone(), "/".to_string()), c.url_on(&iss, p));
            c.pages
                .get_mut(&from)
                .unwrap()
                .links
                .push((to, t.to_string()));
        }
        c.footer();
        c
    }

    /// 日文门户稀释：首页日文，英文区在 /top/en/，入学中心在 /inst/admission/en/，家具页一半。
    fn jp_portal(port: u16) -> Self {
        let mut c = Campus::new("nihon", "non-English portal", port);
        c.home(
            "日本大学",
            vec![
                "日本大学のウェブサイトへようこそ。学部・大学院の入学案内、留学生の皆さんへの情報、キャンパスニュース。".into(),
                "在学生の方、卒業生の方、企業の方への情報はそれぞれのページをご覧ください。".into(),
            ],
        );
        c.nav_main(&[
            ("/about/", "大学について"),
            ("/admission/", "入学案内"),
            ("/campus/", "キャンパスライフ"),
            ("/news/", "ニュース"),
            ("/top/en/", "English"),
        ]);
        for (p, t) in [
            ("/about/", "大学について"),
            ("/admission/", "入学案内"),
            ("/campus/", "キャンパスライフ"),
        ] {
            c.add_main(
                p,
                t,
                Kind::X,
                vec![format!(
                    "{t}のページです。学生生活、留学生、入学試験の情報。"
                )],
            );
            c.link("/", p, t);
        }
        c.news(10, "/");
        c.add_main(
            "/top/en/",
            "Nihon University (English)",
            Kind::X,
            x_text("English portal", 2),
        );
        for (p, t) in [
            ("/top/en/about/", "About the university"),
            ("/top/en/contact/", "Contact"),
            ("/top/en/access/", "Access and maps"),
            ("/top/en/sitemap/", "Sitemap (English)"),
            ("/top/en/search/", "Search"),
            ("/top/en/news/", "News (English)"),
            ("/top/en/faq/", "FAQ"),
        ] {
            c.add_main(p, t, Kind::X, x_text(t, 2));
            c.link("/top/en/", p, t);
        }
        c.link("/top/en/", "/inst/admission/en/", "Admissions Center");
        c.add_main(
            "/inst/admission/en/",
            "Admissions Center",
            Kind::M,
            m_text("Admissions Center"),
        );
        for (p, t) in [
            (
                "/inst/admission/en/contact/",
                "Contact the admissions center",
            ),
            ("/inst/admission/en/access/", "Access"),
            ("/inst/admission/en/sitemap/", "Sitemap"),
            ("/inst/admission/en/events/", "Open campus events"),
        ] {
            c.add_main(p, t, Kind::X, x_text(t, 2));
            c.link("/inst/admission/en/", p, t);
        }
        c.international_hub(
            "/inst/admission/en/international/",
            "International admissions office",
        );
        c.link(
            "/inst/admission/en/",
            "/inst/admission/en/international/",
            "International students",
        );
        c.footer();
        c
    }

    /// 单子树陷阱：展览微站 40 页互链，正文里反复提到 students、international。
    fn trap(port: u16) -> Self {
        let mut c = Campus::new("trap", "single-subtree trap", port);
        c.home("Trap University", x_text("Trap University home", 1));
        c.nav_main(&[
            ("/study/", "Study"),
            ("/exhibition/", "Exhibition: 100 years"),
            ("/news/", "News"),
            ("/about/", "About"),
        ]);
        c.link(
            "/",
            "/exhibition/",
            "Exhibition: 100 years of international students",
        );
        c.add_main(
            "/exhibition/",
            "Exhibition: 100 years of international students",
            Kind::X,
            x_text("The exhibition", 3),
        );
        for n in 0..40 {
            let path = format!("/exhibition/item-{n}/");
            let title = format!(
                "Exhibit {n}: international students in the {}s",
                1920 + 2 * n
            );
            c.add_main(&path, &title, Kind::X, x_text(&title, n));
            c.link("/exhibition/", &path, &title);
            for k in 1..=3 {
                let other = format!("/exhibition/item-{}/", (n + k) % 40);
                c.link(&path, &other, &format!("Next exhibit {}", (n + k) % 40));
            }
        }
        c.news(8, "/");
        c.about_cluster("/about/");
        c.study_tree("/study/", "Study");
        c.international_hub("/study/international/", "International students");
        c.link("/study/", "/study/international/", "International students");
        c.footer();
        c
    }

    /// 错误页：首页链接很多 404 模板页（模板里导航含查询词）和 .well-known 路径。
    fn errors(port: u16) -> Self {
        let mut c = Campus::new("broken", "error pages", port);
        c.home("Broken University", x_text("Broken University home", 1));
        c.nav_main(&[
            ("/admissions/", "Admissions"),
            ("/news/", "News"),
            ("/about/", "About"),
        ]);
        for n in 0..20 {
            let path = format!("/programs/old-{n}/");
            let p = c.add_main(&path, "Page not found", Kind::X, vec![
                "The page you requested could not be found. It may have moved; use the search or the links below.".into(),
                "Prospective students: see admissions. International students: see the international office.".into(),
            ]);
            p.status = 404;
            c.link("/", &path, &format!("Programme {n} (archived)"));
        }
        let p = c.add_main(
            "/.well-known/openid-configuration",
            "Not found",
            Kind::X,
            vec!["Not found.".into()],
        );
        p.status = 404;
        c.link("/", "/.well-known/openid-configuration", "OpenID");
        c.news(6, "/");
        c.about_cluster("/about/");
        c.study_tree("/admissions/", "Admissions");
        c.international_hub("/admissions/international/", "International admissions");
        c.link(
            "/admissions/",
            "/admissions/international/",
            "International",
        );
        c.footer();
        c
    }

    /// 关于/治理集群：首页正文是 15 个 about 链接；招生只在导航里。
    fn governance(port: u16) -> Self {
        let mut c = Campus::new("gov", "about/governance cluster", port);
        c.home("Gov University", x_text("Gov University home", 1));
        c.nav_main(&[
            ("/study/", "Study"),
            ("/about/", "About"),
            ("/news/", "News"),
        ]);
        c.about_cluster("/");
        c.news(6, "/news/");
        c.study_tree("/study/", "Study");
        c.international_hub("/study/international/", "International students");
        c.link("/study/", "/study/international/", "International");
        c.footer();
        c
    }

    /// 结构良好（奥塔哥型）：导航和首页都直达 /international/ 专区。
    fn good(port: u16) -> Self {
        let mut c = Campus::new("good", "direct international section", port);
        c.home("Good University", x_text("Good University home", 1));
        c.nav_main(&[
            ("/study/", "Study"),
            ("/international/", "International"),
            ("/research/", "Research"),
            ("/news/", "News"),
            ("/about/", "About"),
        ]);
        c.link(
            "/",
            "/international/",
            "Information for international students",
        );
        c.add_main("/research/", "Research", Kind::X, x_text("Research", 2));
        c.news(8, "/");
        c.about_cluster("/about/");
        c.study_tree("/study/", "Study");
        c.international_hub("/international/", "International students");
        for (sub, title) in [
            ("english-requirements/", "English language requirements"),
            ("accommodation/", "Accommodation for international students"),
            ("arrival/", "Arriving and orientation"),
            ("fees-payment/", "Paying tuition fees from overseas"),
        ] {
            let path = format!("/international/{sub}");
            c.add_main(&path, title, Kind::U, u_text(title));
            c.link("/international/", &path, title);
        }
        c.footer();
        c
    }

    /// 8 所大学加上两个别的领域的站点（见 `other.rs`）。
    pub fn all(port: u16) -> Vec<Campus> {
        let mut v = vec![
            Campus::portal(port),
            Campus::news_hijack(port),
            Campus::subdomain(port),
            Campus::jp_portal(port),
            Campus::trap(port),
            Campus::errors(port),
            Campus::governance(port),
            Campus::good(port),
        ];
        v.extend(super::other::all(port));
        v
    }

    /// 页面的 HTML（导航在 header，正文在 main，页脚在 footer）。
    pub fn render(&self, page: &Page) -> String {
        let mut s = format!(
            "<!doctype html><html><head><title>{}</title></head><body><header><nav><ul>",
            page.title
        );
        for (href, text) in &self.nav {
            let _ = write!(s, "<li><a href=\"{href}\">{text}</a></li>");
        }
        s.push_str("</ul></nav></header><main><article>");
        let _ = write!(s, "<h1>{}</h1>", page.title);
        for p in &page.paragraphs {
            let _ = write!(s, "<p>{p}</p>");
        }
        if !page.links.is_empty() {
            s.push_str("<ul>");
            for (href, text) in &page.links {
                let _ = write!(s, "<li><a href=\"{href}\">{text}</a></li>");
            }
            s.push_str("</ul>");
        }
        s.push_str("</article></main><footer><ul>");
        for (path, text) in FOOTER {
            let _ = write!(s, "<li><a href=\"{}\">{text}</a></li>", self.url(path));
        }
        s.push_str("</ul></footer></body></html>");
        s
    }

    /// 真值：按 URL 查这一页的类别（主机不属于本校时 None）。
    pub fn kind_of(&self, url: &str) -> Option<Kind> {
        let u = pagesift_net::Url::parse(url).ok()?;
        let host = u.host_str()?.to_string();
        self.pages
            .get(&(host, u.path().to_string()))
            .map(|p| p.kind)
    }
}
