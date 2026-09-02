# Runtime 设计文档

> 状态：**§5 条件 2 已触发；`Runtime` trait 与 `NixRuntime` 已落地（2026-08-31）；
>       §5 条件 3 已触发；`Capabilities` 权限门已落地（2026-08-31，见 §9）；
>       Sandbox（Seatbelt 装饰器）已落地（2026-09-01，见 §10）；
>       §5 条件 1 已触发；`RuntimeConfig`（exec 超时可配置）已落地（2026-09-01，见 §11）**
> 关联：AGENTS.md、README TODO、docs/sandbox-design.md
> §6 方案 B / 方案 C 草案与实际实现的差异见文末 §8 / §9「已落地形态」

---

## 1. 当前执行模型

### 数据流

```text
Agent (src/agent.rs)
  ↓  持有 root: PathBuf = current_dir()
  ↓
Tool::execute_in(&root)   (src/tool.rs)
  ├── read_file   → read_file_within(root, path)
  ├── write_file  → write_file_within(root, path, content)
  ├── search      → search_within(root, query, path)
  ├── edit_file   → edit_file_within(root, path, old, new)
  └── exec        → exec_within(root, command)
        ↓
        std::process::Command::new(program)
          .args(args)
          .current_dir(root)      ← cwd 固定为项目根
          .stdout(piped).stderr(piped)
        ↓
        继承当前进程环境变量       ← env 不可控
        ↓
        60 秒超时 (EXEC_TIMEOUT)  ← 硬编码常量
        ↓
        exit code + stdout + stderr
```

### 当前执行环境的全部事实

| 维度 | 当前实现 | 代码位置 |
| --- | --- | --- |
| **cwd** | `Agent.root`（`current_dir()` 捕获），传入每个 `*_within` | `agent.rs`、`tool.rs` |
| **environment** | 继承父进程 env（`Command` 默认行为），模型无法修改 | `exec_within` |
| **timeout** | `EXEC_TIMEOUT = 60s` 常量，仅 exec 有 | `tool.rs:9` |
| **filesystem boundary** | `resolve_within`：拒绝绝对路径、`..`、symlink 逃逸 | `tool.rs:540` |
| **process execution** | 唯一一处 `Command::new`，白名单 `cargo check/test/build/clippy/fmt --check` | `exec_within` |
| **tool execution context** | `root: &Path` 参数透传 | `Tool::execute_in` |

### 当前限制

1. **执行环境与进程绑定**：所有工具都运行在 Agent 进程自身所在的环境（cwd 来自 `current_dir()`，env 来自父进程）。无法让"某个工具"运行在隔离/不同的环境里。
2. **超时硬编码**：`EXEC_TIMEOUT` 是常量，调用者无法调整（60s 对慢构建可能不够，对死循环又太长）。
3. **env 不可注入**：模型无法控制环境变量（这是设计选择——模型不应能改 PATH 等，但也是限制：无法为 exec 提供受控环境）。
4. **无权限细分**：所有 5 个工具对模型一视同仁，没有"只读工具集 vs 可写工具集"的概念。
5. **路径边界与执行耦合**：`resolve_within` 与文件工具耦合在 `tool.rs` 内部，未来若 exec 需要不同边界（如只允许项目内构建产物）需要重构。

这些**是限制，但不是缺陷**——它们恰好是"单环境单 Agent"模型下最简形态。见 §2。

---

## 2. 为什么现在还没有 Runtime abstraction

### 根本原因：只有一个执行环境

Runtime abstraction 的价值 = **多个具体实现之间的公共接口**。抽象的本质是从 N 个实现中提取共性，当 N = 1 时，抽象是纯负债：

| 抽象对象 | 当前实现数 | 是否值得抽象 |
| --- | --- | --- |
| 文件系统执行 | 1（本地目录） | 否 |
| 进程执行 | 1（`Command::new`） | 否 |
| 路径边界 | 1（`resolve_within`） | 否 |
| 超时策略 | 1（60s 常量） | 否 |

### 没有第二个 backend

当前没有：

- 第二个文件系统后端（如虚拟 FS、远程 FS）
- 第二个进程执行后端（如 sandbox、container、remote executor）
- 第二个工具集（如只读模式、不同权限级别）

没有 N = 2，就没有"公共接口"可言。强行抽象会得到：

