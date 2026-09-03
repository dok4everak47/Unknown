#!/bin/bash
# =============================================================================
# checks.sh — correctness gates for pi-autoresearch (karakuri / Rust project)
#
# run_experiment invokes this after every PASSING benchmark. Non-zero exit
# → experiment logged as checks_failed and cannot be kept (code is reverted).
#
# Order matters (cheap → expensive, fail fast):
#   1. cargo fmt --check   — style drift
#   2. cargo check --quiet — compile errors
#   3. cargo clippy --quiet — lints (warnings shown but do NOT fail the gate;
#                             only a non-zero clippy exit blocks keep)
#   4. cargo test --quiet  — behavior (last lines carry the failure summary)
#
# Every gate captures stdout+stderr AND the real exit code before any output
# filtering. A non-zero exit always fails this script — filtering may shorten
# output but must never swallow the failure status (no `|| true` on cargo).
#
# Output policy: suppress success noise, let only errors/failures through.
# run_experiment feeds back only the last 80 lines on failure.
# =============================================================================
set -euo pipefail

[ -f Cargo.toml ] || {
  echo "ERROR: no Cargo.toml in $(pwd) — run from project root" >&2
  exit 1
}

# Run one gate command:
#   - capture output and the real exit code (no pipe, so no `|| true` can
#     mask a cargo failure);
#   - non-zero exit → print the output tail, then exit 1;
#   - zero exit     → print only error/warning lines (display filter only;
#                     grep's exit status is irrelevant to the gate).
run_gate() {
  local label="$1"
  shift
  local output rc
  output="$("$@" 2>&1)" && rc=0 || rc=$?
  if [ "$rc" -ne 0 ]; then
    printf '%s\n' "$output" | tail -60
    echo "ERROR: ${label} failed (exit ${rc})" >&2
    exit 1
  fi
  printf '%s\n' "$output" | grep -E "^(error|warning)" || true
}

# 1) Format — prints the diff on drift; non-zero exit fails the gate
run_gate "cargo fmt --check" cargo fmt --check

# 2) Compile — surface errors/warnings, suppress progress
run_gate "cargo check" cargo check --quiet

# 3) Lints — warnings shown but do NOT fail the gate; only a non-zero
#    clippy exit blocks keep
run_gate "cargo clippy" cargo clippy --quiet

# 4) Tests — quiet build, failure summary in the tail
run_gate "cargo test" cargo test --quiet
