mod agent;
mod message;
mod model;
mod tool;

use crate::agent::Agent;
use crate::model::OpenAICompatibleModel;
use message::Message;

use std::env;
use std::io::{self, BufRead, Write};

fn main() {
    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("OPENAI_API_KEY is not set!");
            std::process::exit(1);
        }
    };

    let model = OpenAICompatibleModel::new(api_key);
    let agent = match Agent::new(model) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("failed to initialize agent: {err}");
            std::process::exit(1);
        }
    };

    // 完整对话历史，每轮都会带着之前的所有消息重新请求
    let mut conversation: Vec<Message> = Vec::new();

    let stdin = io::stdin();
    let mut input = stdin.lock().lines();

    loop {
        print!("You: ");
        io::stdout()
            .flush()
            .expect("failed to flush stdout");

        let Some(Ok(text)) = input.next() else {
            // EOF（如 Ctrl-D）：正常退出
            break;
        };

        let text = text.trim();
        if text == "/exit" {
            break;
        }
        if text.is_empty() {
            continue;
        }

        match agent.run_turn(&mut conversation, text) {
            Ok(()) => {
                // run_turn 成功时最后一条消息是模型的文本回答
                if let Some(Message {
                    role: message::Role::Assistant,
                    content,
                    ..
                }) = conversation.last()
                {
                    println!("AI: {content}");
                }
            }
            Err(err) => eprintln!("error: {err}"),
        }
    }
}
