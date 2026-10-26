#!/usr/bin/env python3
"""Pinned source-based capability inventory, with explicit evidence and open gates."""
import hashlib,json
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];SRC=ROOT/'bench/data/six/sources';lock=json.loads((ROOT/'bench/six/tools.lock.json').read_text())['tools'];rows=[]
def add(tool,cap,status,reference,local,acceptance):
 p=SRC/tool/reference;assert p.is_file(),p
 rows.append({'tool':tool,'capability':cap,'status':status,'reference_source':str(p.relative_to(ROOT)),'reference_source_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'local_evidence_or_gap':local,'acceptance_remaining':acceptance})
F='apps/api/src/routes/v2.ts'
for cap,status,local,gate in [
 ('scrape formats / metadata / screenshots','implemented_partial_verification','pagesift-api/types.rs, pagesift-cdp, historical SDK 17 checks','Full option-by-option official SDK contract, malformed requests and large outputs'),
 ('crawl/map/batch lifecycle and cancellation','implemented_partial_verification','pagesift-api/jobs.rs; ROUND4 SDK and recovery evidence','Current official SDK pagination, cancel races and multi-tenant load matrix'),
 ('search and scrape enrichment','implemented_not_compared','pagesift-search and configured search backend','Same provider, same budgets and frozen search results end to end'),
 ('schema extraction / extract job','partial','pagesift-llm supports per-page schema; full extract orchestration parity unverified','Multi-URL extract schema, cost, failure and source attribution'),
 ('persistent browser interact lifecycle','excluded_by_user','Direct user instruction excludes browser interaction from this round','Excluded from current scope; no full-feature parity claim'),
 ('agent autonomous data-gathering API','excluded_by_user','Direct user instruction excludes agent tasks from this round','Excluded from current scope; no full-feature parity claim'),
 ('cloud-only services and optional engine behavior','unverified_external_dependency','Self-hosted tests cannot establish managed cloud parity','Separately provision and test cloud-only services; never replace with OSS component score')]:add('firecrawl',cap,status,F,local,gate)
C='README.md'
for cap,status,local,gate in [
 ('persistent request queue / restart recovery','implemented_partial_verification','crawl queue receipts + ROUND4 97/97 process recovery','Large queue, crash points, disk exhaustion and long-running jobs'),
 ('autoscaling / CPU / memory admission','implemented_partial_verification','pagesift-cdp pool and system resource probing','Same workloads repeated, full process tree RSS, CPU-pressure and throughput'),
 ('adaptive rendering classification','partial','origin/path learning plus thin client-page guard','Reduce measured 3-to-8 unnecessary browser starts without losing JS bodies'),
 ('session pool / stateful cookie reuse / login isolation','partial','Origin-isolated CookieSession and SessionFile; native CLI process tests in SESSION-CORE.md','Managed pool leasing, browser storage, end-to-end authenticated crawl and independent identity benchmarks'),
 ('browser engine breadth','partial','Chrome-family CDP, moli, Lightpanda; no native Firefox/WebKit driver','Required engine matrix with screenshots, actions, iframe and state semantics'),
 ('pluggable datasets / storage / crawler hooks','partial','Rust library and output callbacks, local store','Documented hook ordering, rollback and dataset backend contracts'),
 ('proxy rotation / fingerprint behaviors','not_implemented_product_boundary','Named egress proxies exist; rotation/fingerprint emulation is outside current product policy','Explicit product-scope decision and corresponding tests; cannot mark full parity while omitted')]:add('crawlee',cap,status,C,local,gate)
S='README.md'
for cap,status,local,gate in [
 ('CSS/XPath/text/regex parsing','implemented_partial_verification','pagesift-html/pagesift-extract and historical differential parser checks','Current official parser suites, XPath edge cases and controlled performance'),
 ('stable identity relocation','implemented_bounded','Anchor searches identity across tag changes and rejects duplicates','Independent page changes with labeled identity and ambiguity'),
 ('fuzzy relocation without stable identity','partial','Native FuzzyAnchor, confidence/margin abstention and corroborated container-tag changes; seven-next/tasks-verified controlled 64/64 vs native Scrapling 56/64','Independent changed real pages, more negative/ambiguous cases and calibrated false-match cost; appearance is not identity'),
 ('persistent fetch/browser sessions','partial','Native Rust HTTP cookie lifecycle, retirement and explicit persistence; browser state remains missing','Browser session lifecycle, cross-subdomain SSO and identity/cache integration'),
 ('background API capture','partial','CDP interception guards exist; reusable captured API response extraction unverified','Bounded response capture, secrets handling, iframe/worker associations'),
 ('spider middleware / exports / interactive shell','partial','CLI, Rust SDK, JSONL, CSV, MCP; framework API not equivalent','Reusable handlers, structured exports and pause/resume contracts'),
 ('stealth / challenge handling','not_implemented_product_boundary','Access barrier detection only','Scope decision; cannot claim six-project full feature parity while this is omitted')]:add('scrapling',cap,status,S,local,gate)
K='README.md'
for cap,status,local,gate in [
 ('HTML/frame/HTMX/Link-header discovery','implemented_bounded','ROUND4 controlled graph 8/8','Broader page families and live graph recall'),
 ('literal inline navigation and GET discovery','new_verified_local','script_links.rs; media-core/final-navigation contrast','More JavaScript syntax families; source strings alone do not establish read methods'),
 ('external JS files / jsluice / computed endpoints','partial','Native scoped module graph + opt-in HTTP crawl integration; new graph 12/13 vs Katana -jc -jsl 10/13','Full AST/computed function evaluation, import maps/source maps, task-wide script budgets and independent site graphs'),
 ('headless network discovery','partial','Browser rendering and request interception; network-URL frontier integration incomplete','Controlled worker/XHR/fetch/iframe graph with budgets and URL attribution'),
 ('forms / input extraction / automatic fill','partial','Rust form metadata, external owners/disabled controls and explicit GET planning; new controlled 12/12','Full validation, submitter overrides, scripted/dynamic forms and independently labeled input families'),
 ('scope / traps / throttling / retries','implemented_partial_verification','ROUND4 navigation and wild simulation','More independent site families; report coverage regressions rather than only average'),
 ('technology / knowledge-base / secret extractors','partial','Bounded Rust rules with redacted byte-range findings and custom regex; controlled body/form cases and native Katana KB both detect expected indicators','Broader technology/scanner/page-classifier coverage, endpoint classification, independent labels and provider-validation scope')]:add('katana',cap,status,K,local,gate)
E='README.md'
for cap,status,local,gate in [
 ('content extraction / Markdown','implemented_compared_component','SEVEN-CORE: complete new 545-page component contrast; prior 1355-page results retained as regression evidence','Remaining article boundaries; preserve fresh validation after changes'),
 ('crawl/map/search APIs','implemented_not_compared_full_service','Product API, only Essence component benchmarked','Build Essence complete service, same provider/input and whole-stack cost'),
 ('HTTP/browser fallback','implemented_partial_verification','auto engine and ROUND4 controlled render families','End-to-end on independent live inputs with all subprocesses measured'),
 ('crawl streaming / MCP','implemented_not_compared','pagesift MCP and job streaming interfaces','Side-by-side stream cancellation, backpressure and tool contract'),
 ('browser and extraction option surface','unverified','Individual capabilities exist; no exhaustive equivalence proof','Enumerate Essence configuration and errors, test every supported combination')]:add('essence',cap,status,E,local,gate)
G='README.rst'
for cap,status,local,gate in [
 ('direct HTTP file download','new_verified_local','download.rs and native CLI contrast','Independent file types, proxies and large transfer measurements'),
 ('validated resumable transfer / integrity','new_verified_local','ETag/If-Range, range checks, checksum and SIGKILL evidence','More server variants, concurrent readers, Windows/macOS filesystem behavior'),
 ('stable-ID archive and incremental collection','implemented_bounded','Archive source/id/revision and collect updates/deletions','Connect listing item acknowledgements to file commit before pagination checkpoint'),
 ('site-specific media extractors','partial','Explicit WordPress/Mastodon/Wikimedia/JSON-LD Rust adapters; Mastodon originals 20/20, native gallery-dl 20/20','Pagination/auth/site variants and gallery-dl extractor ecosystem; connect listing delivery to archive transactions'),
 ('authentication / OAuth / browser cookie import','partial','Configured host-scoped credentials; no OAuth or cookie import workflow','Authorized account fixture, token refresh, storage isolation and login expiry'),
 ('HLS/DASH / yt-dlp integration','partial','Native finite HLS/DASH planning, ranged segment archives and local DASH MPD preserving timing; docs/native-core.md','Independent stream families, live/renditions/negative-repeat/SegmentBase/encryption and A/V mux remain outside this implementation'),
 ('postprocessors / metadata / filename templates','partial','Rust deterministic templates, no-overwrite media+sidecar publication, recovery and integrity checks','Full naming/tagging/postprocessor ecosystem, additional filesystems, crash points and independent metadata cases')]:add('gallery-dl',cap,status,G,local,gate)
record={'scope_exclusions':{'authority':'direct user instruction in current turn','capabilities':['persistent browser interact lifecycle','agent autonomous data-gathering API']},'scope':'40 source-based categories; browser interact and agent excluded by direct user request. Seven formerly missing categories now have bounded Rust cores; full functional leadership remains unachieved','selection':'fixed versions from tools.lock.json; no silent scope reduction','status_semantics':'implemented is not proven superior; bounded synthetic evidence is not site-wide or full-service evidence','versions':{k:{'tag':v['tag'],'sha':v['sha']} for k,v in lock.items() if k!='essence-main'},'capabilities':rows,'counts':{s:sum(r['status']==s for r in rows) for s in sorted({r['status'] for r in rows})},'next_work_order':['browser session pool and identity-scoped cache integration','fuzzy relocation as explicit optional API','external JS and network frontier','site adapters plus listing-to-archive transaction','HLS/DASH and postprocessors','independent per-task quality and full-stack performance acceptance']}
(ROOT/'bench/six/full-capabilities.json').write_text(json.dumps(record,indent=2)+'\n')
lines=['# 六项目能力清单与仍未完成的验收','',record['scope'],'','本清单按固定源码中的能力类别整理，不是全部函数/参数的穷举。每项都有源码文件哈希；“已有实现”与“优于竞品”是不同状态。未实现、策略上不做、需外部服务的能力仍计作总体目标未完成，不从分母中删掉。','']
for tool in record['versions']:
 lines += ['## '+tool+' '+record['versions'][tool]['tag'],'','| 能力 | 当前状态 | 实现/证据或缺口 | 后续验收 |','|---|---|---|---|']
 for r in rows:
  if r['tool']==tool:lines.append('| '+' | '.join(r[k].replace('|','/') for k in ['capability','status','local_evidence_or_gap','acceptance_remaining'])+' |')
 lines.append('')
lines+=['## 如何把“领先”变成可验证的结论','','每项先冻结参考版本、任务输入、通过条件和成本边界；功能正确后，另取独立输入比质量、稳定性、吞吐、整树内存和运维代价。完整服务、离线组件和第三方托管服务分别计账。单个小样本成功不填成全站支持，未测能力不填成已领先。','','优先顺序：'+ ' → '.join(record['next_work_order'])+'。','','当前实现、公开对照与限制见 [评测报告](REPORT.md) 和 [证据索引](evidence/README.md)。早期逐轮记录仅保留在本地归档。']
(ROOT/'bench/six/CAPABILITIES.md').write_text('\n'.join(lines)+'\n');print(json.dumps(record['counts']))
