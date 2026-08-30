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
}
