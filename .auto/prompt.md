# Autoresearch Prompt: reduce `cargo check` latency

- Status: **ready, not started**
- Created: 2026-08-31
- Owner: Pi autoresearch
- Reviewer: Codex
- Primary metric: `check_seconds`（lower is better）

> 这是第一次 autoresearch 会话建议。默认目标选择 `cargo check` 耗时，因为 `.auto/measure.sh` 的默认 workload 就是 `check`，且它是当前增量开发中最常运行的反馈命令。
>
> 如果要改为优化 `cargo test`、`cargo build --release` 或 binary size，必须先修改本文件的 Goal / Metric，再开始实验。

---

## Goal

在不改变 Agent 行为、不新增功能、不新增依赖、不扩大架构范围的前提下，降低：

```bash
WORKLOAD=check .auto/measure.sh
```

报告的 `check_seconds`。

优化对象是当前 Rust 项目的 **check 反馈延迟**。实验可以做小规模代码组织或编译配置调整，但必须保持所有用户可见行为不变。

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
check_seconds
```

由以下命令输出：

```bash
WORKLOAD=check .auto/measure.sh
```

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

- `WORKLOAD=test .auto/measure.sh` → `test_seconds`
- `WORKLOAD=build .auto/measure.sh` → `build_seconds`
- `WORKLOAD=size .auto/measure.sh` → `binary_kb`

只有 primary metric 改善且 checks 全部通过，才允许保留实验。

### Noise / confidence rule

- 单次结果接近噪声范围时，不要急于 `keep`。
- confidence score `< 1.0x` 时先复跑确认。
- 若改善不稳定，记录为噪声或 inconclusive。
- 不允许通过修改 `.auto/measure.sh`、减少检查、跳过测试或改变 workload 语义来“改善”指标。

---

## Baseline

尚未测量。

开始实验前第一步：

1. 确认 `.auto/prompt.md` 已按流程提交，且代码工作区干净。
2. 运行：

   ```bash
   WORKLOAD=check .auto/measure.sh
   ```

3. 记录 baseline `check_seconds`。
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
   - 记录 baseline。
   - 连续运行几次 `WORKLOAD=check .auto/measure.sh`，观察 warm/cold 波动。

2. **审查现有依赖 features**
   - 当前只有 `reqwest`、`serde`、`serde_json`。
   - 可评估 `reqwest` 默认 features 是否包含当前 OpenAI-compatible blocking JSON client 不需要的能力。
   - 风险：HTTPS / TLS / HTTP client 行为可能受影响；必须通过 mock-HTTP 集成测试和真实构建检查。

3. **审查 dev profile 编译配置**
   - 只考虑对 `cargo check` 有实际影响且不损害开发体验的配置。
   - 不允许为了指标牺牲正确性或调试能力。

4. **审查模块组织**
   - 若存在大型模块导致无效重编译，可考虑最小拆分。
   - 但不要为了“看起来更架构化”而抽象 Runtime / Executor。

5. **避免测试代码影响普通 check 的误判**
   - `cargo check` 与 `cargo test` 的编译目标不同；不要把 test-only 慢编译误判为普通 check 慢。

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

| ID | Hypothesis | Files changed | Before | After | Result | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| — | 尚未开始 | — | — | — | — | baseline 待测量 |

---

## Ideas for later（不在 autoresearch 中实现）

新功能、工具、架构方向统一记录到 `.auto/ideas.md`。退出 autoresearch 后，再按 `AGENTS.md` 和 `docs/agent-collaboration.md` 的正常审批流程讨论。
