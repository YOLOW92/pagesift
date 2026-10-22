# Rust 能力扩展与明确边界

本轮按用户要求排除持久浏览器交互和 Agent 任务，处理原先另外 7 类缺失。下面是可调用的原生 Rust 实现，不等于完整竞品的所有参数、协议和站点均已覆盖。新增模块没有引入其他语言、解释器或子进程运行时；仅将已锁定的 `url` 加为 extract crate 的直接依赖。

## 定位与页面检查

```sh
pagesift relocate current.html --original previous.html --selector '.product'
pagesift inspect page.html --base-url https://example.org/products/
```

`FuzzyAnchor` 是显式选择的外观候选接口，与稳定业务 ID 的 `Anchor` 分开。保存文本、描述属性与父级描述 token，采用加权 Dice 相似度；主要比较相同元素标签；article/section/div 标签变化还要求属性 Dice 至少 0.75、文本 Dice 至少 0.85，并施加 0.04 分惩罚。默认分数至少 0.72、领先第二名至少 0.12，同分始终弃权。CLI 返回 candidate/low_confidence/ambiguous/budget_exceeded。它可以处理部分布局和文字变化，不能证明两个看起来一样的商品具有同一业务身份，也不支持任意标签变化或完整 Scrapling 模糊算法等价。扫描上限 20,000 节点，特征上限 128 token，子树采样上限 256 节点。

