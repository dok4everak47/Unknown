use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::runtime::{EntryKind, ExecError, ExecOutput, Runtime};

#[cfg(test)]
use crate::runtime::LocalRuntime;
#[cfg(test)]
use std::fs;

/// 一个工具的参数 JSON Schema，用于请求中的 `tools` 字段。
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

/// 当前可用的全部工具定义。
///
/// 后续新增工具时，在这里追加，并扩展 [`Tool`] 枚举与 [`Tool::from_call`]。
pub fn all_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file",
            description: "Read a file from the current project. The path must be inside the project directory.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the file to read, relative to the project root."
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "write_file",
            description: "Write a file to the current project. Creates missing parent directories automatically (e.g. src/main.rs). The path must be inside the project directory.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the file to write, relative to the project root."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file."
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "search",
            description: "Search file contents within the project. Returns matching file paths, line numbers and line contents.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Substring to search for in file contents."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional file or directory path to limit the search, relative to the project root."
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "list_dir",
            description: "List the entries of a directory inside the project (non-recursive, one level only). Returns each entry name with its kind (file/directory/symlink), including hidden dotfiles. Use this to discover what files exist before reading or searching; call it again on a subdirectory to go deeper.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path relative to the project root. Omit or leave empty to list the project root."
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "edit_file",
            description: "Replace an exact piece of text in a file. The path must be inside the project directory.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the file to edit, relative to the project root."
                    },
                    "old": {
                        "type": "string",
                        "description": "Exact text to find. Must occur exactly once in the file."
                    },
                    "new": {
                        "type": "string",
                        "description": "Replacement text for the matched `old`."
                    }
                },
                "required": ["path", "old", "new"]
            }),
        },
        ToolDefinition {
            name: "exec",
            description: "Run an allowed project development command (cargo check/build/test/clippy/fmt --check; read-only git status/diff/log/show; plus anything explicitly allowed via KARAKURI_EXEC_ALLOW). The command runs in the project directory and inherits the current environment.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Allowed project development command, e.g. \"cargo check\" or \"cargo test\"."
                    }
                },
                "required": ["command"]
            }),
        },
    ]
}

/// 工具执行或解析时的错误。
#[derive(Debug)]
pub enum ToolError {
    UnknownTool(String),
    InvalidArguments(String),
    NotFound(String),
    OutsideProject(String),
    /// `edit_file` 的 `old` 文本在文件中不存在。
    OldTextNotFound(String),
    /// `edit_file` 的 `old` 文本在文件中出现多次，无法确定替换目标。
    MultipleMatches(String),
    /// `exec` 的命令不在允许列表内。
    CommandNotAllowed(String),
    /// `exec` 的命令执行超时（仅 stdout/stderr 被截断的部分）。
    TimedOut(String),
    /// `exec` 的命令以非零退出码结束，stdout/stderr 原样带回。
    NonZeroExit(i32, String),
    Io(io::Error),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            ToolError::InvalidArguments(msg) => write!(f, "invalid arguments: {msg}"),
            ToolError::NotFound(path) => write!(f, "file not found: {path}"),
            ToolError::OutsideProject(path) => {
                write!(f, "access denied: {path} is outside the project directory")
            }
            ToolError::OldTextNotFound(path) => write!(f, "old text not found in {path}"),
            ToolError::MultipleMatches(path) => write!(
                f,
                "old text occurs multiple times in {path}; refusing to edit (exact replacement requires a single match)"
            ),
            ToolError::CommandNotAllowed(command) => {
                write!(f, "command not allowed: {command}")
            }
            ToolError::TimedOut(output) => write!(f, "command timed out\n\n{output}"),
            ToolError::NonZeroExit(code, output) => {
                write!(f, "command exited with code {code}\n\n{output}")
            }
            ToolError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ToolError::Io(err) => Some(err),
            _ => None,
        }
    }
}

/// 可执行的最小工具集合。
#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    ReadFile(ReadFile),
    WriteFile(WriteFile),
    EditFile(EditFile),
    Search(Search),
    ListDir(ListDir),
    Exec(Exec),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFile {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListDir {
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFile {
    pub path: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub command: String,
}

impl Tool {
    /// 根据模型返回的 ToolCall 解析出对应的工具。
    pub fn from_call(name: &str, arguments: &serde_json::Value) -> Result<Tool, ToolError> {
        match name {
            "read_file" => {
                let path = arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "read_file requires a string argument `path`".to_string(),
                        )
                    })?;
                Ok(Tool::ReadFile(ReadFile {
                    path: path.to_string(),
                }))
            }
            "write_file" => {
                let path = arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "write_file requires a string argument `path`".to_string(),
                        )
                    })?;
                let content = arguments
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "write_file requires a string argument `content`".to_string(),
                        )
                    })?;
                Ok(Tool::WriteFile(WriteFile {
                    path: path.to_string(),
                    content: content.to_string(),
                }))
            }
            "edit_file" => {
                let path = arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "edit_file requires a string argument `path`".to_string(),
                        )
                    })?;
                let old = arguments
                    .get("old")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "edit_file requires a string argument `old`".to_string(),
                        )
                    })?;
                let new = arguments
                    .get("new")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "edit_file requires a string argument `new`".to_string(),
                        )
                    })?;
                Ok(Tool::EditFile(EditFile {
                    path: path.to_string(),
                    old: old.to_string(),
                    new: new.to_string(),
                }))
            }
            "exec" => {
                let command = arguments
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "exec requires a non-empty string argument `command`".to_string(),
                        )
                    })?;
                Ok(Tool::Exec(Exec {
                    command: command.to_string(),
                }))
            }
            "search" => {
                let query = arguments
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .filter(|q| !q.is_empty())
                    .ok_or_else(|| {
                        ToolError::InvalidArguments(
                            "search requires a non-empty string argument `query`".to_string(),
                        )
                    })?;
                let path = arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(|s| s.to_string());
                Ok(Tool::Search(Search {
                    query: query.to_string(),
                    path,
                }))
            }
            "list_dir" => {
                // path 缺失 / null / 非字符串 / 空串 / 全空白 → None（列项目根）；
                // 否则 trim 后作为子目录路径。
                let path = arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| p.to_string());
                Ok(Tool::ListDir(ListDir { path }))
            }
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }

    /// 在指定根目录下执行（便于测试与 Agent 注入 root）。
    ///
    /// 所有副作用都经由 `rt` 完成；`root` 是路径边界。工具自身不再直接
    /// 触碰文件系统 / 进程（那些操作全部位于 [`Runtime`] 的实现中）。
    /// 兼容测试入口：等价于 [`Tool::execute_with_policy`] 传默认策略（空扩展白名单）。
    ///
    /// 仅供现有测试（~30 处 `tool.execute(&rt, &root)`）调用；二进制入口走
    /// [`Tool::execute_with_policy`]。
    #[allow(dead_code)]
    pub(crate) fn execute(&self, rt: &dyn Runtime, root: &Path) -> Result<String, ToolError> {
        self.execute_with_policy(rt, root, &ExecPolicy::default())
    }

    /// 带 exec 白名单策略的执行：与 [`Tool::execute`] 语义一致，仅 `Tool::Exec`
    /// 臂把 `policy` 传给 exec（扩展白名单），其余臂忽略 policy。
    pub(crate) fn execute_with_policy(
        &self,
        rt: &dyn Runtime,
        root: &Path,
        policy: &ExecPolicy,
    ) -> Result<String, ToolError> {
        match self {
            Tool::ReadFile(read_file) => read_file_within(rt, root, &read_file.path),
            Tool::WriteFile(write_file) => {
                write_file_within(rt, root, &write_file.path, &write_file.content)
            }
            Tool::Search(search) => search_within(rt, root, &search.query, search.path.as_deref()),
            Tool::ListDir(list_dir) => list_dir_within(rt, root, list_dir.path.as_deref()),
            Tool::EditFile(edit_file) => {
                edit_file_within(rt, root, &edit_file.path, &edit_file.old, &edit_file.new)
            }
            Tool::Exec(exec) => exec_within_policy(rt, root, &exec.command, policy),
        }
    }
}

