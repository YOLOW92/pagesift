//! 拦截页识别：每类厂商一个手写的最小样本，以及"正常页面带验证码组件或登录按钮"的反例。

use pagesift_content::{Barrier, BarrierKind, Options, process};

fn detect(html: &str, status: Option<u16>) -> Option<Barrier> {
    process(
        html,
        &Options {
            status,
            ..Default::default()
        },
    )
    .barrier
}

fn expect(html: &str, status: Option<u16>, vendor: &str, kind: BarrierKind) {
    let b = detect(html, status).unwrap_or_else(|| panic!("no barrier for:\n{html}"));
    assert_eq!((b.vendor, b.kind), (vendor, kind), "{b:?}");
    assert!(b.confidence >= 0.5 && b.confidence <= 0.99, "{b:?}");
}

/// 一段足够长的正文，模拟正常页面。
fn prose(n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "<p>Paragraph {i} of a normal article, with enough words to read like real \
                 prose that someone wrote for people rather than for robots or crawlers.</p>"
            )
        })
        .collect()
}

#[test]
fn cloudflare_challenge() {
    let html = r#"<!DOCTYPE html><html><head><title>Just a moment...</title></head>
<body><div class="main-wrapper"><h1>example.com</h1>
<h2 id="challenge-running">Checking if the site connection is secure</h2>
<noscript>Enable JavaScript and cookies to continue</noscript></div>
<script>(function(){window._cf_chl_opt={cvId:'3',cType:'managed'};
var a=document.createElement('script');a.src='/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=1';
document.head.appendChild(a);}());</script></body></html>"#;
    expect(html, Some(403), "cloudflare", BarrierKind::Challenge);
    let b = detect(html, Some(403)).unwrap();
    assert!(b.confidence > 0.95, "{b:?}");
    // 没有状态码也能认出来
    expect(html, None, "cloudflare", BarrierKind::Challenge);
}

#[test]
fn cloudflare_block() {
    let html = r#"<html><head><title>Attention Required! | Cloudflare</title></head><body>
<div id="cf-wrapper"><div id="cf-error-details"><h1>Sorry, you have been blocked</h1>
<h2>You are unable to access example.com</h2>
<p>This website is using a security service to protect itself from online attacks.</p>
<span>Cloudflare Ray ID: 0000000000000000</span></div></div></body></html>"#;
    expect(html, Some(403), "cloudflare", BarrierKind::Block);
}

#[test]
fn turnstile() {
    let html = r#"<html><head><title>Security check</title>
<script src="https://challenges.cloudflare.com/turnstile/v0/api.js" async></script></head>
<body><h1>Verify you are human</h1><form method="post">
<div class="cf-turnstile" data-sitekey="x"></div></form></body></html>"#;
    expect(html, None, "turnstile", BarrierKind::Captcha);
}

#[test]
fn akamai() {
    let html = r#"<HTML><HEAD><TITLE>Access Denied</TITLE></HEAD><BODY>
<H1>Access Denied</H1>You don't have permission to access "http&#58;&#47;&#47;www&#46;example&#46;com&#47;" on this server.<P>
Reference&#32;&#35;18&#46;1&#46;2&#46;3<P>https&#58;&#47;&#47;errors&#46;edgesuite&#46;net&#47;18&#46;1</P>
</BODY></HTML>"#;
    expect(html, Some(403), "akamai", BarrierKind::Block);
}

#[test]
fn datadome() {
    let html = r#"<html><head><title>example.com</title></head><body>
<script>var dd={'rt':'c','cid':'x','hsh':'y','t':'fe','host':'geo.captcha-delivery.com'}</script>
<script src="https://ct.captcha-delivery.com/c.js"></script>
<iframe src="https://geo.captcha-delivery.com/captcha/?initialCid=x" title="DataDome CAPTCHA"></iframe>
</body></html>"#;
    expect(html, Some(403), "datadome", BarrierKind::Captcha);
    expect(html, None, "datadome", BarrierKind::Captcha);
}

#[test]
fn perimeterx() {
    let html = r#"<html><head><title>Access to this page has been denied</title></head><body>
<div><h1>Before we continue...</h1><p>Press &amp; Hold to confirm you are a human (and not a bot).</p>
<div id="px-captcha"></div></div>
<script>window._pxAppId='PXabc';</script><script src="//captcha.px-cdn.net/PXabc/captcha.js"></script>
</body></html>"#;
    expect(html, Some(403), "perimeterx", BarrierKind::Captcha);
}

#[test]
fn imperva() {
    let incident = r#"<html><head><META NAME="robots" CONTENT="noindex,nofollow"></head>
<body><iframe id="main-iframe" src="/_Incapsula_Resource?CWUDNSAI=9&xinfo=1" frameborder=0>
Request unsuccessful. Incapsula incident ID: 000000000000000000-0000000000000000</iframe></body></html>"#;
    expect(incident, Some(403), "imperva", BarrierKind::Challenge);

    let interruption = r#"<html><head><title>Pardon Our Interruption</title></head><body>
<h1>Pardon Our Interruption</h1><p>As you were browsing something about your browser made us think you were a bot.</p>
</body></html>"#;
    expect(interruption, Some(200), "imperva", BarrierKind::Challenge);
}

