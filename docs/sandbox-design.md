# Sandbox 设计文档

> 状态：**已实现**（2026-09-01，`src/sandbox.rs`，`MYAGENT_SANDBOX=1/true` 启用）
> 关联：AGENTS.md（roadmap Sandbox）、[runtime-design.md](runtime-design.md)（§10 已落地形态）
> 本文记录威胁模型、方案设计与验收清单；实现细节见 `src/sandbox.rs` 与 §9。

---

## 1. 威胁模型：我们到底在隔离什么

当前安全边界有两层，都在 Agent 进程**内部**：

1. **Capabilities 门**（`src/capabilities.rs`）：模型能不能调用某类工具（读 / 写 / exec）；
2. **路径边界**（`resolve_within`）：文件工具的路径不能逃出项目根。

但 `exec` 工具允许运行 `cargo check/build/test/clippy`，而 cargo 会衍生出**不在我们控制下的代码**：

- `build.rs`（构建脚本）——任意 Rust 代码，编译期执行；
- proc-macro（过程宏）——编译期执行；
- `cargo test` 运行的测试二进制——任意代码。

白名单只校验了"cargo + 合法子命令 + 安全参数字符"，**管不到这些衍生进程做什么**。一个恶意（或被 prompt injection 诱导引入）的依赖，其 `build.rs` 理论上可以：

- 写项目目录之外的文件（`~/.zshrc`、SSH key 等）；
- 联网外泄数据（环境变量里有 `OPENAI_API_KEY`）。

**Sandbox 的目标**：在 OS 层强制约束 exec 衍生进程的能力，即使 build.rs 是恶意的也无法越界。

**不防御**（超出本设计范围）：

- 模型自身的恶意指令（Capabilities 门 + 白名单已覆盖"模型能调什么"）；
- 内核漏洞、root 级攻击；
- 对读取路径的限制（见 §6，第一版不限制读）。

## 2. 现状数据流

```text
Agent → Tool::execute(rt, root)
           └─ exec_within → rt.exec(program, args, cwd=root)
                               ├─ LocalRuntime:  std::process::Command 直调
                               └─ NixRuntime:    nix develop --command <program> <args>
```

Capabilities 回答"**能不能**执行 exec"；Sandbox 回答"执行后**能做什么**"。两者正交。

## 3. 方案选择

| 方案 | 说明 | 结论 |
| --- | --- | --- |
| **A. `SandboxedRuntime` 装饰器** | 包装任意 `Runtime`：文件操作委托内层（已有 `resolve_within` 边界）；`exec` 用 `/usr/bin/sandbox-exec` 包裹命令 | **第一版采用** |
| B. RuntimeContext 加 policy 字段 | 策略只是数据，没有 OS 强制执行点 | 不解决问题 |
| C. 容器 / VM（docker、lima） | 隔离强但重，偏离"keep the core small"；macOS 上无原生容器 | 不做 |
| D. Linux 隔离（bubblewrap / landlock） | flake 声明支持 linux，但当前开发与使用均在 macOS | 留接口，不实现 |

### 为什么是装饰器

Runtime trait 已支持 `Box<dyn Runtime>` 注入，装饰器是自然形态：

```text
LocalRuntime                     → 现状
NixRuntime(LocalRuntime 委托文件) → 现状
SandboxedRuntime(LocalRuntime)   → 本机 cargo 命令进 Seatbelt
SandboxedRuntime(NixRuntime)     → nix develop 进 Seatbelt（sandbox-exec 在最外层）
```

exec argv 变换（纯函数，可单测）：

```text
["cargo", "check"]
→ ["/usr/bin/sandbox-exec", "-p", "<policy>", "cargo", "check"]
（NixRuntime 内层时：sandbox-exec -p <policy> nix develop --command cargo check）
```

构造时探测 `/usr/bin/sandbox-exec` 存在（对齐 `NixRuntime::new` 的 `nix --version` 探测模式）：
不存在或非 macOS → 返回清晰 io::Error，提示该平台不支持，**绝不静默降级为不隔离**。

## 4. Seatbelt 策略（第一版最小集）

`sandbox-exec` 使用 SBPL 策略语言。**关键语义：SBPL 规则"后匹配者生效"**——
因此必须先收紧（deny）再精确放开（allow），只写 `(allow default)` 加
`(allow file-write* ...)` 不能限制任何写入。

### 4.1 策略（network 默认关闭）

```scheme
(version 1)
(allow default)
;; 网络：默认全禁（MYAGENT_SANDBOX_NETWORK=1 时去掉本行）
(deny network*)
;; 写入：默认全禁，仅放开项目根与临时目录
(deny file-write*)
(allow file-write* (subpath "<ROOT>"))
(allow file-write* (subpath "<TMPDIR>"))
```

- `<ROOT>` = Agent root 路径（canonicalize 后注入，注意 SBPL 字符串转义）；
- `<TMPDIR>` = 进程 `TMPDIR`（rustc 落临时产物），缺失回退 `/tmp`；
- `file-write*` 通配覆盖 `file-write-create / -data / -unlink / -rename /
  -mode / -owner / -fsflags / -mount` 等全部写类操作；