```rust
// 反模式：只有一个实现却抽象
trait Runtime {
    fn execute(&self, tool: &Tool) -> Result<...>;
}
struct LocalRuntime; // 唯一实现
```

这等于把 `tool.rs` 里的函数换个名字放进 struct，**不增加任何表达能力**。

### abstraction pressure 不足

AGENTS.md Principle 5 明确禁止"为以后可能有用而加抽象"。当前代码中：

- `root` 已经作为参数注入（`execute_in(&root)`），这是**已有的最小解耦**
- 每个 `*_within` 只调用一次 `resolve_within`，无重复
- 没有出现"同一逻辑写两遍"的情况

**结论**：当前不存在需要 abstraction 消除的重复或耦合。Runtime 是待触发的事件驱动设计，不是待实现的预置架构。

---

## 3. Nix 在未来可能承担什么角色

> 前提：**不假设 Nix 一定成为 Runtime**。以下分析 Nix 各能力与项目需求的匹配度。

### Nix 能提供的

| 能力 | 说明 | 与当前项目的匹配度 |
| --- | --- | --- |
| **reproducible environment** | 同一 flake 在任何机器产生相同工具链 | ✅ 已部分实现（`nix develop` 提供 Rust 工具链） |
| **nix develop** | 进入可复现开发 shell | ✅ 已实现（`flake.nix`），这是**项目环境**层，不是 Runtime |
| **flake** | 声明式定义依赖与构建 | ✅ 已用于开发环境 |
| **build environment** | 在 `nix build` 的纯净环境里构建 | ⚠️ 当前 exec 白名单没有 `nix build`，未使用 |
| **dependency isolation** | 依赖与系统隔离 | ✅ Cargo.lock 已锁定 Rust 依赖；Nix 管编译工具链 |
| **sandbox** | `nix build` 默认 chroot 隔离 | ⚠️ 这是**构建期**隔离，不是 Agent 工具执行期隔离 |

### 关键判断

1. **Nix 当前的角色是"项目环境"，不是"Runtime"**。`nix develop` 提供 shell，Agent 进程跑在这个 shell 里，工具继承其环境。这已经"免费"获得了 Nix 的 reproducibility——因为**整个 Agent 运行在 Nix 环境内**。

2. **Nix 成为 Runtime 意味着什么**：`exec` 工具调用 `nix develop -c <command>` 或 `nix build`，让**每次工具执行**都在一个可复现的纯净环境里跑。这解决的是"工具执行的可复现性"，当前 exec 只跑白名单 cargo 命令（本身就在 Nix shell 里），**没有这个需求**。

3. **sandbox 是安全目标，不是当前需求**：sandbox 用于防御"模型恶意/错误行为破坏宿主"。当前模型是用户自己的 API key（可信），且 exec 白名单 + 路径边界已是最小权限。引入 sandbox 是给"可信模型 + 白名单"叠加一层防御，收益低于成本。

### 结论

Nix 未来最可能承担的角色是**执行环境提供者**（`NixRuntime` 作为 Stage 2 的一个 backend），但那是"出现需求后"的演进，不是现在的目标。当前 Nix 只做 `nix develop`（开发环境），保持现状。

---

## 4. Runtime 演化路线

### Stage 0 — 当前状态（现状）

```text
Agent { model, root: PathBuf }
  ↓
Tool::execute_in(&root)
```

- 单环境、单 backend、单 Agent
- root 硬编码为 `current_dir()`（`Agent::new`）
- 超时、env 全部隐式

### Stage 1 — 最小 Runtime context

引入一个**纯数据**的执行上下文（不引入 trait，不引入多 backend）：

```rust
pub struct RuntimeContext {
    pub root: PathBuf,        // 文件工具边界 + exec cwd
    pub timeout: Duration,    // 从 EXEC_TIMEOUT 常量提升为可配置
    pub env: Vec<(String, String)>,  // 可选：exec 环境变量覆盖
}
```

改动面：

```rust
// 现状：root 单独注入
Tool::execute_in(&self, root: &Path)

// Stage 1：上下文整体注入
Tool::execute_in(&self, ctx: &RuntimeContext)
```

**这是纯重构**：把 `EXEC_TIMEOUT` 从常量变成字段，把 `root` 与 timeout 收拢为一个 struct。不改变任何行为，只是让"执行参数"成为显式数据。**价值**：测试可注入短 timeout（补 `TimedOut` 测试盲区）；未来加字段不需要改函数签名。

