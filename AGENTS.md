# AGENTS.md

## Project

这是一个从零使用 Rust 构建 AI Agent Runtime 的实验项目。

它不是 Pi / OpenCode / Claude Code 的复制品，而是在探索：

```text
Agent
+
Typed Tools
+
Runtime
+
Nix
```

项目从第一性原理出发，优先设计更清晰的原语，而不是照搬现有 Agent 的行为。

## Architecture

当前核心架构：

```text
CLI
 ↓
Agent Loop
 ↓
Model
 ↓
Tool
 ↓
Tool Result
 ↓
Model
 ↓
Final Response
```

当前代码位置：

```text
src/main.rs      CLI entrypoint（输入输出、加载 .env 配置、创建 Model / Agent、选择 Runtime/Capabilities、加载/保存 session）
src/config.rs    .env 配置加载（KEY=VALUE；环境变量优先于 .env；纯 std 解析）
src/agent.rs     Agent Loop（Model ↔ Tool 协调、持有 Runtime、可注入 fake Model / fake Runtime 测试）
src/message.rs   conversation message 类型（Role / Message / ToolCall）
src/model.rs     Model trait + OpenAI-compatible provider + SSE 流式（complete_streaming）
src/tool.rs      Tool 抽象 + read_file + write_file + search + edit_file + exec + 路径边界校验（纯逻辑，副作用经 Runtime）
src/capabilities.rs   Capabilities：工具执行前的权限门（filesystem_read / filesystem_write / process_execute）
src/runtime.rs   Runtime trait（副作用原语）+ LocalRuntime（std 实现）+ 共享 run_command
src/nix_runtime.rs   NixRuntime：Runtime 第二实现（文件操作委托 LocalRuntime，exec 经 `nix develop --command` 落在 devShell）
src/sandbox.rs   SandboxedRuntime 装饰器：把 exec 的衍生进程放进 macOS Seatbelt 沙箱（/usr/bin/sandbox-exec + SBPL 策略 deny 全写/全网 → allow ROOT+TMPDIR；MYAGENT_SANDBOX / MYAGENT_SANDBOX_NETWORK 控制），文件操作委托内层 runtime
src/session.rs   conversation 持久化（Session::load / Session::save）
```

核心类型与 API 层类型分离（`Message` vs `ApiMessage`）；工具执行与 Model provider 解耦；
工具的副作用执行与工具逻辑解耦（`Runtime` trait）：

```text
Model → Response::ToolCall → Agent → Tool → Runtime → Filesystem
```

`Runtime` 有两个实现：

- `LocalRuntime`（默认）— std 直连文件系统与进程；
- `NixRuntime` — 文件操作委托 `LocalRuntime`，exec 经 `nix develop --command`
  在 flake.nix 声明的可复现 devShell 中执行（不改变文件语义）。

CLI 通过 `MYAGENT_RUNTIME` 环境变量选择：`local`（默认）/ `nix`（构造时验证 nix 可用）。

再外层是可选装饰器 `SandboxedRuntime`（`MYAGENT_SANDBOX=1/true` 启用，**默认关**）：
`exec` 被包装为 `sandbox-exec -p <policy> <cmd>`，把 cargo 及其衍生的 build.rs /
proc-macro / 测试二进制放进 macOS Seatbelt 沙箱；文件操作仍直接委托内层 runtime。
`MYAGENT_SANDBOX_NETWORK=1/true` 显式放行沙箱内网络（默认关，不随
`MYAGENT_SANDBOX=1` 隐式开启）。启用时若非 macOS 或 `/usr/bin/sandbox-exec` 不存在
→ 构造失败、清晰报错并退出，**绝不静默降级为不隔离**。与 `MYAGENT_RUNTIME`（local /
nix）、`MYAGENT_READ_ONLY` 三方正交可组合。

权限分化由 `MYAGENT_READ_ONLY` 控制（与 `MYAGENT_RUNTIME` 正交可组合）：
取值为 `1` / `true`（大小写不敏感）时启用只读模式——`write_file` / `edit_file`
与 `exec` 在工具分发处被能力门拦下，拒绝作为 Tool Result 回传 Model，不触碰 Runtime；
其余取值 / 未设置一律按“全允许”处理，默认行为零变化。

