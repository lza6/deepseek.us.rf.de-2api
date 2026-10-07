//! 用量账本：SQLite 持久化请求记录与配额统计（P3-3）。
//!
//! 设计要点：
//! - 同步 `rusqlite` 置于 `tokio::task::spawn_blocking`，避免阻塞异步运行时。
//! - 写入失败**不阻断**请求（仅记录 warn），账本是旁路观测，不是关键路径。
//! - `ledger_path` 为空 → 关闭持久化（纯内存统计）。

use crate::errors::{AppError, AppResult};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// 一条用量记录。
#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub ts: i64,
    pub model: String,
    /// 下游 key 的脱敏标识（前 6 位 + 长度；空 key 记为 "local"）
    pub key_id: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
    pub status: u16,
    pub stream: bool,
    /// 缓存命中（未打上游）
    pub cached: bool,
}

/// 聚合统计。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UsageStats {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub cache_hits: u64,
    pub errors: u64,
    pub avg_latency_ms: u64,
}

/// 用量账本句柄（可克隆，内部共享连接）。
#[derive(Clone)]
pub struct Ledger {
    conn: Option<Arc<Mutex<Connection>>>,
}

impl Ledger {
    /// 打开账本。`path` 为空 → 内存模式（不持久化）。
    ///
    /// **账本是旁路观测，不是关键路径**：打开失败时降级为内存模式（记 warn），
    /// **不**让网关启动失败（修复此前 open 失败会 fail-fast 阻断主服务的问题）。
    pub fn open(path: &str) -> AppResult<Self> {
        if path.trim().is_empty() {
            return Ok(Ledger { conn: None });
        }
        match Self::try_open(path) {
            Ok(l) => Ok(l),
            Err(e) => {
                tracing::warn!("用量账本打开失败，降级为内存模式（不持久化）: {e}");
                Ok(Ledger { conn: None })
            }
        }
    }

