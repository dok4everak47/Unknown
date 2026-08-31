mod agent;
mod message;
mod model;
mod runtime;
mod session;
mod tool;

use crate::agent::Agent;
use crate::model::OpenAICompatibleModel;
use crate::session::Session;
use message::Message;

use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

/// 默认会话文件路径；可用 `MYAGENT_SESSION` 环境变量覆盖。
fn session_path() -> PathBuf {
    env::var("MYAGENT_SESSION")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("session.json"))
}

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

    // 启动时恢复已有 conversation（文件不存在则从空对话开始）
    let path = session_path();
    let mut conversation = match Session::load(&path) {
        Ok(conversation) => conversation,
        Err(err) => {
            eprintln!("failed to load session: {err}");
            std::process::exit(1);
        }
    };

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
                // 每轮成功完成后保存；Model error 时 run_turn 已回滚，不保存半成品
                if let Err(err) = Session::save(&path, &conversation) {
                    eprintln!("failed to save session: {err}");
                }
            }
            Err(err) => eprintln!("error: {err}"),
        }
    }
}