## Principles

1. **Keep the core small** — 只实现当前任务需要的功能。
2. **Prefer typed tools over shell commands** — 文件操作是 typed tool，不是 shell 命令。
3. **Keep Model independent from Tool execution** — Model 只表达意图，不直接执行文件操作。
4. **Keep Tool execution independent from Model provider** — provider 不触碰文件系统。
5. **Avoid premature abstractions** — 不为"以后可能有用"而加抽象。
6. **Prefer standard library where practical** — 能用 std 解决就不用新 crate。
7. **Make the smallest change necessary** — 不重写能正常工作的代码。

## Incremental Development Workflow

项目采用**增量式开发（incremental development）**。

每实现一个独立功能，都必须立即进行验证。

### 1. 每个功能完成后必须运行 `cargo check`

每完成一个功能、一个明确的开发任务或一个逻辑单元后，必须运行：

```bash
cargo check
```

如果 `cargo check` 出现编译错误：

- 不要把错误交给用户处理
- AI 必须自己分析错误原因
- AI 必须自己修改代码
- 再次运行 `cargo check`
- 重复这个过程，直到 `cargo check` 成功

流程：

```text
Implement
    ↓
cargo check
    ↓
Error?
 ┌──┴──┐
Yes    No
 │      │
 ▼      ▼
Fix    Continue
 │
 └──→ cargo check
```

不要在代码无法通过 `cargo check` 的情况下宣称当前功能完成。

### 2. 功能完成后必须进行自我检查

当一个功能实现并通过 `cargo check` 后，AI 应该检查：

- 当前功能是否真正满足任务要求
- 是否引入了不必要的复杂度
- 是否破坏已有功能
- 是否存在明显的错误处理问题
- 是否需要测试
- 当前架构是否仍然清晰

根据检查结果，必要时自行修复问题。

### 3. AI 必须主动规划下一步

完成当前功能后，AI 应该根据：

- 当前代码状态
- 项目架构
- README.md
- AGENTS.md
- 当前 roadmap
- 已经实现的功能

自行判断：

> **下一步最合理应该实现什么？**

AI 应该向用户提出一个明确的下一步建议，而不是直接开始实现。

例如：

```text
Current feature: read_file

Suggested next step:
Implement write_file because it complements the existing
read_file capability and is the next minimal filesystem tool.

Reason:
...
```

### 4. 下一步必须获得用户明确同意

AI 可以：

- 分析下一步
- 提出建议
- 解释为什么这是合理的下一步
- 提供 1～3 个候选方向

但是：

> **未经用户明确同意，不得开始实现下一步功能。**

也就是说：

```text
Implement current feature
        ↓
cargo check
        ↓
Self-review
        ↓
Determine next step
        ↓
Present proposal to user
        ↓
WAIT FOR USER APPROVAL
        ↓
User approves
        ↓
Implement next feature
```

绝对不要：

```text
完成 A
 ↓
自动实现 B
 ↓
自动实现 C
 ↓
自动实现 D
```

### 5. 用户拥有最终开发决策权

AI 是：

```text
Developer + Architect Assistant
```

而不是：

```text
Autonomous Developer
```

AI 可以主动思考和提出方案，但最终的：

- 功能选择
- 架构方向
- Roadmap 顺序
- 是否继续开发

必须由用户决定。

### 6. 不要因为“下一步很明显”而跳过确认

即使 AI 认为某个功能是显而易见的下一步，也必须等待用户确认。

例如：

```text
AI:
read_file 已完成。

我建议下一步实现 write_file，因为它与 read_file
组成最基本的 filesystem tool pair。

是否继续实现 write_file？
```

只有用户明确同意后才能继续。

### 7. 单次任务边界

一次用户授权只代表：

> **实现用户明确授权的当前功能。**

完成后必须停止，并重新提出下一步建议。

