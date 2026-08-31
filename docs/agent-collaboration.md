# Pi / Codex 协作规范

> 状态：**当前有效流程**
> 目的：让 Pi 与 Codex 在同一个 Rust Agent Runtime 项目中稳定协作，同时遵守 [`AGENTS.md`](../AGENTS.md) 的增量开发原则。

---

## 1. 核心原则

Pi 和 Codex 不是两个可以同时自由修改代码的自主开发者，而是两个角色不同的工程助手：

```text
一个 Agent 负责实现
另一个 Agent 负责审查 / 验证
用户负责方向决策、授权与合并
```

默认规则：

1. **一次只授权一个小功能**。
2. **一个任务只有一个 Agent 负责修改代码**。
3. **另一个 Agent 默认只读审查，不顺手改代码**。
4. **所有交接都通过仓库内文件、git diff、commit 和验证结果完成**，不依赖聊天记忆。
5. **每个功能完成后必须通过验证**：
   - `cargo fmt --check`
   - `cargo check`
   - `cargo test`
   - `cargo clippy`
6. **下一步做什么必须由用户明确批准**。

---

## 2. 推荐角色分工

### Pi：默认主开发者

Pi 适合负责：

- 根据用户明确授权实现小功能
- 编写或调整 Rust 代码
- 运行增量验证
- 更新 [`README.md`](../README.md) TODO
- 维护设计说明或实验记录
- 使用 `pi-autoresearch` 做优化类实验

Pi 不应该：

- 在未获批准时连续实现多个功能
- 自动推进 roadmap 中的 Runtime / Sandbox / Nix Runtime / MCP / subagents
- 把 autoresearch 实验中的新想法直接变成功能

### Codex：默认审查者 / 第二意见

Codex 适合负责：

- 只读审查 Pi 的实现
- 检查架构边界是否清晰
- 检查是否存在过早抽象
- 检查路径边界、`exec` 白名单、错误处理和测试覆盖
- 独立运行验证命令
- 对 Runtime、Nix、Sandbox、Capability 等设计方向提供第二意见
- 在用户明确授权时接手实现某个小修复或功能

Codex 默认不应该：

- 在审查任务中直接修改代码
- 扩大任务范围
- 引入未授权依赖
- 替用户决定 roadmap 顺序

---

## 3. 标准开发流程

```text
用户明确一个小任务
  ↓
指定 Owner：Pi 或 Codex
  ↓
Owner 阅读 AGENTS.md / README.md / 相关代码
  ↓
Owner 做最小实现
  ↓
Owner 运行验证
  ↓
Owner 更新 README.md TODO（如功能状态变化）
  ↓
Owner 写完成报告
  ↓
另一个 Agent 只读审查
  ↓
用户决定是否修复 / 合并 / 进入下一任务
```

### 任务授权模板

```text
我批准实现这个功能：<功能名>

Owner：<Pi 或 Codex>
范围：
- 允许修改：<文件或模块>
- 验收标准：<明确标准>

限制：
- 不添加新依赖
- 不实现未授权 roadmap 功能
- 不做大规模重构
- 完成后运行 cargo fmt --check / cargo check / cargo test / cargo clippy
- 更新 README TODO（如需要）
- 报告下一步建议，但等待我确认
```

---

## 4. 审查流程

当一个 Agent 完成实现后，另一个 Agent 应先以只读方式审查。

### 审查提示词模板

```text
请只读审查当前改动，不要修改代码。

重点检查：
1. 是否符合 AGENTS.md 的最小改动原则
2. 是否有过早抽象或不必要依赖
3. Model / Agent / Tool / Session 的职责边界是否清晰
4. 路径边界和 exec 白名单是否有安全问题
5. 错误处理是否明确
6. 测试是否覆盖关键行为
7. README.md 和 docs 是否与实际代码一致

请输出：
- Blocking issues：必须修复的问题
- Non-blocking issues：可以后续处理的问题
- 验证结果
- 建议由谁修复：当前 Owner / 另一个 Agent
```

### 审查结论分类

| 分类 | 含义 | 处理方式 |
| --- | --- | --- |
| Blocking | 会导致错误、安全问题、测试缺失或架构越界 | 必须修复后再进入下一步 |
| Non-blocking | 可改进但不影响当前任务成立 | 记录为后续候选，不自动扩大范围 |
| Design question | 方向不明确或涉及 roadmap 判断 | 交给用户决定 |
| Verified | 实现和验证都满足任务 | 可以由用户决定合并或进入下一任务 |

