# Agent 能力评估设计文档

> 状态：**设计草案，未实现**
> 目的：设计第一个真实 coding task benchmark，用结果驱动下一步决策（不预设 Runtime / Nix / Planner / Subagent）
> 约束：不修改 src / Cargo.toml / flake.nix，不实现任何功能

---

## 1. 当前 Agent 能力模型

### 支持的能力

| 能力 | 入口 | 说明 |
| --- | --- | --- |
| **search** | `search { query, path? }` | 项目内内容搜索，返回 文件路径:行号:内容 |
| **read_file** | `read_file { path }` | 读取项目内文件（UTF-8） |
| **edit_file** | `edit_file { path, old, new }` | 精确文本替换（old 必须恰好出现一次，多匹配拒绝） |
| **write_file** | `write_file { path, content }` | 写入/覆盖项目内文件 |
| **exec** | `exec { command }` | 白名单命令：`cargo check/build/test/clippy/fmt --check`，60s 超时，继承 env |
| **session persistence** | `Session::load/save` | conversation 保存/恢复（单 session） |

### Agent Loop 事实

- `MAX_TOOL_ROUNDS = 8`：单次用户输入最多 8 轮 model ↔ tool 交互
- `run_turn(conversation, user_text)`：push user → 循环 model.complete → Text 结束 / ToolCall 执行工具 / Model error 回滚
- 工具错误回传 Model（`tool error: {err}`），不中止循环，由模型决定如何继续
- 无规划层：Agent 没有独立的 plan-then-execute 阶段，完全依赖模型每一步的即时决策

### 当前限制

1. **无规划能力**：Agent 无法先制定计划再执行，每一步都是模型即时输出。复杂任务（多文件修改）依赖模型在单次 tool round 内组织。
2. **exec 白名单窄**：只能跑 4 个 cargo 子命令，无法执行 `git`、`cargo add`、`cargo expand` 等。对"真实修复任务"可能不够（如需要看 git diff / 添加依赖）。
3. **8 轮上限**：对"定位→修改→检查→修复"的多轮任务，8 轮可能紧张（每轮一个工具调用，8 轮 = 约 8 个工具动作）。
4. **无上下文压缩**：conversation 无限增长，长任务后模型看到的 messages 越来越多，token 成本上升，且早期内容可能被截断（取决于 provider）。
5. **search 返回原始匹配**：无聚合、无 rank、无文件级摘要，模型可能需要多次 search 才能定位。
6. **单 session 单进程**：无并行、无子任务分解。

---

## 2. 设计第一个真实 coding task benchmark

### 目标

验证 Agent 是否能完成完整闭环：

```text
理解问题
 ↓
定位代码
 ↓
修改文件
 ↓
运行检查
 ↓
根据错误修复
 ↓
完成任务
```

### 设计原则

- **不需要新代码 / 新 crate / CI**：benchmark 只是任务描述格式 + 验证脚本思路，不落库
- **最小 Rust fixture**：一个单 crate Rust 项目，故意注入 1–2 个真实 bug
- **任务可自动判定**：success/failure 由 `cargo test` 或 `cargo check` 判定，不需要人工看代码

### Fixture 设计（思路，不实现）

一个最小的 Rust 项目（`Cargo.toml` + `src/lib.rs` + `src/main.rs` 或 tests），含：

- 一个简单的函数（如 `add(a, b)`、字符串处理、结构体方法）
- 故意注入的 bug（如逻辑错误 `a - b` 应为 `a + b`、off-by-one、错误的条件分支）
- 一个能捕获 bug 的测试（`#[test]` 断言正确行为）

**任务描述**（给 Agent 的 prompt）：

```text
项目 src/lib.rs 中有一个 bug：函数 `foo` 在输入 X 时返回错误结果。
运行 cargo test 确认失败，定位 bug，修复它，确保 cargo test 全部通过。
```

### 为什么这个设计有效

