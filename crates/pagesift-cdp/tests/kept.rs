//! 保留的页面（`/interact` 的底层）：真实浏览器，没有时跳过。

mod common;

use std::time::Duration;

use common::*;
use pagesift_cdp::*;

fn route(_: &Req) -> Resp {
    html(
        r#"<button id="b" onclick="this.dataset.n = (+this.dataset.n || 0) + 1; document.getElementById('c').textContent = 'count ' + this.dataset.n;">Add one</button>
           <input aria-label="Name" oninput="document.getElementById('e').textContent = 'name ' + this.value" value="old">
           <p id="c">count 0</p><p id="e">name none</p>"#,
    )
}

#[tokio::test]
async fn kept_page_stays_usable_and_never_blocks_other_renders() {
    require_browser!();
    let (base, _) = serve(route).await;
    let url = format!("{base}/");
    let mut cfg = config();
    // 只有一个浏览器一个标签，内存检查永远不过：只有“没有页面在渲染”这条保证前进
    cfg.max_tabs_per_browser = 1;
    cfg.min_available_memory_mb = u64::MAX / 4;
    let pool = BrowserPool::new(cfg, allow_all()).await.unwrap();

    let (page, kept) = pool.render_kept(&url, &opts()).await.unwrap();
    assert!(page.html.contains("count 0"), "{}", page.html);

    // 选择器动作：状态留在页面上
    kept.run_actions(&[Action::Click {
        selector: "#b".into(),
        all: false,
    }])
    .await
    .unwrap();
    assert!(kept.capture().await.unwrap().html.contains("count 1"));

    // 无障碍树：真实页面上的角色和名称，按节点点击和输入
    let nodes = kept.ax_nodes().await.unwrap();
    let button = nodes
        .iter()
        .find(|n| n.role == "button")
        .expect("button in the AX tree");
    assert_eq!(button.name, "Add one");
    kept.click_node(button.backend_id).await.unwrap();
    assert!(kept.capture().await.unwrap().html.contains("count 2"));
    let input = nodes
        .iter()
        .find(|n| n.role == "textbox")
        .expect("textbox in the AX tree");
    assert_eq!(input.name, "Name");
    assert_eq!(input.value.as_deref(), Some("old"));
    kept.write_node(input.backend_id, "new").await.unwrap();
    // 替换原有内容，不是接在后面
    assert!(kept.capture().await.unwrap().html.contains("name new"));

    // 唯一的标签被占着：别的渲染照样完成（另起一个浏览器），拿到的是新页面
    let other = tokio::time::timeout(Duration::from_secs(30), pool.render(&url, &opts()))
        .await
        .expect("a kept page must not block other renders")
        .unwrap();
    assert!(other.html.contains("count 0"), "{}", other.html);
    assert!(kept.is_alive());

    // 关掉后名额归还
    kept.close().await;
    let tabs: usize = pool.stats().await.iter().map(|s| s.tabs_in_use).sum();
    assert_eq!(tabs, 0);
    shutdown(pool).await;
}

#[tokio::test]
async fn dropping_a_kept_page_returns_its_tab_and_failures_are_reported() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let (_, kept) = pool
        .render_kept(&format!("{base}/"), &opts())
        .await
        .unwrap();
    let e = kept
        .run_actions(&[Action::Click {
            selector: "#missing".into(),
            all: false,
        }])
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Action { index: 0, .. }), "{e}");
    // 校验规则和渲染相同：脚本动作默认关着
    let e = kept
        .run_actions(&[Action::ExecuteJavascript { script: "1".into() }])
        .await
        .unwrap_err();
    assert!(matches!(e, Error::ExecuteJavascriptDisabled), "{e}");
    let room = pool.session_room();
    assert_eq!(room.max_tabs, 4);
    drop(kept);
    let mut tabs = usize::MAX;
    for _ in 0..100 {
        tabs = pool.stats().await.iter().map(|s| s.tabs_in_use).sum();
        if tabs == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(tabs, 0);
    shutdown(pool).await;
}