#[test]
fn aliyun() {
    let waf = r#"<html><head><title>405</title></head><body>
<div class="err"><p>很抱歉，由于您访问的URL有可能对网站造成安全威胁，您的访问被阻断。</p>
<img src="https://errors.aliyun.com/images/TB1.png"></div></body></html>"#;
    expect(waf, Some(405), "aliyun", BarrierKind::Block);

    let slider = r#"<html><head><title>验证码拦截</title>
<script src="//g.alicdn.com/AWSC/AWSC/awsc.js"></script></head><body>
<div id="nc_1_wrapper"><span id="nc_1_n1z" class="nc_iconfont btn_slide"></span>
<span class="nc-lang-cnt">请按住滑块，拖动到最右边</span></div><p>访问验证</p>
<script>var url='/punish?x5secdata=abc';</script></body></html>"#;
    expect(slider, Some(200), "aliyun", BarrierKind::Captcha);
}

#[test]
fn tencent_cloud() {
    let waf = r#"<html><head><title>501</title></head><body><h2>抱歉，您的请求已被拦截</h2>
<p>当前访问疑似黑客攻击，已被网站管理员设置为拦截。</p>
<a href="https://waf.tencent-cloud.com/">腾讯云 Web 应用防火墙</a></body></html>"#;
    expect(waf, None, "tencent_cloud", BarrierKind::Block);

    let captcha = r#"<html><head><title>安全验证</title>
<script src="https://turing.captcha.qcloud.com/TCaptcha.js"></script></head>
<body><p>请完成安全验证后继续访问</p><div id="TencentCaptcha"></div></body></html>"#;
    expect(captcha, None, "tencent_cloud", BarrierKind::Captcha);
}

#[test]
fn recaptcha_and_hcaptcha_and_geetest_interstitials() {
    let recaptcha = r#"<html><head><title>Are you a robot?</title>
<script src="https://www.google.com/recaptcha/api.js"></script></head><body>
<p>Please confirm you are not a robot.</p><form><div class="g-recaptcha" data-sitekey="k"></div></form></body></html>"#;
    expect(recaptcha, None, "recaptcha", BarrierKind::Captcha);

    let hcaptcha = r#"<html><head><title>Security check</title>
<script src="https://js.hcaptcha.com/1/api.js" async defer></script></head><body>
<p>Complete the security check to continue.</p><div class="h-captcha" data-sitekey="k"></div></body></html>"#;
    expect(hcaptcha, Some(429), "hcaptcha", BarrierKind::Captcha);

    let geetest = r#"<html><head><title>人机验证</title>
<script src="https://static.geetest.com/v4/gt4.js"></script></head><body>
<div class="geetest_holder"></div><p>请完成下方验证后继续</p>
<script>initGeetest4({captchaId:'x'})</script></body></html>"#;
    expect(geetest, None, "geetest", BarrierKind::Captcha);
}

#[test]
fn google_unusual_traffic() {
    let html = r#"<html><head><title>https://www.google.com/search</title></head><body>
<div>Our systems have detected unusual traffic from your computer network. This page checks to see if it's really you sending the requests, and not a robot.</div>
<div class="g-recaptcha"></div></body></html>"#;
    expect(html, Some(429), "google", BarrierKind::Captcha);
}

#[test]
fn login_walls() {
    let generic = r#"<html><head><title>Members only</title></head><body>
<h1>Sign in to continue</h1><form action="/login" method="post">
<input name="email"><input type="password" name="pw"><button>Log in</button></form></body></html>"#;
    expect(generic, None, "generic", BarrierKind::LoginWall);

    let chinese = r#"<html><head><title>提示</title></head><body><p>该内容需要登录，请先登录后查看。</p>
<a href="/login">登录</a></body></html>"#;
    expect(chinese, None, "generic", BarrierKind::LoginWall);

    let status_only = "<html><body><p>Unauthorized</p></body></html>";
    let b = detect(status_only, Some(401)).unwrap();
    assert_eq!((b.kind, b.rule_id), (BarrierKind::LoginWall, "status.401"));
}

#[test]
fn bilibili_captcha() {
    let html = r#"<html><head><title>验证码_哔哩哔哩</title></head>
<body><div class="bili-captcha"><div class="geetest_panel"><p>请完成下方验证</p></div></div>
<script src="/vendor/gt.js"></script></body></html>"#;
    expect(html, Some(200), "bilibili", BarrierKind::Captcha);
    // 本地近似样本只有通用提示语时也能识别
    let generic = r#"<html><head><title>安全验证</title></head>
<body><div class="bili-captcha"><p>请完成下方验证</p></div></body></html>"#;
    expect(generic, Some(200), "bilibili", BarrierKind::Captcha);
}

