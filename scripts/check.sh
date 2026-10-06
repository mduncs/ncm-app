#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PYTHONDONTWRITEBYTECODE=1
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$PWD/target/cargo-home}"
# NCM_HEAVY names a wrapper that admits heavy jobs one at a time; without one, run directly.
heavy="${NCM_HEAVY:-env}"
[[ -x "$heavy" ]] || heavy=env
# Keep the entire serial check in one admitted slot, including every cargo child.
exec "$heavy" bash -euo pipefail -c '
  cargo fmt --check
  cargo clippy --workspace -- -D warnings
  mkdir -p target/ncm-check-home
  env HOME="$PWD/target/ncm-check-home" cargo test --workspace
  env HOME="$PWD/target/ncm-check-home" python3 scripts/tty_lifecycle_check.py
  env HOME="$PWD/target/ncm-check-home" python3 scripts/outside_mode_check.py
  python3 scripts/panic_check.py
'
