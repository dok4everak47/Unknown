use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// `exec` 单次命令执行的最长时长。
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);
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
    /// 超过 [`EXEC_TIMEOUT`] 超时，携带已捕获的输出。
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

    /// 写文件（对应 `fs::write`，覆盖已存在文件；父目录必须已存在）。
    fn write_file(&self, path: &Path, content: &str) -> io::Result<()>;

    /// 列目录条目（对应 `fs::read_dir` + `file_type`）。
    ///
    /// 无法读取类型的条目归为 [`EntryKind::Other`]（遍历方会忽略，
    /// 与旧实现"跳过单个坏条目"的语义一致）。
    fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>>;

    /// 执行命令（对应 `std::process::Command` 直调），返回退出码与合并输出。
    ///
    /// `cwd` 固定为项目根目录，继承当前环境变量；60 秒超时。
    fn exec(&self, program: &str, args: &[String], cwd: &Path) -> Result<ExecOutput, ExecError>;
}

/// 本地 Runtime：直接操作真实文件系统与进程（std 实现，当前唯一默认实现）。
pub struct LocalRuntime;

impl Runtime for LocalRuntime {
    fn read_file(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn write_file(&self, path: &Path, content: &str) -> io::Result<()> {
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
        run_command(program, args, cwd)
    }
}

/// 通用命令执行：spawn → 60s 超时轮询 → 合并 stdout/stderr → 退出码。
///
/// 被 [`LocalRuntime`] 与 [`NixRuntime`]（`nix develop --command` 包装）
/// 共用，保证两个实现的超时、输出合并、退出码语义完全一致。
pub(crate) fn run_command(
    program: &str,
    args: &[String],
    cwd: &Path,
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
                if start.elapsed() > EXEC_TIMEOUT {
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
