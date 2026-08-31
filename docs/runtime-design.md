# Runtime 设计文档

> 状态：**设计草案，未实现**
> 关联：AGENTS.md（Principle 5：避免 premature abstraction）、README TODO（Nix Runtime / Sandbox / Capability system 未实现）
> 本文只做架构分析与 API 草案，**不修改任何 src 代码**。

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

以下全部**未实现**，且当前**无需求**，不要提前引入：

- **Runtime trait / Executor trait** —— 只有一个 backend，抽象是负债（§2）
- **RuntimeContext struct** —— 无可变超时/可控 env 需求（§5 条件 1 未触发）
- **Nix Runtime** —— Nix 当前只做 `nix develop`，无工具执行可复现需求（§3）
- **Sandbox** —— 模型可信 + 白名单已是最小权限，无防御需求（§3）
- **Capability system** —— 无权限分化场景（§5 条件 3 未触发）
- **Container** —— 无远程/隔离执行需求

**判据**：上述任何一项，只有当 §5 的对应条件出现时才值得实现。在此之前，`tool.rs` 的 5 个 `*_within` + `Agent.root` + `EXEC_TIMEOUT` 常量就是最简正确形态。
