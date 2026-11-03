mod ddns;

use anyhow::{Context, ensure};
use tokio::task::JoinSet;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
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
