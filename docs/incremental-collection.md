# 带业务身份的重定位与增量采集

## Rust 重定位

`pagesift_extract::Anchor::capture(&document, selector, "data-product-id")` 保存可序列化锚点；`anchor.locate(&new_document)` 返回 `Exact`、`Relocated`、`Missing` 或 `Ambiguous`。匹配结果中的 `NodeRef` 可直接读取属性、HTML、文本，或继续使用相对 CSS 选择器。

这里的重定位解决 class、DOM id、父容器和位置变化，要求业务 ID 保持稳定，允许元素标签变化。旧选择器被其他条目复用时不会直接接受；同一业务 ID 重复出现时也不接受。身份属性缺失或业务 ID 改名返回缺失，不进行模糊外观猜测。调用方应将锚点按站点/业务来源隔离保存。

`pagesift_extract::discover_items(html)` 不需要手写 CSS：读取 `data-item-id`、`data-product-id`、`itemid`，返回 ID、身份属性、文本及 HTML。属性冲突或 ID 重复返回错误。没有这些信号时返回空列表，不把 URL、标题或随机 DOM id 冒充稳定业务身份。

## CLI：真实 JSON API 分页

WordPress REST 集合：

```bash
pagesift collect 'https://example.com/wp-json/wp/v2/posts?per_page=100&orderby=id&order=asc&page=1' \
  --source example-posts --data-dir ./collection-state --out ./events.jsonl --max-pages 10
```

继续运行同一命令会恢复未完成扫描的下一页。完成后再次运行会开始一次新扫描，只交付新增及变化条目；URL 变化会作为原 ID 的更新。`--max-pages` 限制每次调用抓取页数，不表示扫描已经完整结束。JSONL 事件写入并 `sync_data` 成功后，才保存该页及下一页断点并 flush 存储。重新打开日志会移除崩溃留下的不完整尾行；完整行损坏会报错，不继续追加。

默认不推断删除。只有明确添加 `--authoritative`，且该 source 表示完整、稳定且有权威性的集合，才在完整末页之后交付 `value: null` 的删除事件。过滤集合、权限变化和实时变动的列表不应声明为完整来源。WordPress 适配器检查总数/总页数，拒绝总数改变、重复 ID、不完整末页；这不能替代服务器提供的一致性快照，同样总数下的并发替换仍可能无法检测。

通用 JSON API 可提供适配文件：

```json
{"items_pointer":"/results","id_pointer":"/key","next_pointer":"/next"}
```

```bash
pagesift collect 'https://example.com/api/items' --adapter adapter.json \
  --source example-items --data-dir ./collection-state --out ./events.jsonl
```

`next` 必须明确给出 URL 字符串或 `null`；字段缺失、跨源分页、分页循环、重复/缺失业务 ID、错误 HTTP 状态和截断响应均报错，不触发删除。网络请求沿用项目配置的 SSRF、robots、凭证、代理及限速策略。WordPress 依照官方 [分页契约](https://developer.wordpress.org/rest-api/using-the-rest-api/pagination/) 读取 `X-WP-TotalPages`、`X-WP-Total`，使用响应内原生 `id`，不需要用户配置条目 CSS。

## Rust 自定义交付与 HTML 列表

`pagesift::collect::Collection` 提供 `begin`、`next_url`、`accept_page`、`run`、`finish`、`abort`。交付回调返回成功，表示目标已经持久接受事件；调用方可接数据库或其他幂等目标。HTML 列表可用 `accept_html_page` 自动识别上述身份属性，并复用相同的检查点/更新/删除流程；必须显式提供下一页或末页确认，不能仅凭“未发现下一页链接”推断删除。HTML 接口使用 `/items`、`/id`、`/next` 的通用适配配置。

每个 source 仅允许一个采集者，调用方负责协调；同一个 Store 目录不能由不同进程同时打开。回调交付与本地检查点不是跨系统事务：崩溃可重放已交付事件，目标必须幂等，不能宣称 exactly-once。CLI 事件文件也可能出现重复，应按 source/id 顺序折叠最终状态；保留相同输出文件续跑。输出日志和状态目录需要调用方备份。

单个检查点序列化上限 32 MiB，包含旧清单和本次已见条目的业务 ID 与 BLAKE3 内容摘要，正文仅交付到目标日志，不重复塞入断点；大型集合仍应分区。旧版含完整正文的断点在读取时迁移，未知的未来版本会拒绝打开。单页请求继承 Fetcher 大小/超时限制。超过状态上限返回错误，不把该轮计作完整。失效游标或变动清单可通过 Rust `abort()` 放弃本轮检查点并重新开始，已提交清单保留。CLI 对应选项为 `--restart`，仅在需要放弃未完成扫描时使用。

这不是 gallery-dl 的完整站点适配器、下载器、登录处理器或媒体转码器。目前专用 API 协议为 WordPress；其他 JSON API 使用显式指针适配，其他 HTML 网站需要可靠的业务身份声明及分页确认。