**触发条件**：需要调整超时 / 注入环境时（见 §5 条件 1）。

### Stage 2 — 多个 execution backend

引入 trait，出现第二个实现：

```rust
pub trait Executor {
    /// 执行一个工具调用，返回文本结果
    fn execute(&self, tool: &Tool, ctx: &RuntimeContext) -> Result<String, ToolError>;
}

pub struct LocalExecutor;      // 现状：Command::new + resolve_within
pub struct NixExecutor;        // 未来：nix develop -c / nix build
pub struct ContainerExecutor;  // 未来：容器内执行
```

`Agent` 改为持有 `Box<dyn Executor>` 或泛型：

```rust
pub struct Agent<M: Model, E: Executor> {
    model: M,
    executor: E,
}
```

**触发条件**：出现第二个 backend 的真实需求（见 §5 条件 2）。

### Stage 3 — Capability-based execution

从"按 backend 区分"进化为"按权限区分"：

```rust
pub struct Capabilities {
    pub filesystem_read: bool,
    pub filesystem_write: bool,
    pub process_execute: bool,
}

impl Capabilities {
    /// read-only 模式：只允许 read_file / search
    pub fn read_only() -> Self { ... }
}
```

`Executor` 在执行前检查 capability：

```rust
fn execute(&self, tool: &Tool, ctx: &RuntimeContext, caps: &Capabilities) -> Result<...> {
    match tool {
        Tool::WriteFile(_) if !caps.filesystem_write => Err(ToolError::Forbidden),
        Tool::Exec(_) if !caps.process_execute => Err(ToolError::Forbidden),
        _ => self.dispatch(tool, ctx),
    }
}
```

**触发条件**：出现"不同 Agent 不同权限"或"读模式/写模式"的真实场景（见 §5 条件 3）。

---

## 5. 触发 Runtime abstraction 的实际条件

> 不是"未来可能需要"，而是**哪些代码变化会迫使 Runtime 出现**。

### 条件 1：需要可变超时或可控环境

**当前状态**：`EXEC_TIMEOUT` 是 `tool.rs` 里的 `const`。

**触发场景**：以下任意一个发生：
- 用户/调用方需要为 exec 设置不同于 60s 的超时（如构建允许 300s，测试只给 30s）
- 需要为 exec 注入受控环境变量（如 `RUST_BACKTRACE=1`、离线 `CARGO_NET_OFFLINE=true`）
- 需要补 `TimedOut` 分支的测试（当前无法注入短超时）

**迫使**：`RuntimeContext`（Stage 1）出现。因为不把 timeout/env 变成数据，就无法在不改签名的情况下传入。

**这是最可能先发生的条件**——它是配置化需求，不是多环境需求。

### 条件 2：出现第二个执行 backend

**触发场景**：以下任意一个发生：
- exec 需要运行 `nix build` / `nix develop -c`（构建环境可复现）
- exec 需要运行"非 cargo"命令（如 `git`、`cargo expand`），但希望限制在某种隔离内
- 需要支持远程/容器执行（CI 里跑 Agent 工具）

**迫使**：`Executor` trait（Stage 2）出现。因为第二个 backend 需要与 `LocalExecutor` 共享同一调用接口，trait 是唯一干净的方式。

### 条件 3：出现权限分化

**触发场景**：以下任意一个发生：
- 同一进程内存在两个 Agent，一个只读一个可写
- 用户想要"review 模式"（模型只能读不能写）
- 模型工具集需要按任务动态裁剪（如 `cargo test` 任务不给 write 权限）

**迫使**：`Capabilities`（Stage 3）出现。因为权限检查需要成为执行前的一道门。

### 条件 4：工具数量翻倍导致执行参数膨胀

**触发场景**：工具从 5 个增长到 ~10 个，且出现需要各自不同 cwd / timeout / env 的工具（如 `exec` 用项目根、`search` 用另一个索引目录）。

**迫使**：`RuntimeContext` 或 `Executor` 出现——因为 `root` 单参数无法表达"每个工具不同上下文"。

---

## 6. 最小 Rust API 草案（只设计，不实现）

### 方案 A：纯数据 context（Stage 1 目标）

