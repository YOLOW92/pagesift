# Publication date results, 2026-10-21

Machine: Intel Core i7-10700, 15 GB RAM, Ubuntu (Linux 7.0). pagesift 0.12.0 (this commit). htmldate 1.11.0, trafilatura (with htmldate 1.11.0), newspaper4k 0.9.6, goose3 3.1.22.

1000 annotated pages (512 in the hold-out half), no URL given. prec/rec/acc/F1: higher is better; ms/page: lower is better. pagesift's ms/page is the whole `process()` call; its date search alone takes 0.06 ms/page on this set (`date_scan`).

```
1000 pages (512 hold); URL given: False
tool                   half   prec↑   rec↑   acc↑    F1↑  ms/page↓
pagesift               all    0.909  0.991  0.902  0.948      0.71
pagesift               tune   0.912  0.996  0.908  0.952      0.74
pagesift               hold   0.907  0.987  0.896  0.945      0.67
htmldate (fast)        all    0.924  0.929  0.863  0.926      2.00
htmldate (fast)        tune   0.928  0.930  0.867  0.929      2.07
htmldate (fast)        hold   0.921  0.928  0.859  0.924      1.94
htmldate (extensive)   all    0.908  0.993  0.903  0.949      4.04
htmldate (extensive)   tune   0.914  0.996  0.910  0.953      4.56
htmldate (extensive)   hold   0.904  0.991  0.896  0.945      3.55
trafilatura            all    0.907  0.992  0.901  0.948      9.79
trafilatura            tune   0.912  0.996  0.908  0.952      9.85
trafilatura            hold   0.903  0.989  0.895  0.944      9.74
newspaper4k            all    0.912  0.728  0.680  0.810     11.74
newspaper4k            tune   0.913  0.739  0.691  0.817     12.08
newspaper4k            hold   0.910  0.718  0.670  0.802     11.41
goose3                 all    0.830  0.671  0.590  0.742     71.80
goose3                 tune   0.824  0.692  0.602  0.752     72.28
goose3                 hold   0.836  0.652  0.578  0.733     71.35
```

pagesift before this work (meta tags, JSON-LD and `<time>` only, the same sources Firecrawl reads): F1 0.789 (all), 0.778 (hold-out).
