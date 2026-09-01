# Autoresearch Prompt: reduce release binary size

- Status: **ended (final) — binary_kb 5852 → 2060 KB (-64.8%) across 107 experiments in the CURRENT toolchain (rustc LLVM 22.1.8 + Apple CommandLineTools clang + Apple ld64). NOTE: the old-toolchain floor was 1928 KB (-67.1%); a toolchain shift (LLVM 22 codegen + Apple libLTO) added an inherent +148 KB regression (__text +82,932 + __const +56,593 B) that #102-#107 proved unrecoverable in-scope (every codegen dimension re-verified: -Oz, Rust outliner -32 KB full value, C-outliner no-op via all channels, threshold absent, scheduler closed, merge/global-merge/tail-merge no-ops). Apple clang (-16 KB) + Apple ld64 (-16 KB) recovered 32 KB of the shift. Floor 2060 KB is certified terminal: deterministic, byte-accounted, metric-quantized at 4 KB (515 blocks). Zero in-scope behavior-preserving hypotheses remain.**
- Created: 2026-08-31
- Owner: Pi autoresearch
- Reviewer: Codex
- Primary metric: `binary_kb`（lower is better）

> **最终状态（continuation session 补完）**：在 Nix Runtime / Capability 功能落地后，
> 续接会话又找到 4 个真实杠杆，floor 从 1996 KB 降至 **1928 KB**：
> - #51 `-fvisibility=hidden`（CFLAGS，隐藏 BoringSSL 非 API 导出）−8 KB
> - #52 `build.rs cargo:rustc-link-arg-bins=-Wl,-no_exported_symbols`（仅 bin，导出表清空）−8 KB
> - #55 `CFLAGS=-flto`（aws-lc C 转 bitcode，ld64 链接期 LTO 按函数粒度去死代码）−36 KB
> - #56 `-Wl,-mllvm,-enable-machine-outliner=always`（C 代码 link-LTO 机器 outliner）−16 KB
>
> **完整关闭清单（75 实验）**：profile（opt=z/fat LTO/cgu=1/strip/abort）、Rust LLVM
> passes（outliner −32 KB 保留，其余 A/B 或不可用）、C-LTO passes（outliner −16 KB 保留，
> 其余 no-op/不可用）、CFLAGS（-Oz/-flto/fvisibility，unwind/function-sections 均为
> Mach-O no-op）、linker flags（no_pie 因硬化拒绝、no_function_starts/no_compact_unwind
> 为行为变更已量化、embed-bitcode 与 lto 互斥）、依赖 features（全部验证最小）、
> 结构/段/签名/填充/绑定（零可删内容+零可删填充）、crash-diagnostics 量化矩阵
> （fn-starts→1916、unwind_info→1908、合 1896 KB 可实现；eh_frame/gcc_except_tab
> 无任何移除机制）、确定性（sha256 一致）。
>
> **仅剩路径 = 用户决策（正常审批流程）**：
> 1. crash-diagnostics flags → **1896 KB**（build.rs 加两行，牺牲崩溃回溯粒度）
> 2. reqwest `[patch]`-fork → 移除 platform-verifier（改 CA 信任源）/ tls12（仅 TLS 1.3）
> 3. build-std → 需 crates.io/registry 网络（wasip1 未缓存，std 源码树无法解析）
>
> 此前 1996 KB 结论中的 build-std 阻塞描述已纠正：真正阻塞是环境网络（crates.io
> 不可达），不是 nightly/rustup。

> 原目标 `check_seconds` 已在实验 #5 确认到达硬性下限（0.09-0.11s = cargo floor 0.04s + active-graph metadata scan 0.05s；对源码大小、fingerprint 数量、-j、incremental 均不变）。
> 按本文件规定的切换流程，现改为优化 **release 二进制体积** `binary_kb`（`WORKLOAD=size .auto/measure.sh`）。

---

## Goal

在不改变 Agent 行为、不新增功能、不新增依赖、不扩大架构范围的前提下，降低：

```bash
WORKLOAD=size .auto/measure.sh
```

报告的 `binary_kb`（`du -sk target/release/myagent`）。

