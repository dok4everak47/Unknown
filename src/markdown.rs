//! 终端 Markdown 流式渲染（零新依赖，纯 std + [`crate::ui::Ui`]）。
//!
//! 喂入 SSE 文本 delta，按行缓冲、逐行渲染成带 ANSI 样式的字符串返回。
//! 支持的最小子集（覆盖 LLM 回答的常见结构）：
//! - 围栏代码块 ```` ``` ````：围栏本身替换为一条 dim 横线，块内每行加 dim 的
//!   `│ ` 前缀，块内**不做**任何行内解析（`*`/`` ` `` 原样保留）；
//! - 标题 `#`/`##`/`###`（1~3 个 `#` + 空格）→ cyan + bold；
//! - 引用 `> ` → dim + italic；
//! - 无序列表 `- `/`* `/`+ `、有序列表 `1. `：marker dim，正文走行内渲染；
//! - 行内：`` `code` `` → cyan（内部不解析）、`**bold**` → bold、`*it*` /
//!   `_it_` → italic（`_` 带 intraword 保护，`foo_bar` 不触发）。
//!
//! 着色关闭（非 tty / `NO_COLOR` / `KARAKURI_NO_COLOR`）时 [`MarkdownRenderer::push`]
//! 逐字透传、不缓冲、不解析，输出与纯文本完全一致。

use crate::ui::Ui;

/// 有状态的流式 Markdown 渲染器：喂入 SSE 文本 delta，返回应写入终端的字符串。
///
/// 每一轮对话新建一个（代码块开关状态不跨 turn）。
pub struct MarkdownRenderer {
    /// 尚未遇到换行、未渲染的半行缓冲。
    line_buf: String,
    /// 是否正处于围栏代码块内。
    in_code_block: bool,
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        Self {
            line_buf: String::new(),
            in_code_block: false,
        }
    }

    /// 喂入一段 delta，返回应写入终端的字符串（可能含多行，已含换行）。
    pub fn push(&mut self, delta: &str, ui: &Ui) -> String {
        // 着色关闭：逐字透传，不缓冲、不解析（保证字节级一致）。
        if !ui.enabled() {
            return delta.to_string();
        }
        self.line_buf.push_str(delta);
        let mut out = String::new();
        while let Some(idx) = self.line_buf.find('\n') {
            // 取出含换行符在内的完整一行。
            let mut line: String = self.line_buf.drain(..=idx).collect();
            line.pop(); // 去掉 '\n'
            if line.ends_with('\r') {
                line.pop(); // 兼容 CRLF
            }
            out.push_str(&self.render_line(&line, ui));
            out.push('\n');
        }
        out
    }

    /// 一轮结束时冲刷残余半行（模型回答常不以换行结尾）；无残余返回空串。
    pub fn flush(&mut self, ui: &Ui) -> String {
        if !ui.enabled() {
            self.line_buf.clear();
            return String::new();
        }
        let rest = std::mem::take(&mut self.line_buf);
        let line = rest.strip_suffix('\r').unwrap_or(&rest);
        if line.is_empty() {
            String::new()
        } else {
            self.render_line(line, ui)
        }
    }

    /// 渲染单行（不含尾换行）。
    fn render_line(&mut self, line: &str, ui: &Ui) -> String {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];

        // 1. 围栏代码块边界：翻转状态，围栏行本身显示为一条 dim 横线。
        if t.starts_with("```") {
            self.in_code_block = !self.in_code_block;
            return ui.dim("┄┄┄┄┄┄┄┄");
        }
        // 2. 代码块内：原样保留 + dim 竖线前缀，不做行内解析。
        if self.in_code_block {
            return format!("{}{}", ui.dim("│ "), line);
        }
        // 3. 标题（1~3 个 '#' + 空格）。
        if heading_level(t).is_some() {
            return ui.cyan_bold(line.trim_end());
        }
        // 4. 引用。
        if t == ">" || t.starts_with("> ") {
            return ui.italic(&ui.dim(line.trim_end()));
        }
        // 5. 无序列表。
        for marker in ["- ", "* ", "+ "] {
            if let Some(rest) = t.strip_prefix(marker) {
                return format!("{}{}{}", indent, ui.dim(marker), render_inline(rest, ui));
            }
        }
        // 6. 有序列表（数字 + ". "）。
        if let Some((marker, rest)) = ordered_item(t) {
            return format!("{}{}{}", indent, ui.dim(marker), render_inline(rest, ui));
        }
        // 7. 普通段落：行内渲染（前导空白原样保留）。
        render_inline(line, ui)
    }
}