#[test]
fn generic_403_page() {
    let html = "<html><head><title>403 Forbidden</title></head><body><center><h1>403 Forbidden</h1></center><hr><center>nginx</center></body></html>";
    expect(html, Some(403), "generic", BarrierKind::Block);
}

#[test]
fn detection_can_be_turned_off() {
    let html = "<html><head><title>Just a moment...</title></head><body><script>window._cf_chl_opt={}</script></body></html>";
    let doc = process(
        html,
        &Options {
            detect_barrier: false,
            ..Default::default()
        },
    );
    assert!(doc.barrier.is_none());
}

// ---- 反例：正常页面，只是带验证码组件、厂商脚本或登录入口 ----

fn page(head: &str, body: &str) -> String {
    format!(
        "<html><head><title>Normal page</title>{head}</head><body><article><h1>Normal page</h1>{}{body}</article></body></html>",
        prose(20)
    )
}

#[test]
fn normal_pages_are_not_barriers() {
    let cases = [
        // 1. 评论区带 reCAPTCHA
        page(
            r#"<script src="https://www.google.com/recaptcha/api.js"></script>"#,
            r#"<form id="comment"><textarea></textarea><div class="g-recaptcha" data-sitekey="k"></div><p>Please verify you are human before posting.</p></form>"#,
        ),
        // 2. 联系表单带 hCaptcha
        page(
            r#"<script src="https://js.hcaptcha.com/1/api.js"></script>"#,
            r#"<form><input name="email"><div class="h-captcha" data-sitekey="k"></div></form>"#,
        ),
        // 3. 订阅框带 Turnstile
        page(
            "",
            r#"<form class="newsletter"><div class="cf-turnstile" data-sitekey="x"></div></form><script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>"#,
        ),
        // 4. Cloudflare 给正常页面注入的检测脚本
        page(
            "",
            r#"<script>(function(){var a=document.createElement('script');a.src='/cdn-cgi/challenge-platform/scripts/jsd/main.js';window.__CF$cv$params={r:'1'};})();</script>"#,
        ),
        // 5. 页头的登录框（密码输入框 + Log in 按钮）
        page(
            "",
            r#"<form class="login"><input name="u"><input type="password" name="p"><button>Log in</button><a href="/signup">Please sign in or register</a></form>"#,
        ),
        // 6. 正常页面里的 Incapsula、DataDome、PerimeterX、Akamai 传感器脚本
        page(
            r#"<script src="/_Incapsula_Resource?SWJIYLWA=719d34d31c8e3a6e6fffd425f7e032f3"></script><script src="https://js.datadome.co/tags.js"></script><script>window._pxAppId='PXabc';</script>"#,
            r#"<script src="/akam/13/7a6b1c2d" defer></script>"#,
        ),
        // 7. 极验登录组件的门户首页
        page(
            r#"<script src="https://static.geetest.com/static/js/gt.0.4.9.js"></script>"#,
            r#"<div class="geetest_captcha"></div><script>initGeetest({gt:'x'})</script>"#,
        ),
        // 8. 讨论 Cloudflare 挑战页的技术文章
        page(
            "",
            r#"<p>When a site shows "Just a moment..." and "Checking your browser", Cloudflare is running a challenge; the page sets window._cf_chl_opt.</p>"#,
        ),
        // 9. 标题碰巧叫 Access Denied 的影评
        format!(
            "<html><head><title>Access Denied (2024) review</title></head><body><h1>Access Denied</h1>{}</body></html>",
            prose(20)
        ),
        // 10. 付费墙预告：文章开头加"登录继续阅读"
        page(
            "",
            r#"<div class="paywall"><p>Sign in to continue reading.</p><a href="/login">Log in</a></div>"#,
        ),
        // 11. 腾讯验证码 SDK 和阿里滑块脚本被正常页面引用
        page(
            r#"<script src="https://turing.captcha.qcloud.com/TCaptcha.js"></script><script src="//g.alicdn.com/AWSC/AWSC/awsc.js"></script>"#,
            "",
        ),
    ];
    for (i, html) in cases.iter().enumerate() {
        for status in [None, Some(200)] {
            assert_eq!(
                detect(html, status),
                None,
                "case {} misdetected (status {status:?})",
                i + 1
            );
        }
    }
}

#[test]
fn short_normal_pages_are_not_barriers() {
    // 文字很少的正常页面：小产品页带订阅表单验证码、普通 404 页
    let product = r#"<html><head><title>Widget</title><script src="https://www.google.com/recaptcha/api.js"></script></head>
<body><h1>Blue widget</h1><p>A small blue widget. $9.99.</p><form><input name="email"><div class="g-recaptcha"></div></form></body></html>"#;
    assert_eq!(detect(product, Some(200)), None);
    let not_found = "<html><head><title>Not found</title></head><body><h1>Page not found</h1><a href=\"/\">Home</a></body></html>";
    assert_eq!(detect(not_found, Some(404)), None);
    // 空的单页应用外壳
    let spa = r#"<html><head><title>App</title></head><body><div id="root"></div><script src="/static/js/main.js"></script></body></html>"#;
    assert_eq!(detect(spa, Some(200)), None);
}