优化对象是当前 Rust 项目的 **release 二进制体积**。实验只允许做编译配置（`[profile.release]` 等）或行为保持的代码调整，必须保持所有用户可见行为不变。

## Non-goals

本次 autoresearch **不做**：

- 新 Tool
- Runtime abstraction
- Nix Runtime
- Sandbox
- Capability system
- MCP
- subagents
- planner / context compression / benchmark runner 等新功能
- 通用 shell 或 exec 白名单扩展
- 新依赖
- 大规模重构
- 为了降低 metric 而修改测量脚本或 correctness gate

---

## Metrics

### Primary metric

```text
binary_kb
```

由以下命令输出：

```bash
WORKLOAD=size .auto/measure.sh
```

（需要先 `WORKLOAD=build .auto/measure.sh` 生成 release 二进制；该命令同时报告 `build_seconds` 作为副观测。）

方向：**lower is better**。

### Correctness gates

每次实验后必须运行：

```bash
.auto/checks.sh
```

该脚本必须真实执行并通过：

```bash
cargo fmt --check
cargo check --quiet
cargo clippy --quiet
cargo test --quiet
```

任何 gate 失败都必须记录为 `checks_failed`，不得 `keep`。

### Secondary observations

可观察但不得替代 primary metric：

- `WORKLOAD=build .auto/measure.sh` → `build_seconds`（release 构建时间，LTO/opt 配置会使其上升，需权衡）
- `WORKLOAD=test .auto/measure.sh` → `test_seconds`
- `WORKLOAD=check .auto/measure.sh` → `check_seconds`（已到达下限，仅作回归监测）

只有 primary metric 改善且 checks 全部通过，才允许保留实验。

### Noise / confidence rule

- 单次结果接近噪声范围时，不要急于 `keep`。
- confidence score `< 1.0x` 时先复跑确认。
- 若改善不稳定，记录为噪声或 inconclusive。
- 不允许通过修改 `.auto/measure.sh`、减少检查、跳过测试或改变 workload 语义来“改善”指标。

---

## Baseline

新目标的 baseline 尚未测量（旧目标 check_seconds 的 baseline/结果见下方 Tried experiments 与 .auto/log.jsonl）。

开始实验前第一步：

1. 确认 `.auto/prompt.md` 已按流程提交，且代码工作区干净。
2. 运行：

   ```bash
   WORKLOAD=build .auto/measure.sh
   ```

3. 记录 baseline `binary_kb`（与 `build_seconds`）。
4. 运行：

   ```bash
   .auto/checks.sh
   ```

5. 将 baseline 和 checks 结果写入 `.auto/log.jsonl`。

---

## Files in scope

允许修改：

- `src/**`
- `Cargo.toml`（仅限已有依赖的 feature / profile 等编译配置；**不允许新增依赖**）
- `.auto/prompt.md`（会话目标、约束、已尝试内容）
- `.auto/log.jsonl`（实验日志）
- `.auto/ideas.md`（新功能 / 新方向 backlog）

### Scope notes

- `src/**` 只允许做行为保持的编译时间优化。
- `Cargo.toml` 不允许添加新 crate；如调整现有依赖 features，必须证明 HTTPS JSON API 行为不受影响。
- `.auto/ideas.md` 只记录发现的新功能想法，不在 autoresearch 中实现。

---

## Off limits

禁止修改：

- `.auto/measure.sh`
- `.auto/checks.sh`
- `Cargo.lock`（除非工具因合法配置变更自动更新；不得手工随意改）
- `flake.nix`
- `.envrc`
- `rustfmt.toml`
- `README.md`
- `AGENTS.md`
- `docs/**`
- `.env`
- `session.json`
- `target/**`

禁止行为：

- 新增依赖
- 删除或跳过测试
- 放宽 clippy / fmt / check / test gate
- 修改 metric 输出格式
- 改变 Tool / Model / Agent / Session 的用户可见行为
- 引入 Runtime / Sandbox / Capability / MCP / subagents
- 借优化之名做架构重写

---

## Experiment protocol

每个实验必须是一个小假设：

```text
Hypothesis → change one thing → measure → checks → keep/discard → log
```

流程：

