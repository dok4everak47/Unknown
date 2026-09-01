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

> `Capability-based execution`（只读模式，`MYAGENT_READ_ONLY`）、`Runtime` 抽象
> （工具副作用原语）、`Nix Runtime` 与 `Sandbox`（`MYAGENT_SANDBOX`，Seatbelt）
> 均已实现，见下方代码结构。

## 当前状态

已经实现（与代码保持一致）：

- [x] Rust CLI（交互式对话）
- [x] OpenAI-compatible Model provider
- [x] 多轮 conversation（保留完整历史）
- [x] Agent Loop
- [x] Tool Calling（模型可发起工具调用）
- [x] Streaming（SSE 流式输出：逐字显示回复，根治长请求被代理空闲超时掐断）
- [x] `read_file` 工具
- [x] `write_file` 工具
- [x] `search` 工具
- [x] `edit_file` 工具（精确文本替换）
- [x] `exec` 工具（受控的项目开发命令，白名单：`cargo check/test/build/clippy/fmt --check`）
- [x] Runtime abstraction（`Runtime` trait + `LocalRuntime`，工具的全部副作用原语）
- [x] Nix Runtime（`NixRuntime`：exec 经 `nix develop --command` 落在可复现 devShell）
- [x] Sandbox（`SandboxedRuntime` 装饰器：exec 经 `/usr/bin/sandbox-exec` 放进 macOS Seatbelt 沙箱，`MYAGENT_SANDBOX=1/true` 启用，`MYAGENT_SANDBOX_NETWORK=1/true` 放行网络；文件操作仍委托内层 runtime）
- [x] Capability-based execution（`Capabilities` 权限门；`MYAGENT_READ_ONLY=1/true` 只读模式）
- [x] Session persistence（conversation 保存/恢复，单 session）
- [x] REPL 行编辑（rustyline：Ctrl+L 清屏、↑↓ 历史、行内编辑；历史持久化到 `.myagent_history`，`MYAGENT_HISTORY` 可覆盖）
- [x] 基础路径边界校验（限制在工作目录内）
- [x] Tool Result 回传 Model 后生成最终回答
- [x] Nix Flake 开发环境

尚未实现：

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
| `src/model.rs` | `Model` trait + OpenAI-compatible provider + SSE 流式（`complete_streaming`）+ API 层序列化 |
| `src/tool.rs` | `Tool` 抽象 + `read_file` + `write_file` + `search` + `edit_file` + `exec` + 路径边界校验（纯逻辑，副作用经 `Runtime`） |
| `src/capabilities.rs` | `Capabilities` 权限门：`filesystem_read` / `filesystem_write` / `process_execute`，工具名→能力映射与 `allows` 判定 |
| `src/runtime.rs` | `Runtime` trait（读/写文件、列目录、执行命令的副作用原语）+ `LocalRuntime`（std 实现）+ 共享 `run_command`（exec 超时可配置，`MYAGENT_EXEC_TIMEOUT_SECS`） |
| `src/nix_runtime.rs` | `NixRuntime`：`Runtime` 第二实现（文件操作委托 `LocalRuntime`，exec 经 `nix develop --command` 在 devShell 中执行） |
| `src/sandbox.rs` | `SandboxedRuntime` 装饰器：把 `exec` 的衍生进程放进 macOS Seatbelt 沙箱（`/usr/bin/sandbox-exec`，SBPL 策略 deny 全写/全网 → allow ROOT+TMPDIR；`MYAGENT_SANDBOX` / `MYAGENT_SANDBOX_NETWORK` 控制），文件操作委托内层 runtime |
| `src/agent.rs` | Agent Loop：协调 `Model ↔ Tool` 多轮交互（可注入 fake Model 测试） |
| `src/session.rs` | conversation 持久化（`Session::load` / `Session::save`，JSON 格式） |
| `src/main.rs` | CLI entrypoint：加载/保存 session，读入用户输入（tty 下 rustyline 行编辑，非 tty 走 `BufRead::lines()`），创建 Model / Agent，显示结果 |

### 依赖方向

核心类型（`Message`）与 API 层类型（`ApiMessage`）分离；工具的副作用执行与工具逻辑解耦（`Runtime` trait），Model provider 不触碰文件系统；Agent Loop 只依赖 `Model` trait 与 `Tool` 的静态接口，并持有 `Runtime`（默认 `LocalRuntime`，测试可注入 fake）：

