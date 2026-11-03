# Browser fingerprint comparison

Compares what a server receives from `pagesift --browser-fingerprint chrome` with what it receives from a local Google Chrome.

| Script | What it does | Network |
|---|---|---|
| `run.sh <pagesift>` | Records Chrome and pagesift against `capture.py` over HTTP/2 and HTTP/1.1, then `compare.py` compares them item by item | Local only |
| `interop.sh <pagesift>` | pagesift against `openssl s_server`: X25519MLKEM768 only, X25519 only, P-256 only (HelloRetryRequest), TLS 1.2, a server that picks a CBC suite, and an untrusted CA | Local only |
| `live.py <pagesift> [--chrome]` | Reads public fingerprint echo pages and a few common sites, one page each | Public internet |

`capture.py` keeps the raw ClientHello, then lets the system OpenSSL finish the handshake and records HTTP/2 frames and decoded headers. `ja4.py` computes JA4 and the Akamai-format HTTP/2 fingerprint. `hpack_tables.py` is generated from RFC 7541.

Results are in `results/<date>/`. `compare.txt` lists every compared item. Reference: Chrome 156.0.8078.79, Linux, `--headless=new`.

Chrome's headless user agent contains `HeadlessChrome`. `compare.py` replaces it with `Chrome` before comparing, because the profile sends the desktop value.

To add a Chrome version: record it with `run.sh`, add a profile in `crates/pagesift-net/src/fingerprint/profile.rs`, and point the `chrome` name at it.