1. 记录 baseline。
2. 每次只改一个变量。
3. 运行：

   ```bash
   WORKLOAD=check .auto/measure.sh
   ```

4. 运行：

   ```bash
   .auto/checks.sh
   ```

5. 记录：
   - hypothesis
   - changed files
   - primary metric before / after
   - checks result
   - keep / discard / checks_failed / crash
   - asi（假设、结果、教训）
6. 只有 primary metric 真正改善且 checks 通过，才 `keep`。
7. 无效实验必须回退代码改动；`.auto/` 日志保留。
8. 若发现新功能或架构方向，写入 `.auto/ideas.md`，不要直接实现。

---

## Initial idea backlog

这些只是候选假设，不代表必须执行：

1. **先测量，不猜测**
   - 记录 release baseline。
   - 检查二进制内部构成（`cargo bloat` / `llvm-size` 如可用），确认大头（aws-lc BoringSSL、hyper、tokio、rustls、serde_json、我们的代码）。

2. **`[profile.release]` 体积配置**（行为保持，首选）
   - `strip = true`（去掉符号表，直接减小磁盘体积）
   - `opt-level = "z"`（以体积为目标的代码生成，替换默认 opt-level=3）
   - `lto = true`（fat LTO，跨 crate 消除死代码；build_seconds 会上升）
   - `codegen-units = 1`（配合 LTO 效果最佳）
   - 注意：`panic = "abort"` 会改变 release 行为（abort vs unwind），且可能影响 `cargo test`，默认不做，除非确认行为等价。

3. **审查现有依赖 features**
   - 已完成的 reqwest 裁剪（exp #3，drop h2/encoding_rs 等）同时缩小了二进制。
   - 可评估是否还有可安全关闭的 feature。

4. **`#[cfg(test)]` 死代码**
   - 确认测试专用代码不进 release 二进制（通常由 cfg 保证；验证即可）。

5. **避免把 build_seconds 上升误判为失败**
   - LTO / opt-level=z 会显著增加 release 构建时间；binary_kb 是 primary，build_seconds 是权衡项。

---

## Decision rules

保留实验必须同时满足：

- `check_seconds` 有可信改善；
- `.auto/checks.sh` 全部通过；
- 没有新增依赖；
- 没有改变用户可见行为；
- 没有越界修改 off-limits 文件；
- 日志中清楚记录假设和教训。

以下情况必须 discard / checks_failed：

- checks 失败；
- metric 改善在噪声内；
- 需要新增依赖；
- 需要改变 Agent/Tool/Model/Session 行为；
- 修改了 `.auto/measure.sh` 或 `.auto/checks.sh`；
- 改动变成大规模重构。

---

## Tried experiments

### check_seconds 会话（已结束，到达下限）

| ID | Hypothesis | Files changed | Before | After | Result | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| #1 | Baseline check_seconds | — | 0.144s | — | keep | warm no-op check |
| #2 | Noise probe | — | 0.144s | 0.120s | keep | ~0.02s run-to-run variance |
| #3 | Trim reqwest features (drop h2/encoding_rs/etc.) | Cargo.toml, Cargo.lock | 0.144s | 0.108s | keep | dep graph 165 pkgs; only real lever |
| #4 | Stability confirmation | — | 0.108s | 0.108s | keep | src size irrelevant to metric |
| #5 | Floor conclusion (clean rebuild 423→131 fps) | — | 0.108s | 0.116s | keep | floor = cargo 0.04s + graph scan 0.05s |

### binary_kb 会话（当前）

| ID | Hypothesis | Files changed | Before | After | Result | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| #102-#107 | New-toolchain re-optimization (see .auto/log.jsonl and ideas.md) | .cargo/config.toml + .cargo/cc-wrap.sh + build.rs | 2092 | 2060 KB | keep/discard mix | Apple clang + Apple ld64 recovered 32 KB of the +148 KB LLVM-22 toolchain shift; all codegen dimensions re-verified; 2060 KB certified terminal floor |

---

## Ideas for later（不在 autoresearch 中实现）

新功能、工具、架构方向统一记录到 `.auto/ideas.md`。退出 autoresearch 后，再按 `AGENTS.md` 和 `docs/agent-collaboration.md` 的正常审批流程讨论。
