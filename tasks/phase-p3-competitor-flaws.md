# Phase P3 ｜ 爬虫界竞品战争：专门寻找 PageSift 的真实劣势

> **核心哲学**：不掩盖差距，不美化数据。主动暴露 PageSift 相比 Spider、Scrapy、Crawlee、Colly 的短板与缺陷，通过纯 Safe Rust 自研算法完成闭环超越。
> **目标对阵**：
> 1. **Spider (Rust)**：原始网络发包极快，但在 100k 测试中抓到 72k 个 200 OK，而 PageSift 0.50 仅有 62k；且 Spider 内存狂飙至 9GB。
> 2. **Scrapy (Python)**：请求优先级调度与中间件链条成熟。
> 3. **Crawlee (Node/Python)**：会话恢复与错误自适应降级。
> 4. **Colly (Go)**：高并发 Goroutine 网络栈与极低上下文切换。

---

## 阶段细分任务清单

### Step 3.1: 竞品技术对阵与测试口径对齐
- [ ] 梳理 Spider、Scrapy、Crawlee、Colly 核心网络栈、调度机制与内存模型
- [ ] 搭建同口径对照压测场（相同输入 URL 序列、相同网络环境、相同硬件规格）
- [ ] 界定五大核心对决指标：200 OK 获取数、有效 Markdown 正文数、持续 QPS、峰值 RSS、错误率

### Step 3.2: 深度剖析 0.50 遗漏 200 OK 的五大物理根因
- [ ] **根因 1（池化空闲连接断开零重试）**：HTTP/1 连接对端超时断开后，复用时直接抛出 Broken pipe / Reset，未对池化失败执行一次透明新建重试
- [ ] **根因 2（请求头指纹与反爬拦截）**：原版仅发送 `Accept: */*`，缺少 `Sec-Ch-Ua`, `Sec-Fetch-*`, `Accept-Language`，导致被 Cloudflare / Akamai WAF 拦截
- [ ] **根因 3（协议协商与证书容错）**：遇到老旧服务器 TLS 握手异常或证书过期时，缺乏优雅的重试与降级机制
- [ ] **根因 4（超时判定刚性）**：固定 3.5s 连接与 10.0s 总超时在跨洲高延迟网络下导致大量慢速长尾网站被误杀
- [ ] **根因 5（调度器全局互斥锁瓶颈）**：`Frontier` 锁竞争在大并发下延迟升高导致部分任务派发滞后

### Step 3.3: 五大纯 Rust 自研算法针对性攻坚
- [ ] **算法 1: 鲁棒连接池与空闲重试状态机 (`RobustPooledTransport`)**
  - 在 `client.rs` 中实现“池化请求重试”机制：若从池中取出的连接在握手/写请求阶段遭遇物理断开，自动透明使用新建连接重试一次
- [ ] **算法 2: 现代浏览器客户端伪装与自适应指纹 (`BrowserProfile`)**
  - 实现动态匹配的 `Sec-Ch-Ua`、`Sec-Fetch-Dest`、`Accept-Language`、`Accept` 等高仿真请求头体系，显著降低 WAF 拦截率
- [ ] **算法 3: 零分配无锁分片调度器 (`ShardedFrontier`)**
  - 采用多分片环形队列与分段无锁哈希，彻底消除 Frontier 全局 Mutex 争用
- [ ] **算法 4: 动态 RTT 经验分位数自适应超时算法 (`AdaptiveTimeoutEstimator`)**
  - 维护滑动窗口 RTT 分位数（p50/p95/p99），对跨大洋慢速长尾站点动态赋予放宽窗口，夺回误杀的 200 OK
- [ ] **算法 5: SIMD 加速与流式 HTML 零拷贝扫描**
  - 在 HTML 预扫描阶段引入 AVX2/NEON 加速标签与属性定界，削减内存拷贝

### Step 3.4: 竞品多维度横向实战对决与超越验证
- [ ] 在 GCP 80 核 VPS 上搭建真实多样性站点数据集（包含新闻、电商、海外长尾、CDN 防护、慢速站点）
- [ ] 运行 PageSift 0.60 vs Spider / Scrapy / Crawlee / Colly 横向实测
- [ ] 校验 200 OK 成功数是否全面反超历史差距（目标：从 62k 跃升至 75k+）
- [ ] 确保稳态 RSS 依然守死在 200~300MB 级别，完胜 Spider 的 9GB+ 内存泄漏
- [ ] 编写官方深度报告：`docs/benchmarks/0.60_p3_competitor_flaw_analysis.md`