- 读取**不限制**（`allow default` 保留）：构建需读 nix store、`~/.cargo`、
  SDK；密钥外泄必须经网络或越界写，两条路均已收紧。

### 4.2 必须显式验证的文件写语义（反馈①）

路径前缀匹配（subpath）与 symlink / hardlink 的解析关系是 SBPL 里最容易
出错的点，**不能假设**，以下 6 种攻击必须有真实测试（§7.2），全部拒绝才算
策略通过；任一被绕过则策略需要增加显式 deny（如禁 symlink 创建）：

| # | 攻击手法 | 操作 | 期望结果 |
| --- | --- | --- | --- |
| 1 | 绝对路径直写 | 写 `$HOME/.sandbox-victim` | 拒绝，victim 不存在 |
| 2 | symlink 逃逸 | 在 ROOT 内建指向外部的 symlink，再经它写 | **写被拒**（解析后路径在 ROOT 外） |
| 3 | rename 跨界 | `rename(ROOT/x, ROOT外路径)` | 拒绝 |
| 4 | unlink 外部 | 删除 ROOT 外文件 | 拒绝 |
| 5 | hardlink 映射 | 把外部文件 `link()` 进 ROOT 再写 | 外部原文件内容不变，写被拒 |
| 6 | 目录逃逸写 | `write(ROOT/sub/../../victim)` | 拒绝（工具层 resolve_within 已先拦一道） |

> symlink 是重点：`symlink()` 本身属于 file-write-create（在 ROOT 内会被允许），
> 真正要验证的是**沿 symlink 写入时 Seatbelt 是否按解析后路径判定**。
> 若实测发现按 symlink 路径前缀放行（历史上 Seatbelt 出现过此类问题），
> 策略追加 `(deny file-write-create (literal ...))` 禁 symlink 创建，
> 或改用更严格的 vnode 级约束——以测试结果为准。

## 5. 配置与组合

新增环境变量，与现有两个开关正交：

| 变量 | 取值 | 效果 |
| --- | --- | --- |
| `MYAGENT_SANDBOX` | `1` / `true` | exec 经 sandbox-exec 运行（默认关） |
| `MYAGENT_SANDBOX_NETWORK` | `1` / `true` | 沙箱内**放开网络**（默认关） |

### 5.1 网络边界（反馈③）

- **网络默认关闭，放开是用户侧显式 opt-in**，不随 `MYAGENT_SANDBOX=1`
  隐式开启；
- 启动横幅必须显示实际状态，例如 `sandbox: on (network: off)` /
  `sandbox: on (network: ON — cargo commands can access the network)`；
- 使用场景：首次构建需下载 crates.io 依赖时临时开
  `MYAGENT_SANDBOX_NETWORK=1`，拉完依赖关掉；warm cache 下保持关闭。

### 5.2 环境变量边界（反馈③）

- 沙箱内进程**继承完整环境**（包括 `OPENAI_API_KEY`）——第一版**不做**
  env allowlist / scrubbing；
- 补偿控制是网络默认关闭：key 可读但没有外传通道，写入又被限制在
  ROOT/TMPDIR（无法写到 shell 配置等位置）；
- 这是已知局限，写入设计文档与 README：未来 v2 可改为最小环境
  （clear env + 仅 allowlist 构建所需变量）。

组合示例：

```bash
# 推荐（已端到端验证）：在 nix develop shell 内启动 agent + 沙箱——
# 工具链是 nix 的，cargo / build.rs 同被 Seatbelt 禁锢
nix develop
MYAGENT_SANDBOX=1 cargo run
MYAGENT_SANDBOX=1 MYAGENT_SANDBOX_NETWORK=1 cargo run  # 临时放开网络拉依赖
```

> **已验证局限（2026-09-01）**：`MYAGENT_RUNTIME=nix` 与 `MYAGENT_SANDBOX=1`
> 的组合（sandbox-exec 包 `nix develop`）当前**不可用**：nix 评估 flake 时需在
> `$HOME/.cache/nix/fetcher-locks/` 建锁文件（随后建 profile 还需写
> `$HOME/.local/state/nix/profiles/`），均在 ROOT/TMPDIR 之外被策略拒绝
> （`Operation not permitted`）。**不为此放宽策略**：profiles 目录是 GC roots /
> profile 符号链接，对 $HOME 放行写会破坏沙箱意义。上面"shell 内 + sandbox"的
> 用法提供等效保证（工具链来自 nix、衍生进程被禁锢）。未来路径：把
> `XDG_CACHE_HOME` / `XDG_STATE_HOME` 重定向进 ROOT/TMPDIR（已验证 nix 尊重
> 这两个变量），需要 env 注入能力，另立任务。

## 6. 明确不做（第一版边界）

