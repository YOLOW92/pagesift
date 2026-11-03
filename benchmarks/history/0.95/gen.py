"""Timelines for the 0.95 monitor evaluation (seed 950), from the 0.93 corpus of live pages.

Each page gets a true rate of substantive change λ, log-uniform between once per 6 hours and once per
14 days, and noise-only changes (date stamp, view counter, rotating ad slot) at 4λ plus a daily
date roll-over. Events are Poisson over a 30-day horizon. The simulator applies the edits itself:
substantive → a new paragraph in the body, noise → the `ps-noise` block changes.

    python -I gen.py > ../../data/0.95/timelines.jsonl
"""

import json
import math
import pathlib
import random
import sys

DATA = pathlib.Path(__file__).resolve().parents[2] / "data"
SEED = 950
HORIZON = 30 * 86400
DAY = 86400


def poisson_times(rng, rate, horizon):
    t, out = 0.0, []
    while True:
        t += rng.expovariate(rate)
        if t >= horizon:
            return out
        out.append(t)


rng = random.Random(SEED)
for path in sorted((DATA / "0.93" / "live").glob("*.html")):
    html = path.read_bytes().decode("utf-8", "replace")
    lam = math.exp(rng.uniform(math.log(1 / (14 * DAY)), math.log(1 / (6 * 3600))))
    events = [{"t": t, "kind": "substantive"} for t in poisson_times(rng, lam, HORIZON)]
    events += [{"t": t, "kind": "noise"} for t in poisson_times(rng, 4 * lam, HORIZON)]
    events += [{"t": d * DAY, "kind": "noise"} for d in range(1, HORIZON // DAY)]
    events.sort(key=lambda e: e["t"])
    print(json.dumps({"url": path.stem, "rate": lam, "horizon": HORIZON, "html": html, "events": events}))
