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

> ⚠️ 注意：`Runtime` 与 `Nix Runtime` 目前**尚未实现**，只作为未来方向。

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
- [x] 基础路径边界校验（限制在工作目录内）
- [x] Tool Result 回传 Model 后生成最终回答
- [x] Nix Flake 开发环境

尚未实现：

- [ ] `exec`
- [ ] Nix Runtime
- [ ] Sandbox
- [ ] Capability system
- [ ] MCP
- [ ] subagents
- [ ] session persistence

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
| `src/tool.rs` | `Tool` 抽象 + `read_file` + `write_file` + `search` + `edit_file` + 路径边界校验 |
| `src/main.rs` | CLI 与 Agent Loop |

### 依赖方向

核心类型（`Message`）与 API 层类型（`ApiMessage`）分离；工具执行完全在 `tool.rs` 中，Model provider 不直接触碰文件系统：

```text
Model → Response::ToolCall → Agent → Tool → Filesystem
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

## Development

进入 Nix 开发环境后，运行检查：

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy
```

测试覆盖消息序列化、API 响应解析、工具参数解析、路径边界校验（含 `..` 跳转、绝对路径、symlink 逃逸），以及 `read_file` / `write_file` / `search` / `edit_file` 的工具执行。测试不依赖外部 LLM。

## Roadmap

以下均为**未来方向**，尚未实现：

```text
Tool system（扩展更多 typed tools）
    ↓
Runtime abstraction
    ↓
Nix Runtime
    ↓
Capability-based execution
    ↓
Sandbox
```

具体包括：

- `exec` 等更多工具
- 更好的错误处理
- streaming
- sessions
- MCP
- subagents