```rust
// src/runtime.rs（未来）
pub struct RuntimeContext {
    pub root: PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

impl RuntimeContext {
    pub fn new(root: PathBuf) -> Self { ... }      // 默认 timeout: 60s, env: []
}

// Tool 签名变化
impl Tool {
    pub(crate) fn execute_in(&self, ctx: &RuntimeContext) -> Result<String, ToolError>;
}
```

**优点**：无 trait、无多态、纯数据；改动最小（`Agent.root` → `Agent.ctx: RuntimeContext`）；测试友好（可注入短 timeout）。
**缺点**：不解决多 backend；env 语义需明确（覆盖 or 追加）。

### 方案 B：trait Executor（Stage 2 目标）

```rust
// src/executor.rs（未来）
pub trait Executor {
    fn execute(&self, tool: &Tool, ctx: &RuntimeContext) -> Result<String, ToolError>;
}

pub struct LocalExecutor;

impl Executor for LocalExecutor {
    fn execute(&self, tool: &Tool, ctx: &RuntimeContext) -> Result<String, ToolError> {
        // 现有 tool.rs 逻辑移入
    }
}

// Agent 泛型化
pub struct Agent<M: Model, E: Executor = LocalExecutor> {
    model: M,
    executor: E,
}
```

**优点**：支持多 backend；默认 `LocalExecutor` 保持 `Agent::new` 兼容。
**缺点**：引入 trait；需要把 `tool.rs` 的 5 个 `*_within` 函数收进 `LocalExecutor`（重构）；`Agent` 加一个泛型参数（所有测试需适配）。

> **已落地形态与草案的差异**：实际实现没有用 `execute(&Tool, &RuntimeContext)`，
> 而是把 trait 方法收敛为**四个副作用原语**（`read_file` / `write_file` / `read_dir`
> / `exec`），命令解析、白名单与路径策略仍留在 `tool.rs`。这样工具层零改动，
> 两个实现（`LocalRuntime` / `NixRuntime`）只实现副作用本身。详见 §8。

### 方案 C：capability 门（Stage 3 目标）

```rust
// src/capabilities.rs（未来）
#[derive(Clone, Copy)]
pub struct Capabilities {
    pub filesystem_read: bool,
    pub filesystem_write: bool,
    pub process_execute: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self { filesystem_read: true, filesystem_write: true, process_execute: true }
    }
}
```

**优点**：权限模型清晰；与 `Executor` 正交（可组合）。
**缺点**：依赖 Stage 2 的 `Executor` 才有意义；过早引入会变成死代码。

### 方案比较

| 维度 | A: RuntimeContext | B: Executor trait | C: Capabilities |
| --- | --- | --- | --- |
| 解决的问题 | 配置化（timeout/env） | 多 backend | 权限分化 |
| 引入成本 | 低（纯数据） | 中（trait + 重构） | 中（需 E 配合） |
| 测试价值 | 高（补 TimedOut） | 中 | 低（当前无场景） |
| 触发条件 | 条件 1 | 条件 2 | 条件 3 |

**推荐顺序**：A → B → C。A 是 B 的前置（B 的 `Executor` 需要 `RuntimeContext` 作为参数），C 依赖 B 才有载体。不要跳过 A 直接做 B（会引入 trait 却没有第二个实现，纯负债）。

---

## 7. 当前明确不做

已落地：`Runtime` trait（§5 条件 2，见 §8）、`NixRuntime`（§8）、`Capabilities`
权限门（§5 条件 3，见 §9）、`Sandbox`（Seatbelt 装饰器，见 §10）、`RuntimeConfig`
（§5 条件 1，见 §11）、`SshRuntime`（远程 backend，见 §12）。以下仍**未实现**，
且当前**无需求**，不要提前引入：

- **Container / remote executor（容器化）** —— 经 `ssh` 转发到远程已落地（§12，
  覆盖“远程执行”需求）；容器 / Docker 类隔离仍无需求
- **exec 白名单扩展** —— 有明确需求再议，需与 nix develop / sandbox 语义配合

**判据**：上述任何一项，只有当 §5 的对应条件出现时才值得实现。

## 8. 已落地形态（2026-08-31）

§5 条件 2 已触发：exec 需要运行在可复现环境（nix devShell）。实际落地：