`discover_forms` 读取 action/method、input/select/textarea/button、名称、标签、required/disabled/readonly 和敏感类型。支持外部 `form=` 所有者与 disabled fieldset 的首个 legend 例外。元数据不输出输入值、密码或隐藏 token。`plan_form_get` 接受调用方明确提供的字段值，生成 GET URL；不会发送请求，拒绝 POST/dialog、密码、文件、未知替换字段。重复名称、已选复选框、多选项和 disabled optgroup 有独立处理。它不是浏览器完整约束验证器，不处理脚本事件、点击提交按钮的覆盖属性、图片坐标或完整自动填表。调用方仍须对生成 URL 应用 scope、robots 和 SSRF 策略。表单行为参考 [HTML form](https://html.spec.whatwg.org/multipage/forms.html)；本实现对不支持的操作保留显式边界。

`KnowledgeExtractor` 包含 WordPress/Next.js/Drupal/nginx、登录表单，以及 AWS key ID、GitHub/Stripe/Slack token 与私钥标头的受限规则。支持有大小限制的自定义 Rust regex；输出规则名、输入来源和字节范围，不输出匹配原文，不调用提供商验证凭证。它是启发式指标，不是完整技术指纹库、机器学习页面分类、API 请求分类或安全漏洞判定。4 MiB 正文、128 条规则、512 条发现为硬上限；凭证请求头不参与扫描。

## 外部 JS 与实际爬取

```sh
pagesift discover-js https://example.org/ --max-files 32 --max-depth 3
pagesift crawl https://example.org/ --engine http --external-scripts --sitemap skip
```

Rust lexer 读取可执行 script src 与静态 import/export-from/import() 依赖，保守折叠少量 const 字符串拼接，识别字面量导航和单参数 fetch。遇到函数/箭头参数、eval/with 时不折叠标识符；不是完整 AST/JSluice，不处理任意计算、import map、source map 或浏览器运行时请求。候选不代表代码实际执行。

默认最多尝试 32 个脚本、深度 3、累计 4 MiB、每文件 1 MiB；语法 token、声明、候选及队列也有上限。失败请求也消耗文件预算。默认不启用；`discoverScripts` API 扩展要求 HTTP 引擎和 links，并拒绝自定义 headers/egress/条件请求组合。此模式禁用页面结果缓存，以免外部脚本变化被旧页面缓存掩盖。配置的网络层主机凭证仍按原规则应用，不使用 CookieSession 或浏览器状态。

仅调度同源依赖，跨源重定向在发送前拒绝。脚本请求复用原策略的 robots、SSRF、并发和超时；实际 crawl 的页面候选继续走原 frontier/scope/robots/深度预算。依赖请求前释放主文档网络名额，避免根页面占满每主机额度后再等待自己的依赖。单页的脚本预算不是整个大站的全局脚本预算；跨页面脚本缓存与整任务统一预算仍待实现。

## 媒体适配、命名和后处理

```sh
pagesift media-extract listing.json --base-url https://example.org/wp-json/wp/v2/media --adapter wordpress
pagesift media-archive https://example.org/wp-json/wp/v2/media --adapter wordpress --out ./media
```

明确选择四种适配：WordPress REST media、Mastodon status media_attachments、Wikimedia Action API imageinfo、含显式 @id/contentUrl 的 JSON-LD 媒体对象。保留来源/业务 ID，原始媒体 URL、受限元数据及版本摘要；拒绝重复身份，不把预览图当原图。输入上限 4 MiB、条目最多 1024。支持这些接口不等于每个站点、账号、认证或分页方式都已验收；当前一次归档只处理一个响应，未与 collect 分页提交事务绑定。

下载沿用原归档的 checksum、ETag/If-Range、单写者锁与恢复机制。`{id}`、`{title}`、`{source}`、`{ext}`、`{hash}` 组成单个文件名，防止路径穿越并处理 Windows 保留设备名；长度有上限，碰撞明确报错，不覆盖其他条目。输出二进制后最后发布 `.metadata.json` 作为提交标记；每一步为无覆盖发布、文件同步，Unix 同步目录。这是可恢复的两步提交，不是文件系统双文件原子事务。中断后可补齐缺失侧车，现有损坏或属于其他身份/版本的文件会被拒绝。侧车是显式导出的明文原始元数据，可能含媒体 URL 查询参数；新建文件 Unix 权限为 0600。未验收 Windows ACL、非 Unix 断电语义或任意插件后处理生态。

## 流媒体

```sh
pagesift stream-plan playlist.m3u8 --base-url https://example.org/video/index.m3u8
pagesift stream-download https://example.org/video/master.m3u8 --variant 0 --out ./video
pagesift stream-download https://example.org/video/manifest.mpd --out ./video
```

按 [RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html) 和 [DASH-IF timing model](https://dashif.org/Guidelines-TimingModel/) 自行编写 Rust 清单解析器。HLS 支持显式主清单选轨、有限 VOD、初始化映射与字节范围；DASH 支持单 period 的 static MPD、单一路径 BaseURL 继承、SegmentList、有限 SegmentTimeline、Number/Time 模板以及有限时长的固定 duration 模板。限制清单 1 MiB、32 轨、4096 个分片，并拒绝 DTD/外部 XML 引用。

原生分片归档支持完整文件及严格范围请求，独立保存已校验分片，重试复用完成记录；未完成分片按强 ETag 恢复。范围请求收到整文件时拒绝并丢弃旧前缀，下次重试从新范围开始。每轨按清单顺序拼接并单独发布，最多 2 GiB 总预算、单分片 64 MiB。DASH 另外生成本地 MPD：保留原始时长、timescale、presentationTimeOffset 和 SegmentTimeline，重写成已校验的本地分片链接，移除远程 BaseURL/Location/时钟提示，最后发布清单。DASH 命令返回该离线 MPD，保留清单原有的模板或列表寻址方式；分片通过本地硬链接复用归档，不再额外复制整条未裁剪音视频轨，减少磁盘和 I/O 占用。假设 VOD 分片在给定清单版本下保持不变；本地 hash 不证明服务器所有分片属于同一媒体版本。

不包含音视频跨轨 mux、转码、播放器、yt-dlp 调用、DRM/加密、直播追赶、外部音频 rendition、HLS discontinuity、DASH SegmentBase/index、负 repeat 和所有 timing 扩展。拼接只保留输入轨字节；DASH 展示窗口由保留的本地 MPD 时序表达。本地 FFmpeg 生成的 HLS/DASH 样本用于解码回读，不证明所有编码组合都能播放，也不能写成完整 HLS/DASH 或 gallery-dl 生态领先。

## 验收解释

旧数据用于回归；新评测另冻结输入、版本、配置、失败和资源记录。原先缺失的七项即使都有受限实现，也只提升到 partial，不能自动填成 complete 或 superior。浏览器交互/Agent 是本轮用户明确排除项，其余政策边界与未验收项仍照实保留。
