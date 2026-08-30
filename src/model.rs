use crate::message::{Message, Role, ToolCall};
use crate::tool;
use serde::{Deserialize, Serialize};
use std::env;

/// 模型回复：要么是一段文本，要么是一组工具调用请求。
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Text(String),
    ToolCall(Vec<ToolCall>),
}

/// 模型抽象：任何可对话的模型都实现此 trait
pub trait Model {
    fn complete(&self, messages: &[Message]) -> Result<Response, Box<dyn std::error::Error>>;
}

/// OpenAI 兼容 API 客户端（reqwest blocking）
pub struct OpenAICompatibleModel {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

// ---------------- API 层：消息 ----------------

/// API 层专用的消息结构，独立于核心 `Message` 类型。
///
/// 核心 `Message`（Role 枚举、`ToolCall`）与 OpenAI 兼容 API 的字符串角色
/// 是两个不同的概念，由 `from_core` 负责转换。
#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ApiToolCall>,
}

impl ApiMessage {
    fn from_core(message: &Message) -> Self {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        // 工具调用消息没有文本内容，content 用 null 表示
        let content = if message.content.is_empty() && !message.tool_calls.is_empty() {
            None
        } else {
            Some(message.content.clone())
        };

        Self {
            role: role.to_string(),
            content,
            tool_call_id: message.tool_call_id.clone(),
            tool_calls: message
                .tool_calls
                .iter()
                .map(ApiToolCall::from_core)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ApiFunctionCall,
}

impl ApiToolCall {
    fn from_core(call: &ToolCall) -> Self {
        Self {
            id: call.id.clone(),
            kind: "function".to_string(),
            function: ApiFunctionCall {
                name: call.name.clone(),
                // OpenAI 兼容 API 的 function.arguments 是 JSON 字符串，不是对象
                arguments: serde_json::to_string(&call.arguments)
                    .unwrap_or_else(|_| "{}".to_string()),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiFunctionCall {
    name: String,
    arguments: String,
}

// ---------------- API 层：请求 ----------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
}

#[derive(Debug, Serialize)]
struct ApiTool {
    #[serde(rename = "type")]
    kind: String,
    function: ApiFunction,
}

#[derive(Debug, Serialize)]
struct ApiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ---------------- API 层：响应 ----------------

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseFunction {
    name: String,
    arguments: String,
}

impl OpenAICompatibleModel {
    pub fn new(api_key: String) -> Self {
        let base_url =
            env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

        Self::new_with_base_url(api_key, base_url, model)
    }

    /// 测试友好的构造方式：显式指定 base URL / model，不读取环境变量。
    ///
    /// 仅用于集成测试指向本地 mock server；`new` 仍从环境变量读取。
    pub fn new_with_base_url(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
        }
    }
}

impl Model for OpenAICompatibleModel {
    fn complete(&self, messages: &[Message]) -> Result<Response, Box<dyn std::error::Error>> {
        let client = reqwest::blocking::Client::new();

        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages
                .iter()
                .map(ApiMessage::from_core)
                .collect(),
            tools: tool::all_definitions()
                .into_iter()
                .map(|def| ApiTool {
                    kind: "function".to_string(),
                    function: ApiFunction {
                        name: def.name.to_string(),
                        description: def.description.to_string(),
                        parameters: def.parameters,
                    },
                })
                .collect(),
        };

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(format!("API error: {status} body={body}").into());
        }

        let response: ChatResponse = response.json()?;

        let message = &response
            .choices
            .first()
            .ok_or("model returned no choices")?
            .message;

        // 有工具调用优先处理；content 为 null 时也能正确区分
        if let Some(tool_calls) = message
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
        {
            let calls = tool_calls
                .iter()
                .map(|tc| ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Null),
                })
                .collect();
            return Ok(Response::ToolCall(calls));
        }

        Ok(Response::Text(message.content.clone().unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_to_expected_shape() {
        let request = ChatRequest {
            model: "gpt-5.6-sol".to_string(),
            messages: vec![ApiMessage::from_core(&Message::user("hello"))],
            tools: vec![],
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["model"], "gpt-5.6-sol");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hello");
    }

    #[test]
    fn api_message_converts_core_message() {
        let api = ApiMessage::from_core(&Message::assistant("hi"));
        assert_eq!(api.role, "assistant");
        assert_eq!(api.content.as_deref(), Some("hi"));
        assert!(api.tool_call_id.is_none());
        assert!(api.tool_calls.is_empty());
    }

    #[test]
    fn api_message_converts_tool_result() {
        let api = ApiMessage::from_core(&Message::tool("file contents", "call_123"));
        assert_eq!(api.role, "tool");
        assert_eq!(api.content.as_deref(), Some("file contents"));
        assert_eq!(api.tool_call_id.as_deref(), Some("call_123"));
        assert!(api.tool_calls.is_empty());
    }

    #[test]
    fn api_message_converts_assistant_tool_call() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({ "path": "src/main.rs" }),
        };
        let api = ApiMessage::from_core(&Message::assistant_tool_calls(vec![call]));

        assert_eq!(api.role, "assistant");
        assert!(api.content.is_none());
        assert_eq!(api.tool_calls.len(), 1);
        assert_eq!(api.tool_calls[0].id, "call_1");
        assert_eq!(
            api.tool_calls[0].function.arguments,
            r#"{"path":"src/main.rs"}"#
        );
    }

    #[test]
    fn chat_request_serializes_tools() {
        let request = ChatRequest {
            model: "m".to_string(),
            messages: vec![],
            tools: vec![ApiTool {
                kind: "function".to_string(),
                function: ApiFunction {
                    name: "read_file".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
            }],
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn chat_response_deserializes_text() {
        let json = r#"{
            "choices": [ { "message": { "content": "hello back" } } ]
        }"#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.choices[0]
                .message
                .content
                .as_deref(),
            Some("hello back")
        );
    }

    #[test]
    fn chat_response_deserializes_tool_call() {
        let json = r#"{
            "choices": [ {
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_9",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"src/main.rs\"}"
                        }
                    }]
                }
            } ]
        }"#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        let calls = response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .unwrap();
        assert_eq!(calls[0].id, "call_9");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path": "src/main.rs"}"#);
    }

    // ---------------- mock HTTP server + 集成测试 ----------------

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 测试用临时目录（与 agent/session 测试风格一致，避免污染真实项目）。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-model-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 每个测试一个独立的 mock server：监听 127.0.0.1:0（OS 分配端口）。
    ///
    /// `handler` 接收请求 body，返回响应 body（按需区分第一次/第二次请求）。
    /// 每次请求的原始 body 通过 channel 送出，供测试断言。
    ///
    /// server 线程使用非阻塞 accept，`Drop` 时设置停止标志并 join，
    /// 避免无限循环线程导致测试挂起。
    struct MockServer {
        addr: String,
        requests: mpsc::Receiver<String>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl MockServer {
        fn start(handler: impl Fn(usize, &str) -> (u16, String) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let addr = listener.local_addr().unwrap().to_string();
            listener
                .set_nonblocking(true)
                .expect("set nonblocking");
            let (tx, rx) = mpsc::channel();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_flag = stop.clone();

            let join = thread::spawn(move || {
                let mut idx = 0usize;
                while !stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    let mut stream = match listener.accept() {
                        Ok((s, _)) => s,
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let _ = stream.set_nonblocking(false);

                    // 读取请求头 + body（Content-Length 分帧）
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut header_end = None;
                    let mut content_length = 0usize;
                    let mut sent_continue = false;

                    // 读直到拿到完整请求（头 + 指定长度 body）
                    loop {
                        let n = match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        buf.extend_from_slice(&chunk[..n]);

                        if header_end.is_none()
                            && let Some(pos) = find_header_end(&buf)
                        {
                            header_end = Some(pos);
                            content_length = parse_content_length(&buf[..pos]);

                            // reqwest 对 >1KB body 会发 `Expect: 100-continue`，
                            // 必须先回 100 才能收到 body，否则死锁。
                            let headers = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                            if headers.contains("expect: 100-continue") && !sent_continue {
                                let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
                                let _ = stream.flush();
                                sent_continue = true;
                            }
                        }

                        // 已收到完整 body
                        if let Some(pos) = header_end
                            && buf.len() >= pos + content_length
                        {
                            break;
                        }
                    }

                    // 分离 body
                    let body = if let Some(pos) = header_end {
                        let body_bytes = buf[pos..]
                            .get(..content_length.min(buf.len() - pos))
                            .unwrap_or(&buf[pos..]);
                        String::from_utf8_lossy(body_bytes).to_string()
                    } else {
                        String::new()
                    };

                    // 发送请求 body 供断言
                    let _ = tx.send(body.clone());

                    // 生成响应
                    let (status, response_body) = handler(idx, &body);
                    idx += 1;
                    let response = format!(
                        "HTTP/1.1 {status} \r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });

            Self {
                addr,
                requests: rx,
                stop,
                join: Some(join),
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// 阻塞等待第 n 次请求的 body（1-indexed）。
        fn request_body(&self, n: usize) -> String {
            self.requests
                .iter()
                .nth(n - 1)
                .unwrap_or_default()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            // 停止 server 线程并 join，避免挂起
            self.stop
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
    }

    fn parse_content_length(header: &[u8]) -> usize {
        let text = String::from_utf8_lossy(header);
        text.lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap_or(0))
            })
            .unwrap_or(0)
    }

    /// 完整 Agent Loop 用 mock server 跑通：
    ///
    /// ```text
    /// Agent::run_turn
    ///   → OpenAICompatibleModel::complete (base_url → mock)
    ///   → HTTP POST /chat/completions
    ///   → mock JSON
    ///   → Agent 解析 → 执行 → 回传 → 最终回答
    /// ```
    #[test]
    fn agent_plain_text_through_mock_server() {
        let mock = MockServer::start(|_idx, _body| {
            (
                200,
                r#"{"choices": [ { "message": { "content": "hello back" } } ]}"#.to_string(),
            )
        });
        let model = OpenAICompatibleModel::new_with_base_url(
            "test-key".to_string(),
            mock.base_url(),
            "gpt-test".to_string(),
        );
        let agent = crate::agent::Agent::new_with_root_for_test(model, temp_root());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "hi")
            .unwrap();

        // 最终回答
        assert_eq!(conversation.len(), 2);
        assert_eq!(conversation[0].content, "hi");
        assert_eq!(conversation[1].content, "hello back");

        // 请求层断言
        let body = mock.request_body(1);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["model"], "gpt-test");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hi");
        // tools 定义应随请求发送
        assert!(json["tools"].is_array());
        assert!(json["tools"][0]["type"] == "function");
    }

    /// 最重要的测试：ToolCall → 工具执行 → Tool Result 回传 → 最终回答。
    ///
    /// mock server 区分第一次/第二次请求：
    /// - 第一次：返回 tool_calls（read_file）
    /// - 第二次：返回最终文本
    ///
    /// 并验证第二次请求的 messages 包含 assistant tool_call + tool result（正确 tool_call_id）。
    #[test]
    fn agent_tool_call_through_mock_server() {
        let root = temp_root();
        std::fs::write(root.join("note.txt"), "important data").unwrap();

        let mock = MockServer::start(|idx, _body| {
            if idx == 0 {
                // 第一次：tool_call
                (
                    200,
                    r#"{"choices": [ { "message": { "content": null, "tool_calls": [{"id": "call_abc", "type": "function", "function": { "name": "read_file", "arguments": "{\"path\": \"note.txt\"}" }}] } } ]}"#
                        .to_string(),
                )
            } else {
                // 第二次：final text
                (
                    200,
                    r#"{"choices": [ { "message": { "content": "found the data" } } ]}"#
                        .to_string(),
                )
            }
        });

        let model = OpenAICompatibleModel::new_with_base_url(
            "test-key".to_string(),
            mock.base_url(),
            "gpt-test".to_string(),
        );
        let agent = crate::agent::Agent::new_with_root_for_test(model, root.clone());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "read note.txt")
            .unwrap();

        // conversation: user, assistant(tool_calls), tool(result), assistant(final)
        assert_eq!(conversation.len(), 4);
        assert_eq!(conversation[3].content, "found the data");

        // 第二次请求的 messages 必须包含 assistant tool_call + tool result
        let second = mock.request_body(2);
        let json: serde_json::Value = serde_json::from_str(&second).unwrap();
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        // assistant tool_call
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_abc");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"note.txt"}"#
        );

        // tool result
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_abc");
        assert_eq!(messages[2]["content"], "important data");
    }

    /// API 错误（HTTP 500）必须传播为 `AgentError::Model`，不吞掉。
    #[test]
    fn agent_api_error_propagates_from_mock_server() {
        let mock = MockServer::start(|_idx, _body| (500, r#"{"error": "boom"}"#.to_string()));
        let model = OpenAICompatibleModel::new_with_base_url(
            "test-key".to_string(),
            mock.base_url(),
            "gpt-test".to_string(),
        );
        let agent = crate::agent::Agent::new_with_root_for_test(model, temp_root());

        let mut conversation = Vec::new();
        let result = agent.run_turn(&mut conversation, "hi");

        assert!(matches!(result, Err(crate::agent::AgentError::Model(_))));
        // 错误后 conversation 回滚：user 消息被移除
        assert!(conversation.is_empty());
    }
}