---

## 5. 分支与工作目录规则

不要让 Pi 和 Codex 在同一个工作目录中同时修改同一批文件。

推荐：

```text
main：保持可理解、可验证的状态
pi/<task>：Pi 负责的功能分支
codex/<task>：Codex 负责的修复或实验分支
review/<topic>：只读审查或文档整理分支
```

建议：

- 日常主开发由 Pi 在 `pi/<task>` 分支进行。
- Codex 审查时优先只读当前 diff。
- 如果 Codex 需要修改，使用单独 worktree 或 `codex/<task>` 分支。
- 两个 Agent 不要同时修改同一文件。
- 合并前由用户确认。

---

## 6. Handoff 文档

较大的任务建议在 `docs/handoffs/` 中保存交接文档。小任务不必强制创建。

文件名示例：

```text
docs/handoffs/2026-08-31-exec-timeout.md
```

模板：

```md
# Handoff: <任务名>

- Owner: <Pi / Codex>
- Status: <plan / in-progress / review / done>
- Date: <YYYY-MM-DD>

## Goal

<这次要解决的问题>

## In scope

- <允许修改的文件或模块>

## Out of scope

- <明确不做的事情>

## Implementation summary

<改了什么，为什么这样改>

## Verification

- [ ] cargo fmt --check
- [ ] cargo check
- [ ] cargo test
- [ ] cargo clippy

## Review notes

<审查者记录的问题；没有则写 None>

## Open questions

<需要用户决策的问题>

## Suggested next step

<建议的下一步；必须等待用户批准>
```

---

## 7. README 与文档同步

功能状态以 [`README.md`](../README.md) 为准。

每次功能完成后，Owner 需要检查：

- 新功能是否应在 README 的“当前状态”中标记为 `[x]`
- 未完成功能是否仍保持 `[ ]`
- 架构表是否需要更新
- 设计文档是否仍标注“草案 / 未实现”
- 是否把设计草案误写成已实现功能

原则：

```text
代码没有实现，就不能在 README 中标记完成。
文档可以描述未来方向，但必须明确标注“未实现”。
```

---

## 8. autoresearch 与普通开发的边界

`pi-autoresearch` 只用于优化类目标，例如：

- `cargo build` 编译时间
- `cargo check` 检查时间
- `cargo test` 测试时间
- release 二进制体积

它不用于：

- 新工具
- 新 Runtime 抽象
- Sandbox
- Nix Runtime
- MCP
- subagents
- 架构方向变更

autoresearch 的规则：

1. 实验文件放在 `.auto/`。
2. `.auto/prompt.md` 必须写清目标、metric、files in scope、off limits。
3. `.auto/checks.sh` 必须运行完整验证：

   ```bash
   #!/bin/bash
   set -euo pipefail
   cargo fmt --check
   cargo check
   cargo test
   cargo clippy
   ```

4. checks 失败的实验不能保留为有效优化。
5. 实验中发现的新功能想法写入 `.auto/ideas.md`，退出实验后再走正常审批流程。
6. Codex 可以审查实验日志、checks 和最终 diff，但不应在实验模式中擅自扩大范围。

---

## 9. 冲突处理

如果 Pi 和 Codex 的意见不一致：

1. 先以代码事实、测试结果和 [`AGENTS.md`](../AGENTS.md) 原则判断。
2. 涉及架构方向或 roadmap 顺序时，不自行决定，交给用户。
3. 能通过小实验或 benchmark 决定的问题，记录假设和验证方法。
4. 不为了“统一意见”而引入大抽象。
5. 任何 Agent 都不能把“我觉得下一步很明显”当成自动实现的授权。

---

## 10. 最小协作清单

每次任务开始前确认：

- [ ] 任务是否是一个小而明确的功能或修复？
- [ ] Owner 是谁？
- [ ] 允许修改哪些文件？
- [ ] 哪些内容明确不做？
- [ ] 验收标准是什么？

每次任务完成后确认：

- [ ] `cargo fmt --check` 通过
- [ ] `cargo check` 通过
- [ ] `cargo test` 通过
- [ ] `cargo clippy` 通过
- [ ] README TODO 与实际代码一致
- [ ] 没有引入未授权依赖或 roadmap 功能
- [ ] 已写出下一步建议，但没有自动实现
