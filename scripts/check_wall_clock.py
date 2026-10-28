#!/usr/bin/env python3
"""Fail when a test reads the wall clock without saying why a slow machine cannot break it.

Test code is everything under `crates/*/tests/`, `src/**/tests_*.rs`, items under `#[cfg(test)]`
inside ordinary source files (inline `mod tests { … }`, test-only helpers) and the files of
out-of-line test modules (`#[cfg(test)] mod tests;`). Comments and string literals are ignored.

The macOS CI runner has few cores and runs other tests at the same time; a test that judges
"fast enough" by the wall clock fails there now and then, each time a different one (seven
red runs in 0.80.x). Tests should not need the wall clock:

- complexity guards ("this input must not take quadratic time"): measure the calling thread's
  CPU time with `pagesift_testkit::cpu_time`;
- timing logic (queues, thresholds, backoff): run on tokio's paused clock
  (`#[tokio::test(start_paused = true)]`), which only moves when every task waits;
  `tokio::time::Instant::now()` inside such a test is not a wall-clock read;
- ordering ("the fast host finished before the slow one"): compare events with each other,
  not with a fixed number of milliseconds.

Where the wall clock is the point (a deadline the product enforces, a polling loop's upper
bound), put `// wall-clock: <reason>` on the line or in the three lines above `Instant::now()`,
saying why the bound holds on a busy machine (for example: it sits far above the expected time
and below the time the bug would take). A file where every use is fine can say
`wall-clock-file: <reason>` once.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
LOOKBACK = 3


def blank(src):
    """Return `src` with comments, string and char literals replaced by spaces (newlines kept),
    so that braces and `Instant::now()` are only seen in code."""
    out = list(src)
    i, n = 0, len(src)

    def wipe(a, b):
        for k in range(a, b):
            if out[k] != '\n':
                out[k] = ' '

    while i < n:
        c = src[i]
        if src.startswith('//', i):
            j = src.find('\n', i)
            j = n if j < 0 else j
            wipe(i, j)
            i = j
        elif src.startswith('/*', i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith('/*', j):
                    depth, j = depth + 1, j + 2
                elif src.startswith('*/', j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            wipe(i, j)
            i = j
        elif (m := re.match(r'b?r(#*)"', src[i:i + 300])) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == '_')):
            close = '"' + m.group(1)
            j = src.find(close, i + m.end())
            j = n if j < 0 else j + len(close)
            wipe(i, j)
            i = j
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == '\\' else 1
            wipe(i, j + 1)
            i = j + 1
        elif c == "'":
            # 字符字面量（'x'、'\n'、'\u{7b}'）；生命周期（'a）照常保留
            if i + 1 < n and src[i + 1] == '\\':
                j = src.find("'", i + 2)
                wipe(i, j + 1)
                i = j + 1
            elif i + 2 < n and src[i + 2] == "'":
                wipe(i, i + 3)
                i += 3
            else:
                i += 1
        else:
            i += 1
    return ''.join(out)


CFG_TEST = re.compile(r'#\[cfg\(test\)\]')


def test_ranges(code):
    """Character ranges of items under `#[cfg(test)]` (inline test modules, test-only helpers),
    and the names of out-of-line test modules (`#[cfg(test)] mod tests;`)."""
    ranges, external = [], []
    for m in CFG_TEST.finditer(code):
        j = m.end()
        # 后面的其他属性、可见性、关键字，直到条目的 `{` 或 `;`
        k = j
        while k < len(code) and code[k] not in '{;':
            k += 1
        if k >= len(code):
            continue
        head = code[j:k]
        if code[k] == ';':
            if mm := re.search(r'\bmod\s+(\w+)\s*$', head):
                external.append(mm.group(1))
            continue
        depth, e = 0, k
        while e < len(code):
            if code[e] == '{':
                depth += 1
            elif code[e] == '}':
                depth -= 1
                if depth == 0:
                    break
            e += 1
        ranges.append((m.start(), e))
    return ranges, external


def paused_ranges(code):
    """Bodies of `#[tokio::test(start_paused = true)]` functions."""
    out = []
    for m in re.finditer(r'#\[tokio::test\([^)]*start_paused\s*=\s*true[^)]*\)\]', code):
        k = code.find('{', m.end())
        if k < 0:
            continue
        depth, e = 0, k
        while e < len(code):
            depth += {'{': 1, '}': -1}.get(code[e], 0)
            if depth == 0:
                break
            e += 1
        out.append((k, e))
    return out


def module_file(parent, name):
    base = parent.parent if parent.name in ('mod.rs', 'lib.rs', 'main.rs') else parent.with_suffix('')
    for f in (base / f'{name}.rs', base / name / 'mod.rs'):
        if f.is_file():
            return f
    return None


def test_code():
    """(file, character ranges that are test code; None means the whole file)."""
    whole = set()
    for crate in sorted((ROOT / 'crates').iterdir()):
        tests = crate / 'tests'
        if tests.is_dir():
            whole.update(tests.rglob('*.rs'))
    partial = {}
    for crate in sorted((ROOT / 'crates').iterdir()):
        src = crate / 'src'
        if not src.is_dir():
            continue
        for f in sorted(src.rglob('*.rs')):
            if f.name.startswith('tests_'):
                whole.add(f)
                continue
            code = blank(f.read_text(encoding='utf-8'))
            ranges, external = test_ranges(code)
            if ranges:
                partial[f] = (code, ranges)
            for name in external:
                if (mf := module_file(f, name)) is not None:
                    whole.add(mf)
                    if mf.name == 'mod.rs':
                        whole.update(mf.parent.rglob('*.rs'))
    for f in sorted(whole):
        partial.pop(f, None)
        yield f, None
    for f, (code, ranges) in sorted(partial.items()):
        yield f, (code, ranges)


def unexplained(path, scope):
    lines = path.read_text(encoding='utf-8').splitlines()
    if any('wall-clock-file:' in line for line in lines):
        return []
    code, ranges = scope if scope else (blank('\n'.join(lines)), None)
    starts = [0]
    for line in code.split('\n'):
        starts.append(starts[-1] + len(line) + 1)
    paused = paused_ranges(code)
    tokio_instant = re.search(r'use tokio::time::(Instant\b|\{[^}]*\bInstant\b)', code)
    out = []
    for m in re.finditer(r'Instant::now\(\)', code):
        if ranges is not None and not any(a <= m.start() < b for a, b in ranges):
            continue
        # tokio 的暂停时钟上的 Instant 不是墙钟
        tokio = tokio_instant or code[max(0, m.start() - 13):m.start()] == 'tokio::time::'
        if tokio and any(a <= m.start() < b for a, b in paused):
            continue
        i = next(k for k in range(len(starts) - 1) if starts[k] <= m.start() < starts[k + 1])
        window = lines[max(0, i - LOOKBACK):i + 1]
        if not any('wall-clock:' in w for w in window):
            out.append((i + 1, lines[i].strip()))
    return out


def main():
    bad = [(path, n, line) for path, scope in test_code() for n, line in unexplained(path, scope)]
    for path, n, line in bad:
        print(f'{path.relative_to(ROOT)}:{n}: {line}')
    if bad:
        print(f'\n{len(bad)} wall-clock reads in tests without a `// wall-clock:` reason; '
              'see the top of scripts/check_wall_clock.py', file=sys.stderr)
        return 1
    print('wall-clock reads in tests: all explained')
    return 0


if __name__ == '__main__':
    sys.exit(main())
