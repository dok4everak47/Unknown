//! SSH Runtime：`Runtime` trait 的第三实现，把全部副作用（读/写文件、列目录、
//! exec）经系统 `ssh` 转发到远程主机执行。
//!
//! myagent 仍在本机运行、终端体验不变，只是"文件系统"与"进程"落在远程：
//! 工具层传入的**本机绝对路径**（已 canonicalize、在本机 root 之下）经
//! [`map_path`] 映射为远程路径后，交给远程 shell 处理。
//!
//! ```text
//! exec → ssh -T -o BatchMode=yes -o ConnectTimeout=10 [-p PORT] [USER@]HOST -- <remote cmd...>
//! ```
//!
//! 设计要点：
//! - **零新依赖**：只调系统 `ssh` 可执行文件（`std::process::Command`），
//!   不引入 ssh2 / openssh crate。
//! - **绝不交互式提示密码**：`BatchMode=yes` 下没配免密就快速失败，而不是
//!   挂住等输入（配免密：`ssh-copy-id <user>@<host>`）。
//! - 文件内容走 **base64 over the wire**：避免 stdout/stderr 合并污染内容、
//!   避免引号/换行/二进制问题；本地自实现 base64 编解码（字符集仅
//!   `[A-Za-z0-9+/=]`，无 shell 元字符，可安全放进远程命令）。
//! - 文件原语需要 **stdout 与 stderr 分离**（内容不能被 stderr 污染），exec
//!   需要**合并**（对齐 `LocalRuntime` exec 语义）：模块内自带一个与
//!   `runtime::run_command` 同样超时轮询模式的私有 captured-run 辅助，不改动
//!   共享 `run_command` 的签名。
//!
//! 配置经 `MYAGENT_RUNTIME=ssh` 启用（`main.rs` 解析 env 并接线，见
//! [`crate::main`]）；本模块只做纯函数解析与执行。
//!
//! 远程非交互 shell 的 PATH 注意：cargo 需要出现在远程默认 PATH 上（rustup
//! 一般在 `~/.bashrc`，sshd 非交互会 source）；若远程报
//! `cargo: command not found`，需确保 cargo 在远程 PATH。

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::runtime::{EntryKind, ExecError, ExecOutput, Runtime, RuntimeConfig, RuntimeEntry};

/// ssh 连接超时（秒，OpenSSH `ConnectTimeout`）。
const SSH_CONNECT_TIMEOUT_SECS: &str = "10";
/// `exec` 超时轮询间隔（与 `runtime.rs` 的 `EXEC_POLL_INTERVAL` 一致）。
const EXEC_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// SSH Runtime：文件读写 / 列目录 / exec 全部经 `ssh` 转发到远程主机。
///
/// 持有：
/// - `local_root`：本机项目根（构造时 `current_dir()` + canonicalize；
///   macOS 注意 `/var` → `/private/var`）；
/// - `remote_root`：远程项目根（构造时经 ssh 解析的规范绝对路径）；
/// - `host` / `port` / `ssh_bin`：连接目标与 ssh 可执行文件（可注入，测试用）。
pub struct SshRuntime {
    /// 执行参数（exec 超时）。
    config: RuntimeConfig,
    /// ssh 可执行文件路径（默认 `ssh`；`from_parts` 可注入假 ssh 供测试）。
    ssh_bin: String,
    /// 形如 `dok@192.168.64.11` 或 `192.168.64.11`（可含 `user@`，也可不含）。
    host: String,
    /// ssh 端口（默认 22）。
    port: u16,
    /// 本机项目根（canonicalize 后）。
    local_root: PathBuf,
    /// 远程项目根（canonicalize 后的规范绝对路径）。
    remote_root: PathBuf,
}

