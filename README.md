# myagent

从零使用 Rust 构建的、以 Agent Runtime 为核心的 AI coding agent 实验项目。

它不是 Pi / OpenCode / Claude Code 的复制品，而是在探索一个更清晰的原语组合：

```text
Agent
+
Typed Tools
+
Runtime
+
Nix
```

> ⚠️ 注意：`Nix Runtime` 目前**尚未实现**，只作为未来方向。`Runtime` 抽象（工具副作用原语）已实现，见下方代码结构。

## 当前状态

已经实现（与代码保持一致）：

- [x] Rust CLI（交互式对话）
- [x] OpenAI-compatible Model provider
- [x] 多轮 conversation（保留完整历史）
- [x] Agent Loop
- [x] Tool Calling（模型可发起工具调用）
- [x] `read_file` 工具
- [x] `write_file` 工具
- [x] `search` 工具
- [x] `edit_file` 工具（精确文本替换）
- [x] `exec` 工具（受控的项目开发命令，白名单：`cargo check/test/build/clippy/fmt --check`）
- [x] Runtime abstraction（`Runtime` trait + `LocalRuntime`，工具的全部副作用原语）
- [x] Session persistence（conversation 保存/恢复，单 session）
- [x] 基础路径边界校验（限制在工作目录内）
- [x] Tool Result 回传 Model 后生成最终回答
- [x] Nix Flake 开发环境

尚未实现：

- [ ] Nix Runtime
- [ ] Sandbox
- [ ] Capability system
- [ ] MCP
- [ ] subagents

## Architecture

```text
User
 ↓
Agent Loop
 ↓
Model
 ↓
Text / ToolCall
 ↓
Tool
 ↓
Tool Result
 ↓
Model
 ↓
Final Response
```

代码结构：

| 文件 | 职责 |
| --- | --- |
| `src/message.rs` | conversation message 类型（`Role`、`Message`、`ToolCall`） |
| `src/model.rs` | `Model` trait + OpenAI-compatible provider + API 层序列化 |
| `src/tool.rs` | `Tool` 抽象 + `read_file` + `write_file` + `search` + `edit_file` + `exec` + 路径边界校验（纯逻辑，副作用经 `Runtime`） |
| `src/runtime.rs` | `Runtime` trait（读/写文件、列目录、执行命令的副作用原语）+ `LocalRuntime`（std 实现） |
| `src/agent.rs` | Agent Loop：协调 `Model ↔ Tool` 多轮交互（可注入 fake Model 测试） |
| `src/session.rs` | conversation 持久化（`Session::load` / `Session::save`，JSON 格式） |
| `src/main.rs` | CLI entrypoint：加载/保存 session，读入用户输入，创建 Model / Agent，显示结果 |

### 依赖方向

核心类型（`Message`）与 API 层类型（`ApiMessage`）分离；工具的副作用执行与工具逻辑解耦（`Runtime` trait），Model provider 不触碰文件系统；Agent Loop 只依赖 `Model` trait 与 `Tool` 的静态接口，并持有 `Runtime`（默认 `LocalRuntime`，测试可注入 fake）：

```text
Model → Response::ToolCall → Agent → Tool → Runtime → Filesystem
```

## Quick Start

进入开发环境：

```bash
nix develop
```

配置模型 API（使用 OpenAI-compatible 接口，本项目用 DeepSeek 做过验证）：

```bash
export OPENAI_API_KEY="..."         # 必填
export OPENAI_BASE_URL="..."        # 可选，默认 https://api.openai.com/v1
export OPENAI_MODEL="..."           # 可选，默认 gpt-4o-mini
```

> 环境变量默认值见 `OpenAICompatibleModel::new`（`src/model.rs`）。项目根目录的 `.env` 不会被程序自动加载，需手动 `export` 或 `set -a && . ./.env && set +a` 注入。

运行：

```bash
cargo run
```

示例对话：

```text
You: 请读取 src/main.rs，然后告诉我它做了什么。
[tool] read_file {"path":"src/main.rs"}
AI: src/main.rs 实现了……（基于文件内容的回答）

You: /exit
```

退出方式：输入 `/exit`，或按 `Ctrl-D`（EOF）。

对话会自动保存到 `session.json`（可用 `MYAGENT_SESSION` 环境变量指定路径）：

- 启动时自动加载已有 conversation（文件不存在则从空对话开始）
- 每轮成功完成后保存
- 当前只支持单 session 的保存/恢复，不是完整的 session management

## Development

进入 Nix 开发环境后，运行检查：

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy
```

测试覆盖消息序列化、API 响应解析、工具参数解析、路径边界校验（含 `..` 跳转、绝对路径、symlink 逃逸），`read_file` / `write_file` / `search` / `edit_file` / `exec` 的工具执行，Agent Loop 的协调逻辑（用 fake Model 注入，验证文本响应、单次/多次/批量 Tool Call、Tool 错误回传、Model 错误传播、`MAX_TOOL_ROUNDS` 上限），session 的 save→load round-trip（含空/多轮/带 ToolCall 的 conversation）与损坏文件错误处理，以及 `OpenAICompatibleModel` ↔ Agent 的 mock-HTTP 集成测试（验证真实 provider 的请求序列化、ToolCall → 工具执行 → Tool Result 回传、API 错误传播，不依赖外部 LLM）。

Pi 与 Codex 协作时，遵循 [`docs/agent-collaboration.md`](docs/agent-collaboration.md)：默认一个 Agent 负责实现，另一个 Agent 只读审查，用户负责授权与合并。

## 关于 `exec` 的边界

`exec` 目前是**受控的项目开发命令执行**，不是通用 shell：

- 只允许白名单命令：`cargo check` / `cargo test` / `cargo build` / `cargo clippy` / `cargo fmt --check`
- 不使用 shell（无 `sh -c` / `bash -c`），通过 `std::process::Command` 直接传可执行文件与参数
- 命令在项目工作目录内执行，继承当前环境变量，模型无法修改环境
- 单次执行 60 秒超时；stdout / stderr / 退出码全部返回给模型

它**不是** sandbox，也不代表完整 command execution。

## Roadmap

以下均为**未来方向**，尚未实现：

```text
Tool system（扩展更多 typed tools）
    ↓
Nix Runtime
    ↓
Capability-based execution
    ↓
Sandbox
```

具体包括：

- 更多工具
- 更好的错误处理
- streaming
- sessions
- MCP
- subagents