/// 标题层级：1~3 个 `#` 且紧随空格时返回 `Some(level)`。
fn heading_level(t: &str) -> Option<usize> {
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if (1..=3).contains(&hashes) && t.as_bytes().get(hashes) == Some(&b' ') {
        Some(hashes)
    } else {
        None
    }
}

/// 有序列表项：返回 `(marker, rest)`，marker 如 `"1. "`。
fn ordered_item(t: &str) -> Option<(&str, &str)> {
    let digits = t
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits > 0
        && t.as_bytes().get(digits) == Some(&b'.')
        && t.as_bytes().get(digits + 1) == Some(&b' ')
    {
        Some((&t[..digits + 2], &t[digits + 2..]))
    } else {
        None
    }
}

/// 行内渲染：反引号代码 > `**bold**` > `*it*` / `_it_`。已匹配的 span 内容
/// 不再重复解析（扁平、不嵌套）；配对不完整的标记原样输出，绝不丢字 / panic。
fn render_inline(text: &str, ui: &Ui) -> String {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'`' {
            // 行内代码：找下一个反引号；内容 cyan，内部不解析。
            if let Some(rel) = find_byte(&b[i + 1..], b'`') {
                let start = i + 1;
                let end = start + rel;
                let inner = &text[start..end];
                if inner.is_empty() {
                    out.push('`');
                    i += 1;
                } else {
                    out.push_str(&ui.cyan(inner));
                    i = end + 1;
                }
            } else {
                out.push('`');
                i += 1;
            }
        } else if c == b'*' && b.get(i + 1) == Some(&b'*') {
            // 粗体 **...**。
            if let Some(rel) = find_sub(&b[i + 2..], b"**") {
                let start = i + 2;
                let end = start + rel;
                let inner = &text[start..end];
                if inner.is_empty() {
                    out.push('*');
                    i += 1;
                } else {
                    out.push_str(&ui.bold(inner));
                    i = end + 2;
                }
            } else {
                out.push('*');
                i += 1;
            }
        } else if c == b'*' {
            // 斜体 *...*（闭合 '*' 不得属于某个 '**'）。
            match find_star_italic_close(b, i + 1) {
                Some(end) if end > i + 1 => {
                    out.push_str(&ui.italic(&text[i + 1..end]));
                    i = end + 1;
                }
                _ => {
                    out.push('*');
                    i += 1;
                }
            }
        } else if c == b'_' && (i == 0 || is_ws(b[i - 1])) {
            // 斜体 _..._：开界限于行首/空白之后（避免 foo_bar 误判）。
            match find_underscore_close(b, i + 1) {
                Some(end) if end > i + 1 => {
                    out.push_str(&ui.italic(&text[i + 1..end]));
                    i = end + 1;
                }
                _ => {
                    out.push('_');
                    i += 1;
                }
            }
        } else {
            // 普通字符：按 char 推进（UTF-8 安全）。
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&x| x == needle)
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// 找单个 `*` 斜体闭合位：该 `*` 不得与另一个 `*` 相邻（即不属于 `**`）。
fn find_star_italic_close(b: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        if b[j] == b'*' {
            let prev_star = j > 0 && b[j - 1] == b'*';
            let next_star = b.get(j + 1) == Some(&b'*');
            if !prev_star && !next_star {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

/// 找 `_` 斜体闭合位：右侧须为行尾/空白（right-flanking），左侧不得是空白。
fn find_underscore_close(b: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        if b[j] == b'_' && (j + 1 >= b.len() || is_ws(b[j + 1])) && j > 0 && !is_ws(b[j - 1]) {
            return Some(j);
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 剥离 SGR 转义序列，便于断言"内容无损"。
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn fenced_code_renders_border_and_keeps_content_literal() {
        let ui = Ui::new(true);
        let mut md = MarkdownRenderer::new();
        let o1 = md.push("```rust\n", &ui);
        assert!(o1.contains('┄'), "fence should render as dim rule: {o1:?}");
        assert!(!o1.contains("```"));
        let o2 = md.push("let x = *not italic*;\n", &ui);
        assert!(
            o2.starts_with("\x1b[2m│ \x1b[0m"),
            "code line gets dim │ prefix: {o2:?}"
        );
        assert!(o2.contains("let x = *not italic*;"));
        assert!(
            !o2.contains("\x1b[3m"),
            "code content must not be italicized"
        );
        let o3 = md.push("```\n", &ui);
        assert!(o3.contains('┄'));
        assert!(!o3.contains("```"));
    }

    #[test]
    fn unclosed_fence_flush_keeps_trailing_code() {
        let ui = Ui::new(true);
        let mut md = MarkdownRenderer::new();
        md.push("```\n", &ui);
        let tail = md.flush_after("let y = 2;", &ui);
        assert!(
            tail.contains("│ "),
            "trailing code line gets prefix: {tail:?}"
        );
        assert!(strip_ansi(&tail).contains("let y = 2;"));
    }

    #[test]
    fn inline_styles_with_color_and_verbatim_without() {
        let ui = Ui::new(true);
        let mut md = MarkdownRenderer::new();
        let out = md.push("a `code` b **bold** c *it* d\n", &ui);
        assert!(
            out.contains("\x1b[36mcode\x1b[0m"),
            "inline code cyan: {out:?}"
        );
        assert!(out.contains("\x1b[1mbold\x1b[0m"), "bold: {out:?}");
        assert!(out.contains("\x1b[3mit\x1b[0m"), "italic: {out:?}");

        // 着色关闭：逐字透传（含反引号/星号）。
        let ui0 = Ui::new(false);
        let mut md0 = MarkdownRenderer::new();
        let s = "a `code` b **bold** c *it* d\n";
        assert_eq!(md0.push(s, &ui0), s);
        assert_eq!(md0.flush(&ui0), "");
    }

    #[test]
    fn block_level_styles_carry_sgr() {
        let ui = Ui::new(true);
        let mut md = MarkdownRenderer::new();
        let h = md.push("# Title\n", &ui);
        assert!(
            h.contains("\x1b[36m\x1b[1m# Title\x1b[0m"),
            "heading cyan+bold: {h:?}"
        );

        let q = md.push("> quote\n", &ui);
        assert!(
            q.contains("\x1b[3m") && q.contains("\x1b[2m"),
            "quote dim+italic: {q:?}"
        );
        assert!(strip_ansi(&q).contains("> quote"));

        let l = md.push("- item\n", &ui);
        assert!(l.starts_with("\x1b[2m- \x1b[0m"), "list marker dim: {l:?}");
        assert!(strip_ansi(&l).contains("- item"));

        let ol = md.push("3. third\n", &ui);
        assert!(
            ol.starts_with("\x1b[2m3. \x1b[0m"),
            "ordered marker dim: {ol:?}"
        );
    }

    #[test]
    fn half_line_buffered_until_newline_or_flush() {
        let ui = Ui::new(true);
        let mut md = MarkdownRenderer::new();
        assert_eq!(md.push("foo", &ui), "");
        let mid = md.push("bar\nbaz", &ui);
        assert_eq!(mid, "foobar\n");
        assert_eq!(md.flush(&ui), "baz");
    }

    #[test]
    fn no_characters_lost_on_degenerate_markup() {
        let ui = Ui::new(true);
        let cases = [
            "plain * star _ under ` tick",
            "foo_bar_baz",
            "a ** b __ c",
            "**unclosed bold",
            "*unclosed italic",
            "trailing backtick `",
        ];
        for c in cases {
            let mut md = MarkdownRenderer::new();
            let out = md.push(&format!("{c}\n"), &ui);
            assert_eq!(
                strip_ansi(&out),
                format!("{c}\n"),
                "degenerate markup must round-trip verbatim: {c:?}"
            );
        }
    }

    #[test]
    fn intraword_underscore_not_italic_but_leading_is() {
        let ui = Ui::new(true);
        let mut md = MarkdownRenderer::new();
        let intra = md.push("foo_bar_baz\n", &ui);
        assert!(
            !intra.contains("\x1b[3m"),
            "intraword underscore must not italicize: {intra:?}"
        );

        let lead = md.push("_hi_ there\n", &ui);
        assert!(
            lead.contains("\x1b[3mhi\x1b[0m"),
            "leading _hi_ should italicize: {lead:?}"
        );
    }

    // 小助手：模拟再喂一段且无换行后 flush（让未闭合围栏用例可读）。
    impl MarkdownRenderer {
        fn flush_after(&mut self, extra: &str, ui: &Ui) -> String {
            self.push(extra, ui);
            self.flush(ui)
        }
    }
}
