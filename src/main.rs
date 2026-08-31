mod agent;
mod capabilities;
mod config;
mod message;
mod model;
mod nix_runtime;
mod runtime;
mod session;
mod tool;

use crate::agent::Agent;
use crate::capabilities::Capabilities;
use crate::model::OpenAICompatibleModel;
use crate::runtime::{LocalRuntime, Runtime};
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

/// 工具执行 Runtime 的选择：`MYAGENT_RUNTIME=local`（默认）/ `nix`。
///
/// 返回 `Err(String)` 表示配置无效或 nix 不可用，调用方据此清晰报错并 exit 1。
fn build_runtime() -> Result<Box<dyn Runtime>, String> {
    let value = match env::var("MYAGENT_RUNTIME") {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => "local".to_string(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err("MYAGENT_RUNTIME must be valid UTF-8".to_string());
        }
    };

    match value.as_str() {
        "local" => Ok(Box::new(LocalRuntime)),
        "nix" => match crate::nix_runtime::NixRuntime::new() {
            Ok(runtime) => Ok(Box::new(runtime)),
            Err(err) => Err(format!(
                "MYAGENT_RUNTIME=nix but nix is not available: {err}\n  install nix (https://nixos.org/download) or use MYAGENT_RUNTIME=local"
            )),
        },
        other => Err(format!(
            "unknown MYAGENT_RUNTIME value: {other:?} (expected \"local\" or \"nix\")"
        )),
    }
}

/// 只读模式：`MYAGENT_READ_ONLY` 取值为 `1` / `true`（大小写不敏感）时启用。
///
/// 其余取值（含未设置、非法取值）一律按“非只读”处理，保持默认行为零变化。
fn read_only_mode() -> bool {
    match env::var("MYAGENT_READ_ONLY") {
        Ok(value) => matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"),
        Err(_) => false,
    }
}

fn main() {
    // 先加载 .env（工作目录）；真实环境变量优先，.env 仅兜底。
    if let Err(err) = config::load_dotenv(std::path::Path::new(".env")) {
        eprintln!("warning: failed to read .env: {err}");
    }

    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("OPENAI_API_KEY is not set!");
            std::process::exit(1);
        }
    };

    let model = OpenAICompatibleModel::new(api_key);

    // Runtime 选择：MYAGENT_RUNTIME=local（默认）/ nix（exec 落在 devShell）
    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };

    // 能力选择：MYAGENT_READ_ONLY=1/true → 只读模式，否则全允许（行为零变化）。
    let read_only = read_only_mode();
    let capabilities = if read_only {
        Capabilities::read_only()
    } else {
        Capabilities::default()
    };

    let agent = match Agent::new_with_runtime_and_caps(model, runtime, capabilities) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("failed to initialize agent: {err}");
            std::process::exit(1);
        }
    };

    // 启动横幅：当前能力模式，便于用户确认（与 MYAGENT_RUNTIME 正交可组合）。
    eprintln!(
        "capabilities: {}",
        if read_only { "read-only" } else { "full" }
    );

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