- **读路径限制**：不枚举可读路径（见 §4.1）；
- **env allowlist / 环境变量清洗**：第一版继承完整环境，靠"网络默认关 +
  写入受限"补偿（见 §5.2）；
- **Linux 支持**：bubblewrap / landlock 是未来第二个平台后端，接口留可能
  但不实现；
- **资源限额**：CPU / 内存 / 执行时长（60s 超时已在 exec 层）；
- **沙箱内网络主机级白名单**（只允许 crates.io）：SBPL 做不到，需要代理层；
- 替换 deprecated 的 sandbox-exec：Apple 已标记 deprecated 但仍随 macOS
  发布且功能完整；若未来移除，替代路径是 Seatbelt API（`sandbox_init`）
  或进程外策略执行器。

## 7. 测试策略

1. **纯函数单测**（任何平台可跑）：argv 包裹构造、策略字符串含 ROOT/TMPDIR
   且含 `(deny file-write*)` 与（network off 时的）`(deny network*)`、
   网络开关分支；
2. **Seatbelt 攻击矩阵测试**（探测到 `/usr/bin/sandbox-exec` 才运行，
   否则 early return；对齐 nix 冒烟测试模式）：直接用 sandbox-exec 跑
   `/bin/sh` 脚本执行 §4.2 的 6 种攻击 + 一次外网连接，逐条断言被拒；
3. **恶意 build.rs 集成测试（反馈②）**——不只验证 cargo build 成功，
   而是真正模拟攻击：
   - 在临时目录生成一个 cargo 项目，其 `build.rs` 尝试：
     (a) 写 `$HOME` 下文件、(b) 建 symlink 逃逸后写、(c) rename/unlink
     到项目外、(d) 连接网络（`std::net::TcpStream::connect("127.0.0.1:1")`
     或实际外联探测）、(e) 读取 `OPENAI_API_KEY` 并尝试 (d) 外传；
   - 通过 `SandboxedRuntime`（exec `"cargo" ["build"]`）运行；
   - 断言：项目外的攻击产物**全部不存在/未被修改**、网络连接失败
     （构建可能因 build.rs 被拒而失败——这是正确结果）；
   - **对照组**：同样脚手架但 build.rs 无害（只写 OUT_DIR）时
     `cargo build` 必须成功——证明策略没有过度收紧；
4. **真实构建验证**：沙箱内对本项目跑 `cargo check/build/test`，确认
   TMPDIR/ROOT 写权限足够（策略过严的主要风险点由此暴露；若 cargo 需要
   额外交互路径，在策略中显式加注释放开，绝不恢复宽泛写入）。

> 注意：在 Codex / 已有 Seatbelt 嵌套的环境里 `sandbox-exec` 会报
> `sandbox_apply: Operation not permitted`；第 2/3/4 项测试需在普通
> 用户终端运行（CI 为 macOS 时同样适用）。

## 8. 验收标准（2026-09-01 全部通过）

- [x] §4.2 的 6 种写攻击（含 symlink/rename/unlink/hardlink）全部被拒
  （`attack_matrix_blocks_all_write_escapes`，真实 sandbox-exec）；
- [x] 恶意 build.rs 的越界写、外联全部失败，攻击产物不存在
  （`malicious_build_rs_is_confined_and_harmless_control_succeeds`）；
- [x] 无害 build.rs 对照组在沙箱内 `cargo build` 成功（同一测试）；
- [x] 默认策略衍生进程无法联网；`MYAGENT_SANDBOX_NETWORK=1` opt-in 后可以
  （`network_denied_by_default_and_opt_in_connects`）；
- [x] 横幅正确显示 sandbox 与 network 状态（`sandbox: on (network: off/ON)`，
  真实 CLI 冒烟验证）；
- [x] 沙箱内对本项目 `cargo check/build/test` 正常通过（策略不误伤：CLI 冒烟
  `cargo check` exit 0 + 集成测试内 `cargo build`）；
- [x] 非 macOS / 无 sandbox-exec 时构造失败并给清晰错误，不静默降级
  （`SandboxedRuntime::new` 的 `cfg` + 存在性检查返回 `io::Error`；gated 测试
  在沙箱不可用时跳过而非静默降级）；
- [x] 文件工具行为不受影响（装饰器委托内层）；全部既有测试通过（133 passed）。

## 9. 实现形态（2026-09-01 已落地）

```text
src/sandbox.rs        SandboxedRuntime { inner: Box<dyn Runtime> }
                      exec: sandbox_argv(program, args, root, network) → run_command(...)
                      文件操作：self.inner.read_file/write_file/read_dir
src/main.rs           MYAGENT_SANDBOX / MYAGENT_SANDBOX_NETWORK 解析，
                      按标志包装 runtime（在 runtime 选择之后）
```

与草案一致，无新依赖；测试见 §7（9 单测 + 3 个 gated 集成测试，运行时自动探测
sandbox-exec 真实可用才执行，无环境变量开关；嵌套沙箱环境自动跳过）。
