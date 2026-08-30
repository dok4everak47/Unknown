use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

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
    vec![ToolDefinition {
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
    }]
}

/// 工具执行或解析时的错误。
#[derive(Debug)]
pub enum ToolError {
    UnknownTool(String),
    InvalidArguments(String),
    NotFound(String),
    OutsideProject(String),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFile {
    pub path: String,
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
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }

    /// 在当前工作目录下执行工具。
    pub fn execute(&self) -> Result<String, ToolError> {
        let root = std::env::current_dir().map_err(ToolError::Io)?;
        self.execute_in(&root)
    }

    /// 在指定根目录下执行（便于测试）。
    fn execute_in(&self, root: &Path) -> Result<String, ToolError> {
        match self {
            Tool::ReadFile(read_file) => read_file_within(root, &read_file.path),
        }
    }
}

/// 读取文件，但限制路径必须位于 `root` 之内。
///
/// 拒绝绝对路径、`..` 跳转，以及通过 symlink 逃逸出 `root` 的路径。
fn read_file_within(root: &Path, path: &str) -> Result<String, ToolError> {
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
    let resolved = root
        .join(&candidate)
        .canonicalize()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => ToolError::NotFound(path.to_string()),
            _ => ToolError::Io(err),
        })?;

    if !resolved.starts_with(&root) {
        return Err(ToolError::OutsideProject(path.to_string()));
    }

    fs::read_to_string(&resolved).map_err(ToolError::Io)
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
        assert_eq!(tool.execute_in(&root).unwrap(), "fn main() {}");
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
        assert_eq!(tool.execute_in(&root).unwrap(), "pub fn f() {}");
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_root();
        let tool = Tool::ReadFile(ReadFile {
            path: "../../etc/passwd".to_string(),
        });
        assert!(matches!(
            tool.execute_in(&root),
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
            tool.execute_in(&root),
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
        let result = tool.execute_in(&root);

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
            tool.execute_in(&root),
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
            Tool::from_call("write_file", &args),
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
}
