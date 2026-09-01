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
mod ui;

use crate::agent::Agent;
use crate::capabilities::Capabilities;
use crate::message::Message;
use crate::model::{Model, ModelEvent, OpenAICompatibleModel};
use crate::runtime::{LocalRuntime, Runtime, RuntimeConfig};
use crate::session::Session;
use crate::ui::{Ui, color_enabled};

use rustyline::error::ReadlineError;
use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 默认会话文件路径；可用 `MYAGENT_SESSION` 环境变量覆盖。
fn session_path() -> PathBuf {
    env::var("MYAGENT_SESSION")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("session.json"))
}

/// 解析历史文件路径（纯函数，便于单测）：未设置 / 空串 → 默认
/// `<cwd>/.myagent_history`；否则使用给定路径。风格对齐 [`session_path`]。
fn history_path_from(value: Option<String>) -> PathBuf {
    match value {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from(".myagent_history"),
    }
}

/// 默认历史文件路径；可用 `MYAGENT_HISTORY` 环境变量覆盖。
fn history_path() -> PathBuf {
    history_path_from(env::var("MYAGENT_HISTORY").ok())
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

/// 推理过程显示开关：`MYAGENT_SHOW_REASONING` 取值为 `1` / `true`（大小写不敏感）
/// 时启用，**默认关**。开启时流式响应中模型的 `reasoning_content` 以暗色 💭
/// 前缀实时显示（仅"路过"展示，绝不进入对话历史 / conversation）。
fn show_reasoning_mode() -> bool {
    show_reasoning_from(env::var("MYAGENT_SHOW_REASONING").ok())
}

/// 解析 `MYAGENT_SHOW_REASONING`（纯函数，便于单测）：`1` / `true`（大小写
/// 不敏感）→ `true`；其余取值 / 未设置 → `false`。风格对齐
/// `read_only_mode` / `sandbox_mode`，默认关、行为零变化。
fn show_reasoning_from(value: Option<String>) -> bool {
    match value {
        Some(value) => matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"),
        None => false,
    }
}

/// 为一个输出流（stdout / stderr）计算着色开关（其各自的 `is_terminal()`）。
///
/// 须在 `load_dotenv` **之后**调用，使 `.env` 中的 `NO_COLOR` /
/// `MYAGENT_NO_COLOR` 生效。风格对齐 `sandbox_mode()` / `show_reasoning_mode()`。
fn build_ui(stream_is_terminal: bool) -> Ui {
    Ui::new(color_enabled(
        stream_is_terminal,
        env::var("NO_COLOR").ok().as_deref(),
        env::var("MYAGENT_NO_COLOR").ok().as_deref(),
    ))
}

/// 处理一轮用户输入（tty / 非 tty 两条输入路径共用）：流式输出、错误链打印、
/// 成功后保存 session。逻辑与原 turn 处理实现逐字一致；`ui_stdout` / `ui_stderr`
/// 为着色开关（stdout 管 AI/You/推理，stderr 管工具/错误/warning），禁用时
/// 输出与纯文本逐字一致。
fn run_turn<M: Model>(
    agent: &Agent<M>,
    conversation: &mut Vec<Message>,
    path: &Path,
    text: &str,
    ui_stdout: &Ui,
    ui_stderr: &Ui,
) {
    // 推理过程显示开关：MYAGENT_SHOW_REASONING=1/true 时，模型 reasoning_content
    // 以暗色 💭 前缀实时显示（仅"路过"展示，不进对话历史）；默认关，行为零变化。
    let show_reasoning = show_reasoning_mode();
    // 流式：收到第一个 TextDelta 时惰性打印 "AI: " 前缀，随后逐段输出并 flush；
    // 工具轮次期间（模型只发工具调用、无文本）不打印任何内容。
    let mut prefix_printed = false;
    // 是否正处于暗色推理段（开场转义已打印、尚未复位）。
    let mut reasoning_active = false;
    let mut printed_any = false;
    let result = agent.run_turn_streaming(
        conversation,
        text,
        &mut |event| {
            match event {
                ModelEvent::TextDelta(delta) => {
                    // 首个正式文本到来：若仍在暗色推理段，先复位 ANSI 并换行，
                    // 再打印正式回答前缀（暗色只包裹推理段，回答恢复常规样式）。
                    if reasoning_active {
                        println!("{}", ui_stdout.reset());
                        reasoning_active = false;
                    }
                    if !prefix_printed {
                        // 只给 "AI: " 标签着色，回答正文永远不着色。
                        print!("{}", ui_stdout.cyan_bold("AI: "));
                        prefix_printed = true;
                    }
                    print!("{delta}");
                    printed_any = true;
                    io::stdout()
                        .flush()
                        .expect("failed to flush stdout");
                }
                ModelEvent::ReasoningDelta(delta) => {
                    // 仅开关打开时显示；开关关闭时直接忽略（与现状一致）。
                    if !show_reasoning || delta.is_empty() {
                        return;
                    }
                    // 首个推理 chunk 惰性打印暗色（dim + italic）前缀 💭，
                    // 随后逐段输出并 flush；开场转义不含 reset，deltas 延续暗色。
                    if !reasoning_active {
                        print!("{}💭 ", ui_stdout.reasoning_open());
                        reasoning_active = true;
                    }
                    print!("{delta}");
                    printed_any = true;
                    io::stdout()
                        .flush()
                        .expect("failed to flush stdout");
                }
            }
        },
        ui_stderr,
    );

    match result {
        Ok(()) => {
            // 有增量输出则补换行（避免覆盖行内打字效果）；若本轮以推理段结束
            // （未迎来正式文本），先复位 ANSI 再换行，避免残色 / 粘连。
            if reasoning_active {
                println!("{}", ui_stdout.reset());
            } else if printed_any {
                println!();
            }
            // 每轮成功完成后保存；Model error 时 run_turn_streaming 已回滚，不保存半成品
            if let Err(err) = Session::save(path, conversation) {
                eprintln!(
                    "{}",
                    ui_stderr.yellow(&format!("failed to save session: {err}"))
                );
            }
        }
        Err(err) => {
            // 已打印部分内容时先换行，避免错误信息粘在残句上；推理段同样先复位。
            if reasoning_active {
                println!("{}", ui_stdout.reset());
            } else if printed_any {
                println!();
            }
            eprintln!("{} {err}", ui_stderr.red_bold("error:"));
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                eprintln!("{}", ui_stderr.dim(&format!("  caused by: {cause}")));
                source = cause.source();
            }
        }
    }
}

