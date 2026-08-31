//! 启动配置：从工作目录的 `.env` 文件加载 `KEY=VALUE` 配置。
//!
//! 优先级：真实环境变量 > `.env` 文件 > 代码内默认值。
//! `.env` 不会覆盖已存在的环境变量（命令行临时注入优先）；
//! 文件不存在时静默跳过（不是错误）。
//!
//! 格式：每行一个 `KEY=VALUE`，`#` 开头为注释，支持可选 `export` 前缀
//! 与成对的单/双引号。密钥放在 `.env` 里——该文件已在 `.gitignore` 中。

use std::fs;
use std::path::Path;

/// 从 `path` 加载 `.env` 风格配置到进程环境变量，返回实际注入的条数。
///
/// 文件不存在返回 `Ok(0)`；无法读取返回底层 `io::Error`。
/// 畸形行（无 `=`、空 key）打印 warning 到 stderr 并跳过，不中止启动。
pub fn load_dotenv(path: &Path) -> std::io::Result<usize> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };

    let mut loaded = 0;
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);

        let Some((key, value)) = line.split_once('=') else {
            eprintln!(
                ".env:{}: ignoring malformed line (expected KEY=VALUE)",
                index + 1
            );
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            eprintln!(".env:{}: ignoring line with empty key", index + 1);
            continue;
        }

        // 真实环境变量优先：已存在则不覆盖。
        if std::env::var_os(key).is_some() {
            continue;
        }
        // SAFETY: load_dotenv 在 main 最开头调用，此时进程尚未创建任何
        // 其他线程（reqwest/tokio 线程在首次 API 请求时才启动），
        // 不存在与 env 读取的并发竞争。
        unsafe { std::env::set_var(key, trim_quotes(value.trim())) };
        loaded += 1;
    }
    Ok(loaded)
}

/// 去掉成对的单引号或双引号（不成对则原样返回）。
fn trim_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"'))
            || (bytes.first() == Some(&b'\'') && bytes.last() == Some(&b'\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立 .env 路径；key 用测试独有前缀，避免并行 set_var 竞争。
    fn env_path() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("myagent-dotenv-{}-{n}", std::process::id()))
    }

    #[test]
    fn loads_basic_pairs_ignoring_comments_and_blanks() {
        let path = env_path();
        let key = "MYAGENT_TEST_DOTENV_BASIC";
        // SAFETY: 测试独有 key，并行测试互不影响。
        unsafe { std::env::remove_var(key) };
        fs::write(
            &path,
            format!("# a comment\n\n  # indented comment\nexport {key}=hello world\n"),
        )
        .unwrap();
        let loaded = load_dotenv(&path).unwrap();
        assert_eq!(std::env::var(key).unwrap(), "hello world");
        assert!(loaded >= 1);
        // SAFETY: 测试独有 key，并行测试互不影响。
        unsafe { std::env::remove_var(key) };
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn strips_matching_quotes() {
        let path = env_path();
        let k1 = "MYAGENT_TEST_DOTENV_Q1";
        let k2 = "MYAGENT_TEST_DOTENV_Q2";
        let k3 = "MYAGENT_TEST_DOTENV_Q3";
        fs::write(
            &path,
            format!("{k1}=\"quoted value\"\n{k2}='single'\n{k3}=unquoted\n"),
        )
        .unwrap();
        for k in [k1, k2, k3] {
            // SAFETY: 测试独有 key。
            unsafe { std::env::remove_var(k) };
        }
        load_dotenv(&path).unwrap();
        assert_eq!(std::env::var(k1).unwrap(), "quoted value");
        assert_eq!(std::env::var(k2).unwrap(), "single");
        assert_eq!(std::env::var(k3).unwrap(), "unquoted");
        for k in [k1, k2, k3] {
            // SAFETY: 测试独有 key。
            unsafe { std::env::remove_var(k) };
        }
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn does_not_override_existing_env() {
        let path = env_path();
        let key = "MYAGENT_TEST_DOTENV_NO_OVERRIDE";
        // SAFETY: 测试独有 key。
        unsafe { std::env::set_var(key, "from-env") };
        fs::write(&path, format!("{key}=from-file\n")).unwrap();
        load_dotenv(&path).unwrap();
        assert_eq!(std::env::var(key).unwrap(), "from-env");
        // SAFETY: 测试独有 key，并行测试互不影响。
        unsafe { std::env::remove_var(key) };
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_ok() {
        let path = std::env::temp_dir().join("myagent-dotenv-does-not-exist");
        assert_eq!(load_dotenv(&path).unwrap(), 0);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let path = env_path();
        let key = "MYAGENT_TEST_DOTENV_MALFORMED";
        // SAFETY: 测试独有 key，并行测试互不影响。
        unsafe { std::env::remove_var(key) };
        fs::write(&path, format!("no-equals-sign-here\n{key}=ok\n=emptykey\n")).unwrap();
        load_dotenv(&path).unwrap();
        assert_eq!(std::env::var(key).unwrap(), "ok");
        // SAFETY: 测试独有 key，并行测试互不影响。
        unsafe { std::env::remove_var(key) };
        fs::remove_file(&path).unwrap();
    }
}
