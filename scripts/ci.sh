#!/usr/bin/env bash
# 本地检查：和发布前要求的检查一致。GitHub 上只保留 macOS 测试（本机是 Linux，macOS 只能在那里测）。
#
#   scripts/ci.sh          完整检查（发版前）：再加最低 Rust 版本、cargo deny、性能门槛
#   scripts/ci.sh --fast   推送前（.git/hooks/pre-push 调用）：GitHub macOS CI 会跑的检查——发版一致性、
#                          测试里的计时、fmt、clippy 和测试（警告当错误，和 CI 的 RUSTFLAGS 一致）
#
# 警告当错误的编译和日常编译共用 target/：RUSTFLAGS 不同时会重编，但磁盘上只留一份（单独目录会再占一整份）。
set -euo pipefail
cd "$(dirname "$0")/.."
fast=; [ "${1:-}" = "--fast" ] && fast=1

step() { printf '\n==> %s\n' "$*"; }

step "release identity"
python3 scripts/check_release.py
step "wall-clock reads in tests"
python3 scripts/check_wall_clock.py

step "fmt"
cargo fmt --all --check

[ -n "${CI_TARGET_DIR:-}" ] && export CARGO_TARGET_DIR=$CI_TARGET_DIR
step "clippy"
RUSTFLAGS="-D warnings" scripts/build.sh cargo clippy --workspace --all-targets --locked --profile quick

step "test"
# 和 GitHub 的命令一致（不带目标参数：也编 examples、跑文档测试），只是换成 quick 档
RUSTFLAGS="-D warnings" scripts/build.sh cargo test --workspace --locked --profile quick --exclude pagesift-cdp
RUSTFLAGS="-D warnings" scripts/build.sh cargo test -p pagesift-cdp --locked --profile quick --lib --test protocol --test browser --test bench
[ -n "$fast" ] && { step "fast checks passed"; exit 0; }

# 最低版本取自 Cargo.toml 的 rust-version，没装就装上：不跳过（macOS CI 里也有同一步）
msrv=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["rust-version"])')
step "msrv ($msrv)"
rustup toolchain list | grep -q "^$msrv" || rustup toolchain install "$msrv" --profile minimal
cargo "+$msrv" check --workspace --locked

step "cargo deny"
if command -v cargo-deny >/dev/null; then
    cargo deny --config .github/deny.toml check
else
    echo "skipped: run 'cargo install cargo-deny --locked' to enable"
fi

step "perf gate"
cargo run --release --locked -p pagesift-content --example perf_gate
cargo run --release --locked -p pagesift --example crawl_gate
cargo run --release --locked -p pagesift --example focus_gate
cargo run --release --locked -p pagesift --example focus_gate -- --auto

printf '\nall checks passed\n'
