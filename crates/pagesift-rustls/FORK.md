# pagesift-rustls

This crate is [rustls](https://github.com/rustls/rustls) 0.23.46 with one added hook. All credit for the TLS implementation goes to the rustls authors. The upstream license terms apply: `Apache-2.0 OR ISC OR MIT` (`LICENSE-APACHE`, `LICENSE-ISC`, `LICENSE-MIT`).

Only pagesift's optional browser fingerprint mode uses it. The default pagesift network path uses the upstream `rustls` crate.

## Changes from upstream

Everything else under `src/` is byte-for-byte upstream 0.23.46.

| File | Change |
|---|---|
| `src/client/shaper.rs` | New: the `HelloShaper` trait |
| `src/client/client_conn.rs` | `ClientConfig::hello_shaper`; `ClientConnectionData::alps_negotiated` |
| `src/client/builder.rs` | Initialises `hello_shaper` to `None` |
| `src/client/hs.rs` | When a shaper is set, its bytes replace the encoded ClientHello, on the wire and in the transcript; its extension types count as offered; GREASE ECH status is set |
| `src/client/tls13.rs` | Records a negotiated ALPS extension; sends the client's EncryptedExtensions before Finished |
| `src/msgs/handshake.rs` | TLS 1.3 certificate entries may carry SCT and Trust Anchor extensions; they are ignored |
| `src/lib.rs` | Exports the new types |
| `Cargo.toml` | Crate name; only the `ring` provider; no tests, examples or benches |

To see the exact patch, compare `src/` with the `rustls` 0.23.46 crate source.

## Updating

1. Unpack the new `rustls` release over `src/` and reapply the table above.
2. Run the fingerprint tests: `cargo test --profile quick -p pagesift-net fingerprint`.
3. Re-run `benchmarks/history/1.0/fingerprint/run.sh`.
