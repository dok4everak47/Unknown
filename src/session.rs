use crate::message::Message;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// 磁盘上持久化的 conversation 文件格式。
///
/// 第一版只保存消息列表，不引入任何 session 元数据 / 索引 / 数据库。
#[derive(Debug, Serialize, Deserialize)]
struct SessionFile {
    messages: Vec<Message>,
}

/// conversation 持久化的最小接口。
///
/// 只负责 `Vec<Message>` 与磁盘 JSON 之间的转换；`Agent` 不感知
/// 文件路径、JSON 等细节。
pub struct Session;

/// 加载 / 保存失败的错误。
#[derive(Debug)]
pub enum SessionError {
    /// 文件存在但无法解析为合法的 conversation JSON。
    Corrupt(String),
    /// 读取 / 写入失败。
    Io(io::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Corrupt(path) => {
                write!(f, "session file is corrupt (invalid JSON): {path}")
            }
            SessionError::Io(err) => write!(f, "session io error: {err}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Io(err) => Some(err),
            SessionError::Corrupt(_) => None,
        }
    }
}

impl Session {
    /// 从 `path` 加载 conversation。
    ///
    /// - 文件不存在：返回空 conversation（不是错误）
    /// - 文件存在但 JSON 损坏：返回 [`SessionError::Corrupt`]，**不**静默覆盖
    pub fn load(path: &Path) -> Result<Vec<Message>, SessionError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(SessionError::Io(err)),
        };

        let text = String::from_utf8(bytes)
            .map_err(|_| SessionError::Corrupt(path.display().to_string()))?;
        let file: SessionFile = serde_json::from_str(&text)
            .map_err(|_| SessionError::Corrupt(path.display().to_string()))?;
        Ok(file.messages)
    }

    /// 将 conversation 保存到 `path`（JSON，可读）。
    pub fn save(path: &Path, conversation: &[Message]) -> Result<(), SessionError> {
        let file = SessionFile {
            messages: conversation.to_vec(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|err| SessionError::Io(io::Error::new(io::ErrorKind::InvalidData, err)))?;
        fs::write(path, json).map_err(SessionError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_path(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-session-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// 构造一个含多种消息类型的多轮 conversation。
    fn sample_conversation() -> Vec<Message> {
        vec![
            Message::user("hello"),
            Message::assistant("hi there"),
            Message::assistant_tool_calls(vec![crate::message::ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: json!({ "path": "src/main.rs" }),
            }]),
            Message::tool("fn main() {}", "call_1"),
            Message::user("now write it"),
            Message::assistant("done"),
        ]
    }

    #[test]
    fn message_serializes_and_deserializes() {
        let msg = Message::assistant_tool_calls(vec![crate::message::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: json!({ "path": "a.rs" }),
        }]);
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn save_then_load_roundtrips_conversation() {
        let path = temp_path("roundtrip.json");
        let conversation = sample_conversation();

        Session::save(&path, &conversation).unwrap();
        let loaded = Session::load(&path).unwrap();

        assert_eq!(conversation, loaded);
    }

    #[test]
    fn roundtrip_all_message_variants() {
        let path = temp_path("variants.json");
        let conversation = vec![
            Message::user("u"),
            Message::assistant("a"),
            Message::assistant_tool_calls(vec![
                crate::message::ToolCall {
                    id: "c1".to_string(),
                    name: "write_file".to_string(),
                    arguments: json!({ "path": "x.txt", "content": "y" }),
                },
                crate::message::ToolCall {
                    id: "c2".to_string(),
                    name: "search".to_string(),
                    arguments: json!({ "query": "foo", "path": "src" }),
                },
            ]),
            Message::tool("result with \"quotes\" and \n newline", "c1"),
            Message::tool("", "c2"),
        ];

        Session::save(&path, &conversation).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(conversation, loaded);
    }

    #[test]
    fn save_then_load_empty_conversation() {
        let path = temp_path("empty.json");
        let empty: Vec<Message> = Vec::new();

        Session::save(&path, &empty).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_missing_file_returns_empty_conversation() {
        let path = temp_path("missing.json");
        let loaded = Session::load(&path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_corrupt_json_returns_error() {
        let path = temp_path("corrupt.json");
        fs::write(&path, "{ not valid json !!").unwrap();

        assert!(matches!(
            Session::load(&path),
            Err(SessionError::Corrupt(_))
        ));
    }

    #[test]
    fn load_corrupt_utf8_returns_error() {
        let path = temp_path("corrupt_utf8.json");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();

        assert!(matches!(
            Session::load(&path),
            Err(SessionError::Corrupt(_))
        ));
    }

    #[test]
    fn save_to_unwritable_path_returns_error() {
        // 指向一个不存在目录下的文件
        let path = temp_path("no/such/dir/session.json");
        assert!(matches!(
            Session::save(&path, &[]),
            Err(SessionError::Io(_))
        ));
    }

    #[test]
    fn saved_file_is_readable_json_with_messages_key() {
        let path = temp_path("readable.json");
        let conversation = vec![Message::user("hi")];

        Session::save(&path, &conversation).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][0]["content"], "hi");
    }

    #[test]
    fn corrupt_file_is_not_overwritten_by_load() {
        let path = temp_path("preserve.json");
        let original = "{ broken json";
        fs::write(&path, original).unwrap();

        // load 只读不写：失败后文件内容必须保持原样
        let _ = Session::load(&path);
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, original);
    }
}
