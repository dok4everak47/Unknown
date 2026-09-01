use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// `exec` 单次命令执行的最长时长（默认值；可经 [`RuntimeConfig::exec_timeout`] 覆盖）。
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);

/// Runtime 执行参数（当前只有 exec 超时；env 覆盖刻意不做，见
/// `docs/runtime-design.md`）。
///
/// 挂在具体实现结构体上（`LocalRuntime` / `NixRuntime` / `SandboxedRuntime`），
/// **不改变** [`Runtime`] trait 的形状——`Runtime::exec` 签名保持不变。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// exec 单次命令最长时长；超时 kill 子进程并返回 [`ExecError::TimedOut`]。
    pub exec_timeout: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            exec_timeout: EXEC_TIMEOUT,
        }
    }
}
/// `exec` 超时轮询间隔。
const EXEC_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 目录条目的类型分类（供 search 遍历使用，保持现有忽略规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// 一次目录列表的一个条目：路径 + 类型分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
}

/// 命令执行结果：退出码 + 合并后的 stdout/stderr。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub code: i32,
    /// stdout 与 stderr 合并后的输出（与旧 `collect_output` 语义一致）。
    pub output: String,
}

/// 命令执行错误。
#[derive(Debug)]
pub enum ExecError {
    /// spawn / wait 失败。
    Io(io::Error),
    /// 超过 exec 超时（默认 [`EXEC_TIMEOUT`]，可经 [`RuntimeConfig::exec_timeout`]
    /// 配置），携带已捕获的输出。
    TimedOut(String),
}

/// Runtime：工具执行所需的全部副作用原语。
///
/// 工具层（`tool.rs`）只保留参数解析、路径策略与搜索/编辑算法等纯逻辑，
/// 所有对文件系统与进程的真实操作都经由本 trait 完成：
///
/// - [`Runtime::read_file`] — 读文件为 UTF-8 文本（`fs::read_to_string`）
/// - [`Runtime::write_file`] — 写文件（`fs::write`，覆盖已存在文件）
/// - [`Runtime::read_dir`] — 列目录条目（`fs::read_dir` + `file_type`）
/// - [`Runtime::exec`] — 执行命令（`std::process::Command` 直调）
///
/// 当前实现：
/// - [`LocalRuntime`] — std 直接操作真实文件系统与进程；
/// - [`NixRuntime`]（src/nix_runtime.rs）— 文件操作委托本地，exec 经
///   `nix develop --command` 落在可复现的 devShell 中执行。
///
/// 测试可注入 fake 实现，验证工具执行不触碰真实环境。
pub trait Runtime {
    /// 读文件为 UTF-8 文本（对应 `fs::read_to_string`）。
    fn read_file(&self, path: &Path) -> io::Result<String>;

    /// 写文件（对应 `fs::write`，覆盖已存在文件）。
    ///
    /// 父目录不存在时自动创建（`fs::create_dir_all`），这样写入
    /// `src/main.rs` 这类嵌套路径无需先建目录。
    fn write_file(&self, path: &Path, content: &str) -> io::Result<()>;

    /// 列目录条目（对应 `fs::read_dir` + `file_type`）。
    ///
    /// 无法读取类型的条目归为 [`EntryKind::Other`]（遍历方会忽略，
    /// 与旧实现"跳过单个坏条目"的语义一致）。
    fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>>;

    /// 执行命令（对应 `std::process::Command` 直调），返回退出码与合并输出。
    ///
    /// `cwd` 固定为项目根目录，继承当前环境变量；超时由实现配置
    /// （默认 60 秒，见 [`RuntimeConfig`]）。
    fn exec(&self, program: &str, args: &[String], cwd: &Path) -> Result<ExecOutput, ExecError>;
}

/// 本地 Runtime：直接操作真实文件系统与进程（std 实现，当前唯一默认实现）。
pub struct LocalRuntime {
    /// 执行参数（当前只有 exec 超时）。
    config: RuntimeConfig,
}

impl LocalRuntime {
    /// 用指定执行参数构造（当前只有 [`RuntimeConfig::exec_timeout`]）。
    pub fn new(config: RuntimeConfig) -> Self {
        Self { config }
    }

    /// 测试辅助：指定 exec 超时，用于在测试里覆盖 [`ExecError::TimedOut`] 路径
    /// （默认 60s 太长，测试用短超时才能稳定触发超时）。
    #[cfg(test)]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self::new(RuntimeConfig {
            exec_timeout: timeout,
        })
    }
}

impl Default for LocalRuntime {
    fn default() -> Self {
        Self::new(RuntimeConfig::default())
    }
}

