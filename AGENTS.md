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
src/main.rs      CLI entrypoint（输入输出、创建 Model / Agent、加载/保存 session）
src/agent.rs     Agent Loop（Model ↔ Tool 协调、可注入 fake Model 测试）
src/message.rs   conversation message 类型（Role / Message / ToolCall）
src/model.rs     Model trait + OpenAI-compatible provider
src/tool.rs      Tool 抽象 + read_file + write_file + search + edit_file + exec + 路径边界校验
src/session.rs   conversation 持久化（Session::load / Session::save）
```

核心类型与 API 层类型分离（`Message` vs `ApiMessage`）；工具执行与 Model provider 解耦：

```text
Model → Response::ToolCall → Agent → Tool → Filesystem
```

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
- [ ] exec
- [ ] Runtime abstraction
- [ ] Nix Runtime
- [ ] Sandbox
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

## Current Scope

当前只实现：

```text
Model
Conversation
Agent Loop
Tool Calling
read_file
write_file
search
edit_file
exec（受控开发命令，白名单）
session persistence（单 session 保存/恢复）
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

没有明确任务时不要提前实现这些。

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
