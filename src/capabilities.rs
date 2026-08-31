/// 权限能力：控制 Agent 在**工具执行前**允许的副作用类别。
///
/// 与 docs/runtime-design.md §6 方案 C 一致。三个布尔字段分别对应
/// 文件读、文件写、进程执行三类副作用；字段命名与文档保持一致。
///
/// 这是"执行前的一道布尔检查"，不引入权限继承、角色或策略引擎：
/// 工具分发处（`Agent::run_turn`）先查 `Capabilities::allows`，
/// 不允许时直接把拒绝作为 Tool Result 回传给 Model，**不触碰 Runtime**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// 允许读文件（`read_file` / `search`）。
    pub filesystem_read: bool,
    /// 允许写文件（`write_file` / `edit_file`）。
    pub filesystem_write: bool,
    /// 允许执行命令（`exec`）。
    pub process_execute: bool,
}

impl Default for Capabilities {
    /// 全允许——保持现有行为零变化。
    fn default() -> Self {
        Self {
            filesystem_read: true,
            filesystem_write: true,
            process_execute: true,
        }
    }
}

impl Capabilities {
    /// 只读模式：只能读文件，不能写文件，不能执行命令。
    pub fn read_only() -> Self {
        Self {
            filesystem_read: true,
            filesystem_write: false,
            process_execute: false,
        }
    }

    /// 判定 `tool_name` 对应的能力是否被允许。
    ///
    /// 未知工具名（不在能力映射内）返回 `true`——不拦截，交给
    /// [`crate::tool::Tool::from_call`] 的未知工具错误路径处理。
    pub fn allows(&self, tool_name: &str) -> bool {
        match required_capability(tool_name) {
            None => true,
            Some((capability, _)) => match capability {
                Capability::FilesystemRead => self.filesystem_read,
                Capability::FilesystemWrite => self.filesystem_write,
                Capability::ProcessExecute => self.process_execute,
            },
        }
    }

    /// 返回 `tool_name` 被拒绝时缺失的能力名（如 `"filesystem_write"`）。
    ///
    /// 仅当 `allows(tool_name)` 为 `false` 时返回 `Some`；未知工具名或
    /// 能力已允许时返回 `None`。供分发处构造清晰的拒绝消息。
    pub fn denied_capability_name(&self, tool_name: &str) -> Option<&'static str> {
        let (capability, name) = required_capability(tool_name)?;
        let allowed = match capability {
            Capability::FilesystemRead => self.filesystem_read,
            Capability::FilesystemWrite => self.filesystem_write,
            Capability::ProcessExecute => self.process_execute,
        };
        if allowed { None } else { Some(name) }
    }
}

/// 能力类别（内部枚举，映射用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capability {
    FilesystemRead,
    FilesystemWrite,
    ProcessExecute,
}

/// 工具名 → 所需能力（纯函数）。
///
/// 覆盖现有 5 个工具；未知工具名返回 `None`（不拦截）。
fn required_capability(tool_name: &str) -> Option<(Capability, &'static str)> {
    match tool_name {
        "read_file" | "search" => Some((Capability::FilesystemRead, "filesystem_read")),
        "write_file" | "edit_file" => Some((Capability::FilesystemWrite, "filesystem_write")),
        "exec" => Some((Capability::ProcessExecute, "process_execute")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工具名 → 所需能力映射的纯函数单元测试（5 个工具 + 未知名）。
    #[test]
    fn maps_tool_names_to_capabilities() {
        use Capability::*;
        assert_eq!(
            required_capability("read_file"),
            Some((FilesystemRead, "filesystem_read"))
        );
        assert_eq!(
            required_capability("search"),
            Some((FilesystemRead, "filesystem_read"))
        );
        assert_eq!(
            required_capability("write_file"),
            Some((FilesystemWrite, "filesystem_write"))
        );
        assert_eq!(
            required_capability("edit_file"),
            Some((FilesystemWrite, "filesystem_write"))
        );
        assert_eq!(
            required_capability("exec"),
            Some((ProcessExecute, "process_execute"))
        );
        // 未知工具名：不拦截（返回 None，交给 from_call 错误路径）
        assert_eq!(required_capability("unknown_tool"), None);
        assert_eq!(required_capability(""), None);
    }

    /// Default：三个能力全允许。
    #[test]
    fn default_allows_everything() {
        let caps = Capabilities::default();
        for tool in ["read_file", "search", "write_file", "edit_file", "exec"] {
            assert!(caps.allows(tool), "{tool} should be allowed by default");
        }
    }

    /// read_only：只读工具放行，写与执行被拒。
    #[test]
    fn read_only_blocks_writes_and_exec() {
        let caps = Capabilities::read_only();
        assert!(caps.allows("read_file"));
        assert!(caps.allows("search"));
        assert!(!caps.allows("write_file"));
        assert!(!caps.allows("edit_file"));
        assert!(!caps.allows("exec"));
        // 未知工具名不受能力门拦截
        assert!(caps.allows("unknown_tool"));
        // denied_capability_name 只在被拒时返回能力名
        assert_eq!(caps.denied_capability_name("read_file"), None);
        assert_eq!(
            caps.denied_capability_name("write_file"),
            Some("filesystem_write")
        );
        assert_eq!(caps.denied_capability_name("exec"), Some("process_execute"));
    }

    /// 手动构造：单能力关闭时的判定。
    #[test]
    fn per_capability_flag_control() {
        let no_write = Capabilities {
            filesystem_read: true,
            filesystem_write: false,
            process_execute: true,
        };
        assert!(no_write.allows("read_file"));
        assert!(!no_write.allows("write_file"));
        assert!(!no_write.allows("edit_file"));
        assert!(no_write.allows("exec"));

        let no_read = Capabilities {
            filesystem_read: false,
            filesystem_write: true,
            process_execute: true,
        };
        assert!(!no_read.allows("read_file"));
        assert!(!no_read.allows("search"));
        assert!(no_read.allows("write_file"));
    }
}
