#!/usr/bin/env python3
"""Render checked-in SVGs from frozen benchmark summaries (stdlib only)."""
import json
import tomllib
from pathlib import Path
from html import escape

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / 'benchmarks/history/0.70/six/evidence/2026-10-22'
OUT = ROOT / 'docs/assets'
S = json.loads((DATA / 'summary.json').read_text())
VERSION = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']


def text(x, y, value, size=16, fill='#dce5ee', weight=400):
    return f'<text x="{x}" y="{y}" font-size="{size}" fill="{fill}" font-weight="{weight}">{escape(str(value))}</text>'


def rect(x, y, w, h, fill, radius=6):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" fill="{fill}"/>'


def save(name, height, title, desc, body):
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="{height}" viewBox="0 0 1000 {height}" role="img" aria-labelledby="title desc">
<title id="title">{escape(title)}</title><desc id="desc">{escape(desc)}</desc>
<g font-family="system-ui, -apple-system, Segoe UI, sans-serif">{rect(0, 0, 1000, height, '#101b2a', 16)}{body}</g></svg>\n'''
    (OUT / name).write_text(svg)


body = text(38, 52, 'pagesift '+VERSION, 32, '#ffffff', 700)
body += text(38, 82, 'One Rust engine. From page to durable, usable data.', 18, '#a6b8ca')
for x, title, lines in [
    (38, 'ACQUIRE', ['HTTP + bounded discovery', 'Render when needed', 'Origin-isolated sessions']),
    (358, 'UNDERSTAND', ['Main content + Markdown', 'Identity + fuzzy anchors', 'Forms + media adapters']),
    (678, 'DELIVER', ['Incremental checkpoints', 'Verified resumable archives', 'Finite HLS / DASH VOD'])]:
    body += rect(x, 113, 284, 150, '#1d2e43')
    body += text(x+18, 145, title, 15, '#56d9c0', 700)
    for i, line in enumerate(lines):
        body += text(x+18, 179+i*27, line, 15)
for x in [326, 646]:
    body += text(x, 200, '→', 25, '#56d9c0')
body += text(38, 300, 'CLI  /  Firecrawl-compatible HTTP API  /  MCP  /  native Python + Node', 16)
body += text(38, 330, 'New collection and archive cores: Rust APIs + CLI. Binding surfaces remain scoped.', 14, '#a6b8ca')
save('overview.svg', 355, 'Pagesift '+VERSION+' architecture overview', 'Acquire pages, understand content and deliver durable data through Rust modules. New cores are exposed through Rust and CLI.', body)

body = text(38, 47, '545 pages, two separate comparisons', 27, '#ffffff', 700)
body += text(38, 76, 'WebMainBench calibrated subset · diagnostic token-multiset F1 · higher is better', 15, '#a6b8ca')
for y, heading, rows, loss in [
    (115, 'COMPONENTS · original URL context', [('Pagesift', S['components_original_url_context']['pagesift']['f1'], '#56d9c0'), ('Essence v0.3.0', S['components_original_url_context']['essence']['f1'], '#7ca8ed')], '414 wins / 14 ties / 117 losses'),
    (330, 'COMPLETE SERVICES · replay URL context', [('Pagesift', S['complete_services_replay_context']['pagesift']['f1'], '#56d9c0'), ('Firecrawl v2.11.0 self-hosted', S['complete_services_replay_context']['firecrawl']['f1'], '#7ca8ed')], '427 wins / 22 ties / 96 losses')]:
    body += text(38, y, heading, 14, '#a6b8ca', 700)
    for i, (label, value, color) in enumerate(rows):
        yy = y+28+i*46
        body += text(38, yy+20, label, 14)
        body += rect(300, yy, 560, 28, '#1d2e43')+rect(300, yy, value*560, 28, color)
        body += text(884, yy+21, f'{value:.4f}', 18, '#ffffff', 600)
    body += text(300, y+139, '0', 12, '#a6b8ca')+text(850, y+139, '1.0', 12, '#a6b8ca')
    body += text(300, y+163, 'Pagesift: '+loss, 15, '#f6c577')
body += text(38, 543, 'All 545 retained; reference errors: 1 each; Pagesift errors: 0. Not the official leaderboard.', 14, '#a6b8ca')
body += text(38, 570, 'Frozen pre-release candidate. Quality only; these panels do not establish a speed ranking.', 14, '#a6b8ca')
save('extraction-quality.svg', 598, 'Main-content quality with remaining page-level losses', 'Pagesift component F1 .8435 versus Essence .7966 with 117 losses. Pagesift service .8437 versus self-hosted Firecrawl .7999 with 96 losses. Separate URL contexts, not official leaderboard scores.', body)

body = text(38, 48, 'Controlled tasks, with explicit boundaries', 27, '#ffffff', 700)
body += text(38, 77, 'Synthetic task families · fixed reference modes · not a full-product ranking', 15, '#a6b8ca')
rows = [
    ('Navigation graph / 13', [('Pagesift',13,13,'#56d9c0'), ('Katana -jc -jsl',10,13,'#7ca8ed'), ('Crawlee Cheerio',2,13,'#b7a3e8')]),
    ('Fuzzy relocation / 64', [('Pagesift',64,64,'#56d9c0'), ('Scrapling adaptive',56,64,'#7ca8ed')]),
    ('Mastodon originals / 20', [('Pagesift',20,20,'#56d9c0'), ('gallery-dl',20,20,'#7ca8ed')])]
y = 116
for label, entries in rows:
    body += text(38, y+20, label, 16, '#ffffff', 600)
    for i, (name, value, total, color) in enumerate(entries):
        yy=y+i*39
        body += text(300, yy+18, name, 14)
        body += rect(500, yy, 350, 25, '#1d2e43')+rect(500, yy, 350*value/total, 25, color)
        body += text(878, yy+19, f'{value}/{total}', 16)
    y += len(entries)*39+32
body += text(38, 523, 'Navigation misses dynamic evaluation. Relocation is not independent real-site accuracy.', 14, '#a6b8ca')
body += text(38, 550, 'Media task: 8 Mastodon posts, byte-checked originals. Crawlee browser mode is not ranked.', 14, '#a6b8ca')
save('controlled-tasks.svg', 580, 'Controlled navigation, relocation and media comparisons', 'Navigation 13/13, Katana 10/13, Crawlee Cheerio 2/13. Fuzzy 64/64 versus Scrapling 56/64. Media both 20/20. Synthetic bounded tasks only.', body)

body = text(38, 48, 'Capability coverage, without hiding the gaps', 27, '#ffffff', 700)
body += text(38, 77, '40 source-based categories · implementation status is not proof of superiority', 15, '#a6b8ca')
c=S['capabilities']
counts=[('Implemented, bounded or locally verified',sum(v for k,v in c.items() if k.startswith('implemented') or k=='new_verified_local'),'#56d9c0'),('Partial',c['partial'],'#f6c577'),('Unverified / external dependency',c['unverified']+c['unverified_external_dependency'],'#7ca8ed'),('Product boundaries',c['not_implemented_product_boundary'],'#b7a3e8'),('Excluded by user',c['excluded_by_user'],'#8596a8')]
assert sum(v for _,v,_ in counts)==40
x=38
for _, count, color in counts:
    body += rect(x,110,count/40*924,32,color,0);x+=count/40*924
for i,(label,count,color) in enumerate(counts):
    y=180+i*37;body += rect(38,y-14,16,16,color,3)+text(68,y,label,16)+text(930,y,count,18,'#ffffff',600)
body += text(38, 393, '7 formerly missing categories now have bounded native Rust cores; 17 remain partial.', 15, '#f6c577')
body += text(38, 422, 'Browser interaction + autonomous Agent tasks excluded. Anti-bot bypass / rotation not offered.', 14, '#a6b8ca')
save('capability-coverage.svg',450,'Forty category capability inventory', '17 implemented or bounded categories,17 partial,2 unverified,2 product boundaries,2 excluded by user. No full functional leadership claim.',body)
print('Rendered 4 evidence-backed SVGs')
