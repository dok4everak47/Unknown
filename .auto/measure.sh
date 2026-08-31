#!/bin/bash
# =============================================================================
# measure.sh — pi-autoresearch benchmark for myagent (Rust project)
#
# Primary metric: <WORKLOAD> wall-clock time in seconds (lower is better)
#
# Usage:
#   ./auto/measure.sh                  # default: cargo check timing
#   WORKLOAD=build ./auto/measure.sh   # cargo build --release timing + binary size
#   WORKLOAD=test  ./auto/measure.sh   # cargo test timing
#   WORKLOAD=size  ./auto/measure.sh   # release binary size only
#
# Design notes (read before changing):
#   - Single run per iteration, NOT median-of-3. Each autoresearch iteration
#     edits code first, so the first run measures the real incremental compile.
#     Extra runs inside this script would measure a warm no-op cache and skew
#     the metric toward zero.
#   - Pre-checks are intentionally tiny (<1s). Full correctness gates
#     (fmt / check / test / clippy) live in .auto/checks.sh, which
#     run_experiment invokes after every passing benchmark.
#   - Binary size is `du -sk` (KB, 1024-byte blocks). A missing binary is a
#     hard error (exit non-zero) — it is NEVER reported as binary_kb=0, which
#     would look like a false optimal result on a lower-is-better metric.
#   - Output format: lines starting with "METRIC name=value" are parsed by
#     run_experiment. Everything else is captured as raw log output.
# =============================================================================
set -euo pipefail

# --- Config ----------------------------------------------------------------
# One of: check | build | test | size
WORKLOAD="${WORKLOAD:-check}"

# --- Helpers ---------------------------------------------------------------
# Milliseconds since epoch, high resolution (macOS + GNU/Linux).
now_ms() {
  perl -MTime::HiRes=time -e 'printf "%.0f", time() * 1000'
}

# Convert integer ms to seconds with 3 decimals (portable, no awk float quirk).
ms_to_s() {
  perl -e 'printf "%.3f", $ARGV[0]/1000' "$1"
}

# Time a command in ms; echoes the duration. Stderr passes through.
time_ms() {
  local start end
  start="$(now_ms)"
  "$@"
  end="$(now_ms)"
  echo $((end - start))
}

# Fail hard when the release binary is missing — no binary_kb=0 fallback.
require_release_binary() {
  [ -f target/release/myagent ] || {
    echo "ERROR: target/release/myagent not found — run WORKLOAD=build first" >&2
    exit 1
  }
}

# --- Pre-checks (fast, <1s) ------------------------------------------------
# Fail fast on obvious breakage before spending time on the benchmark.
command -v cargo >/dev/null 2>&1 || {
  echo "ERROR: cargo not found on PATH (need: nix develop / direnv)" >&2
  exit 1
}
[ -f Cargo.toml ] || {
  echo "ERROR: no Cargo.toml in $(pwd) — run from project root" >&2
  exit 1
}

# --- Workload --------------------------------------------------------------
case "$WORKLOAD" in
  check)
    check_ms="$(time_ms cargo check --quiet)"
    echo "METRIC check_seconds=$(ms_to_s "$check_ms")"
    ;;
  build)
    build_ms="$(time_ms cargo build --release --quiet)"
    require_release_binary
    size_kb="$(du -sk target/release/myagent | awk '{print $1}')"
    echo "METRIC build_seconds=$(ms_to_s "$build_ms")"
    echo "METRIC binary_kb=${size_kb}"
    ;;
  test)
    test_ms="$(time_ms cargo test --quiet)"
    echo "METRIC test_seconds=$(ms_to_s "$test_ms")"
    ;;
  size)
    require_release_binary
    size_kb="$(du -sk target/release/myagent | awk '{print $1}')"
    echo "METRIC binary_kb=${size_kb}"
    ;;
  *)
    echo "ERROR: unknown WORKLOAD '$WORKLOAD' (check|build|test|size)" >&2
    exit 1
    ;;
esac