不要把用户的一次：

```text
“实现 read_file”
```

理解成：

```text
“把整个 Tool 系统都实现了”
```

### 8. Verification Hierarchy

最基本的验证要求：

```bash
cargo check
```

如果当前任务适合测试，则进一步运行：

```bash
cargo test
```

如果当前任务适合完整质量检查，则运行：

```bash
cargo fmt --check
cargo clippy
```

但无论如何：

> 每完成一个功能，至少必须成功通过 `cargo check`。

### 9. Completion Report

完成一个功能后，向用户报告：

```text
Implemented:
- ...

Verification:
- cargo check: passed
- cargo test: passed / not required
- cargo clippy: passed / not required

Self-review:
- ...

Suggested next step:
- ...

Reason:
- ...

Waiting for approval.
```

报告完成后停止，不继续实现建议中的下一步。

## README TODO Tracking

`README.md` 必须包含一个 `TODO` / `Roadmap` 状态区域，用于记录项目的功能开发进度。

每完成一个功能，都必须同步更新 `README.md` 中的 TODO：

- 已完成的功能标记为 `[x]`
- 尚未完成的功能标记为 `[ ]`
- 只记录实际的功能状态
- 不要把尚未实现的功能标记为完成

例如：

```markdown
## TODO

- [x] CLI
- [x] Model abstraction
- [x] OpenAI-compatible provider
- [x] Multi-turn conversation
- [x] Agent Loop
- [x] Tool Calling
- [x] read_file
- [x] write_file
- [x] search
- [x] exec
- [x] Runtime abstraction
- [x] Nix Runtime
- [x] Sandbox（Seatbelt 真实隔离，MYAGENT_SANDBOX）
```

### Workflow

每完成一个功能：

```text
Implement feature
      ↓
cargo check
      ↓
Tests / verification
      ↓
Update README.md TODO
      ↓
Self-review
      ↓
Propose next step
      ↓
Wait for user approval
```

README TODO 的更新属于当前功能完成的一部分，不能遗漏。

如果当前功能实际上没有完成，不得为了让 TODO 看起来完整而标记 `[x]`。

AI 仍然必须遵守现有规则：

> 完成当前功能后，可以根据 TODO 和项目状态提出下一步建议，但未经用户明确同意，不得实现下一步功能。

## Autoresearch（pi-autoresearch 插件）