- **bug 是确定性的**：`cargo test` 失败 → 修复 → 通过，自动判定
- **需要完整闭环**：理解（读代码）→ 定位（search/read）→ 修改（edit_file）→ 检查（exec cargo test）→ 根据错误修复（迭代）
- **不依赖真实模型质量**：任何能跑通 5 步闭环的 Agent 都算成功

---

## 3. Task 格式

设计 YAML 任务描述（benchmark 本身不实现，只定义格式）：

```yaml
task:
  id: "fix-add-function"            # 任务唯一 ID
  description: |
    项目 src/lib.rs 中的 add 函数有一个逻辑 bug：
    当输入 (2, 3) 时返回 0 而非 5。
    请运行 cargo test 确认失败，定位并修复 bug，
    确保 cargo test 全部通过。
  repository:
    fixture: "fix-add"              # 指向 fixture 目录（不实现在此文档）
    initial_state:
      - "src/lib.rs 含 buggy add 函数"
      - "tests/ 含验证 add 行为的测试"
  expected_changes:                  # 预期 Agent 会做的修改
    - path: "src/lib.rs"
      kind: "edit"                   # edit_file / write_file
      description: "修正 add 函数的逻辑错误"
  validation:
    command: "cargo test"            # 通过标准
    expected: "test result: ok"      # 输出必须包含
    min_rounds: 1
    max_rounds: 8                    # 不超过 MAX_TOOL_ROUNDS
```

### 字段说明

| 字段 | 含义 |
| --- | --- |
| `id` | 任务唯一标识 |
| `description` | 给 Agent 的自然语言任务描述 |
| `repository.fixture` | fixture 目录名（预置 bug 的项目） |
| `repository.initial_state` | 初始状态说明（人读） |
| `expected_changes` | 期望的代码修改（人读，不用于严格匹配——Agent 可能有多种正确修法） |
| `validation.command` | 判定命令 |
| `validation.expected` | 命令输出必须包含的字符串 |
| `validation.max_rounds` | 允许的最大 tool round（默认 8） |

### 为什么 validation 只用命令输出判定

- 不要求 Agent 的 diff 与"预期修改"完全一致（多种正确实现）
- 只看最终行为：`cargo test` 通过 = 成功
- 避免人工检查代码

---

## 4. Evaluation Metrics

### 结果指标

| 指标 | 定义 | 记录方式 |
| --- | --- | --- |
| **success/failure** | `cargo test` 是否通过 | 自动（命令输出） |
| **tool call sequence** | 完整的工具调用序列（时间顺序） | Agent 会话内记录 |
| **number of edits** | `edit_file` / `write_file` 调用次数 | 从 tool call sequence 统计 |
| **cargo check iterations** | `exec cargo check/test` 调用次数 | 从 tool call sequence 统计 |
| **token usage** | 模型 token 消耗 | provider 返回的 usage（如可得） |
| **time** | 任务总耗时 | 计时 |
| **human intervention** | 人工介入次数 | 人工记录（理想为 0） |

### 分析维度

| 维度 | 从哪些指标得出 |
| --- | --- |
| 效率 | edits 次数、cargo check 次数、time |
| 路径 | tool call sequence 的形态（先 search 还是先 read？有没有无效探索） |
| 鲁棒性 | 是否出现 tool error 回传后模型能否自愈（`tool error:` 后续是否成功） |
| 成本 | token usage |

### 判定规则

- **success**：validation.command 通过，且 tool rounds ≤ max_rounds
- **failure**：validation.command 失败，或 rounds 超限，或 Agent 放弃（无输出 / 报错终止）
- **partial**：（可选）rounds 超限但接近完成 / 人为判定部分完成

---

## 5. 当前 Agent 可能暴露的问题

> 这是 benchmark 预期会发现的问题，不是已确认的问题。

### 5.1 context 不足