impl SshRuntime {
    /// 构造并探测远程可用性。
    ///
    /// 步骤：
    /// 1. 连通性 + 免密探测：`ssh ... -- true`（`BatchMode=yes`）。任何
    ///    io 错误 / 超时 / 非零退出 → 返回**清晰** `io::Error`，提示检查
    ///    主机地址、`ssh-copy-id` 免密、网络/防火墙。
    /// 2. 解析 `remote_root`：设了就 `cd '<root>' && pwd -P`（校验存在并
    ///    规范化）；没设就 `-- pwd` 取远程 home 作为远程根。
    /// 3. `local_root`：`current_dir()` + canonicalize。
    ///
    /// `host` 缺失 / 端口非法等 env 解析在 `main.rs`（`ssh_host_from` /
    /// `parse_ssh_port`）完成。
    pub fn new(
        config: RuntimeConfig,
        host: &str,
        port: u16,
        remote_root: Option<&Path>,
    ) -> io::Result<Self> {
        // 1. 连通性 + 免密探测：`ssh ... -- true`（BatchMode=yes 绝不提示密码）
        let probe = ssh_argv(host, port, &["true".to_string()]);
        match capture_remote(&probe, config.exec_timeout) {
            Ok(out) if out.code == 0 => {}
            Ok(out) => {
                return Err(io::Error::other(format!(
                    "ssh to {host} failed (exit {}): {}\n  check MYAGENT_SSH_HOST, passwordless login (ssh-copy-id), network/firewall",
                    out.code,
                    out.stderr.trim()
                )));
            }
            Err(ExecError::Io(err)) => {
                return Err(io::Error::other(format!(
                    "failed to run ssh to {host}: {err}\n  check MYAGENT_SSH_HOST, passwordless login (ssh-copy-id), network/firewall"
                )));
            }
            Err(ExecError::TimedOut(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "ssh to {host} timed out (ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}s)\n  check network/firewall"
                    ),
                ));
            }
        }

        // 2. 解析 remote_root：设了就 cd root && pwd -P（校验存在并规范化）；
        //    没设就 pwd 取远程 home 作为远程根（ssh 非交互默认落在 $HOME）。
        let remote_root = match remote_root {
            Some(root) => {
                let cmd = format!("cd {} && pwd -P", sh_quote(&root.display().to_string()));
                let argv = ssh_argv(host, port, &[cmd]);
                match capture_remote(&argv, config.exec_timeout) {
                    Ok(out) if out.code == 0 => PathBuf::from(out.stdout.trim()),
                    Ok(out) => {
                        return Err(io::Error::other(format!(
                            "remote root {:?} is not accessible (exit {}): {}\n  check MYAGENT_SSH_ROOT is an absolute path that exists on the remote host",
                            root,
                            out.code,
                            out.stderr.trim()
                        )));
                    }
                    Err(ExecError::Io(err)) => {
                        return Err(io::Error::other(format!(
                            "failed to resolve remote root {:?}: {err}",
                            root
                        )));
                    }
                    Err(ExecError::TimedOut(_)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("timed out resolving remote root {:?}", root),
                        ));
                    }
                }
            }
            None => {
                let argv = ssh_argv(host, port, &["pwd".to_string()]);
                match capture_remote(&argv, config.exec_timeout) {
                    Ok(out) if out.code == 0 => PathBuf::from(out.stdout.trim()),
                    Ok(out) => {
                        return Err(io::Error::other(format!(
                            "failed to determine remote home (exit {}): {}\n  check the remote host is reachable and your login works",
                            out.code,
                            out.stderr.trim()
                        )));
                    }
                    Err(ExecError::Io(err)) => {
                        return Err(io::Error::other(format!(
                            "failed to determine remote home: {err}"
                        )));
                    }
                    Err(ExecError::TimedOut(_)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out determining remote home",
                        ));
                    }
                }
            }
        };

        // 3. local_root：当前工作目录 canonicalize（macOS /var → /private/var）
        let cwd = std::env::current_dir()?;
        let local_root = cwd.canonicalize().unwrap_or(cwd);

        Ok(Self::from_parts(
            config,
            "ssh",
            host,
            port,
            &local_root,
            &remote_root,
        ))
    }

    /// 用显式参数构造（不做探测 / 不解析 env；测试注入自定义 ssh 二进制用，
    /// 参考 `sandbox.rs` 的 `from_parts` / `for_test` 模式）。
    ///
    /// `ssh_bin` 为 ssh 可执行文件路径（默认 `"ssh"`）；`local_root` /
    /// `remote_root` 由调用方决定（`local_root` 会被 canonicalize 兜底）。
    pub fn from_parts(
        config: RuntimeConfig,
        ssh_bin: &str,
        host: &str,
        port: u16,
        local_root: &Path,
        remote_root: &Path,
    ) -> Self {
        Self {
            config,
            ssh_bin: ssh_bin.to_string(),
            host: host.to_string(),
            port,
            local_root: canonicalize_or(local_root),
            remote_root: remote_root.to_path_buf(),
        }
    }

    /// 远程项目根（canonicalize 后的规范绝对路径）；供启动横幅显示。
    pub fn remote_root(&self) -> &Path {
        &self.remote_root
    }

    /// 本机路径 → 远程路径（由纯函数 [`map_path`] 完成）。
    fn map_path(&self, path: &Path) -> io::Result<PathBuf> {
        map_path(path, &self.local_root, &self.remote_root)
    }

    /// 构造完整 ssh argv（把注入的 ssh 二进制换成 `self.ssh_bin`）。
    fn argv(&self, remote_cmd: &[String]) -> Vec<String> {
        let mut argv = ssh_argv(&self.host, self.port, remote_cmd);
        argv[0] = self.ssh_bin.clone();
        argv
    }

    /// 跑一次 ssh 远程命令（stdout/stderr 分离捕获，带超时轮询）。
    fn capture(&self, argv: &[String]) -> Result<RemoteOutput, ExecError> {
        capture_remote(argv, self.config.exec_timeout)
    }
}