/// tty 交互模式：rustyline 行编辑。
///
/// - Ctrl+L 清屏、↑↓ 浏览历史、←→ 移动光标、行内编辑（默认绑定，无需自定义）；
/// - 每行成功读取后非空行加入历史，退出时保存到 `history_path`；
/// - Ctrl+C（Interrupted）放弃当前行回到新提示符（bash 语义，不退出进程）；
/// - Ctrl+D（Eof）退出循环；`/exit` 退出；空行跳过。
fn interactive_loop<M: Model>(
    agent: &Agent<M>,
    conversation: &mut Vec<Message>,
    path: &Path,
    history_path: &Path,
    ui_stdout: &Ui,
    ui_stderr: &Ui,
) {
    let mut editor = match rustyline::DefaultEditor::new() {
        Ok(editor) => editor,
        Err(err) => {
            eprintln!(
                "{}",
                ui_stderr.red(&format!("failed to initialize line editor: {err}"))
            );
            std::process::exit(1);
        }
    };

    // 启动时加载历史；文件不存在不算错误，其余失败仅告警不中断会话。
    if let Err(err) = editor.load_history(history_path)
        && !matches!(&err, ReadlineError::Io(e) if e.kind() == io::ErrorKind::NotFound)
    {
        eprintln!(
            "{} failed to load history: {err}",
            ui_stderr.yellow("warning:")
        );
    }

    // 着色提示符：rustyline 15 计算提示符宽度时会跳过 ANSI 转义序列
    // （src/tty/mod.rs::width 对转义返回 0 宽），并把提示符原样写入终端，
    // 故着色不会干扰光标宽度计算（长行 / ←→ / Ctrl+L / 历史均不错位）。
    // 禁用时返回纯文本 "You: "。
    let prompt = ui_stdout.green_bold("You: ");
    loop {
        match editor.readline(&prompt) {
            Ok(line) => {
                let text = line.trim();
                if text == "/exit" {
                    break;
                }
                if text.is_empty() {
                    continue;
                }
                // 非空行加入历史（只存用户输入文本，不存 AI 输出）
                if let Err(err) = editor.add_history_entry(line.as_str()) {
                    eprintln!("warning: failed to add history entry: {err}");
                }
                run_turn(agent, conversation, path, text, ui_stdout, ui_stderr);
            }
            // Ctrl+C：放弃当前行，回到新提示符，不退出进程
            Err(ReadlineError::Interrupted) => {
                println!();
                continue;
            }
            // Ctrl+D：退出循环（与 EOF 退出一致）
            Err(ReadlineError::Eof) => break,
            Err(err) => {
                eprintln!("{}", ui_stderr.red(&format!("readline error: {err}")));
                break;
            }
        }
    }

    // 退出时保存历史；失败仅告警，不中断会话。
    if let Err(err) = editor.save_history(history_path) {
        eprintln!(
            "{} failed to save history: {err}",
            ui_stderr.yellow("warning:")
        );
    }
}