- **症状**：Agent 忘记早期读到的代码内容，重复读同一文件；修改后不记得自己改了什么
- **根因**：conversation 无限增长无压缩；模型 context window 有限；早期 tool result 可能被 provider 截断
- **指标**：tool call sequence 中出现重复 read_file / 重复 search 同一内容；token usage 异常高

### 5.2 search 不足

- **症状**：Agent 无法定位 bug 所在文件；search 返回太多噪声；需要多次 search 才能缩小范围
- **根因**：search 只返回原始匹配，无文件级聚合 / rank；query 表达受限（无正则 / glob）
- **指标**：search 调用次数多但 read_file 命中率低；Agent 在 search 结果里反复翻找

### 5.3 planning 不足

- **症状**：Agent 直接改代码而不先读上下文；修改与任务目标偏离；多次无效 edit
- **根因**：无 plan-then-execute 阶段；8 轮上限下模型倾向"先动手"
- **指标**：首次 edit 前没有 read/search；edit 后立即被后续 edit 撤销（无效编辑）

### 5.4 execution 不足

- **症状**：cargo test 失败后 Agent 无法定位失败原因；重复跑同一命令；无法从 stderr 提取有效信息
- **根因**：exec 返回原始 stdout/stderr，无结构化错误；白名单无 `cargo test -- --nocapture` 等调试手段
- **指标**：重复 exec 同一命令；失败后 tool error 回传但模型未改进

### 5.5 environment 不一致

- **症状**：Agent 修复后本地通过，但 fixture 环境不同（依赖缺失、toolchain 差异）
- **根因**：exec 继承当前进程 env；fixture 可能依赖特定工具链 / 网络（cargo 拉依赖）
- **指标**：validation 环境与 Agent 执行环境分离时结果不同

### 5.6 （可能）tool round 耗尽

- **症状**：Agent 处于"接近完成"状态但 rounds 用尽，`TooManyToolRounds`
- **根因**：8 轮上限对"多文件修复 + 多轮 test 迭代"偏紧
- **指标**：AgentError::TooManyToolRounds 出现

---

## 6. 根据结果决定下一阶段

> **核心原则：让 benchmark 结果决定下一步，不预设方向。**

### 结果 → 决策映射

| Benchmark 结果 | 最可能的下一步方向 | 说明 |
| --- | --- | --- |
| 频繁 `TooManyToolRounds` | 提高/配置 MAX_TOOL_ROUNDS，或引入工具批量 | 执行层问题，先调参 |
| 重复 search / read 但定位失败 | 增强 search（聚合/rank/正则）或加 read_file 范围 | 工具能力缺口 |
| 多次无效 edit / 偏离目标 | 引入显式 plan 阶段（Planner） | 规划缺口，但**先验证再决定** |
| context 不足（重复读文件） | conversation 压缩 / 摘要 / 截断策略 | 上下文管理缺口 |
| 全部通过 | 扩展 benchmark 到更复杂任务 | 能力足够，往难处走 |
| 混合失败 | 逐项修复，按失败模式排序 | 无单一方向 |

### 明确不预设

- **不预设 Runtime**：除非 benchmark 暴露 environment 不一致（§5.5），否则 Runtime 不是优先项
- **不预设 Nix**：除非需要可复现执行环境，否则维持现状
- **不预设 Planner**：除非 planning 缺口（§5.3）被证实
- **不预设 Subagent**：除非任务规模需要并行分解

### 决策流程（未来执行时）

```text
跑 benchmark
 ↓
收集 metrics
 ↓
按失败模式分类（§5 症状）
 ↓
对照 §6 映射表
 ↓
选最小改动方向
 ↓
等待用户批准
```

---

## 附：本文档不包含的内容（明确不做）

- 不实现 fixture 项目（benchmark 只是格式设计）
- 不实现 benchmark runner（无 CI 需求）
- 不修改 src / Cargo.toml / flake.nix
- 不新增任何依赖
