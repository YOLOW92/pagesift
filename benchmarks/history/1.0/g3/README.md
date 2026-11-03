# G3: OCR on GPU, measured on B5

2026-11-04, GTX 1070 (8 GiB), 16 threads. B5 is olmOCR-bench `old_scans` + `old_scans_math`, 134 PDFs, one page each.
Script: `run_gpu_b5.sh`. Each configuration ran once. Other jobs were sharing the CPU, so CPU times are not from an idle machine.

Pages per second (higher is better); memory (lower is better); score (higher is better, mean of categories including baseline).

| Configuration | Pages/s | GPU memory needed | nvidia-smi peak increase | Score |
|---|---|---|---|---|
| CPU, small | 0.48 | – | 0 MiB | 0.407 |
| GPU, small | 1.80 | 1397 MiB | 3818 MiB | 0.407 |
| GPU, small, 4 PDFs at a time | 2.54 | 3037 MiB | 6822 MiB | 0.407 |
| CPU, medium | 0.06 | – | 101 MiB | 0.414 |
| GPU, medium | 0.59 | 2288 MiB | 6672 MiB | 0.414 |

"Needed" is live tensors plus weights. The nvidia-smi figure also counts idle buffers kept in the pool and the driver context.

Output is byte-identical between CPU and GPU on all 134 files for small and for medium, and between 1 and 4 PDFs at a time.

Layout and formula models, routed per operator through `Engine::with_gpu_ops` with small text models, measured 10659 ms/file and
0.483 against 9369 ms/file and 0.542 on CPU (lower and higher is better, respectively). The cause was not found, so the
pipeline keeps those models on CPU.

The sm_70 PTX has not run on a real card; only the sm_61 build was exercised here.
