#!/bin/bash
# Compiler wrapper for the aws-lc (BoringSSL) C compile.
#
# Why: Apple's CommandLineTools clang produces -flto IR that links ~16 KB
# smaller through ld64's libLTO than nix clang 21.1.8 IR (verified A/B:
# 2076 -> 2060 KB, behavior-preserving). BUT in DEBUG builds (cargo test,
# no LTO at link), nix ld cannot consume Apple clang bitcode ("Invalid
# record for architecture arm64"). So this wrapper strips -flto for debug
# builds (native Apple objects link fine), keeping it only for release.
set -euo pipefail
APPLE_CLANG="/Library/Developer/CommandLineTools/usr/bin/clang"
if [ "${OPT_LEVEL:-}" = "z" ] || [ "${OPT_LEVEL:-}" = "s" ] || [ "${PROFILE:-}" = "release" ]; then
  exec "$APPLE_CLANG" "$@"
fi
args=()
for a in "$@"; do
  [ "$a" = "-flto" ] || args+=("$a")
done
exec "$APPLE_CLANG" "${args[@]}"
