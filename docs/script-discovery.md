# 内联脚本入口发现

当请求 `links` 或爬取网页时，Rust 链接提取器还会检查内联可执行脚本，寻找明确的字面量导航与默认 GET：`location.href = "…"`、`window.location = "…"`、`location.assign/replace("…")`、`window.open("…")`、单参数 `fetch("…")`。

候选会按页面 base URL 转为绝对地址、去除 fragment、去重，再经过原爬虫的范围、robots 和预算约束。支持字符串内的 `\/`、固定十六进制和 Unicode 转义；不会执行 JavaScript。

以下情况放弃：动态字符串拼接、模板插值、未知 fetch 选项、带写方法的请求、未被使用的字符串常量、JSON-LD 等数据脚本、带 src 的外部脚本。注释、字符串中的代码和常见表达式起始位置的正则字面量不会当成调用执行。

每段脚本最多 1MiB、65536 个 token，每份文档最多检查 2MiB 内联脚本，每段最多输出 256 个候选。复杂度超限时放弃该段。

这是保守的静态词法发现，不是 JavaScript AST 或执行路径证明。无法完整识别正则/除法的所有语法歧义、函数遮蔽、跨脚本常量、不可达分支、外部 bundle 或计算出的端点。因此不能据此声称覆盖 Katana 的完整 JS/JSluice 和浏览器网络发现能力。第一轮只有 `const article="/script-route"` 的旧案例仍未补齐，它并不构成已知 GET 或导航动作。

已测控制图：旧版 2/8、新版 8/8、固定 Katana `-jc` 7/8。只是同一受控语法集合的入口覆盖，不是互联网召回率。Katana 面向广义端点发现，访问额外候选不应被写成它发生了写请求；该场景中三者非 GET 请求数均为零。逐请求证据见 `benchmarks/history/0.70/six/results/media-core/`。
