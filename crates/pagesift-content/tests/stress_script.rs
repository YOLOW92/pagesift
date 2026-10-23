//! 极限压力与对抗性样本测试：针对 ECMAScript 状态机词法扫描器在畸形 JS 与恶意脚本下的健壮性。

use pagesift_content::scan_script;
use url::Url;

#[test]
fn stress_oversized_script_budget_rejection() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    // 构造超过 1MB (1024 * 1024 字节) 的超大脚本
    let huge = "const X = 1;\n".repeat(100_000);
    assert!(huge.len() > 1024 * 1024);

    let t0 = std::time::Instant::now();
    let scan = scan_script(&huge, &base);
    let elapsed = t0.elapsed();

    // 超限时应毫秒级直接拒绝，不消耗 CPU
    assert!(scan.budget_exceeded);
    assert!(scan.navigation.is_empty());
    assert!(elapsed.as_millis() < 50);
}

#[test]
fn stress_deeply_nested_braces() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    // 5,000 层深度嵌套的大括号
    let open = "{".repeat(5_000);
    let close = "}".repeat(5_000);
    let js = format!("function deep() {open} return fetch('/nested/api'); {close}");

    let scan = scan_script(&js, &base);
    // 深度嵌套不应发生栈溢出或 panic
    assert!(scan.navigation.is_empty() || !scan.budget_exceeded);
}

#[test]
fn stress_redos_regex_literals_in_js() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    // 包含常见 ReDoS 攻击模式的正则字面量
    let js = r#"
        const re1 = /([a-zA-Z0-9]+)+$/;
        const re2 = /((a+)+)+$/;
        const re3 = /^[a-zA-Z0-9]+([._-][a-zA-Z0-9]+)*@[a-zA-Z0-9]+([.-][a-zA-Z0-9]+)*\.[a-zA-Z]{2,}$/;
        fetch('/api/safe/endpoint');
    "#;

    let scan = scan_script(js, &base);
    assert_eq!(scan.navigation, ["https://example.com/api/safe/endpoint"]);
}

#[test]
fn stress_pathological_template_literals() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    // 包含空表达式、空模板、密集插值模板与合法动态路由模板
    let js = r#"
        const a = `${x}`;
        const b = ``;
        const c = `${1}${2}${3}${4}${5}`;
        const prefix = '/api';
        fetch(`${prefix}/v1/items`);
    "#;

    let scan = scan_script(js, &base);
    assert!(
        scan.navigation
            .contains(&"https://example.com/api/v1/items".to_string())
    );
}

#[test]
fn stress_unclosed_template_literal_never_panics() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    let js = "const unclosed = `${fetch('/api/bad');";
    let scan = scan_script(js, &base);
    assert!(!scan.budget_exceeded || scan.navigation.is_empty());
}

#[test]
fn stress_definition_flood_budget() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    // 构造超过 256 个变量定义的脚本，触发防御性预算熔断
    let flood = (0..500)
        .map(|i| format!("const var_{i} = '/endpoint_{i}';\n"))
        .collect::<String>();

    let scan = scan_script(&flood, &base);
    assert!(scan.budget_exceeded);
}

#[test]
fn stress_massive_string_concatenation_chain() {
    let base = Url::parse("https://example.com/app.js").unwrap();
    // 100 个字符拼接串，单链深度超过 16 限制时应优雅中止而不发生 OOM
    let mut chain = String::from("const big = 'a'");
    for i in 0..100 {
        chain.push_str(&format!(" + '/sub_{i}'"));
    }
    chain.push_str(";\nfetch(big);\n");

    let scan = scan_script(&chain, &base);
    // 超出 16 级深度时 expression 优雅退出，不 panic
    assert!(scan.navigation.is_empty());
}