/// 读取文件，但限制路径必须位于 `root` 之内。
///
/// 拒绝绝对路径、`..` 跳转，以及通过 symlink 逃逸出 `root` 的路径。
fn read_file_within(rt: &dyn Runtime, root: &Path, path: &str) -> Result<String, ToolError> {
    let resolved = resolve_within(root, path, false)?;
    rt.read_file(&resolved)
        .map_err(ToolError::Io)
}

/// 列目录条目（非递归，一层），但限制路径必须位于 `root` 之内。
///
/// 与 `read_file_within` 共用同一套路径边界校验；`path` 为 None / 空时列项目根。
///
/// 输出格式（逐行、稳定）：目录 → `<name>/ (directory)`；普通文件 →
/// `<name> (file)`；符号链接 → `<name> (symlink)`；`EntryKind::Other` 跳过
/// （与 search 遍历的忽略语义一致）。条目名取 `RuntimeEntry.path` 的
/// `file_name()`（该字段是完整路径）。含隐藏文件（dotfile），不过滤；
/// 按条目名排序保证输出确定性（`fs::read_dir` 与远程脚本的返回顺序都不稳定）。
fn list_dir_within(rt: &dyn Runtime, root: &Path, path: Option<&str>) -> Result<String, ToolError> {
    let resolved = resolve_within(root, path.unwrap_or("."), false)?;
    let mut entries = rt
        .read_dir(&resolved)
        .map_err(ToolError::Io)?;

    entries.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));

    let mut lines: Vec<String> = Vec::new();
    for entry in entries {
        let Some(name) = entry.path.file_name() else {
            continue;
        };
        let name = name.to_string_lossy();
        match entry.kind {
            EntryKind::Dir => lines.push(format!("{name}/ (directory)")),
            EntryKind::File => lines.push(format!("{name} (file)")),
            EntryKind::Symlink => lines.push(format!("{name} (symlink)")),
            // EntryKind::Other：遍历方忽略
            EntryKind::Other => {}
        }
    }

    if lines.is_empty() {
        Ok("Directory is empty.".to_string())
    } else {
        Ok(lines.join("\n"))
    }
}

/// 写入文件，但限制路径必须位于 `root` 之内。
///
/// 与 `read_file_within` 共用同一套路径边界校验。
fn write_file_within(
    rt: &dyn Runtime,
    root: &Path,
    path: &str,
    content: &str,
) -> Result<String, ToolError> {
    let resolved = resolve_within(root, path, true)?;
    rt.write_file(&resolved, content)
        .map_err(ToolError::Io)?;
    Ok(format!("wrote {} bytes to {}", content.len(), path))
}

/// 精确替换文件中的一段文本，但限制路径必须位于 `root` 之内。
///
/// 语义：`old` 必须**恰好出现一次**（按 `str::matches` 的非重叠匹配计数）。
///
/// - 0 次 → [`ToolError::OldTextNotFound`]
/// - 多次 → [`ToolError::MultipleMatches`]（拒绝修改，避免歧义）
///
/// 只有确认恰好一次之后才写回文件；任何失败路径都不会修改原文件。
/// 第一版只处理有效 UTF-8 文本文件，不做编码检测。
fn edit_file_within(
    rt: &dyn Runtime,
    root: &Path,
    path: &str,
    old: &str,
    new: &str,
) -> Result<String, ToolError> {
    if old.is_empty() {
        return Err(ToolError::InvalidArguments(
            "edit_file requires a non-empty `old`".to_string(),
        ));
    }

    let resolved = resolve_within(root, path, false)?;
    let content = rt
        .read_file(&resolved)
        .map_err(|err| match err.kind() {
            // read_to_string 对非 UTF-8 文件返回 InvalidData
            io::ErrorKind::InvalidData => {
                ToolError::InvalidArguments(format!("{path} is not a valid UTF-8 text file"))
            }
            _ => ToolError::Io(err),
        })?;

    match content.matches(old).count() {
        0 => return Err(ToolError::OldTextNotFound(path.to_string())),
        1 => {}
        _ => return Err(ToolError::MultipleMatches(path.to_string())),
    }

    let replaced = content.replacen(old, new, 1);
    rt.write_file(&resolved, &replaced)
        .map_err(ToolError::Io)?;
    Ok(format!("edited {path} successfully"))
}

/// 扩展白名单中的一条：程序名（裸命令名）+ 可选子命令。
///
/// 由 [`parse_exec_allow`] 从 `KARAKURI_EXEC_ALLOW` 解析得到。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdPattern {
    /// 程序名（如 `make`），不带路径（靠 PATH 解析）。
    pub program: String,
    /// 可选子命令；`Some` 时仅放行 `program subcommand ...`，`None` 时放行整个程序。
    pub subcommand: Option<String>,
}

/// exec 白名单策略：内置 cargo / 只读 git 规则之上，叠加用户显式放行的扩展白名单。
///
/// `Default` 为空（内置规则之外一律拒绝），保持默认行为零变化。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecPolicy {
    /// `KARAKURI_EXEC_ALLOW` 解析出的扩展白名单。
    pub extra_allow: Vec<CmdPattern>,
}

/// 解析 `KARAKURI_EXEC_ALLOW`：逗号分隔多项；每项 1 个 token = 仅程序名
/// （如 `make`），2 个 token = 程序 + 子命令（如 `git grep`）；trim 后空项跳过；
/// 每项 token 必须过 [`is_valid_arg`]，program 不得含 `/`（裸命令名，靠 PATH 解析）；
/// token 数 > 2 或含非法字符 → `Err`（说明哪一项非法）。
pub fn parse_exec_allow(raw: &str) -> Result<Vec<CmdPattern>, String> {
    let mut patterns = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = item.split_whitespace().collect();
        if tokens.len() > 2 {
            return Err(format!(
                "{item:?} has {} tokens (expected a program name, or \"program subcommand\")",
                tokens.len()
            ));
        }
        let program = tokens[0];
        if program.contains('/') {
            return Err(format!(
                "{program:?} contains '/' — program must be a bare command name resolved via PATH"
            ));
        }
        if !is_valid_arg(program) {
            return Err(format!("{program:?} contains invalid characters"));
        }
        let subcommand = match tokens.get(1) {
            Some(sub) => {
                if !is_valid_arg(sub) {
                    return Err(format!("{sub:?} contains invalid characters"));
                }
                Some(sub.to_string())
            }
            None => None,
        };
        patterns.push(CmdPattern {
            program: program.to_string(),
            subcommand,
        });
    }
    Ok(patterns)
}

/// 结构化的允许命令：可执行文件 + 白名单参数。
///
/// 由 [`ExecCommand::parse_with`] 从字符串解析得到，绝不包含 shell 元字符。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecCommand {
    /// 可执行文件名（如 `cargo` / `git`），不带路径。
    program: String,
    args: Vec<String>,
}

impl ExecCommand {
    /// 解析命令字符串，并对解析结果做参数级白名单校验。
    ///
    /// 等价于 [`ExecCommand::parse_with`] 传空扩展白名单（内置规则之外一律拒绝）。
    /// 仅供测试与 [`exec_within`] 使用；新代码走 [`ExecCommand::parse_with`]。
    #[allow(dead_code)]
    fn parse(command: &str) -> Result<Self, ToolError> {
        Self::parse_with(command, &[])
    }

    /// 解析命令字符串，并对解析结果做参数级白名单校验。
    ///
    /// 白名单（内置）：
    /// - `cargo <subcommand> [<args>...]`：`check` / `build` / `test` / `clippy`
    ///   任意参数，`fmt` 仅 `--check`
    /// - `git <status|diff|log|show> [<args>...]`：只读子命令，强制 `--no-pager`
    ///   前置，危险选项（`-C` / `--git-dir` / `-c` / `--output` 等）被拒
    ///
    /// 可配置扩展白名单（`extra`，来自 `KARAKURI_EXEC_ALLOW`）：内置规则都不匹配时，
    /// 若 `tokens[0]` 等于某项 program 且（无 subcommand 或 `tokens[1]` 等于该项
    /// subcommand）则放行，参数（`tokens[1..]`）仍须过 [`is_valid_arg`]。
    ///
    /// 整个字符串按空白拆分（不支持引号，引号属于非法参数），
    /// 因此 `;`、`&&`、`|`、`>`、`$()` 等 shell 拼接无法混入。
    fn parse_with(command: &str, extra: &[CmdPattern]) -> Result<Self, ToolError> {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(ToolError::CommandNotAllowed(command.to_string()));
        }

