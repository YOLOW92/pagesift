# 0.70 showdown runners (Docker)

**These depend on Docker images that have been removed. They are kept as a historical record only and
cannot be rerun as they are.**

`showdown_run.py` ran one tool inside a container under a fixed memory cap; `d1_run.sh` (WCXB content
extraction, D1) and `d9_run.sh` (dynamic rendering, D9) ran the 0.70 showdown with it or with
`docker run` directly. All three use the `pagesift-bench:*` images (`native`, `pytools`, `browser`, …),
whose Dockerfiles were in `bench/docker`; by the owner's decision benchmarks no longer ship Docker
images, and reproduction is described with install commands in each domain's README instead
([benchmarks/README.md](../../../README.md#tool-environments)).

The scripts still mount the per-domain script directories (`benchmarks/web-extraction/scripts`,
`benchmarks/crawl/scripts`) as `/tools`. Results of these runs are in [../results/](../results/) and the
reports in [docs/history/](../../../../docs/history/README.md) (`0.70_showdown_D*.md`).
