//! deepseek-es-2api 入口。

use deepseek_es_2api::api::{build_router, AppState, SharedState, SESSION_TTL_SECS};
use deepseek_es_2api::cache::ResponseCache;
use deepseek_es_2api::config::Config;
use deepseek_es_2api::ledger::Ledger;
use deepseek_es_2api::replay::ReplayStore;
use deepseek_es_2api::session::SessionStore;
use deepseek_es_2api::upstream::UpstreamClient;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 配置路径：--config <path> 或默认 config.json
    let args: Vec<String> = std::env::args().collect();
    let cfg_path: Option<PathBuf> = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| PathBuf::from(&w[1]))
        .or_else(|| {
            let p = PathBuf::from("config.json");
            if p.exists() {
                Some(p)
            } else {
                None
            }
        });

    let cfg = Config::load(cfg_path.as_deref())?;

    // 安全前置校验：非回环监听 + 空 api_keys → 拒绝启动（fail-fast）
    if let Err(e) = cfg.validate_security() {
        eprintln!("{e}");
        std::process::exit(1);
    }

    tracing::info!(
        "启动 deepseek-es-2api v{} | 监听 {} | 上游 {} | bot_id {}",
        env!("CARGO_PKG_VERSION"),
        cfg.listen_addr,
        cfg.upstream_base_url,
        cfg.bot_id
    );
    if cfg.api_keys.is_empty() {
        tracing::warn!("未配置 api_keys：仅本机放行（请勿直接暴露公网）");
    }

    let upstream = Arc::new(UpstreamClient::new(cfg.clone())?);
    // M8：后台认证预取（cookie 剩余 TTL < 20% 时主动续期，消除头阻塞）
    let _prefetch = upstream.clone().spawn_prefetch();
    let ledger = Ledger::open(&cfg.ledger_path)?;
    if ledger.enabled() {
        tracing::info!("用量账本已启用: {}", cfg.ledger_path);
    }
    // R3：后台账本清理——按保留期删除旧记录，避免 DB 无限增长、聚合查询变慢。
    if ledger.enabled() && cfg.ledger_retention_days > 0 {
        let l = ledger.clone();
        let days = cfg.ledger_retention_days;
        tokio::spawn(async move {
            // 启动时先清一次，之后每 6h 一次
            let mut ticker = tokio::time::interval(Duration::from_secs(6 * 3600));
            loop {
                match l.prune(days).await {
                    Ok(n) if n > 0 => tracing::info!("账本清理：删除 {n} 条超过 {days} 天的记录"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("账本清理失败: {e}"),
                }
                ticker.tick().await;
            }
        });
    }
    let cache = ResponseCache::new(
        cfg.cache_ttl_secs,
        cfg.cache_max_entries,
        cfg.cache_min_chars,
    );
    if cache.enabled() {
        tracing::info!("响应缓存已启用: TTL {}s", cfg.cache_ttl_secs);
    }
    if cfg.admin_enabled {
        if cfg.admin_token.is_empty() {
            tracing::warn!("admin_enabled=true 但 admin_token 为空：控制台将拒绝访问");
        } else {
            tracing::info!("控制台已启用: /admin");
        }
    }
    let state: SharedState = Arc::new(AppState {
        cfg: cfg.clone(),
        upstream,
        sessions: SessionStore::new(Duration::from_secs(SESSION_TTL_SECS)),
        cache,
        ledger,
        replay: ReplayStore::new(cfg.cache_ttl_secs.max(60), 1000, 10_000),
    });

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr).await?;
    tracing::info!("控制面板/API: http://{}/", cfg.listen_addr);
    tracing::info!("OpenAI:    http://{}/v1", cfg.listen_addr);
    tracing::info!("Anthropic: http://{}/v1/messages", cfg.listen_addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// 等待 Ctrl-C（SIGINT）或 SIGTERM，触发优雅关闭（P2-13）。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("收到关闭信号，优雅停止（等待在途请求完成）");
}