- `Runtime` trait（`src/runtime.rs`）：四个副作用原语 `read_file` / `write_file` /
  `read_dir` / `exec`。与 §6 方案 B 草案的差异是**没有 `execute(&Tool)`**——
  工具层保持纯逻辑，命令解析 / 白名单 / 路径策略仍在 `tool.rs`。
- `LocalRuntime`（`src/runtime.rs`）：std 直连文件系统与进程。
- `NixRuntime`（`src/nix_runtime.rs`）：文件操作**委托** `LocalRuntime`（组合，
  nix 不虚拟化文件系统，语义与本地完全一致）；exec 经 `nix develop --command`
  在 flake.nix 声明的 devShell 中执行。构造时 `nix --version` 验证可用性；
  argv 构造为纯函数 `nix_develop_argv`（无 nix 环境下可单元测试）。
- 两个实现共用 `run_command`（spawn + 60s 超时轮询 + stdout/stderr 合并 + 退出码），
  超时 / 输出 / 退出码语义逐字节一致（零行为回归）。
- `Agent` 持有 `Box<dyn Runtime>`；CLI 用 `MYAGENT_RUNTIME=local|nix` 选择
  （local 默认，nix 不存在时清晰报错并 exit 1）。
- `flake.nix` `shellHook` 横幅改为仅交互式 tty 打印，避免污染
  `nix develop --command` 的输出。

仍未实现（后续方向）：Sandbox、Container、RuntimeContext（timeout / env 可配）。
Capability-based execution 已落地（见 §9）。

## 9. Capabilities 已落地形态（2026-08-31）

§5 条件 3 已触发：需要支持“只读 / 受限模式”——同一 Agent 可被配置为只能读。
实际落地的是 §6 方案 C 的最小形态：

- `Capabilities`（`src/capabilities.rs`）：三个布尔字段 `filesystem_read` /
  `filesystem_write` / `process_execute`，`Default` 全允许（行为零变化），
  `Capabilities::read_only()` 为只读模式（read=true，write/execute=false）。
- 工具名 → 所需能力的映射（纯函数 `required_capability`）：
  `read_file` / `search` → `filesystem_read`；`write_file` / `edit_file` →
  `filesystem_write`；`exec` → `process_execute`；未知名返回 `None`（不拦截，
  交给 `Tool::from_call` 的未知工具错误路径）。
- 判定 `Capabilities::allows(tool_name) -> bool`（无能力要求的工具名返回 `true`）
  与 `denied_capability_name(tool_name) -> Option<&'static str>`（构造拒绝消息）。
- `Agent` 新增 `capabilities: Capabilities` 字段；`new_with_runtime` 签名不变
  （内部委托 `new_with_runtime_and_caps` + `Default`），新增
  `new_with_runtime_and_caps(model, runtime, caps)` 公开构造器。

**与 §6 方案 C 草案的差异**：草案假设能力门依赖 Stage 2 的 `Executor`（“依赖
E 才有意义”）。实际落地**没有引入 Executor**——门是纯布尔检查，放在
`Agent::run_turn` 的工具分发处（`Tool::from_call` 成功之后、`tool.execute` 之前）：

```text
Model → Response::ToolCall → Agent
  ↓ allows(&call.name)？
  ├─ 否 → 拒绝作为 Tool Result 回传（不触碰 Runtime）
  └─ 是 → Tool::from_call → tool.execute(Runtime) → Filesystem
```

被拒时回传的消息复用现有工具错误格式：
`tool error: permission denied: write_file requires filesystem_write capability`，
与“工具错误回传模型不中止循环”的语义一致。能力门是执行前的一道布尔检查，
未引入权限继承、角色或策略引擎，也未修改 `Runtime` trait。

CLI 用 `MYAGENT_READ_ONLY` 控制（与 `MYAGENT_RUNTIME` 正交可组合）：
`1` / `true`（大小写不敏感）→ `Capabilities::read_only()`；其余 / 未设置 →
`Capabilities::default()`（全允许）。启动时在 stderr 打印一行当前模式
（`capabilities: full` / `capabilities: read-only`）便于确认。

## 10. Sandbox 已落地形态（2026-09-01）

`Sandbox` 从“§7 明确不做”转为落地：需求触发点是**让 exec 的衍生进程（cargo →
build.rs / proc-macro / 测试二进制）获得 OS 层真实隔离**，作为能力门之上的
第二道防御（`docs/sandbox-design.md` 全文）。实现是一个**装饰器 runtime**，不是
第四个 backend：