impl Runtime for SshRuntime {
    fn read_file(&self, path: &Path) -> io::Result<String> {
        let remote = self.map_path(path)?;
        // 远程 base64 编码到 stdout；stderr 分离（不污染内容）。
        let cmd = format!("base64 -- {}", sh_quote(&remote.display().to_string()));
        let out = self
            .capture(&self.argv(&[cmd]))
            .map_err(io_from_exec)?;
        if out.code != 0 {
            let stderr = out.stderr.trim();
            let msg = if stderr.is_empty() {
                format!("remote read failed (exit {})", out.code)
            } else {
                stderr.to_string()
            };
            return Err(if msg.contains("No such file") {
                io::Error::new(io::ErrorKind::NotFound, msg)
            } else {
                io::Error::other(msg)
            });
        }
        let bytes = b64_decode(&out.stdout)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e}")))?;
        String::from_utf8(bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "remote file is not valid UTF-8")
        })
    }

    fn write_file(&self, path: &Path, content: &str) -> io::Result<()> {
        let remote = self.map_path(path)?;
        // base64 字符集仅 [A-Za-z0-9+/=]，无 shell 元字符，可安全进 argv。
        let b64 = b64_encode(content.as_bytes());
        let dir = remote.parent().unwrap_or(&remote);
        let cmd = format!(
            "mkdir -p {} && printf '%s' '{}' | base64 -d > {}",
            sh_quote(&dir.display().to_string()),
            b64,
            sh_quote(&remote.display().to_string())
        );
        let out = self
            .capture(&self.argv(&[cmd]))
            .map_err(io_from_exec)?;
        if out.code != 0 {
            let stderr = out.stderr.trim();
            let msg = if stderr.is_empty() {
                format!("remote write failed (exit {})", out.code)
            } else {
                stderr.to_string()
            };
            return Err(io::Error::other(msg));
        }
        Ok(())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>> {
        let remote = self.map_path(path)?;
        // 远程 POSIX sh 输出 tab 分隔的 <type>\t<name> 行；含隐藏文件。
        let script = format!(
            "cd {} && for f in * .[!.]* ..?*; do [ -e \"$f\" ] || [ -L \"$f\" ] || continue; \
             if [ -d \"$f\" ]; then t=D; elif [ -L \"$f\" ]; then t=L; elif [ -f \"$f\" ]; then t=F; \
             else t=O; fi; printf '%s\\t%s\\n' \"$t\" \"$f\"; done",
            sh_quote(&remote.display().to_string())
        );
        let out = self
            .capture(&self.argv(&[script]))
            .map_err(io_from_exec)?;
        if out.code != 0 {
            let stderr = out.stderr.trim();
            let msg = if stderr.is_empty() {
                format!("remote read_dir failed (exit {})", out.code)
            } else {
                stderr.to_string()
            };
            return Err(io::Error::other(msg));
        }
        Ok(parse_dir_listing(&remote, &out.stdout))
    }

    fn exec(&self, program: &str, args: &[String], _cwd: &Path) -> Result<ExecOutput, ExecError> {
        // 远程：cd '<remote_root>' && exec <program> <args...>。
        // program/args 已被工具白名单校验为安全字符集（无 shell 元字符），
        // 无需引用；ssh 透传远程命令退出码。cwd 固定为 remote_root（忽略入参）。
        let mut cmd = format!(
            "cd {} && exec {program}",
            sh_quote(&self.remote_root.display().to_string())
        );
        for arg in args {
            cmd.push(' ');
            cmd.push_str(arg);
        }
        let out = self.capture(&self.argv(&[cmd]))?;
        Ok(ExecOutput {
            code: out.code,
            output: merge_output(&out.stdout, &out.stderr),
        })
    }
}