/// 非 tty 模式（管道输入 / `</dev/null` / 脚本）：保持原有 `BufRead::lines()`
/// 循环，不构造 rustyline Editor，行为与以前一致。
fn noninteractive_loop<M: Model>(
    agent: &Agent<M>,
    conversation: &mut Vec<Message>,
    path: &Path,
    ui_stdout: &Ui,
    ui_stderr: &Ui,
) {
    let stdin = io::stdin();
    let mut input = stdin.lock().lines();

    // 非 tty 路径：io_is_terminal=false → color_enabled 恒 false，提示符必为纯文本；
    // ui_stderr 传 run_turn（其中 stderr 样式同样保持禁用），此处仅需保持签名对齐。
    loop {
        print!("{}", ui_stdout.green_bold("You: "));
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

        run_turn(agent, conversation, path, text, ui_stdout, ui_stderr);
    }
}

fn main() {
    // 先加载 .env（工作目录）；真实环境变量优先，.env 仅兜底。
    // .env 可能声明 NO_COLOR / MYAGENT_NO_COLOR，故着色开关须在加载之后计算。
    if let Err(err) = config::load_dotenv(std::path::Path::new(".env")) {
        // .env 读取失败时未注入任何变量，此处的 stderr UI 仅基于真实环境变量。
        let stderr_ui = build_ui(io::stderr().is_terminal());
        eprintln!(
            "{} failed to read .env: {err}",
            stderr_ui.yellow("warning:")
        );
    }

    // 着色开关：stdout（AI/You/推理）与 stderr（工具/横幅/错误/warning）各判断一次。
    let ui_stdout = build_ui(io::stdout().is_terminal());
    let ui_stderr = build_ui(io::stderr().is_terminal());

    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("{}", ui_stderr.red("OPENAI_API_KEY is not set!"));
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
                eprintln!(
                    "{}",
                    ui_stderr.red(&format!("MYAGENT_EXEC_TIMEOUT_SECS is invalid: {msg}"))
                );
                std::process::exit(1);
            }
        },
        Err(_) => RuntimeConfig::default(),
    };

    // Runtime 选择：MYAGENT_RUNTIME=local（默认）/ nix（exec 落在 devShell）
    let runtime = match build_runtime(config.clone()) {
        Ok(runtime) => runtime,
        Err(msg) => {
            eprintln!("{}", ui_stderr.red(&msg));
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
                eprintln!(
                    "{}",
                    ui_stderr.red(&format!("failed to resolve working directory: {err}"))
                );
                std::process::exit(1);
            }
        };
        match crate::sandbox::SandboxedRuntime::new(&root, network, config.clone(), runtime) {
            Ok(runtime) => Box::new(runtime),
            Err(err) => {
                eprintln!("{}", ui_stderr.red(&format!(
                    "MYAGENT_SANDBOX=1 but failed to enable sandbox: {err}\n  Seatbelt sandbox requires macOS with /usr/bin/sandbox-exec"
                )));
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
            eprintln!(
                "{}",
                ui_stderr.red(&format!("failed to initialize agent: {err}"))
            );
            std::process::exit(1);
        }
    };

    // 启动横幅：当前能力模式（整条 dim；read-only 状态值黄色提示，保持克制）。
    let capabilities_line = if read_only {
        format!("capabilities: {}", ui_stderr.yellow("read-only"))
    } else {
        "capabilities: full".to_string()
    };
    eprintln!("{}", ui_stderr.dim(&capabilities_line));

    // 启动横幅：沙箱实际状态（整条 dim；network off 黄色提示，保持克制）。
    if sandbox {
        let network = if sandbox_network() {
            "ON".to_string()
        } else {
            ui_stderr.yellow("off")
        };
        eprintln!(
            "{}",
            ui_stderr.dim(&format!("sandbox: on (network: {network})"))
        );
    }

    // 启动时恢复已有 conversation（文件不存在则从空对话开始）
    let path = session_path();
    let mut conversation = match Session::load(&path) {
        Ok(conversation) => conversation,
        Err(err) => {
            eprintln!(
                "{}",
                ui_stderr.red(&format!("failed to load session: {err}"))
            );
            std::process::exit(1);
        }
    };

    // 输入路径：tty 下用 rustyline 行编辑（Ctrl+L 清屏、↑↓ 历史、行内编辑）；
    // 非 tty（管道 / </dev/null / 脚本）走原有 BufRead::lines() 循环，行为不变。
    let history = history_path();
    if io::stdin().is_terminal() {
        interactive_loop(
            &agent,
            &mut conversation,
            &path,
            &history,
            &ui_stdout,
            &ui_stderr,
        );
    } else {
        noninteractive_loop(&agent, &mut conversation, &path, &ui_stdout, &ui_stderr);
    }
}

