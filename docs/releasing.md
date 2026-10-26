# 发布说明与发布流程

本文写给维护者：发布说明怎么写才站得住，以及发布产物怎么验证来源。发布本身由维护者手动打 `v*` 标签触发（见 `.github/workflows/release.yml`），任何自动化和会话都不替维护者打标签、推送或发布。

## 一、发布说明的写作规则

### 1. 涉及真实网站的说法，必须在真实网站上验证过

- 凡是写到某个真实网站、某类真实页面的行为（“Substack 上会误判”“Steam 仍能识别年龄确认页”“某站点返回 429”），写进发布说明之前，必须用**发布用的二进制**在那个真实网站上跑过，并且把旧版本和新版本都跑一遍。
- 验证过的，写明怎么验证的：版本、日期、引擎、是否遵守 robots.txt、样本数量（例如“66 个 Substack 页面，0.72.8 误判 2 个，0.73.0 误判 0 个”）。
- **验证不了的**（站点现在不再出现这种页面、被反爬挡住、被 robots 禁止、需要登录），必须原样写“只有本地用例覆盖”，并说明本地用例测的是什么。不允许从本地夹具推断真实站点的行为，也不允许用“应该”“通常”“仍能”这类没有验证的词。
- 一个现象在真实站点上的原因，只有看过那个站点的真实响应才能写。从代码规则推断出的原因，要写成“规则是……，在真实页面上的影响未测”。
- 离线保存的真实页面（语料快照）不等于真实网站：写成“N 个已保存的真实页面”，不要写成“在真实网站上”。

### 2. 每个数字都要能追溯

- 数字后面能指到脚本和结果文件（`bench/` 下的脚本，`docs/benchmarks/` 或 `tasks/` 下的结果文件），并写明机器、版本、日期。
- 对比表标明指标方向（越高越好 / 越低越好）。
- 合成负载（本地源站、无网络）的数字必须写明是合成的，不得与真实网络的数字或别的工具的真实网络数字放在一起比。
- 不同评分口径的同名基准（例如 WCXB 官方评分器的 0.85 和自带逐词 F1 的 0.67）不得并排出现而不加说明。
- 没有测过的竞品数字不写；旧数字沿用时写“历史数据，当前版本未复测”，并带版本和日期。
- 已发布的发布说明不改写；发现错误时在下一版的发布说明里更正，并在 `tasks/` 的核对清单里记一笔。

### 3. 发布说明的核对清单（每次发布前逐条打勾）

1. 逐条列出发布说明里所有涉及真实网站的说法，每条标注：真实网站已验证（附样本和结果）/ 只有本地用例覆盖。
2. 逐条列出所有数字，标注出处文件。
3. 所有“修复了 X”的说法，旧版本能复现、新版本不复现，二者都有记录。
4. 没有“全部”“所有”“零”“完美”这类绝对词，除非有对应的穷举证据。
5. README 四种语言版本里的版本号、数字与发布说明一致。
6. 发布说明末尾附上 Discord 链接：https://discord.gg/esA3wMZVvA

## 二、发布产物与 Trusted Publishing

三个注册表都不使用长期令牌（`release.yml` 顶部有同样的说明）：

| 渠道 | 方式 | 来源证明 |
| :--- | :--- | :--- |
| crates.io | `rust-lang/crates-io-auth-action` 用 OIDC 换 30 分钟的临时令牌；`scripts/publish_crates.py` 在 Actions 里每 20 分钟自己续换，结束时吊销 | crates.io 的发布记录里能看到发布来源 |
| npm | npm CLI ≥ 11.5.1 用 OIDC 登录，带 `--provenance` | `npm audit signatures`，包页面上的 provenance 标记 |
| PyPI | `pypa/gh-action-pypi-publish` 用 OIDC | PEP 740 证明 |
| GitHub Release | 压缩包、`.sha256` 文件和 `SHA256SUMS` 都做了构建证明（`actions/attest-build-provenance`） | `gh attestation verify <文件> --repo YOLOW92/pagesift`；`sha256sum --check SHA256SUMS` |
| GHCR 镜像 | 镜像带 provenance 证明 | `gh attestation verify oci://ghcr.io/yolow92/pagesift:<版本> --repo YOLOW92/pagesift` |

要点：

- 可信发布者在 crates.io 和 npm 网站上登记为：仓库 `YOLOW92/pagesift`，工作流 `release.yml`（补发布用 `publish-rest.yml`；npm 包 `pagesift` 另有 `publish-npm-wrapper.yml`），环境 `release`。逐步操作见维护者笔记 `tasks/0.80/d/trusted-publishing.md`。
- `release.yml` 的发布部分按 `github-release → pypi → npm-publish → publish` 依次进行，各任务只拿自己需要的权限；`publish` 在所有渠道成功后才把 GitHub 草稿版本公开。
- 新增 crate 的第一次发布必须手动用 API 令牌发一次，之后才能在 crates.io 上登记可信发布者。
- npm 新建的可信发布者配置有 2 天有效期，需要在这段时间内完成一次成功发布；建议发版前一天再配置。
- 第一次完整的 OIDC 发布成功之前，不要删除仓库里的旧 secrets，也不要在 npm 上勾选“disallow tokens”。
- npm 的绕过 2FA 令牌最迟从 2027-01 起不能直接发布（官方公告：https://github.blog/changelog/2026-07-31-restricting-npm-bypass-2fa-granular-access-tokens/ ）。
- 工作流里用到 `release` 环境的任务有 4 个；如果环境设置了必须审批，每个任务都会等一次批准。

官方文档：

- crates.io：https://crates.io/docs/trusted-publishing
- npm：https://docs.npmjs.com/trusted-publishers
- 构建证明：https://github.com/actions/attest-build-provenance
