#!/usr/bin/env python3
"""Fail when a test reads the wall clock without saying why a slow machine cannot break it.

The macOS CI runner has few cores and runs other tests at the same time; a test that judges
"fast enough" by the wall clock fails there now and then, each time a different one (seven
red runs in 0.80.x). Tests should not need the wall clock:

- complexity guards ("this input must not take quadratic time"): measure the calling thread's
  CPU time with `pagesift_testkit::cpu_time`;
- timing logic (queues, thresholds, backoff): run on tokio's paused clock
  (`#[tokio::test(start_paused = true)]`), which only moves when every task waits;
- ordering ("the fast host finished before the slow one"): compare events with each other,
  not with a fixed number of milliseconds.

Where the wall clock is the point (a deadline the product enforces, a polling loop's upper
bound), put `// wall-clock: <reason>` on the line or in the three lines above `Instant::now()`,
saying why the bound holds on a busy machine (for example: it sits far above the expected time
and below the time the bug would take). A file where every use is fine can say
`wall-clock-file: <reason>` once.
"""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
LOOKBACK = 3


def test_files():
    for crate in sorted((ROOT / 'crates').iterdir()):
        tests = crate / 'tests'
        if tests.is_dir():
            yield from sorted(tests.rglob('*.rs'))
        src = crate / 'src'
        if src.is_dir():
            yield from sorted(src.rglob('tests_*.rs'))


def unexplained(path):
    lines = path.read_text(encoding='utf-8').splitlines()
    if any('wall-clock-file:' in line for line in lines):
        return []
    out = []
    for i, line in enumerate(lines):
        if 'Instant::now()' not in line or line.lstrip().startswith('//'):
            continue
        window = lines[max(0, i - LOOKBACK):i + 1]
        if not any('wall-clock:' in w for w in window):
            out.append((i + 1, line.strip()))
    return out


def main():
    bad = [(path, n, line) for path in test_files() for n, line in unexplained(path)]
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
