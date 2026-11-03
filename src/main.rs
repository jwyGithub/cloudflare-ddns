mod ddns;

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, ensure};
use tokio::task::JoinSet;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let executable = std::env::current_exe().context("获取 exe 路径失败")?;
    let log_file = open_log_file(&executable)?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(log_file),
        )
        .init();

    let config_path = match std::env::args().nth(1) {
        Some(path) => path.into(),
        None => std::env::current_exe()?
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("config.toml"),
    };
    let config = ddns::Config::load(&config_path)
        .with_context(|| format!("加载配置失败: {}", config_path.display()))?;
    ensure!(!config.ddns.is_empty(), "配置中至少需要一条 [[ddns]]");

    let mut tasks = JoinSet::new();
    for entry in config.ddns {
        tasks.spawn(ddns::run(entry));
    }
    info!("Cloudflare DDNS 已启动");

    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("监听 Ctrl-C 失败")?,
        _ = async {
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    warn!(%error, "DDNS zone 任务异常退出，其它 zone 继续运行");
                }
            }
        } => error!("所有 DDNS 任务均已退出"),
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    info!("Cloudflare DDNS 已停止");
    Ok(())
}

fn open_log_file(executable: &Path) -> anyhow::Result<File> {
    let log_dir = executable.parent().unwrap_or(Path::new(".")).join("log");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("创建日志目录失败: {}", log_dir.display()))?;
    let log_path = log_dir.join("cloudflare-ddns.log");
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("打开日志文件失败: {}", log_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn creates_log_next_to_executable_and_appends_on_reopen() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cloudflare-ddns-log-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("cloudflare-ddns.exe");

        open_log_file(&exe).unwrap().write_all(b"first").unwrap();
        open_log_file(&exe).unwrap().write_all(b"second").unwrap();

        let mut content = String::new();
        File::open(dir.join("log/cloudflare-ddns.log"))
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "firstsecond");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