impl Runtime for LocalRuntime {
    fn read_file(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn write_file(&self, path: &Path, content: &str) -> io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>> {
        let entries = fs::read_dir(path)?;
        let mut result = Vec::new();
        for entry in entries.flatten() {
            let kind = match entry.file_type() {
                Ok(t) if t.is_dir() => EntryKind::Dir,
                Ok(t) if t.is_file() => EntryKind::File,
                Ok(t) if t.is_symlink() => EntryKind::Symlink,
                // 无法读取类型或其它类型：归为 Other，遍历方忽略
                Ok(_) | Err(_) => EntryKind::Other,
            };
            result.push(RuntimeEntry {
                path: entry.path(),
                kind,
            });
        }
        Ok(result)
    }

    fn exec(&self, program: &str, args: &[String], cwd: &Path) -> Result<ExecOutput, ExecError> {
        run_command(program, args, cwd, self.config.exec_timeout)
    }
}

/// 通用命令执行：spawn → 超时轮询（`timeout`）→ 合并 stdout/stderr → 退出码。
///
/// 被 [`LocalRuntime`] 与 [`NixRuntime`]（`nix develop --command` 包装）
/// 共用，保证两个实现的超时、输出合并、退出码语义完全一致。
/// 超时由调用方传入（实现从各自 [`RuntimeConfig`] 取）。
pub(crate) fn run_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<ExecOutput, ExecError> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ExecError::Io)?;

    // 先接管 stdout/stderr，避免子进程输出写满管道时阻塞
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let output = collect_output(stdout, stderr);
                    return Err(ExecError::TimedOut(output));
                }
                std::thread::sleep(EXEC_POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExecError::Io(err));
            }
        }
    };

    // 等子进程退出后再统一读取，输出量小不会死锁
    let output = collect_output(stdout, stderr);
    let code = status.code().unwrap_or(-1);
    Ok(ExecOutput { code, output })
}

/// 依次读取 stdout 与 stderr 管道（子进程已退出，不会阻塞）。
fn collect_output(mut stdout: ChildStdout, mut stderr: ChildStderr) -> String {
    let mut out = String::new();
    let mut err = String::new();
    let _ = stdout.read_to_string(&mut out);
    let _ = stderr.read_to_string(&mut err);

    let mut output = String::new();
    if !out.is_empty() {
        output.push_str(&out);
    }
    if !err.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&err);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-runtime-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `RuntimeConfig::default()` 超时为 60s（默认值，行为零变化）。
    #[test]
    fn default_config_timeout_is_60s() {
        assert_eq!(
            RuntimeConfig::default().exec_timeout,
            Duration::from_secs(60)
        );
        assert_eq!(EXEC_TIMEOUT, Duration::from_secs(60));
        // `LocalRuntime::default()` 与显式 `new(RuntimeConfig::default())` 等价
        assert_eq!(
            LocalRuntime::default().config.exec_timeout,
            LocalRuntime::new(RuntimeConfig::default())
                .config
                .exec_timeout
        );
    }

    /// TimedOut 路径（核心）：短超时下 `sleep 1` 被 kill，返回
    /// `Err(ExecError::TimedOut(_))`（macOS/Linux 均有 sleep/sh）。
    #[test]
    fn exec_times_out_with_short_timeout() {
        let rt = LocalRuntime::with_timeout(Duration::from_millis(200));
        let cwd = temp_root();
        let result = rt.exec("sh", &["-c".to_string(), "sleep 1".to_string()], &cwd);
        assert!(
            matches!(result, Err(ExecError::TimedOut(_))),
            "expected TimedOut, got: {result:?}"
        );
    }

    /// 超时前已捕获的部分输出必须保留在 `TimedOut(output)` 里。
    #[test]
    fn timed_out_captures_partial_output() {
        let rt = LocalRuntime::with_timeout(Duration::from_millis(200));
        let cwd = temp_root();
        let result = rt.exec(
            "sh",
            &["-c".to_string(), "echo before; sleep 1".to_string()],
            &cwd,
        );
        match result {
            Err(ExecError::TimedOut(output)) => {
                assert!(
                    output.contains("before"),
                    "partial output must be captured, got: {output:?}"
                );
            }
            other => panic!("expected TimedOut with partial output, got: {other:?}"),
        }
    }

    /// 超时内正常完成：200ms 超时跑 `echo ok` → `Ok`，code=0。
    #[test]
    fn exec_completes_within_timeout() {
        let rt = LocalRuntime::with_timeout(Duration::from_millis(200));
        let cwd = temp_root();
        let output = rt
            .exec("sh", &["-c".to_string(), "echo ok".to_string()], &cwd)
            .unwrap();
        assert_eq!(output.code, 0);
        assert!(output.output.contains("ok"), "got: {:?}", output.output);
    }
}