```text
Model → Response::ToolCall → Agent → Tool → Runtime → Filesystem
```

`Runtime` 有两个实现，CLI 用 `MYAGENT_RUNTIME` 环境变量选择：

- `local`（默认）— `LocalRuntime`，std 直连文件系统与进程；
- `nix` — `NixRuntime`，文件操作委托本地（nix 不虚拟化文件系统），exec 经
  `nix develop --command` 在 flake.nix 声明的可复现 devShell 中执行（构造时验证 nix 可用）。

exec 单次超时默认 60 秒，可用 `MYAGENT_EXEC_TIMEOUT_SECS`（正整数秒）调整——
沙箱内冷构建 / LTO 较慢时调大；未设置走默认，非法取值（0 / 非数字 / 溢出）会
清晰报错并退出。

工具执行前还有一道能力门（`Capabilities`，`src/capabilities.rs`），CLI 用
`MYAGENT_READ_ONLY` 控制：`1` / `true` 时为只读模式（`write_file` / `edit_file` /
`exec` 被拒，拒绝作为 Tool Result 回传 Model，不触碰 `Runtime`），其余为全允许。
`MYAGENT_READ_ONLY` 与 `MYAGENT_RUNTIME` 正交可组合，默认行为零变化。

再外层是可选装饰器 `SandboxedRuntime`（`src/sandbox.rs`），CLI 用 `MYAGENT_SANDBOX`
控制：`1` / `true` 时，`exec` 被包装为 `sandbox-exec -p <policy> <cmd>`，把 cargo 及其
衍生的 `build.rs` / proc-macro / 测试二进制放进 macOS Seatbelt 沙箱（SBPL 策略先
deny 全部写与网络，再仅放行工作目录与 `TMPDIR` 两个 subpath 的写）；文件操作仍直接
委托内层 runtime。`MYAGENT_SANDBOX_NETWORK=1/true` 显式放行沙箱内网络（默认关，
不会随 `MYAGENT_SANDBOX=1` 隐式开启）。启用时若非 macOS 或 `/usr/bin/sandbox-exec`
不可用则构造失败、清晰报错并退出，绝不静默降级为不隔离。与 `MYAGENT_RUNTIME`（local /
nix）、`MYAGENT_READ_ONLY` 三方正交可组合，默认行为零变化。

> 已验证局限：`MYAGENT_RUNTIME=nix` + `MYAGENT_SANDBOX=1`（sandbox-exec 包
> `nix develop`）当前不可用——nix 需在 `$HOME/.cache/nix` 等目录写锁文件，
> 被策略拒绝（不为之放宽 `$HOME` 写权限）。等效用法：在 `nix develop` shell
> 内启动 agent 再加 `MYAGENT_SANDBOX=1`——工具链仍是 nix 的，cargo /
> build.rs 同被 Seatbelt 禁锢（已端到端验证）。

手动验证隔离效果（需在普通终端运行，嵌套沙箱环境会自动报错退出）：
`./scripts/seatbelt-policy-test.sh`——直测 Seatbelt 策略（5 种写逃逸攻击
均被拒、ROOT/TMPDIR 写入放行、网络默认关 / opt-in 放行）。

## Quick Start

进入开发环境：

```bash
nix develop
```

配置模型 API（使用 OpenAI-compatible 接口，本项目用 DeepSeek 做过验证）。
推荐方式：复制 `.env.example` 为 `.env` 并填入配置，程序启动时自动从工作目录
的 `.env` 加载（`KEY=VALUE`，`#` 注释，支持引号）：

```bash
cp .env.example .env
# 编辑 .env：
#   OPENAI_API_KEY=...          # 必填
#   OPENAI_BASE_URL=...         # 可选，默认 https://api.openai.com/v1
#   OPENAI_MODEL=...            # 可选，默认 gpt-4o-mini
```

`.env` 含密钥，已被 `.gitignore` 忽略。优先级：**真实环境变量 > `.env` 文件 >
代码默认值**（命令行临时注入会覆盖 `.env`）。也可以直接用环境变量：

```bash
export OPENAI_API_KEY="..."
```