    fn try_open(path: &str) -> AppResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| AppError::Internal(format!("打开用量账本失败: {e}")))?;
        // 多实例/并发打开同一账本时避免 "database is locked"：设置忙等待超时（5s）。
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| AppError::Internal(format!("设置账本忙等待失败: {e}")))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             -- 自动 checkpoint：WAL 超约 4MB 时折叠回主库，避免 -wal 无限增长
             PRAGMA wal_autocheckpoint=1000;
             CREATE TABLE IF NOT EXISTS usage (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts INTEGER NOT NULL,
                 model TEXT NOT NULL,
                 key_id TEXT NOT NULL,
                 prompt_tokens INTEGER NOT NULL,
                 completion_tokens INTEGER NOT NULL,
                 latency_ms INTEGER NOT NULL,
                 status INTEGER NOT NULL,
                 stream INTEGER NOT NULL,
                 cached INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage(ts);",
        )
        .map_err(|e| AppError::Internal(format!("初始化账本失败: {e}")))?;
        Ok(Ledger {
            conn: Some(Arc::new(Mutex::new(conn))),
        })
    }

    /// 按保留期清理历史记录（`retention_days` 天前）。返回删除行数。
    ///
    /// 生产长期运行必须定期调用，否则 `usage.db` 无限增长、聚合查询变慢。
    /// `retention_days` = 0 → 不清理。
    pub async fn prune(&self, retention_days: u64) -> AppResult<usize> {
        if retention_days == 0 {
            return Ok(0);
        }
        let Some(conn) = self.conn.clone() else {
            return Ok(0);
        };
        let cutoff = chrono::Utc::now().timestamp() - (retention_days as i64) * 86_400;
        tokio::task::spawn_blocking(move || {
            let c = conn
                .lock()
                .map_err(|e| AppError::Internal(format!("账本锁中毒: {e}")))?;
            let n = c
                .execute("DELETE FROM usage WHERE ts < ?1", rusqlite::params![cutoff])
                .map_err(|e| AppError::Internal(format!("账本清理失败: {e}")))?;
            // 清理后折叠 WAL，回收磁盘
            let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            Ok(n)
        })
        .await
        .map_err(|e| AppError::Internal(format!("账本清理任务失败: {e}")))?
    }

    /// 是否启用持久化。
    pub fn enabled(&self) -> bool {
        self.conn.is_some()
    }

    /// 异步写入一条记录（失败仅告警，不阻断请求）。
    pub async fn record(&self, rec: UsageRecord) {
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || {
            let c = match conn.lock() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("账本锁中毒: {e}");
                    return;
                }
            };
            if let Err(e) = c.execute(
                "INSERT INTO usage (ts,model,key_id,prompt_tokens,completion_tokens,latency_ms,status,stream,cached)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![
                    rec.ts,
                    rec.model,
                    rec.key_id,
                    rec.prompt_tokens as i64,
                    rec.completion_tokens as i64,
                    rec.latency_ms as i64,
                    rec.status as i64,
                    rec.stream as i32,
                    rec.cached as i32,
                ],
            ) {
                tracing::warn!("账本写入失败: {e}");
            }
        })
        .await;
    }

    /// 查询聚合统计（可选起始时间戳）。
    pub async fn stats(&self, since_ts: Option<i64>) -> AppResult<UsageStats> {
        let Some(conn) = self.conn.clone() else {
            return Ok(UsageStats::default());
        };
        tokio::task::spawn_blocking(move || {
            let c = conn
                .lock()
                .map_err(|e| AppError::Internal(format!("账本锁中毒: {e}")))?;
            let (where_clause, param): (&str, Vec<i64>) = match since_ts {
                Some(t) => ("WHERE ts >= ?1", vec![t]),
                None => ("", vec![]),
            };
            let sql = format!(
                "SELECT COUNT(*),
                        COALESCE(SUM(prompt_tokens),0),
                        COALESCE(SUM(completion_tokens),0),
                        COALESCE(SUM(cached),0),
                        COALESCE(SUM(CASE WHEN status>=400 THEN 1 ELSE 0 END),0),
                        COALESCE(CAST(AVG(latency_ms) AS INTEGER),0)
                 FROM usage {where_clause}"
            );
            let mut stmt = c
                .prepare(&sql)
                .map_err(|e| AppError::Internal(format!("账本查询准备失败: {e}")))?;
            let mut rows = stmt
                .query(rusqlite::params_from_iter(param.iter()))
                .map_err(|e| AppError::Internal(format!("账本查询失败: {e}")))?;
            let row = rows
                .next()
                .map_err(|e| AppError::Internal(format!("账本读取失败: {e}")))?
                .ok_or_else(|| AppError::Internal("账本无结果".into()))?;
            Ok(UsageStats {
                total_requests: row.get::<_, i64>(0).unwrap_or(0) as u64,
                total_prompt_tokens: row.get::<_, i64>(1).unwrap_or(0) as u64,
                total_completion_tokens: row.get::<_, i64>(2).unwrap_or(0) as u64,
                cache_hits: row.get::<_, i64>(3).unwrap_or(0) as u64,
                errors: row.get::<_, i64>(4).unwrap_or(0) as u64,
                avg_latency_ms: row.get::<_, i64>(5).unwrap_or(0) as u64,
            })
        })
        .await
        .map_err(|e| AppError::Internal(format!("账本任务失败: {e}")))?
    }

    /// 最近 N 条记录（供控制台）。
    pub async fn recent(&self, limit: usize) -> AppResult<Vec<serde_json::Value>> {
        let Some(conn) = self.conn.clone() else {
            return Ok(vec![]);
        };
        tokio::task::spawn_blocking(move || {
            let c = conn
                .lock()
                .map_err(|e| AppError::Internal(format!("账本锁中毒: {e}")))?;
            let mut stmt = c
                .prepare(
                    "SELECT ts,model,key_id,prompt_tokens,completion_tokens,latency_ms,status,stream,cached
                     FROM usage ORDER BY id DESC LIMIT ?1",
                )
                .map_err(|e| AppError::Internal(format!("账本查询准备失败: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params![limit as i64], |r| {
                    Ok(serde_json::json!({
                        "ts": r.get::<_, i64>(0)?,
                        "model": r.get::<_, String>(1)?,
                        "key_id": r.get::<_, String>(2)?,
                        "prompt_tokens": r.get::<_, i64>(3)?,
                        "completion_tokens": r.get::<_, i64>(4)?,
                        "latency_ms": r.get::<_, i64>(5)?,
                        "status": r.get::<_, i64>(6)?,
                        "stream": r.get::<_, i64>(7)? != 0,
                        "cached": r.get::<_, i64>(8)? != 0,
                    }))
                })
                .map_err(|e| AppError::Internal(format!("账本查询失败: {e}")))?;
            let out: Vec<_> = rows.filter_map(|r| r.ok()).collect();
            Ok(out)
        })
        .await
        .map_err(|e| AppError::Internal(format!("账本任务失败: {e}")))?
    }
}

