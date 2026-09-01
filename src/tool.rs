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
            description: "Run an allowed project development command (cargo check/build/test/clippy/fmt --check). The command runs in the project directory and inherits the current environment.",
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
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }

    /// 在指定根目录下执行（便于测试与 Agent 注入 root）。
    ///
    /// 所有副作用都经由 `rt` 完成；`root` 是路径边界。工具自身不再直接
    /// 触碰文件系统 / 进程（那些操作全部位于 [`Runtime`] 的实现中）。
    pub(crate) fn execute(&self, rt: &dyn Runtime, root: &Path) -> Result<String, ToolError> {
        match self {
            Tool::ReadFile(read_file) => read_file_within(rt, root, &read_file.path),
            Tool::WriteFile(write_file) => {
                write_file_within(rt, root, &write_file.path, &write_file.content)
            }
            Tool::Search(search) => search_within(rt, root, &search.query, search.path.as_deref()),
            Tool::EditFile(edit_file) => {
                edit_file_within(rt, root, &edit_file.path, &edit_file.old, &edit_file.new)
            }
            Tool::Exec(exec) => exec_within(rt, root, &exec.command),
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

/// 结构化的允许命令：可执行文件 + 白名单参数。
///
/// 由 [`ExecCommand::parse`] 从字符串解析得到，绝不包含 shell 元字符。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecCommand {
    /// 可执行文件名（如 `cargo`），不带路径。
    program: String,
    args: Vec<String>,
}

impl ExecCommand {
    /// 解析命令字符串，并对解析结果做参数级白名单校验。
    ///
    /// 支持两种形式：
    /// - `cargo <subcommand> [<args>...]`（白名单内）
    /// - 单个白名单命令（如 `git status`），第一版为空集
    ///
    /// 整个字符串按空白拆分（不支持引号，引号属于非法参数），
    /// 因此 `;`、`&&`、`|`、`>`、`$()` 等 shell 拼接无法混入。
    fn parse(command: &str) -> Result<Self, ToolError> {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(ToolError::CommandNotAllowed(command.to_string()));
        }

        match tokens[0] {
            "cargo" => {
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

                Ok(ExecCommand {
                    program: "cargo".to_string(),
                    args: std::iter::once(sub.to_string())
                        .chain(args)
                        .collect(),
                })
            }
            other => Err(ToolError::CommandNotAllowed(other.to_string())),
        }
    }
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

/// 在 `root` 内执行允许的开发命令，返回 `exit code` + stdout + stderr。
///
/// 安全约束：
/// - 只允许 `cargo check/build/test/clippy/fmt --check`（白名单，见 [`ExecCommand::parse`]）
/// - 不使用 shell；进程执行位于 [`Runtime::exec`]（对应 `std::process::Command` 直调）
/// - 工作目录固定为 `root`，继承当前环境变量（模型无法修改）
/// - 60 秒超时，超时返回已捕获的输出
fn exec_within(rt: &dyn Runtime, root: &Path, command: &str) -> Result<String, ToolError> {
    let parsed = ExecCommand::parse(command)?;
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
            std::env::temp_dir().join(format!("myagent-tool-test-{}-{n}", std::process::id()));
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
            .join(format!("myagent-outside-{}", std::process::id()));
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
            .join(format!("myagent-write-outside-{}", std::process::id()));
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
            .join(format!("myagent-search-outside-{}", std::process::id()));
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
            "myagent-search-walk-outside-{}",
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
            .join(format!("myagent-edit-outside-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("myagent-exec-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"myagent-exec-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\n",
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
            result.contains("myagent-exec-fixture"),
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
}
