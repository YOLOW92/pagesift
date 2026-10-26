# Media streams and WARC

[`benchmark.py`](benchmark.py) is the 0.40 baseline for stream ingestion (yt-dlp, FFmpeg, PageSift)
and WARC archiving (warcio, PageSift); [`fixtures/`](fixtures/) holds the HLS and DASH test streams and
[`baseline_results.json`](baseline_results.json) the recorded results.

## Install and run

```bash
python3 -I benchmarks/media/benchmark.py     # rewrites baseline_results.json
```

It needs `ffmpeg` on the PATH and a Python environment at `benchmarks/data/six/venv` with yt-dlp
(2026.10.21 on the benchmark machine) and warcio 1.8.1.

## Dry run (2026-10-27)

Ran end to end in under 10 s (HLS and DASH with yt-dlp and FFmpeg, 1,000-record WARC with warcio);
the rewritten `baseline_results.json` was not kept. The script measures only yt-dlp, FFmpeg and warcio:
there is no PageSift run in it, despite its docstring.