#[cfg(test)]
mod tests {
    use super::{history_path_from, parse_exec_timeout, show_reasoning_from};
    use std::path::PathBuf;
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

    /// MYAGENT_SHOW_REASONING 开关解析（纯函数）：`1` / `true`（大小写不敏感）→ true。
    #[test]
    fn show_reasoning_flag_parses_truthy_values() {
        assert!(show_reasoning_from(Some("1".to_string())));
        assert!(show_reasoning_from(Some("true".to_string())));
        assert!(show_reasoning_from(Some("TRUE".to_string())));
        assert!(show_reasoning_from(Some(" True ".to_string())));
    }

    /// MYAGENT_SHOW_REASONING 开关解析（纯函数）：其余取值 / 未设置 → false
    /// （默认关，行为零变化）。
    #[test]
    fn show_reasoning_flag_defaults_to_off() {
        assert!(!show_reasoning_from(None));
        assert!(!show_reasoning_from(Some("0".to_string())));
        assert!(!show_reasoning_from(Some("false".to_string())));
        assert!(!show_reasoning_from(Some("yes".to_string())));
        assert!(!show_reasoning_from(Some(String::new())));
    }

    /// MYAGENT_HISTORY 未设置 → 默认 `<cwd>/.myagent_history`（相对路径）。
    #[test]
    fn history_path_defaults_to_cwd() {
        assert_eq!(history_path_from(None), PathBuf::from(".myagent_history"));
    }

    /// MYAGENT_HISTORY 设置 → 使用该路径。
    #[test]
    fn history_path_uses_env_value() {
        assert_eq!(
            history_path_from(Some("/tmp/x".to_string())),
            PathBuf::from("/tmp/x")
        );
    }

    /// 空串 / 全空白按未设置处理 → 默认路径（固定行为）。
    #[test]
    fn history_path_empty_env_value_uses_default() {
        assert_eq!(
            history_path_from(Some(String::new())),
            PathBuf::from(".myagent_history")
        );
        assert_eq!(
            history_path_from(Some("   ".to_string())),
            PathBuf::from(".myagent_history")
        );
    }
}
