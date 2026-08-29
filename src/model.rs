use crate::prompt::Prompt;
use serde::{Deserialize, Serialize};

/// 模型回复
pub struct Response {
    pub text: String,
}

/// 模型抽象：任何可对话的模型都实现此 trait
pub trait Model {
    fn complete(&self, prompt: &Prompt) -> Result<Response, Box<dyn std::error::Error>>;
}

/// OpenAI 兼容 API 客户端（reqwest blocking）
pub struct OpenAICompatibleModel {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

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
    content: String,
}

impl OpenAICompatibleModel {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.commandcode.ai/provider/v1".to_string(),
            model: "gpt-5.6-sol".to_string(),
        }
    }
}

impl Model for OpenAICompatibleModel {
    fn complete(&self, prompt: &Prompt) -> Result<Response, Box<dyn std::error::Error>> {
        let client = reqwest::blocking::Client::new();

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt.text.clone(),
            }],
        };

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(format!("API error: {status} {body}").into());
        }

        let response: ChatResponse = response.json()?;

        let text = response
            .choices
            .first()
            .ok_or("model returned no choices")?
            .message
            .content
            .clone();

        Ok(Response { text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_to_expected_shape() {
        let request = ChatRequest {
            model: "gpt-5.6-sol".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["model"], "gpt-5.6-sol");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hello");
    }

    #[test]
    fn chat_response_deserializes_from_api_payload() {
        let json = r#"{
            "choices": [
                { "message": { "content": "hello back" } }
            ]
        }"#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.choices[0].message.content, "hello back");
    }
}