> 环境变量默认值见 `OpenAICompatibleModel::new`（`src/model.rs`），加载逻辑见
> `src/config.rs`。

运行：

```bash
cargo run                 # 默认：exec 直接在当前环境执行（MYAGENT_RUNTIME=local），全能力
MYAGENT_RUNTIME=nix cargo run   # exec 经 `nix develop --command` 在 devShell 中执行
MYAGENT_READ_ONLY=1 cargo run   # 只读模式：不能写文件 / 不能执行命令（与 MYAGENT_RUNTIME 正交）
MYAGENT_SANDBOX=1 cargo run     # 沙箱：exec 放进 macOS Seatbelt，默认禁网
MYAGENT_SANDBOX=1 MYAGENT_SANDBOX_NETWORK=1 cargo run  # 沙箱 + 放行网络
MYAGENT_EXEC_TIMEOUT_SECS=300 cargo run   # exec 超时调到 300s（默认 60s；冷构建/LTO 较慢时用）
```

启动时会在 stderr 打印当前能力模式（`capabilities: full` / `capabilities: read-only`）
与沙箱状态（`sandbox: on (network: off)` / `sandbox: on (network: ON)`），便于确认设置是否生效。
`MYAGENT_SANDBOX` 与 `MYAGENT_RUNTIME`、`MYAGENT_READ_ONLY` 正交可组合（例如
`MYAGENT_SANDBOX=1 MYAGENT_RUNTIME=nix`：`sandbox-exec` 包裹 `nix develop --command`，
策略对整个进程树生效）。

示例对话：

```text
You: 请读取 src/main.rs，然后告诉我它做了什么。
[tool] read_file {"path":"src/main.rs"}
AI: src/main.rs 实现了……（基于文件内容的回答）

You: /exit
```

退出方式：输入 `/exit`，或按 `Ctrl-D`（EOF）。

交互模式（stdin 是终端时）使用 rustyline 行编辑：

- `Ctrl-L` 立即清屏（提示符回到顶部，不需要回车）
- `↑` / `↓` 浏览输入历史，`←` / `→` 移动光标修改当前行
- `Ctrl-C` 放弃当前行、回到新提示符（bash 语义，不退出进程）
- `Ctrl-D` 退出；`/exit` 退出；空行跳过

历史记录只存用户输入文本（不存 AI 输出），每次退出时保存到 `.myagent_history`
（默认在工作目录，可用 `MYAGENT_HISTORY` 环境变量覆盖），下次启动自动加载。
该文件与 `session.json`（对话持久化）互不干扰，已加入 `.gitignore`。
非 tty（管道输入 / `</dev/null` / 脚本）不使用 rustyline，行为与以前一致。

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
- 单次执行 60 秒超时（默认；可用 `MYAGENT_EXEC_TIMEOUT_SECS` 调整为其他秒数）；stdout / stderr / 退出码全部返回给模型

当 `MYAGENT_RUNTIME=nix` 时，exec 会被包装为 `nix develop --command <cmd>`，在
flake.nix 声明的可复现 devShell 中执行（文件操作仍走本地）。flake 的 `shellHook`
横幅只在交互式 tty 下打印，不会污染 exec 工具的输出。

exec 本身仍不是完整 command execution（无 shell、白名单、60 秒默认超时，
可用 `MYAGENT_EXEC_TIMEOUT_SECS` 调整）。
当需要真实隔离时（`MYAGENT_SANDBOX=1`），exec 会被包装为
`sandbox-exec -p <policy> <cmd>`：Seatbelt 在 OS 层约束 cargo 及其衍生的
`build.rs` / proc-macro / 测试二进制的写入路径与网络（deny 全写/全网 → 仅放行工作目录
与 `TMPDIR`；`MYAGENT_SANDBOX_NETWORK=1/true` 显式放行网络）。详细设计见
[`docs/sandbox-design.md`](docs/sandbox-design.md)。

## Roadmap

```text
Tool system（扩展更多 typed tools）
    ↓
Capability-based execution ✅（已实现：只读模式，MYAGENT_READ_ONLY）
    ↓
Sandbox ✅（已实现：Seatbelt 真实隔离，MYAGENT_SANDBOX）
```

尚未实现（未来方向）：

- 更多工具
- 更好的错误处理
- sessions
- MCP
- subagents