本项目已安装 [pi-autoresearch](https://github.com/davebcn87/pi-autoresearch) 插件，用于**自主优化实验循环**：提出想法 → 测量 → 保留有效 / 回退无效 → 重复。

它**不是**功能开发的替代流程，而是增量式开发之外的一个独立实验模式，**仅用于优化类目标**。

### 何时使用

- 只有用户显式发起时才进入该模式（`/skill:autoresearch-create` 或 `/autoresearch <text>`）。
- 适合本项目的优化目标（metric 均为 **lower is better**）：
  - `cargo build` 编译时间（秒）
  - `cargo check` 检查时间（秒）
  - `cargo test` 测试时间（秒）
  - 二进制体积（KB，`du -sk target/release/myagent`）
- 不适合：新功能、新工具、架构变更（这些仍走正常的审批流程）。

### 会话结构

所有会话文件放在工作目录根部的 `.auto/` 文件夹（该文件夹必须提交，且不会因实验回退而丢失）：

| 文件 | 用途 |
| --- | --- |
| `.auto/prompt.md` | 会话文档：目标、metric、files in scope、off limits、已尝试内容（会话的心脏，续跑时据此恢复） |
| `.auto/measure.sh` | 基准脚本，输出 `METRIC name=value` 行 |
| `.auto/log.jsonl` | 每次实验的追加日志（由工具写入） |
| `.auto/checks.sh` | 正确性检查（必需，见下方“本项目的硬约束”） |
| `.auto/ideas.md` | 想法 backlog（可选） |
| `.auto/hooks/` | 生命周期 hooks（可选，见 autoresearch-hooks skill） |

启动流程：

1. `git checkout -b autoresearch/<goal>-<date>`
2. 阅读源码，深入理解工作负载后再动手
3. 写入 `.auto/prompt.md` 与 `.auto/measure.sh` 并提交
4. `init_experiment` → 跑 baseline → `log_experiment` → 开始循环

### 本项目的硬约束

自动优化实验同样必须遵守本文件的验证要求，在 `.auto/checks.sh` 中落实：

```bash
#!/bin/bash
set -euo pipefail
cargo fmt --check
cargo check
cargo test
cargo clippy
```

- 任何实验改动不得破坏 `cargo fmt --check` / `cargo check` / `cargo test` / `cargo clippy`。checks 失败 → `checks_failed`，不得 `keep`。
- 遵守 Principles：优先 std、不添加不必要依赖、只做最小改动。
- 每个实验结果都用 `log_experiment` 的 `asi` 参数记录假设与教训（失败/回退的实验尤其要记录——代码被回退后，日志是唯一留存）。
- 只对真正改进 primary metric 且通过 checks 的实验 `keep`；`discard` / `crash` 自动回退代码改动（`.auto/` 保留）。
- 关注 confidence score：<1.0× 说明在噪声内，先复跑确认再决定是否 keep。

### 与增量式开发规则的关系

- autoresearch 只允许修改 `.auto/prompt.md` 中声明的 **Files in Scope** 内的代码。
- 循环中发现的**新功能 / 新工具 / 架构方向**不得自行实现，应记入 `.auto/ideas.md`，退出循环后按正常审批流程提出。
- 优化实验完成后用 `/skill:autoresearch-finalize` 整理成干净的、从 merge-base 出发的独立分支；合入前仍需通过完整验证（`cargo fmt --check` / `cargo check` / `cargo test` / `cargo clippy`）并更新 README TODO。

## Current Scope

当前只实现：

```text
Model
Conversation
Agent Loop
Tool Calling
streaming（SSE 流式输出，complete_streaming）
read_file
write_file
search
edit_file
exec（受控开发命令，白名单）
session persistence（单 session 保存/恢复）
Runtime abstraction（Runtime trait + LocalRuntime，工具副作用原语）
Nix Runtime（NixRuntime：exec 落在 nix devShell，MYAGENT_RUNTIME 选择）
Capability-based execution（Capabilities 权限门，MYAGENT_READ_ONLY=1/true 只读模式）
Sandbox（SandboxedRuntime 装饰器：exec 经 sandbox-exec 放进 macOS Seatbelt 沙箱，MYAGENT_SANDBOX=1/true 启用，MYAGENT_SANDBOX_NETWORK=1/true 放行网络；文件操作委托内层 runtime）
.env 配置文件（工作目录 .env 自动加载，环境变量优先；模板见 .env.example）
```

不要自动实现 roadmap 中的功能。每一步只做被明确要求的、最小的一步。

## Important Restrictions

除非任务明确要求，否则不要：

- 添加新的 Tool
- 添加 shell / exec（通用命令执行）
- 添加 MCP
- 添加 subagents
- 添加 session persistence
- 添加 sandbox
- 添加 Nix Runtime
- 大规模重构
- 更换 Model provider
- 添加新依赖（除非当前代码确实缺少）

## Nix

Nix 当前仅用于：

```text
Development Environment
```

即 `nix develop` 提供的 Rust 工具链，**不是** Runtime。

未来才会探索：

```text
Nix
 ↓
Runtime
 ↓
Capabilities
 ↓
Sandbox
```

`Nix`（Development Environment）、`Runtime` 抽象、`Capabilities` 权限门与
`Sandbox`（Seatbelt）均已实现（见 Current Scope）。没有明确任务时不要提前实现这些之外的下一步。

## Verification

任何代码修改完成后至少运行：

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy
```

如果修改涉及实际 Agent 行为（Agent Loop、Tool、Model 交互），应进行一次手动测试。

## Documentation Rule

README.md 和 AGENTS.md 必须与实际代码保持一致。

如果代码架构发生变化，应同步更新相关文档。
