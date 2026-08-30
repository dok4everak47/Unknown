mod message;
mod model;
mod tool;

use crate::model::{Model, OpenAICompatibleModel, Response};
use crate::tool::Tool;
use message::Message;

use std::env;
use std::io::{self, BufRead, Write};

/// 单次用户输入最多允许的模型↔工具交互轮数，防止模型陷入循环。
const MAX_TOOL_ROUNDS: usize = 8;

fn main() {
    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("OPENAI_API_KEY is not set!");
            std::process::exit(1);
        }
    };

    let model = OpenAICompatibleModel::new(api_key);

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

        // 记录本回合开始的位置，出错时回滚到此处
        let turn_start = conversation.len();
        conversation.push(Message::user(text));

        let mut tool_rounds = 0;

        // 一次用户输入可能触发多轮 model ↔ tool 交互，直到模型给出文本回答
        loop {
            match model.complete(&conversation) {
                Ok(Response::Text(answer)) => {
                    println!("AI: {answer}");
                    conversation.push(Message::assistant(answer));
                    break;
                }
                Ok(Response::ToolCall(calls)) => {
                    tool_rounds += 1;
                    if tool_rounds > MAX_TOOL_ROUNDS {
                        eprintln!("error: too many tool-call rounds, giving up");
                        conversation.truncate(turn_start);
                        break;
                    }

                    conversation.push(Message::assistant_tool_calls(calls.clone()));
                    for call in calls {
                        println!("[tool] {} {}", call.name, call.arguments);
                        let result = match Tool::from_call(&call.name, &call.arguments) {
                            Ok(tool) => match tool.execute() {
                                Ok(text) => text,
                                Err(err) => format!("tool error: {err}"),
                            },
                            Err(err) => format!("tool error: {err}"),
                        };
                        conversation.push(Message::tool(result, call.id));
                    }
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    // 丢弃本回合产生的消息，保持对话状态一致
                    conversation.truncate(turn_start);
                    break;
                }
            }
        }
    }
}