/// 下游 key → 脱敏标识。
///
/// 仅保留前 4 位 + 末 2 位与长度，**避免暴露足够熵**去猜测完整密钥
/// （此前保留前 6 位，熵偏高）。
pub fn key_id(headers: &axum::http::HeaderMap) -> String {
    let raw = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().trim_start_matches("Bearer ").trim().to_string())
        })
        .unwrap_or_default();
    if raw.is_empty() {
        return "local".into();
    }
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let id = if n <= 6 {
        // 过短：只给长度，不给任何字符
        "***".to_string()
    } else {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[n - 2..].iter().collect();
        format!("{head}**{tail}")
    };
    format!("{id}({n})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_ledger_disabled() {
        let l = Ledger::open("").unwrap();
        assert!(!l.enabled());
        l.record(UsageRecord {
            ts: 1,
            model: "m".into(),
            key_id: "k".into(),
            prompt_tokens: 1,
            completion_tokens: 2,
            latency_ms: 3,
            status: 200,
            stream: false,
            cached: false,
        })
        .await;
        let s = l.stats(None).await.unwrap();
        assert_eq!(s.total_requests, 0);
    }

    #[tokio::test]
    async fn sqlite_roundtrip_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.db");
        let l = Ledger::open(path.to_str().unwrap()).unwrap();
        assert!(l.enabled());
        for i in 0..3u32 {
            l.record(UsageRecord {
                ts: 100 + i as i64,
                model: "deepseek-es".into(),
                key_id: "sk-a**99(10)".into(),
                prompt_tokens: 10,
                completion_tokens: 5,
                latency_ms: 100,
                status: 200,
                stream: false,
                cached: i == 0,
            })
            .await;
        }
        // 再记一条错误
        l.record(UsageRecord {
            ts: 200,
            model: "deepseek-es".into(),
            key_id: "local".into(),
            prompt_tokens: 1,
            completion_tokens: 0,
            latency_ms: 10,
            status: 500,
            stream: true,
            cached: false,
        })
        .await;

        let s = l.stats(None).await.unwrap();
        assert_eq!(s.total_requests, 4);
        assert_eq!(s.total_prompt_tokens, 31);
        assert_eq!(s.total_completion_tokens, 15);
        assert_eq!(s.cache_hits, 1);
        assert_eq!(s.errors, 1);
        assert_eq!(s.avg_latency_ms, 77); // (100*3+10)/4 = 77

        // since 过滤
        let s2 = l.stats(Some(150)).await.unwrap();
        assert_eq!(s2.total_requests, 1);

        let recent = l.recent(2).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0]["status"], 500);
    }

    #[test]
    fn key_id_masks_secret() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-api-key", "sk-supersecretvalue".parse().unwrap());
        let id = key_id(&h);
        assert!(id.starts_with("sk-s"), "前 4 位: {id}");
        assert!(id.contains("**"), "应含掩码: {id}");
        assert!(id.contains("ue"), "末 2 位: {id}");
        assert!(!id.contains("supersecret"), "不得泄露完整密钥: {id}");
        assert!(id.contains("(19)"), "含长度: {id}");
        assert_eq!(key_id(&axum::http::HeaderMap::new()), "local");
    }

    #[test]
    fn key_id_short_key_hides_all() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-api-key", "short".parse().unwrap());
        let id = key_id(&h);
        assert_eq!(id, "***(5)", "过短密钥不得暴露任何字符: {id}");
    }

    #[tokio::test]
    async fn open_failure_degrades_not_fatal() {
        // 无效路径（目录不存在）→ 降级为内存模式，不返回 Err
        let l = Ledger::open("/nonexistent-dir-xyz/sub/u.db").unwrap();
        assert!(!l.enabled(), "打开失败应降级为内存模式");
    }

    // ── R3：账本保留期清理（生产长期运行防无限增长） ──────────────

    #[tokio::test]
    async fn prune_removes_old_records_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.db");
        let l = Ledger::open(path.to_str().unwrap()).unwrap();
        let now = chrono::Utc::now().timestamp();
        let old = now - 40 * 86_400; // 40 天前
                                     // 一条旧记录 + 一条新记录
        for ts in [old, now] {
            l.record(UsageRecord {
                ts,
                model: "deepseek-es".into(),
                key_id: "local".into(),
                prompt_tokens: 1,
                completion_tokens: 1,
                latency_ms: 1,
                status: 200,
                stream: false,
                cached: false,
            })
            .await;
        }
        assert_eq!(l.stats(None).await.unwrap().total_requests, 2);
        // 保留 30 天 → 删掉 40 天前的那条
        let n = l.prune(30).await.unwrap();
        assert_eq!(n, 1, "应删除 1 条旧记录");
        assert_eq!(
            l.stats(None).await.unwrap().total_requests,
            1,
            "新记录应保留"
        );
    }

    #[tokio::test]
    async fn prune_zero_days_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p0.db");
        let l = Ledger::open(path.to_str().unwrap()).unwrap();
        l.record(UsageRecord {
            ts: 1,
            model: "m".into(),
            key_id: "local".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            latency_ms: 1,
            status: 200,
            stream: false,
            cached: false,
        })
        .await;
        assert_eq!(l.prune(0).await.unwrap(), 0, "0 天 = 不清理");
        assert_eq!(l.stats(None).await.unwrap().total_requests, 1);
    }

    #[tokio::test]
    async fn prune_on_memory_ledger_is_noop() {
        let l = Ledger::open("").unwrap();
        assert_eq!(l.prune(30).await.unwrap(), 0);
    }
}