        // 内置 cargo 规则
        if tokens[0] == "cargo" {
            let Some(sub) = tokens.get(1) else {
                return Err(ToolError::CommandNotAllowed(command.to_string()));
            };
            let args: Vec<String> = tokens[2..]
                .iter()
                .map(|t| {
                    let s = t.to_string();
                    if is_valid_arg(&s) {
                        Ok(s)
                    } else {
                        Err(ToolError::CommandNotAllowed(s))
                    }
                })
                .collect::<Result<_, _>>()?;

            let allowed = match *sub {
                "check" | "build" | "test" | "clippy" => true,
                "fmt" => args == ["--check"],
                _ => false,
            };
            if !allowed {
                return Err(ToolError::CommandNotAllowed(command.to_string()));
            }

            return Ok(ExecCommand {
                program: "cargo".to_string(),
                args: std::iter::once(sub.to_string())
                    .chain(args)
                    .collect(),
            });
        }

        // 内置只读 git 规则：子命令命中 status/diff/log/show 时放行（`--no-pager` 前置、
        // 危险选项被拒）；未命中（如 git grep / git commit）落入扩展白名单继续判断。
        if tokens[0] == "git"
            && let Some(cmd) = parse_read_only_git(&tokens)?
        {
            return Ok(cmd);
        }

        // 可配置扩展白名单（KARAKURI_EXEC_ALLOW）
        for pattern in extra {
            if pattern.program != tokens[0] {
                continue;
            }
            let sub_matches = match &pattern.subcommand {
                Some(sub) => tokens
                    .get(1)
                    .map(|t| *t == sub)
                    .unwrap_or(false),
                None => true,
            };
            if !sub_matches {
                continue;
            }
            let args: Vec<String> = tokens[1..]
                .iter()
                .map(|t| {
                    let s = t.to_string();
                    if is_valid_arg(&s) {
                        Ok(s)
                    } else {
                        Err(ToolError::CommandNotAllowed(s))
                    }
                })
                .collect::<Result<_, _>>()?;
            return Ok(ExecCommand {
                program: pattern.program.clone(),
                args,
            });
        }

        Err(ToolError::CommandNotAllowed(tokens[0].to_string()))
    }
}

/// 解析只读 git 子命令（`status` / `diff` / `log` / `show`）。
///
/// - 命中内置只读子命令：逐参数过 [`is_valid_arg`] 与 [`is_forbidden_git_option`]，
///   构造 `git --no-pager <sub> [<args>...]`（`--no-pager` 插在最前，杜绝
///   pager / 外部工具执行）；
/// - 未命中（裸 `git` 或非只读子命令，如 `git grep` / `git commit`）→ `Ok(None)`，
///   由调用方落入扩展白名单继续判断；
/// - 命中但含危险选项 / 非法参数 → `Err`（不可被扩展白名单覆盖）。
fn parse_read_only_git(tokens: &[&str]) -> Result<Option<ExecCommand>, ToolError> {
    let Some(sub) = tokens.get(1) else {
        return Ok(None);
    };
    if !matches!(*sub, "status" | "diff" | "log" | "show") {
        return Ok(None);
    }

    let args: Vec<String> = tokens[2..]
        .iter()
        .map(|t| {
            let s = t.to_string();
            if !is_valid_arg(&s) || is_forbidden_git_option(&s) {
                Err(ToolError::CommandNotAllowed(s))
            } else {
                Ok(s)
            }
        })
        .collect::<Result<_, _>>()?;

    Ok(Some(ExecCommand {
        program: "git".to_string(),
        args: std::iter::once("--no-pager".to_string())
            .chain(std::iter::once(sub.to_string()))
            .chain(args)
            .collect(),
    }))
}

/// 允许出现在参数中的字符：字母、数字、`-`、`_`、`.`、`/`、`=`、`:`、`+`、`#`。
///
/// 排除空格（本身按空白拆分）、引号、`;`、`&`、`|`、`>`、`<`、`$`、\`、`*` 等
/// 所有 shell 元字符，使命令无法拼接或重定向。
fn is_valid_arg(arg: &str) -> bool {
    !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | '+' | '#' | ',')
        })
}

/// 只读 git 参数的危险选项黑名单：命中即拒绝该参数。
///
/// - 短选项：仅"精确等于"被拒（如 `-C` / `-c`）；
/// - 长选项："精确等于"或"以 `--opt=` 开头"被拒（如 `--git-dir=/x`）。
///
/// 覆盖：定位逃逸（`-C`、`--git-dir`、`--work-tree`、`--namespace`、
/// `--super-prefix`）、配置注入 / 代执行（`-c`、`--config-env`）、写文件
/// （`--output`）、调用外部程序（`--ext-diff`、`--textconv`）、分页（`--paginate`）。
fn is_forbidden_git_option(arg: &str) -> bool {
    const FORBIDDEN_SHORT: &[&str] = &["-C", "-c"];
    const FORBIDDEN_LONG: &[&str] = &[
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--super-prefix",
        "--config-env",
        "--output",
        "--ext-diff",
        "--textconv",
        "--paginate",
    ];

    if FORBIDDEN_SHORT.contains(&arg) {
        return true;
    }
    FORBIDDEN_LONG
        .iter()
        .any(|opt| arg == *opt || arg.starts_with(&format!("{opt}=")))
}

/// 在 `root` 内执行允许的开发命令，返回 `exit code` + stdout + stderr。
///
/// 等价于 [`exec_within_policy`] 传默认策略（空扩展白名单）。仅供测试与既有调用方使用。
#[allow(dead_code)]
fn exec_within(rt: &dyn Runtime, root: &Path, command: &str) -> Result<String, ToolError> {
    exec_within_policy(rt, root, command, &ExecPolicy::default())
}

/// 在 `root` 内执行允许的开发命令，返回 `exit code` + stdout + stderr。
///
/// 安全约束：
/// - 白名单：内置 `cargo check/build/test/clippy/fmt --check` + 只读 git
///   `status/diff/log/show`（强制 `--no-pager`、危险选项被拒），叠加 `policy`
///   声明的扩展白名单（`KARAKURI_EXEC_ALLOW`），见 [`ExecCommand::parse_with`]
/// - 不使用 shell；进程执行位于 [`Runtime::exec`]（对应 `std::process::Command` 直调）
/// - 工作目录固定为 `root`，继承当前环境变量（模型无法修改）
/// - 60 秒超时，超时返回已捕获的输出
fn exec_within_policy(
    rt: &dyn Runtime,
    root: &Path,
    command: &str,
    policy: &ExecPolicy,
) -> Result<String, ToolError> {
    let parsed = ExecCommand::parse_with(command, &policy.extra_allow)?;
    match rt.exec(&parsed.program, &parsed.args, root) {
        Ok(ExecOutput { code, output }) => {
            if code == 0 {
                Ok(format!("exit code: {code}\n\nstdout:\n{output}\nstderr:\n"))
            } else {
                Err(ToolError::NonZeroExit(code, output))
            }
        }
        Err(ExecError::Io(err)) => Err(ToolError::Io(err)),
        Err(ExecError::TimedOut(output)) => Err(ToolError::TimedOut(output)),
    }
}

