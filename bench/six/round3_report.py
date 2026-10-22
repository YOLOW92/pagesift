#!/usr/bin/env python3
"""Assemble recovery evidence, retaining scope and unsuccessful setup attempts."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT=Path(__file__).resolve().parents[2]
RESULTS=ROOT/'bench/six/results'
OUT=RESULTS/'round3'


def main():
    ci_dir=sorted(RESULTS.glob('*round3-full-ci-final*'))[-1]
    ci=json.loads((ci_dir/'run.json').read_text());log=(ci_dir/'stdout.log').read_text()
    assert ci['status']=='completed' and 'all checks passed' in log
    process=json.loads((OUT/'process-recovery-v3/summary.json').read_text())
    identity=json.loads((OUT/'item-identity-v2/summary.json').read_text())
    replay=json.loads((OUT/'replay/summary.json').read_text())
    sdk=[json.loads((OUT/f'sdk/{n}.json').read_text()) for n in ('python','js')]
    assert all(c['ok'] for s in sdk for c in s['checks'])
    tests=sum(map(int,re.findall(r'test result: ok\. (\d+) passed',log)))
    patch=subprocess.check_output(['git','diff','--binary','HEAD'],cwd=ROOT)
    for name in ['crates/pagesift/src/discovery.rs','crates/pagesift-store/src/items.rs','crates/pagesift-store/examples/item_archive.rs']:
        r=subprocess.run(['git','diff','--no-index','--binary','/dev/null',name],cwd=ROOT,capture_output=True)
        assert r.returncode==1;patch+=r.stdout
    (OUT/'candidate-cumulative.patch').write_bytes(patch)
    summary={'round':3,'rust_tests_and_doctests_passed':tests,'sdk_checks_passed':sum(len(s['checks']) for s in sdk),
             'ci':str(ci_dir.relative_to(ROOT)),'recovery':process,'item_identity':identity,'replay':replay,
             'release_acceptance_complete':False}
    (OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    crawlee=process['crawlee']
    text=f'''# 第三轮：增量身份与崩溃恢复 — 2026-10-22

本轮完成一个已复现的恢复缺陷修复，并新增显式的 Rust 条目完成记录接口。没有把 URL 缓存包装成站点条目归档，也没有声称已完成真实站点的自动增量同步。修改与结果保留本地，未提交、推送或发布。

## 已复现并修复的恢复窗口

原实现先追加页面/错误结果，再单独写入 done 集合。两条记录之间退出时，结果已保存而 done 缺失，启动后会把该任务再次放入队列。确定性测试构造这一存储边界：改前批量任务原本应完成 2 项，却计为 3 项，失败记录保留在 `*round3-receipt-before*`。

本轮自研修复把任务身份随结果一起写入**同一条带 CRC 的日志记录**，done 集合作为可重建索引。重启在启动任何 worker 前，按每批 256 条扫描结果并修复缺失 done；索引修复失败则不启动该任务。公开 Document/JobError 解码不包含内部字段 `_pagesiftDone`。

验收矩阵覆盖 crawl/batch × 成功/错误 × 新回执/旧格式，共 8 种组合，包含相同 URL 的两个批量下标，检查完成数、结果集合、唯一性、错误和内部字段不泄漏。旧格式依赖其已有 done 标记；**无法追溯修复旧二进制已经写下且缺失身份信息的崩溃边界**。外部 webhook 或目标系统的 exactly-once 交付不在本修复承诺内。

## 真实进程 SIGKILL 复测

同一受控 HTTP 站点有 80 个唯一页面，两次并发请求。第一次确认至少 15 个已交付结果后直接 SIGKILL，保留真实存储目录，重新启动；Pagesift 完成后再重启一次检查结束任务不会重新抓取。Crawlee 使用原生本地 RequestQueue + Dataset，没有额外结果去重层。

| 工具 | 最终结果数 | 唯一结果数 | 已保存 URL 在重启后再次请求 |
|---|---:|---:|---:|
| Pagesift 第二轮冻结二进制 | {process['pagesift-baseline']['delivered']} | {process['pagesift-baseline']['unique_delivered']} | {process['pagesift-baseline']['already_saved_urls_requested_after_restart']} |
| Pagesift 本轮候选 | {process['pagesift-candidate']['delivered']} | {process['pagesift-candidate']['unique_delivered']} | {process['pagesift-candidate']['already_saved_urls_requested_after_restart']} |
| Crawlee 原生持久队列与 Dataset | {crawlee['delivered']} | {crawlee['unique_delivered']} | {crawlee['already_saved_urls_requested_after_restart']} |

Pagesift 改前和改后此场景均完整交付 80 页，各产生 82 次内容请求，多出的 2 次是 SIGKILL 时在途且尚未完成的请求。**本轮收益由确定性的写入边界测试证明，不能把常规恢复原本成功归功于本轮修复**。请求允许重试，不能宣传成“网络请求恰好一次”。

[原始访问、服务日志与最终输出](results/round3/process-recovery-v3/summary.json)。该场景没有注入断电、文件系统故障、磁盘写满或远程存储故障，不代表所有部署条件下的无丢失保证，也不发布恢复速度排行榜。

## 条目身份接口与 gallery-dl 对照

新增 `Store::items()`，提供 `matches(source, item, revision)`、`commit(...)`、`forget(...)`：

- 来源和条目 ID 按长度前缀编码，避免简单字符串拼接冲突；传输 URL 不参与身份。
- 空版本表示身份去重，显式版本令牌允许检测同一条目的更新。令牌由调用方提供，可以是业务版本、ETag 或内容哈希；并非系统自动判断内容变化。
- 成功交付后调用 commit，单条记录完成持久化；遵循 Store 的 SyncPolicy。未 commit 的条目仍会重试。没有检查与预占的原子租约，多个消费者需要调度协调或幂等交付目标。
- 条目记录与缓存隔离，不受缓存容量淘汰；不会因为条目暂未出现在列表中就自动标记删除。forget 是显式操作。

真实 gallery-dl DownloadArchive 与 Rust Store 以相同来源/条目 ID 做身份去重：正常关闭并重开，URL 全部更换后双方均识别 **20/20** 个已处理条目。Rust 端另测版本更新、SIGKILL 前已 fsync 的版本保留、未 commit 条目重试和 forget 重开保留；这些是扩展语义，**不把 gallery-dl 默认身份归档不跟踪版本写成其故障**。

[归档实验](results/round3/item-identity-v2/summary.json)。Rust 使用 SyncPolicy::Always，gallery-dl 使用正常 close；不比较断电持久性、速度或数据库成本。Rust 单元测试还强制造成哈希碰撞、实际压缩非空段、缓存容量压力、Unicode 和复合键冲突，随后重开检查记录。

**使用边界：这是 Rust 库接口和示例，不是新增 Firecrawl HTTP 参数，也没有自动接入普通 crawl 的业务条目提取。** 页面 URL、业务条目和内容版本仍是三种身份。站点专用 extractor、真实分页 API 的断点及删除同步仍待后续工作负载验证。

## 回归、来源与资源

- 全量本地 CI **{tests} 项 Rust 测试与文档测试通过**；fmt、clippy、Rust 1.88、cargo-deny、正文及爬取性能门槛通过。[日志]({ci_dir.relative_to(ROOT/'bench/six').as_posix()}/stdout.log)。
- 官方 Python/JS SDK 共 **{summary['sdk_checks_passed']} 项通过**，含持久任务相关 API 轮询、取消和错误映射。
- 冻结 181 篇 AEB + 12 个图片样例，同源回放双方各 193 次：Markdown **193/193 字节一致**，图片 **193/193 一致**，links 变化 **{replay['links_changed']}**。第一轮正文 14 个落后案例和改版重定位仍未修复；新功能不等于那些差距已经消失。
- 参考 gallery-dl v1.32.17 的归档语义及 Crawlee v3.18.2 的请求队列确认/租约机制，版本与源码锁沿用 [tools.lock.json](tools.lock.json)。本轮没有复制对手实现，存储和恢复修改均为自研 Rust。
- 继续使用共享 8 逻辑 CPU、8 GiB RAM、0 swap 上限；磁盘保留阈值 10 GiB。结束后仅清理确认闲置的 debug 编译产物，保留原始结果、候选和基线二进制。

所有失败记录保留：第一次进程实验未打开整个站点范围，只抓了起始路径，因此不计为恢复质量失败；v2 对 Crawlee 的等待预算小于原生锁租约释放所需时间，延长预算后 v3 采用原生恢复。首个归档计数脚本未将 gallery-dl 返回的 truthy SQLite tuple 转成 bool，修正后使用新目录重跑。Unicode 测试的初始字节串语法已改为 UTF-8 as_bytes。全量 CI 后进一步强化了“确实发生段压缩”的测试，并重新全量运行。

累计补丁 [candidate-cumulative.patch](results/round3/candidate-cumulative.patch)，SHA-256 `{hashlib.sha256(patch).hexdigest()}`；结构化摘要见 [summary.json](results/round3/summary.json)。完整独立验收（干净克隆、新样本及更广泛旧领域复测）仍属于第四轮；72 小时稳定性未验证。
'''
    (ROOT/'bench/six/ROUND3.md').write_text(text)
    print(json.dumps(summary,indent=2))


if __name__=='__main__':main()
