# 0.95 monitor: adaptive revisits vs fixed polling

Measured 2026-11-03, virtual time (no real waiting), `crates/pagesift/examples/monitor_sim.rs`
built with `--profile release-fast`; timelines from `gen.py` (seed 950).

- **Pages**: the 27 live pages of the 0.93 corpus as the base HTML.
- **Changes**: per page a true substantive change rate λ, log-uniform between once per 6 hours and
  once per 14 days; substantive changes add a paragraph inside the main content. Noise-only changes
  (a date and clock-time stamp, a view counter, a rotating "Sponsored:" slot) happen at 4λ plus a
  daily roll-over. Poisson event times over 30 days: 779 substantive changes, about 5000 noise changes.
- **Detection**: every check converts the page to Markdown and diffs it against the previous check
  with the monitor's noise categories (date, relative time, count, advertisement); only substantive
  differences notify — the same code path as the server.
- **Policies**: *adaptive* is the server's rule — per URL the δ rule (next check where the posterior
  probability of an unseen change reaches δ = 0.05, Gamma(1, 1 day) prior, ≥ 5 min) sets the
  request budget, which is then spread over the monitor's URLs as T_i ∝ λ_i^{-1/2}; all 27 pages are one
  monitor. *fixed* polls every page at one interval chosen so that the total number of requests is
  the same (1.54 h). *adaptive_per_url* is the δ rule alone, for reference.

| Metric | Direction | adaptive | fixed (same requests) | δ rule alone |
|---|---|---:|---:|---:|
| requests | — | 12 680 | 12 663 | 12 639 |
| mean detection delay | ↓ | **0.65 h** | 0.80 h | 0.90 h |
| false-alarm rate (notifications without a substantive change since the last check) | ↓ | 2.1% (14 of 682) | 1.8% (12 of 670) | 1.8% (12 of 684) |
| miss rate (substantive changes not reported by the next check) | ↓ | 9.4% | 9.6% | 9.1% |
| requests per detected change | ↓ | 17.96 | 17.99 | 17.85 |

All remaining false alarms come from one page (news.ycombinator.com, table layout) whose
main-content region flips when the injected noise block changes; they occur under every policy.
14 vs 12 false alarms is not a significant difference (two-proportion test, p ≈ 0.7). Misses are
mostly changes after the last check before the horizon ends.

The δ rule alone puts intervals proportional to 1/λ; with the same request budget that gives the same
expected delay as uniform polling (Σλ_i·T_i/2 / Σλ_i = N/(2R) for T_i ∝ 1/λ_i), which is why it is
slower here. The square-root allocation is the delay-optimal split of the same budget.
