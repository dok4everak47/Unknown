//! 终端输出富文本化（ANSI 颜色层次），零新依赖。
//!
//! 只使用 ANSI 转义序列 + std。所有样式方法在 `enabled=false` 时原样返回
//! 输入文本（不含任何转义码），保证非 tty / `NO_COLOR` / `KARAKURI_NO_COLOR`
//! 下输出与纯文本完全一致（逐字保持现状）。
//!
//! 着色开关在 main.rs 启动时各计算一次（stdout / stderr 各自的 `is_terminal()`），
//! 风格对齐 `sandbox_mode()` / `show_reasoning_from()`；`Ui` 结构体无内部状态
//! （`Copy`），可注入、可测试，不用宏或全局 lazy 状态。

/// SGR 转义序列（8 色基础色，兼容性最好）。
const SGR_RESET: &str = "\x1b[0m";
const SGR_BOLD: &str = "\x1b[1m";
const SGR_DIM: &str = "\x1b[2m";
const SGR_ITALIC: &str = "\x1b[3m";
const SGR_RED: &str = "\x1b[31m";
const SGR_GREEN: &str = "\x1b[32m";
const SGR_YELLOW: &str = "\x1b[33m";
const SGR_CYAN: &str = "\x1b[36m";
/// 前景色恢复默认（用于在保持 dim 的同时取消 cyan）。
const SGR_FG_DEFAULT: &str = "\x1b[39m";
/// 恢复正常强度（用于在保持 red 的同时取消 bold）。
const SGR_NORMAL_INTENSITY: &str = "\x1b[22m";

/// 终端富文本 UI：持有着色开关，样式方法在禁用时原样返回输入。
///
/// `Copy`（无内部状态），可注入、可测试；不用宏或全局 lazy 状态。
#[derive(Debug, Clone, Copy)]
pub struct Ui {
    enabled: bool,
}

impl Ui {
    /// 用给定的着色开关创建 UI。
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// 包裹单个 SGR 属性；禁用时原样返回。
    fn paint(&self, sgr: &str, text: &str) -> String {
        if self.enabled {
            format!("{sgr}{text}{SGR_RESET}")
        } else {
            text.to_string()
        }
    }

    /// 包裹两个 SGR 属性（组合样式）；禁用时原样返回。
    fn paint2(&self, sgr_a: &str, sgr_b: &str, text: &str) -> String {
        if self.enabled {
            format!("{sgr_a}{sgr_b}{text}{SGR_RESET}")
        } else {
            text.to_string()
        }
    }

    /// 暗色（dim）。
    pub fn dim(&self, text: &str) -> String {
        self.paint(SGR_DIM, text)
    }

    /// 加粗（bold）。当前 CLI 未直接调用（组合样式经 `paint2` 完成），
    /// 属于完整样式 API 的一部分（测试已覆盖）。
    #[allow(dead_code)]
    pub fn bold(&self, text: &str) -> String {
        self.paint(SGR_BOLD, text)
    }

    /// 斜体（italic）。当前 CLI 未直接调用（推理段用 [`Ui::reasoning_open`]
    /// 的非复位转义），属于完整样式 API 的一部分（测试已覆盖）。
    #[allow(dead_code)]
    pub fn italic(&self, text: &str) -> String {
        self.paint(SGR_ITALIC, text)
    }

    /// 绿色。当前 CLI 未直接调用（绿色经 `green_bold` 组合），属于完整样式
    /// API 的一部分（测试已覆盖）。
    #[allow(dead_code)]
    pub fn green(&self, text: &str) -> String {
        self.paint(SGR_GREEN, text)
    }

    /// 青色。当前 CLI 未直接调用（青色经 `cyan_bold` 组合），属于完整样式
    /// API 的一部分（测试已覆盖）。
    #[allow(dead_code)]
    pub fn cyan(&self, text: &str) -> String {
        self.paint(SGR_CYAN, text)
    }

    /// 红色。
    pub fn red(&self, text: &str) -> String {
        self.paint(SGR_RED, text)
    }

    /// 黄色。
    pub fn yellow(&self, text: &str) -> String {
        self.paint(SGR_YELLOW, text)
    }

    /// 绿色 + 加粗（`You: ` 提示符）。
    pub fn green_bold(&self, text: &str) -> String {
        self.paint2(SGR_GREEN, SGR_BOLD, text)
    }

    /// 青色 + 加粗（`AI: ` 前缀）。
    pub fn cyan_bold(&self, text: &str) -> String {
        self.paint2(SGR_CYAN, SGR_BOLD, text)
    }

