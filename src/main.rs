mod agent;
mod capabilities;
mod config;
mod message;
mod model;
mod nix_runtime;
mod runtime;
mod sandbox;
mod session;
mod tool;

use crate::agent::Agent;
use crate::capabilities::Capabilities;
use crate::model::{ModelEvent, OpenAICompatibleModel};
use crate::runtime::{LocalRuntime, Runtime, RuntimeConfig};
use crate::session::Session;

use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

/// 默认会话文件路径；可用 `MYAGENT_SESSION` 环境变量覆盖。
fn session_path() -> PathBuf {
    env::var("MYAGENT_SESSION")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("session.json"))
}

/// 工具执行 Runtime 的选择：`MYAGENT_RUNTIME=local`（默认）/ `nix`。
///
/// 返回 `Err(String)` 表示配置无效或 nix 不可用，调用方据此清晰报错并 exit 1。
/// `config` 携带执行参数（当前只有 exec 超时），传给选中的 Runtime。
fn build_runtime(config: RuntimeConfig) -> Result<Box<dyn Runtime>, String> {
    let value = match env::var("MYAGENT_RUNTIME") {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => "local".to_string(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err("MYAGENT_RUNTIME must be valid UTF-8".to_string());
        }
    };

    match value.as_str() {
        "local" => Ok(Box::new(LocalRuntime::new(config))),
        "nix" => match crate::nix_runtime::NixRuntime::new(config) {
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

/// 解析 `MYAGENT_EXEC_TIMEOUT_SECS`：正整数秒 → `Ok`；0、非数字、溢出 → `Err`。
///
/// 纯函数（不读 env、无副作用），便于单元测试；调用方对 `Err` 打印清晰错误
/// 并 exit 1（与 nix 不可用的处理风格一致）。
fn parse_exec_timeout(value: &str) -> Result<Duration, String> {
    let secs: u64 = value
        .trim()
        .parse()
        .map_err(|_| format!("{value:?} is not a positive integer number of seconds"))?;
    if secs == 0 {
        return Err(format!(
            "{value:?} is 0; exec timeout must be a positive number of seconds"
        ));
    }
    Ok(Duration::from_secs(secs))
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

/// 沙箱开关：`MYAGENT_SANDBOX` 取值为 `1` / `true`（大小写不敏感）时启用，
/// **默认关**。与 `MYAGENT_RUNTIME`、`MYAGENT_READ_ONLY` 正交可组合。
fn sandbox_mode() -> bool {
    match env::var("MYAGENT_SANDBOX") {
        Ok(value) => matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"),
        Err(_) => false,
    }
}

/// 沙箱内放开网络：`MYAGENT_SANDBOX_NETWORK` 取值为 `1` / `true` 时启用，
/// **默认关**，不随 `MYAGENT_SANDBOX=1` 隐式开启（用户侧显式 opt-in）。
fn sandbox_network() -> bool {
    match env::var("MYAGENT_SANDBOX_NETWORK") {
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

    // exec 超时配置：MYAGENT_EXEC_TIMEOUT_SECS（秒）；未设置 → 默认 60s。
    // 设置了但非法（0 / 非数字 / 溢出）→ 清晰报错并退出（与 nix 不可用一致）。
    // 启动横幅不打印超时（避免噪声）。
    let config = match env::var("MYAGENT_EXEC_TIMEOUT_SECS") {
        Ok(value) => match parse_exec_timeout(&value) {
            Ok(exec_timeout) => RuntimeConfig { exec_timeout },
            Err(msg) => {
                eprintln!("MYAGENT_EXEC_TIMEOUT_SECS is invalid: {msg}");
                std::process::exit(1);
            }
        },
        Err(_) => RuntimeConfig::default(),
    };

    // Runtime 选择：MYAGENT_RUNTIME=local（默认）/ nix（exec 落在 devShell）
    let runtime = match build_runtime(config.clone()) {
        Ok(runtime) => runtime,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };

    // Sandbox 装饰（在 runtime 选择之后、最外层包装）：MYAGENT_SANDBOX=1/true
    // 时，exec 经 sandbox-exec 放进 Seatbelt 沙箱；文件操作仍委托内层 runtime。
    // 启用时若非 macOS 或 sandbox-exec 不可用 → 构造失败、清晰报错并退出，
    // 绝不静默降级为不隔离。与 Capabilities（MYAGENT_READ_ONLY）正交。
    let sandbox = sandbox_mode();
    let runtime = if sandbox {
        let network = sandbox_network();
        let root = match std::env::current_dir() {
            Ok(root) => root,
            Err(err) => {
                eprintln!("failed to resolve working directory: {err}");
                std::process::exit(1);
            }
        };
        match crate::sandbox::SandboxedRuntime::new(&root, network, config.clone(), runtime) {
            Ok(runtime) => Box::new(runtime),
            Err(err) => {
                eprintln!(
                    "MYAGENT_SANDBOX=1 but failed to enable sandbox: {err}\n  Seatbelt sandbox requires macOS with /usr/bin/sandbox-exec"
                );
                std::process::exit(1);
            }
        }
    } else {
        runtime
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

    // 启动横幅：沙箱实际状态（启用时打印；与 MYAGENT_READ_ONLY 正交可组合）。
    if sandbox {
        eprintln!(
            "sandbox: on (network: {})",
            if sandbox_network() { "ON" } else { "off" }
        );
    }

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

        // 流式：收到第一个 TextDelta 时惰性打印 "AI: " 前缀，随后逐段输出并 flush；
        // 工具轮次期间（模型只发工具调用、无文本）不打印任何内容。
        let mut prefix_printed = false;
        let mut printed_any = false;
        let result = agent.run_turn_streaming(&mut conversation, text, &mut |event| {
            let ModelEvent::TextDelta(delta) = event;
            if !prefix_printed {
                print!("AI: ");
                prefix_printed = true;
            }
            print!("{delta}");
            printed_any = true;
            io::stdout()
                .flush()
                .expect("failed to flush stdout");
        });

        match result {
            Ok(()) => {
                // 有增量输出则补换行（避免覆盖行内打字效果）
                if printed_any {
                    println!();
                }
                // 每轮成功完成后保存；Model error 时 run_turn_streaming 已回滚，不保存半成品
                if let Err(err) = Session::save(&path, &conversation) {
                    eprintln!("failed to save session: {err}");
                }
            }
            Err(err) => {
                // 已打印部分内容时先换行，避免错误信息粘在残句上
                if printed_any {
                    println!();
                }
                eprintln!("error: {err}");
                let mut source = std::error::Error::source(&err);
                while let Some(cause) = source {
                    eprintln!("  caused by: {cause}");
                    source = cause.source();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_exec_timeout;
    use std::time::Duration;

    /// 正整数秒 → `Ok`。
    #[test]
    fn parse_valid_positive_seconds() {
        assert_eq!(parse_exec_timeout("30"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_exec_timeout("1"), Ok(Duration::from_secs(1)));
        assert_eq!(parse_exec_timeout("3600"), Ok(Duration::from_secs(3600)));
    }

    /// 0、空串、非数字、溢出 → `Err`（清晰错误信息）。
    #[test]
    fn parse_invalid_values_are_rejected() {
        assert!(parse_exec_timeout("0").is_err());
        assert!(parse_exec_timeout("").is_err());
        assert!(parse_exec_timeout("abc").is_err());
        assert!(parse_exec_timeout("-5").is_err());
        assert!(parse_exec_timeout("1.5").is_err());
        // 溢出 u64 → 解析失败
        assert!(parse_exec_timeout("99999999999999999999999999").is_err());
    }
}