- `SandboxedRuntime`（`src/sandbox.rs`）：持有内层 `Box<dyn Runtime>`；
  `read_file` / `write_file` / `read_dir` 直接委托内层（文件操作语义零变化）；
  `exec` 把命令包装为 `/usr/bin/sandbox-exec -p <policy> <cmd>`，放进 macOS
  Seatbelt 沙箱。
- SBPL 策略（`sandbox_policy` 纯函数）：`(version 1)` + `(allow default)`，
  然后 `(deny network*)`（仅 network=off 时）、`(deny file-write*)`，最后
  仅 allow 工作目录（ROOT）与 `TMPDIR` 两个 subpath 的写；
  `MYAGENT_SANDBOX_NETWORK=1/true` 时不输出 deny network 行。
  注意 SBPL 后匹配者生效，deny 必须在 allow 之前；ROOT/TMPDIR 注入前 canonicalize。
- 构造安全：非 macOS 或 `/usr/bin/sandbox-exec` 不存在 →
  `io::Error`（`io::Error::other` / `ErrorKind::NotFound`，清晰报错、CLI 退出），
  **绝不静默降级为不隔离**。
- CLI：`MYAGENT_SANDBOX=1/true` 启用（默认关）；与 `MYAGENT_READ_ONLY`
  正交可组合；与 `MYAGENT_RUNTIME=local` 正交可组合。启动时 stderr 打印
  `sandbox: on (network: off)` / `sandbox: on (network: ON)`。
  **已验证局限**：`MYAGENT_RUNTIME=nix` + `MYAGENT_SANDBOX=1`（sandbox-exec 包
  `nix develop`）当前不可用：nix 评估 flake 需写 `$HOME/.cache/nix/
  fetcher-locks/` 与 `$HOME/.local/state/nix/profiles/`，均在 ROOT/TMPDIR 之外
  被拒；不为之放宽策略（profiles 是 GC roots / profile 符号链接）。等效且已
  端到端验证的用法：在 `nix develop` shell 内（local runtime）启动 agent +
  `MYAGENT_SANDBOX=1`。未来路径：`XDG_CACHE_HOME`/`XDG_STATE_HOME` 重定向进
  ROOT/TMPDIR（已验证 nix 尊重），需 env 注入能力，另立任务。
- 测试：9 个单测（任何平台）+ 3 个 gated 集成测试（攻击矩阵 /
  恶意 build.rs + 无害对照组 / 网络边界），gated 测试运行时自动探测
  sandbox-exec 真实可用才执行，嵌套沙箱环境自动跳过（对齐 nix 冒烟测试
  模式）；需在普通终端验证真实隔离。

**与 §6 草案的差异**：草案（方案 B/C）没有装饰器形态；实际落地保持 `Runtime`
trait 不变，把沙箱作为**包裹层**加在构造链最外层，因此对 `tool.rs`、`agent.rs`
零改动，且天然可与 NixRuntime、Capabilities 组合。

## 11. RuntimeContext（exec 超时可配置）已落地形态（2026-09-01）

§5 条件 1 已触发：需要为 exec 设置不同于 60s 的超时（沙箱内冷构建 / LTO 较慢），
且 `ExecError::TimedOut` 路径在默认 60s 超时下完全测不到——测试需要可注入的短超时。
实际落地的是 §6 方案 A 的**最小子集**（只取 timeout，不取 env）：

- `RuntimeConfig`（`src/runtime.rs`）——执行参数结构：
  `pub struct RuntimeConfig { pub exec_timeout: Duration }`，`Default` =
  60s（`EXEC_TIMEOUT` 常量保留为默认值来源）。config 挂在**实现结构体**上
  （`LocalRuntime` / `NixRuntime` / `SandboxedRuntime` 各持一份），
  **不改 `Runtime` trait 形状**——`Runtime::exec` 签名不变。
- `run_command` 增加 `timeout: Duration` 参数（轮询逻辑不变，`EXEC_TIMEOUT`
  换成传入的 `timeout`）；`LocalRuntime` / `NixRuntime` / `SandboxedRuntime`
  各自把 `self.config.exec_timeout` 传给 `run_command`。
