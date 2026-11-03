#!/usr/bin/env bash
# 本地检查：和发布前要求的检查一致。GitHub 上只保留 macOS 测试（本机是 Linux，macOS 只能在那里测）。
#
#   scripts/ci.sh          完整检查（发版前）：再加最低 Rust 版本、cargo deny、性能门槛
#   scripts/ci.sh --fast --bindings  推送标签（发版）前：再加 Node、Python 绑定的测试（发布流程里打包前都要跑）
#   scripts/ci.sh --fast   推送前（.git/hooks/pre-push 调用）：GitHub macOS CI 会跑的检查——发版一致性、
#                          测试里的计时、fmt、clippy 和测试（警告当错误，和 CI 的 RUSTFLAGS 一致），
#                          以及对 macOS 的类型检查
#
# 警告当错误的编译和日常编译共用 target/：RUSTFLAGS 不同时会重编，但磁盘上只留一份（单独目录会再占一整份）。
set -euo pipefail
cd "$(dirname "$0")/.."
fast=; bindings=
for a in "$@"; do
    case "$a" in --fast) fast=1 ;; --bindings) bindings=1 ;; esac
done
[ -z "$fast" ] && bindings=1

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
# GitHub 上跑的是 macOS：Linux 本地看不见"只在 Linux 才有"的引用（例如只给 Linux 引入的 libc）。
# 对 macOS 做一遍完整的类型检查（不链接）；本机没有 macOS 的 C 工具链，构建脚本里的 C 编译用
# scripts/check-cc.sh 生成空目标文件代替，check 用不到它们
step "loopback addresses"
# macOS、Windows 默认只有 127.0.0.1；写死 127.0.0.2 这类地址的测试只在 Linux 上能绑定
# （0.90 的集群测试在 GitHub macOS 上因此失败）。需要多个地址时用 tests/it/cluster.rs 的 loopback()
# 只是数据、不绑定的地址在行尾写 `// loopback: not bound`
if git grep -nE '"127\.0\.0\.([2-9]|[1-9][0-9]|2[0-5][0-9])(:[0-9]+)?"' -- 'crates/*.rs' ':!crates/pagesift/tests/it/cluster.rs' \
    | grep -v 'loopback: not bound'; then
    echo "hard-coded loopback addresses other than 127.0.0.1 only bind on Linux"; exit 1
fi
step "macos type check"
rustup target list --installed | grep -qx aarch64-apple-darwin \
    || { echo "missing target: run 'rustup target add aarch64-apple-darwin'"; exit 1; }
CC_aarch64_apple_darwin=$PWD/scripts/check-cc.sh AR_aarch64_apple_darwin=$PWD/scripts/check-cc.sh \
    RUSTFLAGS="-D warnings" scripts/build.sh cargo check --workspace --locked --target aarch64-apple-darwin \
    --all-targets --exclude pagesift-fuzz
if [ -n "$bindings" ]; then
    # 发布流程在打 npm 包、wheel 之前跑这两套测试；不在本地跑，引擎改了行为要到发版时才发现
    step "node binding tests"
    scripts/build.sh cargo build --profile quick --locked -p pagesift-node
    cp "${CARGO_TARGET_DIR:-target}/quick/libpagesift_node.so" crates/pagesift-node/pagesift.linux-x64-gnu.node
    (cd crates/pagesift-node && node --test --test-concurrency=1)
    step "python binding tests"
    venv=${PY_VENV:-benchmarks/venv}
    [ -x "$venv/bin/maturin" ] && [ -x "$venv/bin/pytest" ] \
        || { echo "missing maturin/pytest in $venv (PY_VENV)"; exit 1; }
    (cd crates/pagesift-py && VIRTUAL_ENV=$PWD/../../$venv PATH=$PWD/../../$venv/bin:$PATH \
        ../../$venv/bin/maturin develop --profile quick --locked)
    "$venv/bin/python" -m pytest crates/pagesift-py/tests -q
fi
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
