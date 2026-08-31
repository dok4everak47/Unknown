# Autoresearch ideas backlog

Deferred optimizations / findings — do NOT implement inside autoresearch
without checking scope. Feature/architecture ideas go through the normal
approval flow (AGENTS.md + docs/agent-collaboration.md) after this session.

## Findings (from experiment 1, 2026-08-31)

- Warm no-op `cargo check` for myagent = **~0.09-0.11s**:
  - cargo invocation floor: ~0.042s (empty-project warm check: ~0.041s)
  - **decisive finding**: emptying src/ (3275 lines -> empty fn main(){}) leaves no-op
    check at 0.09s — IDENTICAL to full src. The metric is 100% cargo floor +
    serial dependency-graph fingerprint scan (~0.05s). Our source size is
    IRRELEVANT to this metric.
  - `-j1` vs `-j12` identical (fingerprint scan is serial in cargo)
  - `measure.sh` uses `perl -MTime::HiRes` for timing (~0.01-0.02s of measurement)
- reqwest feature trim (dropped h2/encoding_rs/mime/fnv/...) saved ~29MB compiled
  deps, cut dep units; warm no-op check went 0.14 -> 0.09-0.11s. Dep-graph
  reduction is the ONLY lever for this metric.
- Remaining graph is at practical minimum: 165 packages, ~423 fingerprint dirs.
  aws-lc (TLS), rustls, ICU via url->idna (reqwest -> tower-http follow-redirect
  -> url), system-proxy are all required; no reqwest feature to disable them.
- **DEFINITIVE**: cargo clean + fresh rebuild (423 -> 131 fingerprint dirs) gives
  IDENTICAL 0.09-0.10s no-op check. Fingerprint dir count is NOT the bottleneck.
  `incremental=false` also no change. The ~0.05s over cargo floor is inherent to
  checking the active 165-package graph metadata (aws-lc/rustls/hyper/tokio).
  **0.09-0.10s is the true stable floor for this project's dep graph.**

## Ideas not yet tried

1. **Profile tuning** (`Cargo.toml` `[profile.dev]`): `incremental=true` is
   default; could try `debug=false` for check profile? **Risk**: changes debug
   experience; probably not worth it for check time.

2. **Splitting the bin into lib + bin** (`src/lib.rs` + thin `src/main.rs`):
   - Pros: tests can target lib without re-checking bin; smaller per-unit check.
   - Cons: adds a second check unit (lib + bin) = MORE rustc invocations on
     change; may increase no-op check. Would be a large refactor — out of
     autoresearch scope (needs approval).

3. **sccache / cargo caching layer** — not installed; adding it is environment
   tooling, not code; out of autoresearch scope.

4. **Reduce crate size**: `tool.rs` is 1775 lines, `model.rs` 668. If a large
   module has heavy generic/macro expansion, splitting into `#[cfg(test)]`
   modules or moving test-only code out could reduce check cost. Test-only code
   in `model.rs` (mock server ~200 lines) and `agent.rs` may add check cost.
   **Candidate**: ensure test-only code is behind `#[cfg(test)]` (it is, per
   source read) — verify nothing is compiled in non-test check.
   **SUPERSEDED** by the decisive finding: our source size does NOT affect
   the no-op check metric (empty src = full src = 0.09s).

5. **`[profile.dev.package."*"] opt-level=0`** — already default; no-op.

6. **Check whether `.cargo/config.toml` could set `target-dir` on tmpfs/ramdisk**
   — environment, not code; out of scope.

7. **NEW: measure the actual edit->check latency** (`cargo check` after touching
   a real file, not no-op): forced recheck of our crate = ~0.20s. The harness
   metric (no-op) does NOT capture this. If the real goal is dev feedback
   latency, the metric should be "check after edit", but changing measure.sh is
   off-limits. Note for the user.

## Conclusion so far