- `LocalRuntime` 从单元结构体改为持有 `config: RuntimeConfig`：
  `LocalRuntime::new(config)` 公开构造；`Default` 委托
  `RuntimeConfig::default()`（行为零变化）；`#[cfg(test)] with_timeout(duration)`
  测试构造器，用于覆盖 `TimedOut` 路径。
- `NixRuntime::new(config)`：`nix --version` 可用性探测用**默认超时**（60s）；
  exec 用 `config.exec_timeout`。
- `SandboxedRuntime::new(root, network, config, inner)`：它不调 inner.exec
  （自己 re-wrap 后调 `run_command`），故同样需要 config。
- CLI：`MYAGENT_EXEC_TIMEOUT_SECS`（秒）——未设置 → 默认 60s；设置了但非法
  （0 / 非数字 / 溢出）→ 清晰报错并 exit 1（与 nix 不可用的处理风格一致）。
  解析为纯函数 `parse_exec_timeout`（`src/main.rs`，单测覆盖）。启动横幅**不**
  打印超时（避免噪声）。
- 测试：`LocalRuntime::with_timeout(200ms)` 覆盖——`sh -c "sleep 1"` →
  `Err(TimedOut)`；`sh -c "echo before; sleep 1"` → `TimedOut(output)` 保留部分
  输出；`sh -c "echo ok"` 在超时内正常完成 → `Ok` code=0；`RuntimeConfig::default()`
  = 60s；`parse_exec_timeout` 的合法/非法输入单测。

**env 覆盖仍不做**：§5 条件 1 的"可控环境变量"部分未触发——沙箱设计已把
“继承完整环境”定为 v1 接受的局限（`docs/sandbox-design.md` §5.2），模型不应
能改 PATH 等；`RuntimeConfig` 刻意只含 `exec_timeout`，env 覆盖/清洗/allowlist
留作未来项。§6 方案 A 的 `RuntimeContext`（root+timeout+env 收拢）**未**整体落地
——root 仍由 `Agent` 持有并注入，只落地了 timeout 一个字段。

**与 §6 草案的差异**：草案的 `RuntimeContext` 是 `root + timeout + env` 三字段
并整体替换 `Tool::execute_in(&root)`。实际落地**没有引入 context 结构，也没有改
工具层签名**——config 直接挂在 runtime 实现上，改动面收敛在 `runtime.rs` 与其
调用点，工具层与 Agent 零架构变化（仅测试里 `&LocalRuntime` 机械更新为
`&LocalRuntime::default()`）。

## 12. SSH Runtime 已落地形态（2026-09-01）

§5 条件 2（第二个执行 backend）再次触发：需求是把 myagent 的“文件系统与进程”
放到远程主机——本机只做终端与 Agent 逻辑，工具的全部副作用（读/写文件、列目录、
exec）落在远程。实际落地是第三个 `Runtime` 实现 `SshRuntime`
（`src/ssh_runtime.rs`）：