    /// 红色 + 加粗（`error:` 标签）。
    pub fn red_bold(&self, text: &str) -> String {
        self.paint2(SGR_RED, SGR_BOLD, text)
    }

    /// 工具执行进度行：整条 dim，工具名额外 cyan（仍 dim），参数保持 dim。
    pub fn tool_allowed(&self, name: &str, args: &str) -> String {
        if self.enabled {
            format!("{SGR_DIM}🔧 {SGR_CYAN}{name}{SGR_FG_DEFAULT} {args}{SGR_RESET}")
        } else {
            format!("🔧 {name} {args}")
        }
    }

    /// 工具被拒行：整条 red，工具名额外 bold（仍 red）。
    pub fn tool_denied(&self, name: &str) -> String {
        if self.enabled {
            format!(
                "{SGR_RED}🚫 {SGR_BOLD}{name}{SGR_NORMAL_INTENSITY} (permission denied){SGR_RESET}"
            )
        } else {
            format!("🚫 {name} (permission denied)")
        }
    }

    /// 推理段开场转义（dim + italic），**不含 reset**——后续流式 deltas 延续该
    /// 样式，结束需配对调用 [`Ui::reset`]。禁用时返回空串（`💭 ` 保持纯文本）。
    pub fn reasoning_open(&self) -> &'static str {
        if self.enabled { "\x1b[2;3m" } else { "" }
    }

    /// 复位转义（`\x1b[0m`）；禁用时返回空串。
    pub fn reset(&self) -> &'static str {
        if self.enabled { SGR_RESET } else { "" }
    }
}