/// 构造 `ssh -T -o BatchMode=yes -o ConnectTimeout=10 [-p PORT] [USER@]HOST -- <remote cmd...>`
/// 的 argv（**纯函数**，便于无 ssh 环境下单测）。
///
/// - `-T`：不分配 pty（输出干净）；
/// - `BatchMode=yes`：绝不交互式提示密码（没配免密就快速失败，而非挂住等输入）；
/// - `ConnectTimeout=10`：连接超时；
/// - `port != 22` 时才加 `-p <port>`（默认端口不加）；
/// - `--` 分隔 host 与远程命令（OpenSSH 会消费该分隔符，不传给远程 shell）；
/// - `host` 可含 `user@` 也可不含，原样透传。
pub fn ssh_argv(host: &str, port: u16, remote_cmd: &[String]) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_string(),
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
    ];
    if port != 22 {
        argv.push("-p".to_string());
        argv.push(port.to_string());
    }
    argv.push(host.to_string());
    argv.push("--".to_string());
    argv.extend(remote_cmd.iter().cloned());
    argv
}

/// POSIX sh 单引号引用：`'` 包裹，内部 `'` 转义为 `'\''`（**纯函数**）。
///
/// 用于把所有插入远程命令的**路径**安全引用（含空格、单引号的路径）。
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 本机绝对路径 → 远程绝对路径（**纯函数**）。
///
/// `strip_prefix(local_root)` 后 `join` 到 `remote_root`；不在 `local_root`
/// 之下 → `io::Error`（正常不会发生，工具层已保证路径在本机 root 之内）。
/// trailing slash 由 `Path` 归一化处理（`/a/b/` 与 `/a/b` 等价）。
pub fn map_path(path: &Path, local_root: &Path, remote_root: &Path) -> io::Result<PathBuf> {
    let rel = path.strip_prefix(local_root).map_err(|_| {
        io::Error::other(format!(
            "path {:?} is outside the local root {:?}; refusing to map to remote",
            path, local_root
        ))
    })?;
    Ok(remote_root.join(rel))
}

/// 解析远程 `read_dir` 输出的 tab 分隔列表（**纯函数**）。
///
/// 每行 `<type>\t<name>`，`type`：`D` 目录 / `L` 符号链接 / `F` 普通文件 /
/// `O` 其他。结果中的路径以 `dir`（远程目录）为前缀拼接。
/// 文件名含 tab / 换行属极端情况，当前不支持（文档注明）。
pub fn parse_dir_listing(dir: &Path, stdout: &str) -> Vec<RuntimeEntry> {
    let mut entries = Vec::new();
    for line in stdout.lines() {
        let Some((kind, name)) = line.split_once('\t') else {
            continue;
        };
        let kind = match kind {
            "D" => EntryKind::Dir,
            "L" => EntryKind::Symlink,
            "F" => EntryKind::File,
            _ => EntryKind::Other,
        };
        entries.push(RuntimeEntry {
            path: dir.join(name),
            kind,
        });
    }
    entries
}

/// 一次远程命令捕获：退出码 + 分离的 stdout / stderr。
struct RemoteOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

/// 私有 captured-run 辅助：与 `runtime::run_command` 同样的超时轮询模式，
/// 但 **stdout / stderr 分离捕获**（文件内容不能被 stderr 污染）。
///
/// 不修改 `runtime.rs` 里共享 `run_command` 的签名；exec 需要合并输出时由
/// 调用方用 [`merge_output`] 合并。
fn capture_remote(argv: &[String], timeout: Duration) -> Result<RemoteOutput, ExecError> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
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
                    let (out, err) = read_both(stdout, stderr);
                    return Err(ExecError::TimedOut(merge_output(&out, &err)));
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

    let (out, err) = read_both(stdout, stderr);
    let code = status.code().unwrap_or(-1);
    Ok(RemoteOutput {
        code,
        stdout: out,
        stderr: err,
    })
}

/// 依次读取 stdout 与 stderr 管道（子进程已退出，不会阻塞）。
fn read_both(mut stdout: ChildStdout, mut stderr: ChildStderr) -> (String, String) {
    let mut out = String::new();
    let mut err = String::new();
    let _ = stdout.read_to_string(&mut out);
    let _ = stderr.read_to_string(&mut err);
    (out, err)
}