- **零新依赖 + 与登录 shell 解耦**：只调用系统 `ssh` 可执行文件
  （`std::process::Command`），不引入 ssh2 / openssh crate。argv 固定为
  `ssh -T -o BatchMode=yes -o ConnectTimeout=10 [-p PORT] [USER@]HOST -- sh -s`
  （纯函数 `ssh_argv`，无 ssh 环境下可单元测试）：`-T` 不分配 pty（输出干净）；
  `BatchMode=yes` 绝不交互式提示密码（没配免密就快速失败，不挂住等输入）；
  `-- sh -s` 让远程登录 shell（bash/zsh/**fish** 任意）只负责执行 `sh -s`，
  要跑的 **POSIX 脚本经 ssh 子进程 stdin 喂入**、由 POSIX sh 解析——因此远程
  默认 shell 是 fish 也能正确执行 `for...do...done` 等 POSIX 语法（真机 fish
  曾因登录 shell 直接解析 POSIX 脚本而失败，此为修复点）。`capture_remote`
  `.stdin(piped())` 写入脚本后 drop 发 EOF；stdout/stderr 分离读取。
- **路径映射**：`map_path`（纯函数）把本机绝对路径 `strip_prefix(local_root)`
  后 `join` 到 `remote_root`；不在本机 root 之下 → 拒绝。`local_root` 构造时
  canonicalize（macOS `/var` → `/private/var`）；`remote_root` 启动时经 ssh 解析
  （`MYAGENT_SSH_ROOT` 指定则 `cd '<root>' && pwd -P` 校验规范化，未指定则
  `pwd` 取远程 home）。工具层保证路径在本机 root 之内，映射失败正常不会发生。
- **文件内容 base64 over the wire**：`read_file` 远程 `base64 -- <file>`（GNU coreutils 写法，目标 Linux 可用；macOS 远程需 `base64 -i`）（内容
  走 stdout，stderr 分离不污染）；`write_file` 本地 base64 后 `printf '%s' '…'
  | base64 -d > <file>`（base64 字符集仅 `[A-Za-z0-9+/=]`，无 shell 元字符，
  可安全放进远程命令）。自实现 base64 编解码（`b64_encode` / `b64_decode` 纯函数，
  零依赖），解码容忍 GNU base64 的 76 列换行。
- **stdout/stderr 分离捕获**：模块内私有 `capture_remote`（与 `run_command` 同样
  的超时轮询模式）**分离**捕获 stdout/stderr——文件内容不能被 stderr 污染；
  exec 需要合并输出时由 `merge_output` 合并（对齐 `LocalRuntime` exec 语义）。
  不修改 `runtime.rs` 共享 `run_command` 的签名。
- **read_dir**：远程 POSIX sh `for f in * .[!.]* ..?*` 输出 tab 分隔的
  `<type>\t<name>`（含隐藏文件；D/L/F/O），`parse_dir_listing`（纯函数）解析。
- **exec**：`cd '<remote_root>' && exec <program> <args...>`，cwd 固定为远程根
  （忽略入参）；program/args 已被工具白名单校验为安全字符集（无 shell 元字符），
  无需引用；ssh 透传远程退出码。
- **构造探测**：`SshRuntime::new` 先经 `ssh ... -- sh -s`（stdin 喂 `true`）
  探测连通性 + 免密（任何失败 → 清晰 `io::Error`，提示检查 `MYAGENT_SSH_HOST` /
  `ssh-copy-id` / 网络防火墙），再解析远程根。`from_parts` 可注入自定义 ssh 二进制
  与 root 供测试（对齐 `sandbox.rs` 的 `from_parts` / `for_test` 模式）。gated
  假-ssh 测试断言 argv 以 `-- sh -s` 结尾且脚本来自 stdin，防回归到登录 shell 解析。
- **CLI**：`MYAGENT_RUNTIME=ssh` 启用；`MYAGENT_SSH_HOST` 必填（可含 `user@`），
  `MYAGENT_SSH_PORT` 默认 22（1..=65535，非法清晰报错），`MYAGENT_SSH_ROOT`
  默认远程 home。解析为纯函数 `ssh_host_from` / `parse_ssh_port`（`src/main.rs`，
  单测覆盖）。启动时 stderr 打印 `runtime: ssh (<host>)` 与
  `remote root: <path>` 便于确认。
- **测试**：纯函数单测（`ssh_argv` / `sh_quote` / `map_path` / `parse_dir_listing` /
  base64 往返 / `ssh_host_from` / `parse_ssh_port`）+ 一个 gated 集成测试
  `gated_fake_ssh_roundtrip`（`SSH_RUNTIME_TESTS=1` 时运行）：用临时目录里的假
  `ssh` 脚本模拟远程（backing dir 映射 root，支持 `true` / `pwd` / `mkdir` /
  base64 编解码 / `ls` / `exec`），端到端覆盖 write（含父目录自动创建）→
  大文件 base64 往返 → read → read_dir → exec 成功/失败 → NotFound 映射 →
  路径越界拒绝。

**与 §6 草案的差异**：草案方案 B 的 `Executor` 假设 backend 只有“命令执行”；
实际 `Runtime` trait 的四个原语（含文件读写/列目录）都被 SshRuntime 实现了——
远程 backend 的文件语义（路径映射 + base64 传输）落在 SshRuntime 内部，
`tool.rs` / `agent.rs` 零改动，与 `SandboxedRuntime` 装饰器、`Capabilities`
正交可组合。`MYAGENT_RUNTIME=nix + ssh` 互斥（单值选择）；`MYAGENT_RUNTIME=ssh`
+ `MYAGENT_SANDBOX=1` 的组合会把 `ssh` 命令本身放进 Seatbelt 沙箱（未验证、
无默认需求，文档注明）。