/// 校验路径是否位于 `root` 之内，并返回 canonicalized 后的路径。
///
/// 拒绝绝对路径、`..` 跳转，以及通过 symlink 逃逸出 `root` 的路径。
///
/// `allow_missing` 为 true 时（write 场景），目标文件可以尚不存在：
/// 此时改由父目录的 canonicalized 路径参与边界判断，文件名拼接其后。
fn resolve_within(root: &Path, path: &str, allow_missing: bool) -> Result<PathBuf, ToolError> {
    let candidate = PathBuf::from(path);

    if candidate.is_absolute() {
        return Err(ToolError::OutsideProject(path.to_string()));
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(ToolError::OutsideProject(path.to_string()));
    }

    let root = root.canonicalize().map_err(ToolError::Io)?;
    let full = root.join(&candidate);

    match full.canonicalize() {
        Ok(resolved) => {
            if !resolved.starts_with(&root) {
                return Err(ToolError::OutsideProject(path.to_string()));
            }
            Ok(resolved)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound && allow_missing => {
            // 目标路径尚不存在（write 场景），可能含多级尚未创建的目录
            // （如 src/main.rs）：沿父级向上找到最近的已存在目录，
            // canonicalize 后确认它仍在 root 内（已存在的祖先若为指向
            // root 外的 symlink 会在此被拦截），再把不存在的尾部拼回。
            let mut existing = full.clone();
            let mut tail: Vec<std::ffi::OsString> = Vec::new();
            loop {
                match existing.canonicalize() {
                    Ok(resolved) => {
                        if !resolved.starts_with(&root) {
                            return Err(ToolError::OutsideProject(path.to_string()));
                        }
                        let mut result = resolved;
                        for component in tail.iter().rev() {
                            result = result.join(component);
                        }
                        return Ok(result);
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        let (Some(name), Some(parent)) = (existing.file_name(), existing.parent())
                        else {
                            return Err(ToolError::Io(err));
                        };
                        tail.push(name.to_os_string());
                        existing = parent.to_path_buf();
                    }
                    Err(err) => return Err(ToolError::Io(err)),
                }
            }
        }
        Err(err) => match err.kind() {
            io::ErrorKind::NotFound => Err(ToolError::NotFound(path.to_string())),
            _ => Err(ToolError::Io(err)),
        },
    }
}

/// 在 `root` 内搜索文件内容，返回 `path:line:content` 形式的匹配。
///
/// `path` 为 None 时从 `root` 开始递归搜索；为 Some 时限制在该路径
/// （可以是文件或目录）范围内。结果中的路径相对于 `root`。
fn search_within(
    rt: &dyn Runtime,
    root: &Path,
    query: &str,
    path: Option<&str>,
) -> Result<String, ToolError> {
    let root = root.canonicalize().map_err(ToolError::Io)?;
    let start = match path {
        Some(p) => resolve_within(&root, p, false)?,
        None => root.clone(),
    };

    let mut matches: Vec<String> = Vec::new();

    if start.is_file() {
        search_file(rt, &root, &start, query, &mut matches);
    } else if start.is_dir() {
        walk_dir(rt, &root, &start, query, &mut matches);
    } else {
        return Err(ToolError::NotFound(path.unwrap_or(".").to_string()));
    }

    if matches.is_empty() {
        Ok(format!("no matches for \"{query}\""))
    } else {
        Ok(matches.join("\n"))
    }
}

/// 递归遍历目录，跳过无法读取的条目与 symlink（避免逃逸与循环）。
fn walk_dir(rt: &dyn Runtime, root: &Path, dir: &Path, query: &str, matches: &mut Vec<String>) {
    let Ok(entries) = rt.read_dir(dir) else {
        return;
    };

    for entry in entries {
        match entry.kind {
            EntryKind::Dir => walk_dir(rt, root, &entry.path, query, matches),
            EntryKind::File => search_file(rt, root, &entry.path, query, matches),
            // symlink 等其他类型不跟随、不搜索
            EntryKind::Symlink | EntryKind::Other => {}
        }
    }
}

/// 在单个文件内搜索，跳过二进制 / 非 UTF-8 / 无法读取的文件。
///
/// 文件读取经由 `rt.read_file`（`read_to_string` 语义）：非 UTF-8 / 无法读取
/// 的文件返回错误，按"跳过该文件"处理（与旧 `fs::read` + `from_utf8` 行为一致）。
fn search_file(rt: &dyn Runtime, root: &Path, file: &Path, query: &str, matches: &mut Vec<String>) {
    let Ok(text) = rt.read_file(file) else {
        return;
    };

    for (index, line) in text.lines().enumerate() {
        if line.contains(query) {
            let rel = file.strip_prefix(root).unwrap_or(file);
            matches.push(format!("{}:{}:{}", rel.display(), index + 1, line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("karakuri-tool-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_file_inside_root() {
        let root = temp_root();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let tool = Tool::ReadFile(ReadFile {
            path: "main.rs".to_string(),
        });
        assert_eq!(
            tool.execute(&LocalRuntime::default(), &root)
                .unwrap(),
            "fn main() {}"
        );
    }

    #[test]
    fn reads_file_in_subdirectory() {
        let root = temp_root();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("lib.rs"), "pub fn f() {}").unwrap();

        let tool = Tool::ReadFile(ReadFile {
            path: "src/lib.rs".to_string(),
        });
        assert_eq!(
            tool.execute(&LocalRuntime::default(), &root)
                .unwrap(),
            "pub fn f() {}"
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_root();
        let tool = Tool::ReadFile(ReadFile {
            path: "../../etc/passwd".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn rejects_absolute_path() {
        let root = temp_root();
        let tool = Tool::ReadFile(ReadFile {
            path: "/etc/passwd".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        let outside = root
            .parent()
            .unwrap()
            .join(format!("karakuri-outside-{}", std::process::id()));
        fs::write(&outside, "outside").unwrap();
        symlink(&outside, root.join("link.txt")).unwrap();

        let tool = Tool::ReadFile(ReadFile {
            path: "link.txt".to_string(),
        });
        let result = tool.execute(&LocalRuntime::default(), &root);

        fs::remove_file(&outside).ok();
        assert!(matches!(result, Err(ToolError::OutsideProject(_))));
    }

    #[test]
    fn not_found_reports_clear_error() {
        let root = temp_root();
        let tool = Tool::ReadFile(ReadFile {
            path: "missing.rs".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::NotFound(_))
        ));
    }

    #[test]
    fn from_call_parses_arguments() {
        let args = serde_json::json!({ "path": "src/main.rs" });
        let tool = Tool::from_call("read_file", &args).unwrap();
        assert_eq!(
            tool,
            Tool::ReadFile(ReadFile {
                path: "src/main.rs".to_string(),
            })
        );
    }

    #[test]
    fn from_call_rejects_unknown_tool() {
        let args = serde_json::json!({});
        assert!(matches!(
            Tool::from_call("delete_file", &args),
            Err(ToolError::UnknownTool(_))
        ));
    }

    #[test]
    fn from_call_rejects_missing_path() {
        let args = serde_json::json!({});
        assert!(matches!(
            Tool::from_call("read_file", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn from_call_parses_write_file_arguments() {
        let args = serde_json::json!({ "path": "out.txt", "content": "hello" });
        let tool = Tool::from_call("write_file", &args).unwrap();
        assert_eq!(
            tool,
            Tool::WriteFile(WriteFile {
                path: "out.txt".to_string(),
                content: "hello".to_string(),
            })
        );
    }

    #[test]
    fn from_call_rejects_write_file_missing_content() {
        let args = serde_json::json!({ "path": "out.txt" });
        assert!(matches!(
            Tool::from_call("write_file", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn writes_file_inside_root() {
        let root = temp_root();
        let tool = Tool::WriteFile(WriteFile {
            path: "out.txt".to_string(),
            content: "hello world".to_string(),
        });

        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("out.txt"));

        let written = fs::read_to_string(root.join("out.txt")).unwrap();
        assert_eq!(written, "hello world");
    }

    #[test]
    fn writes_file_in_subdirectory() {
        let root = temp_root();
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();

        let tool = Tool::WriteFile(WriteFile {
            path: "src/lib.rs".to_string(),
            content: "pub fn f() {}".to_string(),
        });
        tool.execute(&LocalRuntime::default(), &root)
            .unwrap();

        let written = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(written, "pub fn f() {}");
    }

    #[test]
    fn write_overwrites_existing_file() {
        let root = temp_root();
        fs::write(root.join("out.txt"), "old").unwrap();

        let tool = Tool::WriteFile(WriteFile {
            path: "out.txt".to_string(),
            content: "new".to_string(),
        });
        tool.execute(&LocalRuntime::default(), &root)
            .unwrap();

        assert_eq!(fs::read_to_string(root.join("out.txt")).unwrap(), "new");
    }

    #[test]
    fn write_rejects_parent_traversal() {
        let root = temp_root();
        let tool = Tool::WriteFile(WriteFile {
            path: "../../etc/passwd".to_string(),
            content: "evil".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn write_rejects_absolute_path() {
        let root = temp_root();
        let tool = Tool::WriteFile(WriteFile {
            path: "/etc/passwd".to_string(),
            content: "evil".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        let outside = root
            .parent()
            .unwrap()
            .join(format!("karakuri-write-outside-{}", std::process::id()));
        fs::write(&outside, "outside").unwrap();
        symlink(&outside, root.join("link.txt")).unwrap();

        let tool = Tool::WriteFile(WriteFile {
            path: "link.txt".to_string(),
            content: "evil".to_string(),
        });
        let result = tool.execute(&LocalRuntime::default(), &root);

        fs::remove_file(&outside).ok();
        assert!(matches!(result, Err(ToolError::OutsideProject(_))));
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        let root = temp_root();
        let tool = Tool::WriteFile(WriteFile {
            path: "no/such/dir/out.txt".to_string(),
            content: "x".to_string(),
        });
        let result = tool.execute(&LocalRuntime::default(), &root);
        assert!(result.is_ok(), "{result:?}");
        // 多级父目录被自动创建，内容落盘
        assert_eq!(
            fs::read_to_string(root.join("no/such/dir/out.txt")).unwrap(),
            "x"
        );
    }

    #[test]
    fn search_finds_matches_with_path_and_line() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/model.rs"),
            "pub struct Model {}\npub fn build() {}\n// Model again\n",
        )
        .unwrap();

        let tool = Tool::Search(Search {
            query: "Model".to_string(),
            path: None,
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("src/model.rs:1:pub struct Model {}"));
        assert!(result.contains("src/model.rs:3:// Model again"));
    }

    #[test]
    fn search_respects_path_scope() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("src/a.rs"), "needle here\n").unwrap();
        fs::write(root.join("tests/b.rs"), "needle there\n").unwrap();

        let tool = Tool::Search(Search {
            query: "needle".to_string(),
            path: Some("src".to_string()),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("src/a.rs:1:needle here"));
        assert!(!result.contains("tests/b.rs"));
    }

    #[test]
    fn search_can_target_single_file() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "needle\n").unwrap();
        fs::write(root.join("src/b.rs"), "other\n").unwrap();

        let tool = Tool::Search(Search {
            query: "needle".to_string(),
            path: Some("src/a.rs".to_string()),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("src/a.rs:1:needle"));
        assert!(!result.contains("src/b.rs"));
    }

    #[test]
    fn search_reports_no_matches() {
        let root = temp_root();
        fs::write(root.join("a.rs"), "hello\n").unwrap();

        let tool = Tool::Search(Search {
            query: "zzz".to_string(),
            path: None,
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("no matches"));
    }

    #[test]
    fn from_call_rejects_search_missing_query() {
        let args = serde_json::json!({ "path": "src" });
        assert!(matches!(
            Tool::from_call("search", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn from_call_parses_search_arguments() {
        let args = serde_json::json!({ "query": "foo" });
        let tool = Tool::from_call("search", &args).unwrap();
        assert_eq!(
            tool,
            Tool::Search(Search {
                query: "foo".to_string(),
                path: None,
            })
        );

        let args = serde_json::json!({ "query": "foo", "path": "src" });
        let tool = Tool::from_call("search", &args).unwrap();
        assert_eq!(
            tool,
            Tool::Search(Search {
                query: "foo".to_string(),
                path: Some("src".to_string()),
            })
        );
    }

    #[test]
    fn search_rejects_parent_traversal() {
        let root = temp_root();
        let tool = Tool::Search(Search {
            query: "x".to_string(),
            path: Some("../../etc".to_string()),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn search_rejects_absolute_path() {
        let root = temp_root();
        let tool = Tool::Search(Search {
            query: "x".to_string(),
            path: Some("/etc".to_string()),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn search_rejects_nonexistent_path() {
        let root = temp_root();
        let tool = Tool::Search(Search {
            query: "x".to_string(),
            path: Some("missing".to_string()),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::NotFound(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn search_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        let outside = root
            .parent()
            .unwrap()
            .join(format!("karakuri-search-outside-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.rs"), "needle\n").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        let tool = Tool::Search(Search {
            query: "needle".to_string(),
            path: Some("link".to_string()),
        });
        let result = tool.execute(&LocalRuntime::default(), &root);

        fs::remove_dir_all(&outside).ok();
        assert!(matches!(result, Err(ToolError::OutsideProject(_))));
    }

    #[cfg(unix)]
    #[test]
    fn search_does_not_follow_symlinks_during_walk() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        let outside = root.parent().unwrap().join(format!(
            "karakuri-search-walk-outside-{}",
            std::process::id()
        ));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.rs"), "needle\n").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        // 从 root 递归搜索，不应跟随 link 进入 outside
        let tool = Tool::Search(Search {
            query: "needle".to_string(),
            path: None,
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();

        fs::remove_dir_all(&outside).ok();
        assert!(result.contains("no matches"), "unexpected: {result}");
    }

    #[test]
    fn search_skips_binary_files() {
        let root = temp_root();
        fs::write(root.join("text.rs"), "needle\n").unwrap();
        fs::write(
            root.join("blob.bin"),
            [0x00, 0xff, 0xfe, b'n', b'e', b'e', b'd', b'l', b'e'],
        )
        .unwrap();

        let tool = Tool::Search(Search {
            query: "needle".to_string(),
            path: None,
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("text.rs:1:needle"));
        assert!(!result.contains("blob.bin"));
    }

    // ---------------- edit_file --------------

    #[test]
    fn from_call_parses_edit_file_arguments() {
        let args = serde_json::json!({ "path": "src/main.rs", "old": "foo", "new": "bar" });
        let tool = Tool::from_call("edit_file", &args).unwrap();
        assert_eq!(
            tool,
            Tool::EditFile(EditFile {
                path: "src/main.rs".to_string(),
                old: "foo".to_string(),
                new: "bar".to_string(),
            })
        );
    }

    #[test]
    fn from_call_rejects_edit_file_missing_old() {
        let args = serde_json::json!({ "path": "a.txt", "new": "bar" });
        assert!(matches!(
            Tool::from_call("edit_file", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn from_call_rejects_edit_file_missing_new() {
        let args = serde_json::json!({ "path": "a.txt", "old": "foo" });
        assert!(matches!(
            Tool::from_call("edit_file", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn from_call_rejects_edit_file_missing_path() {
        let args = serde_json::json!({ "old": "foo", "new": "bar" });
        assert!(matches!(
            Tool::from_call("edit_file", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn edit_replaces_single_occurrence() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "hello world\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "hello".to_string(),
            new: "goodbye".to_string(),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("a.txt"));

        let edited = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(edited, "goodbye world\n");
    }

    #[test]
    fn edit_keeps_unmatched_text_intact() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "line one\nline two\nline three\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "line two".to_string(),
            new: "LINE TWO".to_string(),
        });
        tool.execute(&LocalRuntime::default(), &root)
            .unwrap();

        let edited = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(edited, "line one\nLINE TWO\nline three\n");
    }

    #[test]
    fn edit_rejects_missing_old_text() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "hello world\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "zzz".to_string(),
            new: "bar".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OldTextNotFound(_))
        ));
    }

    #[test]
    fn edit_rejects_multiple_matches_and_leaves_file_unchanged() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "hello\nhello\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "hello".to_string(),
            new: "goodbye".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::MultipleMatches(_))
        ));

        // 拒绝时不得修改原文件
        let content = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(content, "hello\nhello\n");
    }

    #[test]
    fn edit_rejects_overlapping_matches() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "aaaa").unwrap();

        // "aa" 在 "aaaa" 中按非重叠匹配出现 2 次，应拒绝
        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "aa".to_string(),
            new: "b".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::MultipleMatches(_))
        ));
    }

    #[test]
    fn edit_allows_multiline_old_text() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "one\ntwo".to_string(),
            new: "ONE\nTWO".to_string(),
        });
        tool.execute(&LocalRuntime::default(), &root)
            .unwrap();

        let edited = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(edited, "ONE\nTWO\nthree\n");
    }

    #[test]
    fn edit_rejects_empty_old_text() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "hello\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "".to_string(),
            new: "bar".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn edit_rejects_nonexistent_file() {
        let root = temp_root();
        let tool = Tool::EditFile(EditFile {
            path: "missing.txt".to_string(),
            old: "foo".to_string(),
            new: "bar".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::NotFound(_))
        ));
    }

    #[test]
    fn edit_rejects_parent_traversal() {
        let root = temp_root();
        let tool = Tool::EditFile(EditFile {
            path: "../../etc/passwd".to_string(),
            old: "root".to_string(),
            new: "x".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn edit_rejects_absolute_path() {
        let root = temp_root();
        let tool = Tool::EditFile(EditFile {
            path: "/etc/passwd".to_string(),
            old: "root".to_string(),
            new: "x".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn edit_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        let outside = root
            .parent()
            .unwrap()
            .join(format!("karakuri-edit-outside-{}", std::process::id()));
        fs::write(&outside, "outside\n").unwrap();
        symlink(&outside, root.join("link.txt")).unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "link.txt".to_string(),
            old: "outside".to_string(),
            new: "inside".to_string(),
        });
        let result = tool.execute(&LocalRuntime::default(), &root);

        fs::remove_file(&outside).ok();
        assert!(matches!(result, Err(ToolError::OutsideProject(_))));
    }

    #[test]
    fn edit_rejects_non_utf8_file() {
        let root = temp_root();
        fs::write(root.join("blob.bin"), [0xff, 0xfe, 0x00]).unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "blob.bin".to_string(),
            old: "foo".to_string(),
            new: "bar".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::InvalidArguments(_))
        ));

        // 非 UTF-8 文件保持不变
        let bytes = fs::read(root.join("blob.bin")).unwrap();
        assert_eq!(bytes, [0xff, 0xfe, 0x00]);
    }

    #[test]
    fn edit_replaces_single_occurrence_of_repeated_word() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "foo and foo").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "foo".to_string(),
            new: "bar".to_string(),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::MultipleMatches(_))
        ));
    }

    #[test]
    fn edit_with_identical_old_and_new_is_noop() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "hello\n").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "hello".to_string(),
            new: "hello".to_string(),
        });
        tool.execute(&LocalRuntime::default(), &root)
            .unwrap();

        let edited = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(edited, "hello\n");
    }

    #[test]
    fn edit_replaces_occurrence_inside_larger_text() {
        let root = temp_root();
        fs::write(root.join("a.txt"), "function foo() { return foo; }").unwrap();

        let tool = Tool::EditFile(EditFile {
            path: "a.txt".to_string(),
            old: "return foo".to_string(),
            new: "return bar".to_string(),
        });
        tool.execute(&LocalRuntime::default(), &root)
            .unwrap();

        let edited = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(edited, "function foo() { return bar; }");
    }

    // ---------------- list_dir ----------------

    #[test]
    fn from_call_list_dir_defaults_to_none() {
        // 无 path 字段
        let tool = Tool::from_call("list_dir", &serde_json::json!({})).unwrap();
        assert_eq!(tool, Tool::ListDir(ListDir { path: None }));
        // path 为 null
        let tool = Tool::from_call("list_dir", &serde_json::json!({ "path": null })).unwrap();
        assert_eq!(tool, Tool::ListDir(ListDir { path: None }));
        // 空串
        let tool = Tool::from_call("list_dir", &serde_json::json!({ "path": "" })).unwrap();
        assert_eq!(tool, Tool::ListDir(ListDir { path: None }));
        // 全空白
        let tool = Tool::from_call("list_dir", &serde_json::json!({ "path": "   " })).unwrap();
        assert_eq!(tool, Tool::ListDir(ListDir { path: None }));
    }

    #[test]
    fn from_call_list_dir_parses_path() {
        let args = serde_json::json!({ "path": "src" });
        let tool = Tool::from_call("list_dir", &args).unwrap();
        assert_eq!(
            tool,
            Tool::ListDir(ListDir {
                path: Some("src".to_string()),
            })
        );
        // 显式 path 会 trim 空白
        let args = serde_json::json!({ "path": "  src  " });
        let tool = Tool::from_call("list_dir", &args).unwrap();
        assert_eq!(
            tool,
            Tool::ListDir(ListDir {
                path: Some("src".to_string()),
            })
        );
    }

    #[test]
    fn list_dir_lists_project_root() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join(".hidden"), "dotfile").unwrap();

        let tool = Tool::ListDir(ListDir { path: None });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("main.rs (file)"), "unexpected: {result}");
        assert!(result.contains("src/ (directory)"), "unexpected: {result}");
        // 含隐藏文件（dotfile）
        assert!(result.contains(".hidden (file)"), "unexpected: {result}");
    }

    #[test]
    fn list_dir_is_non_recursive() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn f() {}").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let tool = Tool::ListDir(ListDir { path: None });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("main.rs (file)"), "unexpected: {result}");
        assert!(result.contains("src/ (directory)"), "unexpected: {result}");
        // 非递归：子目录内的文件不出现
        assert!(!result.contains("lib.rs"), "unexpected: {result}");
    }

    #[test]
    fn list_dir_lists_subdirectory() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn f() {}").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let tool = Tool::ListDir(ListDir {
            path: Some("src".to_string()),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("lib.rs (file)"), "unexpected: {result}");
        assert!(!result.contains("main.rs"), "unexpected: {result}");
    }

    #[cfg(unix)]
    #[test]
    fn list_dir_marks_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        fs::write(root.join("target.txt"), "x").unwrap();
        symlink("target.txt", root.join("link.txt")).unwrap();

        let tool = Tool::ListDir(ListDir { path: None });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("target.txt (file)"), "unexpected: {result}");
        assert!(
            result.contains("link.txt (symlink)"),
            "unexpected: {result}"
        );
    }

    #[test]
    fn list_dir_rejects_parent_traversal() {
        let root = temp_root();
        let tool = Tool::ListDir(ListDir {
            path: Some("../outside".to_string()),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn list_dir_rejects_absolute_path() {
        let root = temp_root();
        let tool = Tool::ListDir(ListDir {
            path: Some("/etc".to_string()),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[test]
    fn list_dir_rejects_nonexistent_directory() {
        let root = temp_root();
        let tool = Tool::ListDir(ListDir {
            path: Some("missing".to_string()),
        });
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), &root),
            Err(ToolError::NotFound(_))
        ));
    }

    #[test]
    fn list_dir_reports_empty_directory() {
        let root = temp_root();
        let tool = Tool::ListDir(ListDir { path: None });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert_eq!(result, "Directory is empty.");
    }

    #[test]
    fn list_dir_output_is_sorted_and_deterministic() {
        let root = temp_root();
        fs::write(root.join("zeta.txt"), "x").unwrap();
        fs::write(root.join("alpha.rs"), "x").unwrap();
        fs::create_dir(root.join("mid")).unwrap();
        fs::write(root.join(".dot"), "x").unwrap();

        let tool = Tool::ListDir(ListDir { path: None });
        let first = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        let second = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        // 两次调用结果一致（确定性）
        assert_eq!(first, second);
        // 按条目名排序（'.' < 'a' < 'm' < 'z'），目录/文件混合，逐行稳定
        assert_eq!(
            first,
            ".dot (file)\nalpha.rs (file)\nmid/ (directory)\nzeta.txt (file)"
        );
    }

    #[test]
    fn all_definitions_includes_list_dir() {
        let defs = all_definitions();
        let list = defs
            .iter()
            .find(|d| d.name == "list_dir")
            .unwrap();

        assert_eq!(list.name, "list_dir");
        assert!(list.description.contains("non-recursive"));
        assert_eq!(list.parameters["type"], "object");
        assert_eq!(list.parameters["properties"]["path"]["type"], "string");
        assert_eq!(list.parameters["required"], serde_json::json!([]));
    }

    #[test]
    fn all_definitions_includes_edit_file() {
        let defs = all_definitions();
        let edit = defs
            .iter()
            .find(|d| d.name == "edit_file")
            .unwrap();

        assert_eq!(edit.name, "edit_file");
        assert!(
            edit.description
                .contains("Replace an exact piece of text")
        );
        assert_eq!(edit.parameters["type"], "object");
        assert_eq!(edit.parameters["properties"]["path"]["type"], "string");
        assert_eq!(edit.parameters["properties"]["old"]["type"], "string");
        assert_eq!(edit.parameters["properties"]["new"]["type"], "string");
        assert_eq!(
            edit.parameters["required"],
            serde_json::json!(["path", "old", "new"])
        );
    }

    #[test]
    fn all_definitions_keeps_existing_tools() {
        let defs = all_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"search"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"edit_file"));
    }

    // ---------------- exec：参数解析 ---------------

    #[test]
    fn exec_parse_allows_plain_cargo_subcommands() {
        for sub in ["check", "test", "build", "clippy"] {
            let cmd = ExecCommand::parse(&format!("cargo {sub}")).unwrap();
            assert_eq!(cmd.program, "cargo");
            assert_eq!(cmd.args, vec![sub]);
        }
    }

    #[test]
    fn exec_parse_allows_fmt_check_only() {
        let cmd = ExecCommand::parse("cargo fmt --check").unwrap();
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.args, vec!["fmt", "--check"]);
    }

    #[test]
    fn exec_parse_rejects_fmt_without_check() {
        assert!(matches!(
            ExecCommand::parse("cargo fmt"),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_parse_rejects_fmt_with_extra_args() {
        assert!(matches!(
            ExecCommand::parse("cargo fmt --check --verbose"),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_parse_rejects_unknown_cargo_subcommand() {
        assert!(matches!(
            ExecCommand::parse("cargo run"),
            Err(ToolError::CommandNotAllowed(_))
        ));
        assert!(matches!(
            ExecCommand::parse("cargo publish"),
            Err(ToolError::CommandNotAllowed(_))
        ));
        assert!(matches!(
            ExecCommand::parse("cargo doc"),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_parse_rejects_cargo_without_subcommand() {
        assert!(matches!(
            ExecCommand::parse("cargo"),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_parse_rejects_shell_metacharacters() {
        for command in [
            "cargo check; rm -rf .",
            "cargo check && touch x",
            "cargo check | grep error",
            "cargo check > /dev/null",
            "cargo check 2>&1",
            "cargo check $(date)",
            "cargo check `id`",
            "cargo check --target 'x86_64'",
            "cargo check '--foo'",
            "cargo check --features \"a b\"",
            "sh -c 'cargo check'",
            "bash -c 'cargo check'",
        ] {
            assert!(
                matches!(
                    ExecCommand::parse(command),
                    Err(ToolError::CommandNotAllowed(_))
                ),
                "should reject: {command}"
            );
        }
    }

    #[test]
    fn exec_parse_rejects_unknown_programs() {
        for command in [
            "rm", "cat", "grep", "ls", "bash", "sh", "zsh", "python", "curl", "git",
        ] {
            assert!(
                matches!(
                    ExecCommand::parse(command),
                    Err(ToolError::CommandNotAllowed(_))
                ),
                "should reject: {command}"
            );
        }
    }

    #[test]
    fn exec_parse_rejects_unknown_program_with_args() {
        assert!(matches!(
            ExecCommand::parse("rm -rf ."),
            Err(ToolError::CommandNotAllowed(_))
        ));
        assert!(matches!(
            ExecCommand::parse("cat Cargo.toml"),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_parse_allows_known_cargo_flags() {
        for command in [
            "cargo check --quiet",
            "cargo check --all-targets",
            "cargo check --all-features",
            "cargo check --no-default-features",
            "cargo check --manifest-path=Cargo.toml",
            "cargo check --lib",
            "cargo check --bins",
            "cargo check --tests",
            "cargo check --benches",
            "cargo check --examples",
            "cargo test -- --nocapture",
            "cargo clippy --all-targets",
            "cargo build --release",
            "cargo build --features foo",
            "cargo build --features foo,bar",
            "cargo build --target x86_64-apple-darwin",
            "cargo build --jobs 4",
            "cargo build --color always",
            "cargo build --offline",
            "cargo check --offline",
            "cargo test --offline",
            "cargo clippy --offline",
        ] {
            ExecCommand::parse(command).unwrap_or_else(|e| panic!("should allow: {command} ({e})"));
        }
    }

    // ---------------- exec：只读 git 子命令 ----------------

    #[test]
    fn exec_parse_allows_read_only_git_subcommands() {
        for command in [
            "git status",
            "git diff --staged",
            "git log --oneline -n 5",
            "git show HEAD",
        ] {
            let cmd = ExecCommand::parse(command)
                .unwrap_or_else(|e| panic!("should allow: {command} ({e})"));
            assert_eq!(cmd.program, "git");
            // argv 以 --no-pager 开头（强制关闭分页）
            assert_eq!(cmd.args[0], "--no-pager", "command: {command}");
        }
    }

    #[test]
    fn exec_parse_git_argv_prepends_no_pager() {
        let cmd = ExecCommand::parse("git log --oneline -n 5").unwrap();
        assert_eq!(cmd.program, "git");
        assert_eq!(cmd.args, vec!["--no-pager", "log", "--oneline", "-n", "5"]);
    }

    #[test]
    fn exec_parse_rejects_git_write_commands() {
        for command in [
            "git commit",
            "git push",
            "git add",
            "git checkout",
            "git reset",
            "git rm",
        ] {
            assert!(
                matches!(
                    ExecCommand::parse(command),
                    Err(ToolError::CommandNotAllowed(_))
                ),
                "should reject: {command}"
            );
        }
    }

    #[test]
    fn exec_parse_rejects_dangerous_git_options() {
        for command in [
            "git -C /tmp status",
            "git status --git-dir=/x",
            "git -c core.pager=x log",
            "git diff --output=x",
            "git diff --ext-diff",
            "git log --textconv",
            "git --work-tree /x status",
        ] {
            assert!(
                matches!(
                    ExecCommand::parse(command),
                    Err(ToolError::CommandNotAllowed(_))
                ),
                "should reject: {command}"
            );
        }
    }

    #[test]
    fn exec_parse_rejects_git_shell_metacharacters() {
        for command in ["git status; rm -rf .", "git status && x", "git status | x"] {
            assert!(
                matches!(
                    ExecCommand::parse(command),
                    Err(ToolError::CommandNotAllowed(_))
                ),
                "should reject: {command}"
            );
        }
    }

    // ---------------- exec：git 危险选项黑名单 ----------------

    #[test]
    fn is_forbidden_git_option_rejects_dangerous_options() {
        for arg in [
            "-C",
            "-c",
            "--git-dir",
            "--git-dir=/x",
            "--work-tree",
            "--work-tree=/x",
            "--namespace",
            "--namespace=foo",
            "--super-prefix",
            "--super-prefix=foo",
            "--config-env",
            "--config-env=VAR",
            "--output",
            "--output=x",
            "--ext-diff",
            "--ext-diff=x",
            "--textconv",
            "--textconv=x",
            "--paginate",
        ] {
            assert!(is_forbidden_git_option(arg), "should forbid: {arg}");
        }
    }

    #[test]
    fn is_forbidden_git_option_allows_safe_args() {
        for arg in [
            "--stat",
            "--oneline",
            "--staged",
            "--cached",
            "-n",
            "HEAD",
            "src/main.rs",
        ] {
            assert!(!is_forbidden_git_option(arg), "should allow: {arg}");
        }
    }

    // ---------------- exec：KARAKURI_EXEC_ALLOW 扩展白名单 ----------------

    #[test]
    fn parse_exec_allow_parses_program_and_subcommand() {
        let patterns = parse_exec_allow("git grep, npm test").unwrap();
        assert_eq!(patterns.len(), 2);
        assert_eq!(
            patterns[0],
            CmdPattern {
                program: "git".to_string(),
                subcommand: Some("grep".to_string()),
            }
        );
        assert_eq!(
            patterns[1],
            CmdPattern {
                program: "npm".to_string(),
                subcommand: Some("test".to_string()),
            }
        );
    }

    #[test]
    fn parse_exec_allow_parses_program_only() {
        let patterns = parse_exec_allow("make").unwrap();
        assert_eq!(
            patterns,
            vec![CmdPattern {
                program: "make".to_string(),
                subcommand: None,
            }]
        );
    }

    #[test]
    fn parse_exec_allow_empty_and_blank_yields_empty() {
        assert_eq!(parse_exec_allow("").unwrap(), vec![]);
        assert_eq!(parse_exec_allow("   ").unwrap(), vec![]);
        // 逗号间空项 / 全空白项跳过
        assert_eq!(
            parse_exec_allow("make, , npm test").unwrap(),
            vec![
                CmdPattern {
                    program: "make".to_string(),
                    subcommand: None,
                },
                CmdPattern {
                    program: "npm".to_string(),
                    subcommand: Some("test".to_string()),
                },
            ]
        );
    }

    #[test]
    fn parse_exec_allow_rejects_invalid_entries() {
        // 3 个 token
        assert!(parse_exec_allow("git a b c").is_err());
        // 引号
        assert!(parse_exec_allow("sh 'x'").is_err());
        // 美元符
        assert!(parse_exec_allow("echo $HOME").is_err());
        // program 含 '/'
        assert!(parse_exec_allow("/bin/sh").is_err());
        // program 含 shell 元字符
        assert!(parse_exec_allow("make;rm").is_err());
    }

    #[test]
    fn parse_with_extra_allow_matches_program_only() {
        let extra = vec![CmdPattern {
            program: "make".to_string(),
            subcommand: None,
        }];
        let cmd = ExecCommand::parse_with("make build", &extra).unwrap();
        assert_eq!(cmd.program, "make");
        assert_eq!(cmd.args, vec!["build"]);
        // 未配置的 npm test 拒绝
        assert!(matches!(
            ExecCommand::parse_with("npm test", &extra),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn parse_with_extra_allow_matches_subcommand() {
        let extra = vec![CmdPattern {
            program: "git".to_string(),
            subcommand: Some("grep".to_string()),
        }];
        let cmd = ExecCommand::parse_with("git grep foo", &extra).unwrap();
        assert_eq!(cmd.program, "git");
        assert_eq!(cmd.args, vec!["grep", "foo"]);
        // git grep 不在内置只读列表，extra 只匹配 grep 子命令 → 其他 git 子命令拒绝
        assert!(matches!(
            ExecCommand::parse_with("git stash", &extra),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn parse_with_extra_does_not_affect_cargo_rules() {
        let extra = vec![CmdPattern {
            program: "make".to_string(),
            subcommand: None,
        }];
        let cmd = ExecCommand::parse_with("cargo check", &extra).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.args, vec!["check"]);
        // cargo 规则不受 extra 影响：cargo run 仍拒绝
        assert!(matches!(
            ExecCommand::parse_with("cargo run", &extra),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn parse_with_extra_args_still_validated() {
        let extra = vec![CmdPattern {
            program: "make".to_string(),
            subcommand: None,
        }];
        // extra 放行的命令参数仍须过 is_valid_arg：shell 拼接被拒
        assert!(matches!(
            ExecCommand::parse_with("make build && rm -rf .", &extra),
            Err(ToolError::CommandNotAllowed(_))
        ));
        assert!(matches!(
            ExecCommand::parse_with("make build; x", &extra),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_from_call_parses_command() {
        let args = serde_json::json!({ "command": "cargo check" });
        let tool = Tool::from_call("exec", &args).unwrap();
        assert_eq!(
            tool,
            Tool::Exec(Exec {
                command: "cargo check".to_string(),
            })
        );
    }

    #[test]
    fn exec_from_call_rejects_missing_command() {
        let args = serde_json::json!({});
        assert!(matches!(
            Tool::from_call("exec", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn exec_from_call_rejects_empty_command() {
        let args = serde_json::json!({ "command": "   " });
        assert!(matches!(
            Tool::from_call("exec", &args),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[test]
    fn exec_from_call_does_not_trim_internal_spaces() {
        let args = serde_json::json!({ "command": "cargo  check" });
        let tool = Tool::from_call("exec", &args).unwrap();
        assert_eq!(
            tool,
            Tool::Exec(Exec {
                command: "cargo  check".to_string(),
            })
        );
    }

    #[test]
    fn exec_all_definitions_schema() {
        let defs = all_definitions();
        let exec = defs
            .iter()
            .find(|d| d.name == "exec")
            .unwrap();

        assert_eq!(exec.name, "exec");
        assert!(
            exec.description
                .contains("allowed project development command")
        );
        assert_eq!(exec.parameters["type"], "object");
        assert_eq!(exec.parameters["properties"]["command"]["type"], "string");
        assert_eq!(exec.parameters["required"], serde_json::json!(["command"]));
    }

    #[test]
    fn exec_all_definitions_keeps_existing_tools() {
        let defs = all_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"search"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"exec"));
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static EXEC_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的最小 Cargo 工程目录，避免并行测试互相干扰。
    fn temp_cargo_root() -> PathBuf {
        let n = EXEC_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("karakuri-exec-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"karakuri-exec-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn exec_parse_echoes_back_error_for_unknown_command() {
        // 通过 Tool::from_call 走完整链路：命令进入工具后应被白名单拒绝
        let args = serde_json::json!({ "command": "echo hello" });
        let tool = Tool::from_call("exec", &args).unwrap();
        assert!(matches!(
            tool.execute(&LocalRuntime::default(), Path::new(".")),
            Err(ToolError::CommandNotAllowed(_))
        ));
    }

    #[test]
    fn exec_cargo_test_passes_in_fixture() {
        let root = temp_cargo_root();
        let tool = Tool::Exec(Exec {
            command: "cargo test --offline".to_string(),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("exit code: 0"), "unexpected: {result}");
        assert!(result.contains("test result: ok"), "unexpected: {result}");
    }

    #[test]
    fn exec_cargo_build_passes_in_fixture() {
        let root = temp_cargo_root();
        let tool = Tool::Exec(Exec {
            command: "cargo build --offline".to_string(),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("exit code: 0"), "unexpected: {result}");
    }

    #[test]
    fn exec_cargo_clippy_passes_in_fixture() {
        let root = temp_cargo_root();
        let tool = Tool::Exec(Exec {
            command: "cargo clippy --offline".to_string(),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("exit code: 0"), "unexpected: {result}");
    }

    #[test]
    fn exec_failing_command_reports_errors_and_output() {
        let root = temp_cargo_root();
        fs::write(root.join("src/main.rs"), "fn main() { let x = ; }\n").unwrap();

        let tool = Tool::Exec(Exec {
            command: "cargo check --offline".to_string(),
        });
        match tool.execute(&LocalRuntime::default(), &root) {
            Err(ToolError::NonZeroExit(code, output)) => {
                assert_ne!(code, 0);
                assert!(
                    output.contains("expected expression"),
                    "unexpected: {output}"
                );
            }
            other => panic!("expected NonZeroExit, got: {other:?}"),
        }
    }

    #[test]
    fn exec_working_directory_is_root() {
        let root = temp_cargo_root();
        let tool = Tool::Exec(Exec {
            command: "cargo check --offline".to_string(),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(
            result.contains("karakuri-exec-fixture"),
            "unexpected: {result}"
        );
    }

    #[test]
    fn exec_stderr_is_captured() {
        let root = temp_cargo_root();
        fs::write(root.join("src/main.rs"), "fn main() { let x = ; }\n").unwrap();

        let tool = Tool::Exec(Exec {
            command: "cargo check --offline".to_string(),
        });
        match tool.execute(&LocalRuntime::default(), &root) {
            Err(ToolError::NonZeroExit(_, output)) => {
                // cargo 将编译错误写到 stderr，应出现在结果里
                assert!(
                    output.contains("expected expression"),
                    "unexpected: {output}"
                );
            }
            other => panic!("expected NonZeroExit, got: {other:?}"),
        }
    }

    /// 端到端 smoke：真实 git 只读命令经工具跑通（git init 后 git status 退出码 0）。
    ///
    /// 在临时目录内 `git init -q` 并配置最小 user，避免依赖外部仓库状态；
    /// 证明只读 git 子命令走通真实进程（argv 带 `--no-pager`，不触发分页）。
    #[test]
    fn exec_git_status_passes_in_initialized_repo() {
        let root = temp_cargo_root();

        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("git should be available in test environment")
        };

        let init = run(&["init", "-q"]);
        assert!(init.status.success(), "git init failed: {:?}", init);
        let email = run(&["config", "user.email", "test@example.com"]);
        assert!(email.status.success(), "git config failed: {:?}", email);
        let name = run(&["config", "user.name", "test"]);
        assert!(name.status.success(), "git config failed: {:?}", name);

        let tool = Tool::Exec(Exec {
            command: "git status".to_string(),
        });
        let result = tool
            .execute(&LocalRuntime::default(), &root)
            .unwrap();
        assert!(result.contains("exit code: 0"), "unexpected: {result}");
    }
}