/// 计算着色开关（纯函数，便于单测），风格对齐 `sandbox_mode()` /
/// `show_reasoning_from()`：
///
/// - `io_is_terminal`：stdout / stderr 各自的 `is_terminal()`；
/// - `no_color_env`：`NO_COLOR` 环境变量值（未设置 → `None`）。按
///   https://no-color.org，只要**设置了**（任何值，含空串）即禁用颜色；
/// - `karakuri_no_color`：`KARAKURI_NO_COLOR` 环境变量值（未设置 → `None`）。
///   取值为 `1` / `true`（大小写不敏感）时禁用颜色。
///
/// 规则：`io_is_terminal && NO_COLOR 未设置 && KARAKURI_NO_COLOR 不是 1/true`。
pub fn color_enabled(
    io_is_terminal: bool,
    no_color_env: Option<&str>,
    karakuri_no_color: Option<&str>,
) -> bool {
    if !io_is_terminal {
        return false;
    }
    if no_color_env.is_some() {
        return false;
    }
    match karakuri_no_color {
        Some(value) => !matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// enabled=false 时所有样式方法返回的文本不含任何转义码（表驱动）。
    #[test]
    fn disabled_styles_have_no_escape_codes() {
        let ui = Ui::new(false);
        let cases: Vec<(&str, String)> = vec![
            ("dim", ui.dim("abc")),
            ("bold", ui.bold("abc")),
            ("italic", ui.italic("abc")),
            ("green", ui.green("abc")),
            ("cyan", ui.cyan("abc")),
            ("red", ui.red("abc")),
            ("yellow", ui.yellow("abc")),
            ("green_bold", ui.green_bold("abc")),
            ("cyan_bold", ui.cyan_bold("abc")),
            ("red_bold", ui.red_bold("abc")),
            (
                "tool_allowed",
                ui.tool_allowed("read_file", "{\"path\":\"x\"}"),
            ),
            ("tool_denied", ui.tool_denied("write_file")),
            ("reasoning_open", ui.reasoning_open().to_string()),
            ("reset", ui.reset().to_string()),
        ];
        for (name, output) in &cases {
            assert!(
                !output.contains('\x1b'),
                "{name}: disabled output leaked escape code: {output:?}"
            );
        }
    }

    /// enabled=false 时 wrapper 输出即原文（逐字保持现状）。
    #[test]
    fn disabled_styles_return_verbatim_text() {
        let ui = Ui::new(false);
        assert_eq!(ui.dim("abc"), "abc");
        assert_eq!(ui.green_bold("You: "), "You: ");
        assert_eq!(ui.cyan_bold("AI: "), "AI: ");
        assert_eq!(ui.red_bold("error:"), "error:");
        assert_eq!(
            ui.tool_allowed("read_file", "{\"path\":\"x\"}"),
            "🔧 read_file {\"path\":\"x\"}"
        );
        assert_eq!(
            ui.tool_denied("write_file"),
            "🚫 write_file (permission denied)"
        );
        assert_eq!(ui.reasoning_open(), "");
        assert_eq!(ui.reset(), "");
    }

    /// enabled=true 时每个样式方法以对应 SGR 开头、以 reset 结尾。
    #[test]
    fn enabled_styles_wrap_with_sgr_and_reset() {
        let ui = Ui::new(true);
        let cases: Vec<(&str, &str, String)> = vec![
            ("dim", "\x1b[2m", ui.dim("abc")),
            ("bold", "\x1b[1m", ui.bold("abc")),
            ("italic", "\x1b[3m", ui.italic("abc")),
            ("green", "\x1b[32m", ui.green("abc")),
            ("cyan", "\x1b[36m", ui.cyan("abc")),
            ("red", "\x1b[31m", ui.red("abc")),
            ("yellow", "\x1b[33m", ui.yellow("abc")),
            ("green_bold", "\x1b[32m\x1b[1m", ui.green_bold("abc")),
            ("cyan_bold", "\x1b[36m\x1b[1m", ui.cyan_bold("abc")),
            ("red_bold", "\x1b[31m\x1b[1m", ui.red_bold("abc")),
            (
                "tool_allowed",
                "\x1b[2m",
                ui.tool_allowed("read_file", "{\"path\":\"x\"}"),
            ),
            ("tool_denied", "\x1b[31m", ui.tool_denied("write_file")),
        ];
        for (name, prefix, output) in &cases {
            assert!(
                output.starts_with(prefix),
                "{name}: expected prefix {prefix:?}, got {output:?}"
            );
            assert!(
                output.ends_with(SGR_RESET),
                "{name}: expected reset suffix, got {output:?}"
            );
        }
        // 推理开场无 reset（流式延续），单独断言。
        assert_eq!(ui.reasoning_open(), "\x1b[2;3m");
        assert_eq!(ui.reset(), "\x1b[0m");
    }

    /// 工具行组合样式：工具名 cyan + dim，参数保持 dim（整条 dim）。
    #[test]
    fn tool_allowed_composes_dim_cyan_and_dim_args() {
        let ui = Ui::new(true);
        let line = ui.tool_allowed("read_file", "{\"path\":\"x\"}");
        // 整条 dim 开场；工具名处追加 cyan；参数前恢复前景默认（仍 dim）。
        assert_eq!(
            line,
            "\x1b[2m🔧 \x1b[36mread_file\x1b[39m {\"path\":\"x\"}\x1b[0m"
        );
    }

    /// 工具被拒行组合样式：整条 red，工具名追加 bold。
    #[test]
    fn tool_denied_composes_red_and_bold_name() {
        let ui = Ui::new(true);
        let line = ui.tool_denied("write_file");
        assert_eq!(
            line,
            "\x1b[31m🚫 \x1b[1mwrite_file\x1b[22m (permission denied)\x1b[0m"
        );
    }

    /// color_enabled 真值表：tty/非tty × NO_COLOR 设/未设 × KARAKURI_NO_COLOR 各值。
    #[test]
    fn color_enabled_truth_table() {
        // 非 tty → 恒 false（无论 env）。
        assert!(!color_enabled(false, None, None));
        assert!(!color_enabled(false, Some(""), None));
        assert!(!color_enabled(false, None, Some("1")));
        assert!(!color_enabled(false, None, Some("true")));
        assert!(!color_enabled(false, None, Some("0")));

        // tty、无任何禁色变量 → true。
        assert!(color_enabled(true, None, None));

        // tty + KARAKURI_NO_COLOR 不是 1/true（0 / false / 空串 / 未设）→ true。
        assert!(color_enabled(true, None, Some("0")));
        assert!(color_enabled(true, None, Some("false")));
        assert!(color_enabled(true, None, Some("")));
        assert!(color_enabled(true, None, None));

        // tty + NO_COLOR 设置（任何值，含空串、任意内容）→ false。
        assert!(!color_enabled(true, Some(""), None));
        assert!(!color_enabled(true, Some("1"), None));
        assert!(!color_enabled(true, Some("0"), None));
        assert!(!color_enabled(true, Some("anything"), None));

        // NO_COLOR 优先：即使 KARAKURI_NO_COLOR 非 1/true，NO_COLOR 设了仍关闭。
        assert!(!color_enabled(true, Some(""), Some("0")));
        assert!(!color_enabled(true, Some(""), Some("false")));

        // tty + KARAKURI_NO_COLOR=1 / true（大小写不敏感，可带空白）→ false。
        assert!(!color_enabled(true, None, Some("1")));
        assert!(!color_enabled(true, None, Some("true")));
        assert!(!color_enabled(true, None, Some("TRUE")));
        assert!(!color_enabled(true, None, Some(" True ")));
        assert!(!color_enabled(true, None, Some("1 ")));
    }
}