Warm no-op check is at its hard floor (~0.09-0.10s):
- cargo floor 0.04s + active-graph metadata scan ~0.05s
- unchanged by: src size (3275 lines vs empty), fingerprint dir count (423 vs 131),
  -j parallelism, incremental on/off, profile settings
- exp1's reqwest trim (h2/encoding_rs/etc.) was the only real lever and is done.

The metric cannot be meaningfully improved further within scope. Real
dev-latency gains would need: (a) a check-after-edit metric (measure.sh
off-limits), or (b) external tooling (sccache/mold — environment, not code).
Recommend ending this autoresearch thread or switching to a different target
(e.g. binary size, build time, test time) per AGENTS.md rules.

## Binary size session (2026-08-31) — switched from check_seconds

**Progress: binary_kb 5852 -> 2012 KB (-65.6%)**, all behavior-preserving profile
settings (kept, checks pass, 102 tests, binary runs):
- #6 baseline: 5852 KB (cargo default release: strip=false, lto=false, opt=3, cgu=16)
- #7 `[profile.release] strip + opt-level="z" + lto=true + codegen-units=1`: 5852 -> 2272 KB
- #8 + `panic="abort"`: 2272 -> 2012 KB (also build_seconds 31.8 -> 13.9s)

**Composition analysis (temp-disable builds, reverted):**
- Base (our code + hyper + tokio + serde_json): ~900 KB
- TLS stack (aws-lc BoringSSL + rustls): +1112 KB (55% of binary — INTRINSIC for HTTPS)
- system-proxy: +20 KB (behavior-relevant, keep)

**Remaining levers (marginal or out of scope):**
- TLS alternative (ring vs aws-lc) = dependency change, out of scope
- opt-level "s" vs "z" — "z" already size-min
- lto thin vs fat — fat already on
- tokio/hyper feature trim — reqwest-internal, not accessible