/// 合并 stdout / stderr（与 `runtime::run_command` / `collect_output` 语义一致）。
fn merge_output(stdout: &str, stderr: &str) -> String {
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(stderr);
    }
    output
}

/// 文件原语的 `ExecError` → `io::Error` 映射（exec 原语保留 `ExecError`）。
fn io_from_exec(err: ExecError) -> io::Error {
    match err {
        ExecError::Io(io) => io,
        ExecError::TimedOut(output) => io::Error::new(
            io::ErrorKind::TimedOut,
            format!("remote command timed out\n\n{output}"),
        ),
    }
}

/// canonicalize 路径；失败（如路径不存在）时回退原始路径。
fn canonicalize_or(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
}

/// base64 字母表（标准 RFC 4648）。
const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 编码为标准 base64（无换行；字符集仅 `[A-Za-z0-9+/=]`）。
fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(B64_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(B64_ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// base64 字符 → 6 位值。
fn b64_val(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// 解码标准 base64（容忍空白 / 换行，如 GNU `base64` 的 76 列换行）。
fn b64_decode(s: &str) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut quad = [0u8; 4];
    let mut n = 0usize;
    for &b in s.as_bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let v = b64_val(b).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "remote file is not valid base64",
            )
        })?;
        quad[n] = v;
        n += 1;
        if n == 4 {
            out.push((quad[0] << 2) | (quad[1] >> 4));
            out.push((quad[1] << 4) | (quad[2] >> 2));
            out.push((quad[2] << 6) | quad[3]);
            n = 0;
        }
    }
    match n {
        0 => {}
        1 => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "remote file is not valid base64",
            ));
        }
        2 => out.push((quad[0] << 2) | (quad[1] >> 4)),
        3 => {
            out.push((quad[0] << 2) | (quad[1] >> 4));
            out.push((quad[1] << 4) | (quad[2] >> 2));
        }
        _ => {}
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-sshrt-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---------------- 第一层：纯函数单测（无需真 ssh） ----------------

    /// ssh_argv：必含 -T / BatchMode=yes / ConnectTimeout=10 / --；
    /// host 带或不带 user@ 均原样透传。
    #[test]
    fn ssh_argv_basic_form() {
        let argv = ssh_argv("dok@192.168.64.11", 22, &["true".to_string()]);
        assert_eq!(argv[0], "ssh");
        assert!(argv.contains(&"-T".to_string()));
        assert!(argv.contains(&"BatchMode=yes".to_string()));
        assert!(argv.contains(&"ConnectTimeout=10".to_string()));
        assert!(argv.contains(&"dok@192.168.64.11".to_string()));
        assert!(argv.contains(&"--".to_string()));
        assert!(argv.ends_with(&["--".to_string(), "true".to_string()]));
        // 默认端口 22：不加 -p
        assert!(!argv.contains(&"-p".to_string()));
    }

    #[test]
    fn ssh_argv_without_user() {
        let argv = ssh_argv("192.168.64.11", 22, &["true".to_string()]);
        assert!(argv.contains(&"192.168.64.11".to_string()));
        assert!(!argv.contains(&"-p".to_string()));
    }

    #[test]
    fn ssh_argv_non_default_port_adds_p() {
        let argv = ssh_argv("dok@192.168.64.11", 2222, &["true".to_string()]);
        let p = argv
            .iter()
            .position(|a| a == "-p")
            .expect("-p present");
        assert_eq!(argv[p + 1], "2222");
        // 远程命令仍以 -- 开头
        let ddash = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[ddash + 1..], &["true".to_string()]);
    }

    #[test]
    fn ssh_argv_multiple_remote_args() {
        let argv = ssh_argv("h", 22, &["echo".to_string(), "hi".to_string()]);
        assert!(argv.ends_with(&["--".to_string(), "echo".to_string(), "hi".to_string()]));
    }

    /// sh_quote：普通 / 空格 / 单引号 / 空串。
    #[test]
    fn sh_quote_plain_and_space() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("my file.txt"), "'my file.txt'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn sh_quote_escapes_inner_quote() {
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        // 多个单引号
        assert_eq!(sh_quote("a'b'c"), "'a'\\''b'\\''c'");
    }

    /// map_path：普通文件 / 嵌套子目录 / 越界报错 / trailing slash。
    #[test]
    fn map_path_plain_and_nested() {
        let local = PathBuf::from("/home/u/proj");
        let remote = PathBuf::from("/remote/proj");
        assert_eq!(
            map_path(&local.join("main.rs"), &local, &remote).unwrap(),
            PathBuf::from("/remote/proj/main.rs")
        );
        assert_eq!(
            map_path(&local.join("src").join("lib.rs"), &local, &remote).unwrap(),
            PathBuf::from("/remote/proj/src/lib.rs")
        );
    }

    #[test]
    fn map_path_outside_root_errors() {
        let local = PathBuf::from("/home/u/proj");
        let remote = PathBuf::from("/remote/proj");
        assert!(map_path(&PathBuf::from("/etc/passwd"), &local, &remote).is_err());
        // 相邻前缀（/home/u/project2 不在 /home/u/proj 之下）
        assert!(map_path(&PathBuf::from("/home/u/proj2/x"), &local, &remote).is_err());
    }

    #[test]
    fn map_path_trailing_slash_is_normalized() {
        let local = PathBuf::from("/home/u/proj");
        let remote = PathBuf::from("/remote/proj");
        let with_slash = PathBuf::from(format!("{}/", local.join("src").display()));
        assert_eq!(
            map_path(&with_slash, &local, &remote).unwrap(),
            PathBuf::from("/remote/proj/src")
        );
    }

    /// parse_dir_listing：D/F/L/O、隐藏文件、文件名含空格。
    #[test]
    fn parse_dir_listing_kinds_and_hidden() {
        let dir = PathBuf::from("/remote/proj");
        let out = "D\tsrc\nF\tmain.rs\nL\tlink\nO\tfifo\nF\t.gitignore\n";
        let mut entries = parse_dir_listing(&dir, out);
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let kinds: Vec<_> = entries.iter().map(|e| e.kind).collect();
        // 按 path 排序：.gitignore < fifo < link < main.rs < src
        assert_eq!(
            kinds,
            vec![
                EntryKind::File,    // .gitignore
                EntryKind::Other,   // fifo
                EntryKind::Symlink, // link
                EntryKind::File,    // main.rs
                EntryKind::Dir,     // src
            ]
        );
        assert!(
            entries
                .iter()
                .any(|e| e.path == dir.join(".gitignore") && e.kind == EntryKind::File)
        );
    }

    #[test]
    fn parse_dir_listing_names_with_spaces() {
        let dir = PathBuf::from("/remote/proj");
        let out = "F\tmy file.txt\nD\tmy dir\n";
        let entries = parse_dir_listing(&dir, out);
        assert!(
            entries
                .iter()
                .any(|e| e.path == dir.join("my file.txt") && e.kind == EntryKind::File)
        );
        assert!(
            entries
                .iter()
                .any(|e| e.path == dir.join("my dir") && e.kind == EntryKind::Dir)
        );
    }

    /// b64 编解码：往返 / 空串 / 各种长度（触发 = 填充）。
    #[test]
    fn b64_roundtrip_various_lengths() {
        for len in 0..=70usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let enc = b64_encode(&data);
            assert_eq!(b64_decode(&enc).unwrap(), data, "len={len}");
        }
    }

    #[test]
    fn b64_encode_known_values() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn b64_decode_tolerates_newlines() {
        // GNU base64 默认 76 列换行；解码应容忍 \n。用模块自己的编码器生成
        // 一个 >76 列的合法 base64，在 76 列处插入换行，再验证解码一致。
        let plain = "foobar".repeat(10) + "baz";
        let mut wrapped = b64_encode(plain.as_bytes());
        let insert_at = 76;
        assert!(wrapped.len() > insert_at);
        wrapped.insert(insert_at, '\n');
        assert_eq!(b64_decode(&wrapped).unwrap(), plain.as_bytes());
    }

    #[test]
    fn b64_decode_rejects_invalid() {
        assert!(b64_decode("!!not-base64!!").is_err());
        assert!(b64_decode("aGVsbG8").is_ok()); // 无填充（合法）
        assert!(b64_decode("a").is_err()); // 长度非法
    }

    /// 门控：SSH_RUNTIME_TESTS=1 时返回 true（用于启用假 ssh 集成测试）。
    fn ssh_tests_enabled() -> bool {
        std::env::var("SSH_RUNTIME_TESTS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    // ---------------- 第二层：假 ssh 集成测试（SSH_RUNTIME_TESTS=1 启用） ----------------
    // 生成一个假的 ssh 可执行脚本：把"远程路径"映射到本地 backing 目录，
    // 真实执行 base64 / mkdir / ls / cargo，端到端验证 SshRuntime 的
    // 路径映射与命令构造，而无需真 ssh / 真远程主机。

    #[cfg(unix)]
    fn write_fake_ssh(local_backing: &Path, remote_root: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let mut s = String::new();
        s.push_str("#!/usr/bin/env bash\nset -uo pipefail\n");
        s.push_str("REMOTE_ROOT='");
        s.push_str(remote_root);
        s.push_str("'\nLOCAL_ROOT='");
        s.push_str(&local_backing.display().to_string());
        s.push_str("'\ndeclare -a cmd=()\ndeclare after=false\n");
        s.push_str("for a in \"$@\"; do\n  if [[ \"$after\" == true ]]; then cmd+=(\"$a\"); continue; fi\n");
        s.push_str("  if [[ \"$a\" == \"--\" ]]; then after=true; continue; fi\ndone\n");
        s.push_str("CMD=\"${cmd[*]}\"\n");
        s.push_str("map() { local p=\"$1\"; if [[ \"$p\" == \"$REMOTE_ROOT\"* ]]; then echo \"${LOCAL_ROOT}${p#\"$REMOTE_ROOT\"}\"; else echo \"$p\"; fi; }\n");
        s.push_str("if [[ \"$CMD\" == \"true\" ]]; then exit 0; fi\n");
        s.push_str("if [[ \"$CMD\" == \"pwd\" ]]; then echo \"$REMOTE_ROOT\"; exit 0; fi\n");
        s.push_str("if [[ \"$CMD\" == cd\\ * && \"$CMD\" == *\"pwd -P\"* ]]; then\n");
        s.push_str("  d=$(sed -n \"s|^cd '\\([^']*\\)' && pwd -P$|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  m=$(map \"$d\")\n  if [[ ! -d \"$m\" ]]; then echo \"cd: $d: No such file or directory\" >&2; exit 1; fi\n");
        s.push_str("  echo \"$d\"; exit 0\nfi\n");
        s.push_str("if [[ \"$CMD\" == base64\\ * ]]; then\n");
        s.push_str("  p=$(sed -n \"s|^base64 -- '\\([^']*\\)'$|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  m=$(map \"$p\")\n  if [[ ! -f \"$m\" ]]; then echo \"base64: $p: No such file or directory\" >&2; exit 1; fi\n");
        s.push_str("  base64 \"$m\"; exit 0\nfi\n");
        s.push_str("if [[ \"$CMD\" == mkdir\\ * && \"$CMD\" == *\"printf '%s'\"* ]]; then\n");
        s.push_str("  d=$(sed -n \"s|^mkdir -p '\\([^']*\\)'.*|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  b64=$(sed -n \"s|.*printf '%s' '\\([^']*\\)'.*|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  p=$(sed -n \"s|.*> '\\([^']*\\)'$|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  mkdir -p \"$(map \"$d\")\"\n");
        s.push_str("  printf '%s' \"$b64\" | base64 -d > \"$(map \"$p\")\"\n  exit 0\nfi\n");
        s.push_str("if [[ \"$CMD\" == cd\\ * && \"$CMD\" == *\"for f in\"* ]]; then\n");
        s.push_str("  d=$(sed -n \"s|^cd '\\([^']*\\)' && for .*|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  m=$(map \"$d\")\n  if [[ ! -d \"$m\" ]]; then echo \"cd: $d: No such file or directory\" >&2; exit 1; fi\n");
        s.push_str("  (cd \"$m\" && for f in * .[!.]* ..?*; do [ -e \"$f\" ] || [ -L \"$f\" ] || continue; if [ -d \"$f\" ]; then t=D; elif [ -L \"$f\" ]; then t=L; elif [ -f \"$f\" ]; then t=F; else t=O; fi; printf '%s\\t%s\\n' \"$t\" \"$f\"; done)\n");
        s.push_str("  exit 0\nfi\n");
        s.push_str("if [[ \"$CMD\" == cd\\ * && \"$CMD\" == *\"exec cargo\"* ]]; then\n");
        s.push_str("  r=$(sed -n \"s|^cd '\\([^']*\\)' && exec .*|\\1|p\" <<<\"$CMD\")\n");
        s.push_str("  m=$(map \"$r\")\n  cd \"$m\" 2>/dev/null || { echo \"cd: $r: No such file or directory\" >&2; exit 1; }\n");
        s.push_str("  if [[ \"$CMD\" == *\"--should-fail\"* ]]; then\n");
        s.push_str("    echo \"error: simulated compile failure\" >&2\n    exit 101\n  fi\n");
        s.push_str("  echo \"    Checking myagent-ssh-fixture v0.1.0\"\n");
        s.push_str("  echo \"    Finished \\`check\\` profile [unoptimized + debuginfo] target(s) in 0.01s\"\n");
        s.push_str("  echo ran >> cargo-ran.txt\n  exit 0\nfi\n");
        s.push_str("echo \"fake-ssh: unrecognized command: $CMD\" >&2\nexit 2\n");
        let path = local_backing.join("fake-ssh");
        fs::write(&path, s).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn gated_fake_ssh_roundtrip() {
        if !ssh_tests_enabled() {
            eprintln!("skipped (set SSH_RUNTIME_TESTS=1 to enable)");
            return;
        }
        let dir = temp_root();
        let local_root = dir.join("local");
        let backing = dir.join("remote-backing");
        fs::create_dir_all(&local_root).unwrap();
        fs::create_dir_all(&backing).unwrap();
        // macOS /var → /private/var：SshRuntime::from_parts 会 canonicalize
        // local_root，测试须用 canonicalize 后的根构造路径，否则 strip_prefix 失败。
        let local_root = local_root.canonicalize().unwrap();
        let remote_root = "/remote/project";
        let fake = write_fake_ssh(&backing, remote_root);

        let config = RuntimeConfig::default();
        let rt = SshRuntime::from_parts(
            config,
            fake.to_str().unwrap(),
            "dok@fake-host",
            22,
            &local_root,
            Path::new(remote_root),
        );

        // write → 落盘在 backing（= 映射后的远程路径）
        rt.write_file(&local_root.join("hello.txt"), "hi there\n")
            .unwrap();
        assert_eq!(
            fs::read_to_string(backing.join("hello.txt")).unwrap(),
            "hi there\n"
        );

        // write 创建父目录
        rt.write_file(&local_root.join("a/b/c.txt"), "nested")
            .unwrap();
        assert_eq!(
            fs::read_to_string(backing.join("a/b/c.txt")).unwrap(),
            "nested"
        );

        // 大文件经 base64 往返（触发 76 列换行）
        let big: String = "line\n".repeat(500);
        rt.write_file(&local_root.join("big.txt"), &big)
            .unwrap();
        assert_eq!(fs::read_to_string(backing.join("big.txt")).unwrap(), big);
        assert_eq!(
            rt.read_file(&local_root.join("big.txt"))
                .unwrap(),
            big
        );

        // read_file（小文件）
        assert_eq!(
            rt.read_file(&local_root.join("hello.txt"))
                .unwrap(),
            "hi there\n"
        );

        // read_dir
        let entries = rt.read_dir(&local_root).unwrap();
        let names: Vec<String> = entries
            .iter()
            .map(|e| {
                e.path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(names.contains(&"hello.txt".to_string()));
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"big.txt".to_string()));
        // 条目路径映射为远程路径
        assert!(
            entries
                .iter()
                .all(|e| e.path.starts_with(remote_root))
        );

        // exec 成功：cd 到远程根跑 cargo，backing 出现 cargo-ran.txt 标记
        let out = rt
            .exec("cargo", &["check".to_string()], &local_root)
            .unwrap();
        assert_eq!(out.code, 0);
        assert!(
            out.output
                .contains("Checking myagent-ssh-fixture")
        );
        assert!(backing.join("cargo-ran.txt").exists());

        // exec 失败：非零退出码 + stderr 合并进 output
        let out = rt
            .exec(
                "cargo",
                &["check".to_string(), "--should-fail".to_string()],
                &local_root,
            )
            .unwrap();
        assert_eq!(out.code, 101);
        assert!(
            out.output
                .contains("simulated compile failure")
        );

        // read_file 缺失文件 → NotFound（映射 stderr "No such file"）
        let err = rt
            .read_file(&local_root.join("missing.txt"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        // read_dir 缺失目录 → 报错
        assert!(
            rt.read_dir(&local_root.join("nope"))
                .is_err()
        );

        // 越界路径 → 拒绝映射，不触碰远程
        assert!(
            rt.read_file(Path::new("/etc/passwd"))
                .is_err()
        );
    }
}
