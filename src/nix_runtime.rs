use std::io;
use std::path::Path;

use crate::runtime::{ExecError, ExecOutput, LocalRuntime, Runtime, RuntimeEntry, run_command};

/// 基于 Nix devShell 的 [`Runtime`] 第二实现。
///
/// - 文件操作（`read_file` / `write_file` / `read_dir`）**委托**给
///   [`LocalRuntime`]：nix 不虚拟化文件系统，语义必须与本地完全一致
///   （组合而非复制）。
/// - `exec` 包装为 `nix develop --command <program> <args...>`，让命令在
///   flake.nix 声明的可复现 devShell 中运行；`cwd` 仍为传入的 root，
///   nix 据此在项目根找到 devShell。
///
/// 超时轮询、stdout/stderr 合并、退出码处理与 [`LocalRuntime`] 完全一致
/// （共用 [`run_command`]），只是进程调用被 `nix develop` 包装。
pub struct NixRuntime {
    /// 文件操作委托本地实现。
    local: LocalRuntime,
}

impl NixRuntime {
    /// 构造并验证 `nix` 可执行文件可用（`nix --version` 成功）。
    ///
    /// nix 不存在（PATH 中找不到）时返回 `io::Error`（`NotFound`），
    /// 让调用方能给出清晰错误，而不是等到第一次 exec 才失败。
    pub fn new() -> io::Result<Self> {
        let cwd = std::env::current_dir()?;
        match run_command("nix", &["--version".to_string()], &cwd) {
            Ok(ExecOutput { code: 0, .. }) => Ok(Self {
                local: LocalRuntime,
            }),
            Ok(ExecOutput { code, .. }) => Err(io::Error::other(format!(
                "`nix --version` exited with code {code}"
            ))),
            Err(ExecError::Io(err)) => Err(err),
            Err(ExecError::TimedOut(_)) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "`nix --version` timed out",
            )),
        }
    }
}

impl Runtime for NixRuntime {
    fn read_file(&self, path: &Path) -> io::Result<String> {
        self.local.read_file(path)
    }

    fn write_file(&self, path: &Path, content: &str) -> io::Result<()> {
        self.local.write_file(path, content)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>> {
        self.local.read_dir(path)
    }

    fn exec(&self, program: &str, args: &[String], cwd: &Path) -> Result<ExecOutput, ExecError> {
        let argv = nix_develop_argv(program, args);
        run_command(&argv[0], &argv[1..], cwd)
    }
}

/// 构造 `nix develop --command <program> <args...>` 的 argv。
///
/// 纯函数，便于在无 nix 环境下做单元测试。
fn nix_develop_argv(program: &str, args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(3 + args.len());
    argv.push("nix".to_string());
    argv.push("develop".to_string());
    argv.push("--command".to_string());
    argv.push(program.to_string());
    argv.extend(args.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-nixrt-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nix_develop_argv_builds_full_command() {
        let argv = nix_develop_argv("cargo", &["check".to_string(), "--all".to_string()]);
        assert_eq!(
            argv,
            vec!["nix", "develop", "--command", "cargo", "check", "--all"]
        );
    }

    #[test]
    fn nix_develop_argv_without_args() {
        let argv = nix_develop_argv("true", &[]);
        assert_eq!(argv, vec!["nix", "develop", "--command", "true"]);
    }

    #[test]
    fn file_operations_match_local_semantics() {
        // 直接构造（绕过 nix 探测）：文件操作与 nix 无关，必须与 LocalRuntime 语义一致。
        let nix_rt = NixRuntime {
            local: LocalRuntime,
        };
        let local_rt = LocalRuntime;
        let root = temp_root();

        let path = root.join("note.txt");
        nix_rt.write_file(&path, "hello").unwrap();
        // 写出的内容本地同样可读：证明写操作就是普通文件系统写（委托语义）
        assert_eq!(local_rt.read_file(&path).unwrap(), "hello");
        assert_eq!(nix_rt.read_file(&path).unwrap(), "hello");

        let mut entries = nix_rt.read_dir(&root).unwrap();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.file_name().unwrap(), "note.txt");

        // read_dir 结果与 LocalRuntime 完全一致（委托语义）
        let mut local_entries = local_rt.read_dir(&root).unwrap();
        local_entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(entries, local_entries);

        fs::remove_dir_all(&root).unwrap();
    }

    /// 冒烟测试：仅在 nix 可用时运行（`NixRuntime::new()` 即探测）。
    ///
    /// 无 nix 的环境（如 CI）early return 静默跳过，不失败；也绝不
    /// 用 `#[ignore]` 跳过。断言 `cargo --version` 在 devShell 中执行成功，
    /// 且 shellHook 的 🦀 横幅不会泄漏进 exec 输出。
    #[test]
    fn exec_runs_inside_nix_dev_shell_without_banner() {
        let Ok(nix_rt) = NixRuntime::new() else {
            eprintln!("skipping: nix not available");
            return;
        };
        let root = std::env::current_dir().unwrap();
        let output = nix_rt
            .exec("cargo", &["--version".to_string()], &root)
            .unwrap();
        assert_eq!(output.code, 0, "output: {}", output.output);
        assert!(
            output.output.contains("cargo"),
            "expected cargo version in output, got: {}",
            output.output
        );
        assert!(
            !output.output.contains("🦀"),
            "devShell banner leaked into exec output: {}",
            output.output
        );
    }
}