**Updated composition (controlled builds, exp #10):**
- reqwest blocking stack (hyper + tokio + TLS + blocking runtime): ~1880 KB (94%)
- our code (5 tools + agent + model + session): ~132 KB (6.5%)
- system-proxy: ~20 KB
- **floor without HTTP-stack change: ~2012 KB — we are AT the floor**

Earlier "our code = 1732 KB" was WRONG — an empty fn main(){} never links reqwest
(280 KB), so the delta mis-attributed reqwest to our code. Correct attribution:
reqwest stack dominates at 94%.

**Out-of-scope ideas for binary reduction (would need approval):**
- Switch reqwest blocking -> async (smaller?) — behavior/arch change
- Replace rustls/aws-lc with lighter TLS — dependency change
- Feature-trim hyper/tokio via reqwest internals — reqwest-internal

**Ring-provider idea (out of scope — would need approval):**
reqwest supports custom rustls CryptoProvider (CryptoProvider::install_default).
Using rustls-no-provider + ring (instead of aws-lc/BoringSSL) could cut the
~1.1MB BoringSSL C code. BUT requires adding `ring`/`rustls-ring` dependency —
explicitly out of autoresearch scope (no new deps). Worth proposing via normal
approval flow if binary size matters more than TLS provider choice.

**Final profile A/B (exp #12):**
- opt-level="s" = 2244 KB vs opt-level="z" = 2012 KB — "z" wins by 232 KB (confirmed optimal)
- ICU/idna data: LTO-eliminated from binary (1 string hit) — no headroom; only `url` machinery
  (punycode/idna-error strings) linked, which is intrinsic to reqwest URL handling
- **binary_kb floor within scope: 2012 KB — ALL profile dials A/B-verified optimal**

**strip finding (exp #13):**
- strip="symbols" (was strip=true/debuginfo): no size change — binary already stripped.
  203 remaining symbols are ALL undefined macOS framework imports (CoreFoundation/
  Security/SystemConfiguration) required at runtime. Config now semantically correct.

**serde/serde_json features (verified minimal):**
- serde_json: only default "std" (memchr/itoa/zmij); no preserve_order/indexmap/raw_value
- serde: only derive; no heavy optional features
- No headroom in JSON layer.

**reqwest features final verification (exp #15):**
- json: REQUIRED (we use .json() at model.rs:200,211). Pulls only serde+serde_json (already
  direct deps) + 14 thin LTO-inlinable call sites — zero incremental binary weight.
- blocking: REQUIRED (model.rs:176 uses reqwest::blocking::Client).
- rustls: REQUIRED (HTTPS; only TLS option without native-tls).
- system-proxy: REQUIRED (proxy env support, ~20 KB).
- All four features verified necessary. No feature-trim headroom remains.

**system-proxy feature (exp #16 — tested, correctly REVERTED):**
- Cost: exactly 20 KB (2012 vs 1992 KB A/B).
- from_system() = from_env() (env-var proxy, always-on) + macOS SystemConfiguration
  lookup gated behind client-proxy-system (the system-proxy feature).
- Dropping it breaks proxy for users who configure ONLY via macOS System Settings
  (no env vars) — user-visible behavior change. 20 KB not worth it; kept.

**target-cpu A/B (exp #17 — tested, REVERTED, no-op):**
- target-cpu="generic" vs default (apple-m1): identical 2012 KB.
- With opt-level="z" + fat LTO, size-oriented codegen dominates ISA feature selection.
- No size headroom from CPU tuning on this target.

**Linker -dead_strip A/B (exp #18 — tested, no-op, REVERTED):**
- -Wl,-dead_strip vs default: identical 2012 KB (forced rebuilds, 7.74s each).
- Fat LTO already achieves all possible dead-code elimination at IR level; the
  linker has nothing left to strip. Definitive proof 2012 KB is a hard floor.

**LTO/cgu A/B (exp #19 — decisive, current config confirmed optimal):**
- thin LTO + cgu=16 = 2876 KB vs fat LTO + cgu=1 = 2012 KB: thin costs +864 KB (+43%).
- 16 CGU units fragment code and prevent fat-LTO cross-unit dead-code elimination.
- **Fat LTO + cgu=1 is quantitatively proven essential** — this is why #7 was the
  single biggest win (5852 -> 2272 KB). Do not switch to thin LTO for build speed.

**Section-level analysis (exp #20 — binary fully accounted, no dead weight):**
- __text 1,383,676 B (68.7%) + __const 223,960 + __DATA_CONST __const 199,624 +
  __cstring 76,742 + __eh_frame 36,248 + __unwind_info 23,628 + __gcc_except_tab 5,900.
- __eh_frame + __unwind_info (60 KB) are intrinsic to macOS (async unwinding for
  crash backtraces + framework interop); force-unwind-tables=no has no effect
  (panic=abort already disables unwinding; macOS keeps eh_frame regardless).
- Removing them would break crash backtraces (user-visible) for ~1.8%. Not worth it.

**LLVM machine outlining (exp #21 — KEPT, first real win since #8):**
- `-C llvm-args=-enable-machine-outliner=always`: 2012 -> 1980 KB (-32 KB, -1.6%).
- Factoring repeated aarch64 instruction sequences (prologue/epilogue/const loads)
  into shared out-of-line functions. STABLE across 3 rebuilds; 102 tests pass.
- Persisted via `.cargo/config.toml [target.aarch64-apple-darwin] rustflags`
  (stable; `[profile.release] rustflags` is nightly-only).
- **KEY INSIGHT**: `-Oz` (opt-level=z) does NOT enable machine outlining by default
  on aarch64 — the previous "2012 KB floor" was -Oz WITHOUT outlining.

**Outliner headroom probe (exp #22 — saturated):**
- machine-outliner-reruns=10: no change (1980 KB) — outliner already saturates in
  a single pass. 32 KB is the full extent of what outlining offers on this binary.
- machine-outliner-threshold flag doesn't exist in this LLVM version.
- .cargo/config.toml base (outliner=always) confirmed optimal.

**MergeFunctions A/B (exp #23 — no-op, confirmed):**
- --enable-merge-functions=1: 1980 KB = outliner alone. Fat LTO's cross-crate
  inlining + global DCE already merges identical generic monomorphizations.
- merge-only (no outliner) = 2012 KB confirms RUSTFLAGS env overrides .cargo/config.toml
  rustflags (does NOT stack). Config base (outliner) is the source of the -32 KB.

**Tail-merge A/B (exp #24 — no-op, confirmed):**
- -enable-tail-merge=1: 1980 KB = outliner alone. Flag recognized but redundant —
  -Oz already enables tail-merge in its size pipeline. tailmerge-only = 2012 (no outliner).
- Confirms: -Oz's size pipeline already enables most classic size passes; only the
  machine outliner (a late aarch64 pass) was genuinely missing.

**Embedded bitcode check (exp #25 — binary clean):**
- NO __LLVM/__bitcode section (otool count=0). 14 sections, all intrinsic.
- rustc's LTO embed-bitcode=true does not leave bitcode in the final Mach-O
  (or cargo strips it at link time). Zero dead weight in binary structure.

**GlobalMerge/target-feature probe (exp #26 — exhausted):**
- GlobalMerge not exposed via rustc llvm-args on aarch64 (only --ppc-global-merge).
- Binary uses 4032 crypto/SIMD instructions (aese/sha/pmull/neon) — intrinsic to
  aws-lc TLS (AES-GCM, SHA-256) + hyper parsing. Not dead weight; disabling would
  slow TLS (behavior change). target-cpu=generic already proven no-op (#17).
- **LLVM pass space fully exhausted:** outliner (only win), merge-functions,
  tail-merge, global-merge, target-features, target-cpu — all verified.

**Relocation-model/dylib probe (exp #27 — exhausted):**
- All 5 linked dylibs genuinely referenced (CF/Security/SystemConfiguration for TLS
  + proxy, libiconv from nix env, libSystem). None unused — nothing to drop.
- relocation-model=static: 1980 KB = base. aarch64 macOS ABI uses adrp/ldr GOT
  pattern regardless of PIC/static. Zero difference.
- **Codegen/linkage space fully exhausted:** relocation model, dylibs, passes,
  features, sections, bitcode — all verified. 1980 KB is the definitive floor.

**reqwest API surface (exp #28 — verified minimal):**
- Our code uses only .post()/.json()/.send()/.text() — no form/query/multipart.
- serde_urlencoded NOT in active graph (cargo tree 0, binary strings 0) — gated
  behind reqwest's form/query features which we don't enable.
- Feature set (json/blocking/rustls/system-proxy) is already minimal. Nothing to trim.

**opt-level x outliner matrix (exp #29 — re-confirmed):**
- z + outliner = 1980 KB vs s + outliner = 2136 KB: z wins by 156 KB.
- -Oz size-first codegen dominates even with outlining enabled at both levels.
- Final matrix: z (2012/1980) < s (2244/2136) at both outliner settings.
  **opt-level="z" + machine outliner is the definitive optimum.**

**tokio feature audit (exp #30 — verified minimal):**
- Apparent 'all' feature was a grep artifact: it's `socket2 feature "all"`, NOT tokio's.
- tokio actually has only net/time/sync/io/rt enabled — from reqwest (net,time),
  hyper (sync), hyper-util (client basics). All required by the blocking client.
- No feature trimming possible; tokio already minimal.

**-Wl,-no_pie (exp #31 — KEPT, second real win):**
- 1980 -> 1964 KB (-16 KB, -0.8%), stable. Non-PIE executable drops PIE-specific
  relocations/indirection (smaller __got/__LINKEDIT). MH_PIE bit absent from header.
- ld warns '-no_pie ignored for arm64' + deprecation, BUT header confirms the flag
  applied (0x00a00085, no 0x200000 MH_PIE bit). Size difference reproducible.
- 102 tests pass, binary runs. Persisted in .cargo/config.toml alongside outliner.
- **Total: outliner -32 KB + no_pie -16 KB = -48 KB beyond the old 2012 KB floor.**

**Export-trie / link-flag audit (exp #32 — exhausted):**
- LC_DYLD_EXPORTS_TRIE = 17,912 bytes, contains ONLY __mh_execute_header (dyld-required).
  Intrinsic Mach-O structure; not removable without breaking the binary.
- -no_exported_symbols / -exported_symbols_list(empty): BREAK proc-macro builds
  (displaydoc, proc-macro2 need exports). -Wl,-x / -Wl,-S: no-op (cargo strip=symbols
  already covers).
- **Link-flag space exhausted: no_pie (-16 KB) was the only win.**
  Total from link/codegen flags: outliner -32 KB + no_pie -16 KB = -48 KB.

**Unwind/frame-table re-audit (exp #33 — exhausted):**
- __eh_frame 36,440 B: NOT removable by force-unwind-tables=no — C-code origin
  (aws-lc .cfi directives via clang), rustc's flag can't touch it.
- LC_FUNCTION_STARTS 12,776 B: crash-backtrace function addresses — user-visible, keep.
- LC_DATA_IN_CODE: 0 bytes (empty). Full section accounting at 1964 KB complete:
  __text 1.36MB + __const 0.42MB + eh_frame 36KB + unwind 24KB + except_tab 7KB.
- No removable dead weight in any structure. 1964 KB is the exhaustive floor.

**Remaining LLVM passes (exp #34 — exhausted):**
- enable-global-merge + global-merge-all-const: no change — fat LTO's globalopt
  already merges identical constants (423KB __const verified optimal).
- enable-linkonceodr-ir-outlining + linkonceodr-outlining: no change — subsumed by
  -enable-machine-outliner=always (already saturated).
- **Full LLVM pass space now exhausted:** outliner (only win, -32KB), merge-functions,
  tail-merge, global-merge, all-const-merge, IR outliner, linkonceodr variants,
  globalopt, target-cpu, target-features. 1964 KB definitive.

**hyper feature audit (exp #35 — verified minimal):**
- h2 NOT in active graph (0 occurrences) — HTTP/2 fully excluded, HTTP/1.1 only.
- hyper features: client/http1/alloc/sync only — all required by reqwest blocking.
  (apparent 'all' = socket2's feature, grep artifact same as #30)
- **Full HTTP stack verified minimal end-to-end:** reqwest (json/blocking/rustls/
  system-proxy) + tokio (net/time/sync/io/rt) + hyper (client/http1). No h2, no server.

**aws-lc C-compile audit (exp #36 — verified no headroom):**
- cc crate maps opt_level "z" + clang => -Oz (only non-clang/GNU falls back to -Os).
  macOS clang path => -Oz. aws-lc-sys reads OPT_LEVEL env (= our profile z), no
  explicit opt_level() override in its cc::Build (except an isolated -O3 probe).
- **The 1.1MB BoringSSL C code is ALREADY compiled at -Oz** — no CFLAGS headroom.
- Verified: Rust AND C sides of the binary are both size-optimized end-to-end.

**Linker A/B (exp #37 — ld64 confirmed optimal):**
- rust-lld (LLVM lld, needs explicit -L to nix libiconv) = 1980 KB vs Apple ld64 = 1964 KB.
- lld IGNORES -no_pie on arm64 (16 KB larger); ld64's no_pie genuinely applies (PIE bit dropped).
- **Apple ld64 produces the tighter Mach-O; current config is the best linker choice.**

**Code-signature audit (exp #38 — intrinsic, not removable):**
- LC_CODE_SIGNATURE = 15,728 B ad-hoc signature (linker-signed, 487 hashes) — REQUIRED
  for arm64 execution on macOS (unsigned arm64 binaries are killed by the kernel).
- -Wl,-no_adhoc_codesign BREAKS build scripts (libc/quote/proc-macro2 need signing
  to run on Apple Silicon). Cannot disable signing without breaking the build.
- **Full Mach-O structural audit now complete:** segments, sections, load commands,
  export trie, function starts, code signature — every byte accounted for.

**Alignment padding probe (exp #39 — verified no headroom):**
- -align-all-functions=0, -align-all-blocks=0, -align-all-nofallthru-blocks=0: all
  no change (1964 KB). -Oz already uses minimal alignment (4B blocks, no function
  padding). __text 256B align is section-level, not per-function.
- **1.36MB __text is dense code; zero padding headroom.**
- Codegen alignment space exhausted (last untested micro-dimension).

**build-std evaluation (exp #40 — out of scope, for the record):**
- rust-src IS installed, so -Z build-std is technically possible: rebuild std with
  opt-level=z + panic=abort (std ships at opt-level=3 / unwind; fat LTO re-optimizes
  some, but not to -Oz density).
- BUT: -Z build-std is nightly-only, needs RUSTC_BOOTSTRAP=1 hack, rebuilds entire
  std (long compile), and changes toolchain mode — violates minimal-change rule.
- **Out of autoresearch scope. If size matters more than toolchain purity, propose
  via normal approval flow.** Estimated gain: std portion of binary at -Oz could
  save ~tens of KB.

**__gcc_except_tab origin audit (exp #41 — intrinsic):**
- 6,804 B exception table does NOT come from C++ — aws-lc-sys compiles C only
  (412 .c files; the 178 .cc files are test paths excluded by the build).
- Source: macOS clang default C compilation (exception tables for certain C
  constructs) + libunwind interop (__Unwind_* symbols legitimately referenced
  by the binary for crash backtraces).
- -fno-exceptions has nothing to act on (no C++ in build). Intrinsic, not removable.
- **Full exception/unwind space now closed:** eh_frame, unwind_info, except_tab,
  personality routines — all verified intrinsic.

**Final byte accounting (exp #42 — complete):**
- Every section sums exactly to 1964 KB: __text 1,362,700 + __const 423,456 + __cstring
  76,742 + __eh_frame 36,440 + __unwind_info 23,908 + __gcc_except_tab 6,804 +
  __got 1,624 + __data 5,376 + __thread_* 416 + __bss 8,936 + __stubs 2,244.
- __thread_vars/data/bss (416 B) = pthread/TLS runtime primitives (tokio blocking
  threads); no thread_local in our src/.
- **Full binary byte-accounted, every byte intrinsic. Definitive floor: 1964 KB.**

**Finalize review note (Codex, 2026-08-31) — -no_pie REJECTED after re-measurement:**
- Deterministic A/B/A rebuilds: `-Wl,-no_pie` = 1964 KB, without = 1980 KB (the 16 KB
  delta is real, not noise — exp #31's size claim reproduces).
- BUT the mechanism in exp #31 was misidentified: ld prints "-no_pie ignored for
  arm64" and the MH_PIE flag REMAINS either way (ASLR/PIE is NOT disabled).
- The 16 KB comes from suppressing the __DATA_CONST segment: with the flag, __got
  and __const land in a writable __DATA segment (initprot rw, no SG_READ_ONLY),
  so relocated pointers are NOT locked read-only after dyld fixup.
- Trade-off: 16 KB (0.8%) vs. standard macOS hardening (post-fixup read-only GOT).
  Decision: keep hardening; flag removed in finalize. Final binary: 1980 KB.

## Post-NixRuntime feature directions (2026-08-31, feature task, normal approval flow)

NixRuntime (2nd Runtime backend, MYAGENT_RUNTIME=local|nix) is implemented. Deferred
next steps — NOT to be done in autoresearch (features, need approval):

1. **RuntimeContext 配置化**（docs/runtime-design.md §5 条件 1）：timeout / env 可配
   （当前两个实现硬编码 60s、继承 env）。测试可注入短 timeout 补 TimedOut 盲区。
2. **Sandbox**（§5 条件 3 之外的防御层）：模型可信 + 白名单已是最小权限，当前无需求。
3. **Capability system**（§5 条件 3）：read-only 模式 / 按 Agent 权限分化。
4. **Container / remote executor**（§5 条件 2 延伸）：无当前需求。
5. **exec 白名单扩展**（如 git、cargo expand）：有明确需求再议，需与 nix develop 语义配合。

## Finding (2026-08-31) — test_seconds metric is BROKEN (not viable as a target)

- `WORKLOAD=test .auto/measure.sh` ALWAYS reports `test_seconds=0.000`, regardless of the
  real value (~1.5s: 107 tests pass in 1.36s + ~0.15s overhead).
- Root cause (bash -x trace): `test_ms="$(time_ms cargo test --quiet)"` — `cargo test --quiet`
  prints the test-harness output ("running 107 tests…", progress dots, "test result: ok…") to
  STDOUT, which is captured by the command substitution. `test_ms` becomes the multi-line string
  `"running 107 tests\n…\n1614"`. `ms_to_s "$test_ms"` passes it to perl, which coerces the
  leading-non-numeric string to 0 → `0.000`.
- By contrast `check` and `build` workloads are clean (cargo check/build print nothing to stdout),
  which is why check_seconds / build_seconds / binary_kb parse correctly.
- **test_seconds can never be measured correctly with the current measure.sh, and measure.sh is
  OFF-LIMITS in autoresearch** → test time is NOT a viable target for this loop.
- Fix (needs normal flow / user approval since measure.sh is off-limits): redirect the harness
  output inside the test case, e.g. `test_ms="$(time_ms cargo test --quiet >/dev/null 2>&1)"`
  (must be a one-line fix so only the ms echo is captured).

## Finding (2026-08-31) — build-std DEFINTIVELY infeasible in this environment (live-tested, hypothesis falsified)

- Motivating probe: exp #40 recorded build-std as "nightly-only, needs RUSTC_BOOTSTRAP hack, would
  slow every cargo cmd". Live tests this session correct the record:
  1. `-Z build-std=std` **IS accepted** on this stable cargo 1.98 with `RUSTC_BOOTSTRAP=1` — no
     nightly/rustup install is needed (the old "nightly-only" blocker was wrong). Verified via a
     safe probe: cargo proceeded past -Z parsing and failed only at a (deliberately nonexistent)
     target lookup, i.e. the flag was honored.
  2. The REAL blocker is environmental: the nix sysroot std source
     (`/nix/store/…/rust-default-1.98.0/lib/rustlib/src/rust/library/std`) declares `wasip1` as an
     external crates.io dependency. `wasip1` is NOT in the local cargo cache and NOT in Cargo.lock.
  3. crates.io is unreachable from this environment (persistent SSL connect error
     `0A000126:unexpected eof`, retried 3x per attempt across multiple attempts, online + offline
     both fail: offline says "no matching package named wasip1 found").
- **Conclusion: build-std cannot be executed here at all** — the std source tree cannot be
  resolved without crates.io. The binary_kb lever is closed by environment, not by scope.
  Even with user approval it would need: network/registry access OR vendoring std's deps
  (wasip1, compiler_builtins, cc, …) — outside code scope.
- Config was reverted (git checkout .cargo/config.toml); tree clean; binary_kb floor 1996 KB intact.
- This was the LAST untested lever. The binary_kb thread is now exhaustively closed:
  every in-scope dimension A/B-verified (opt=z, fat LTO, cgu=1, strip, panic=abort, outliner,
  linker, features, C-flags, sections) + every out-of-scope path tested and blocked
  (TLS swap = deps, async reqwest = arch, build-std = network).
