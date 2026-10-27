#!/usr/bin/env bash
# 本地检查：和发布前要求的检查一致。推送前运行：scripts/ci.sh
# GitHub 上只保留 macOS 测试（本机是 Linux，macOS 只能在那里测）。
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "release identity"
python3 scripts/check_release.py

step "fmt"
cargo fmt --all --check

step "clippy"
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked

step "test"
RUSTFLAGS="-D warnings" cargo test --workspace --locked

step "msrv (1.88)"
if rustup toolchain list | grep -q '^1\.88'; then
    cargo +1.88 check --workspace --locked
else
    echo "skipped: run 'rustup toolchain install 1.88' to enable"
fi

step "cargo deny"
if command -v cargo-deny >/dev/null; then
    cargo deny check
else
    echo "skipped: run 'cargo install cargo-deny --locked' to enable"
fi

step "perf gate"
cargo run --release --locked -p pagesift-content --example perf_gate
cargo run --release --locked -p pagesift --example crawl_gate
cargo run --release --locked -p pagesift --example focus_gate

printf '\nall checks passed\n'
