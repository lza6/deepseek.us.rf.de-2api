//! 断线重连（P3-5）。
//!
//! ## 上游能力（已实测）
//!
//! 上游 deepseek.es 的 SSE **不发送 `id:` 行**，且 `cache_key` 为**一次性**消费
//! （`aipkit_cache_sse_message` 产出的 key 只能流一次）。因此：
//!
//! - **无法**实现真正的 `Last-Event-ID` 续传（上游不提供事件序号，也无法重放）。
//! - 可实现的是**网关侧断线恢复**：网关向下游发出的每个 SSE 事件带 `id:`（单调递增），
//!   并在内存中保留**本次响应的完整事件缓冲**（`ReplayBuffer`），
//!   当客户端带 `Last-Event-ID` 重连时，网关**从缓冲回放**尚未收到的部分。
//!
//! 限制（如实披露，不假装支持）：
//! - 回放依赖网关进程内存；进程重启或缓冲淘汰后无法续传。
//! - 回放仅在**同一次上游响应**内有效（上游流结束后缓冲保留 TTL，期间可回放）。
//! - 若上游流已结束且缓冲过期，返回 `409`，提示客户端重新发起。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 一次响应的事件缓冲（供断线重放）。
#[derive(Debug, Clone)]
pub struct ReplayEntry {
    /// SSE 事件序号（从 1 开始）
    pub seq: u64,
    /// 完整 SSE 帧（含 `id:`/`event:`/`data:`），不含结尾空行
    pub frame: String,
}

struct Buffer {
    entries: Vec<ReplayEntry>,
    updated: Instant,
    /// 是否已结束（收到 done）
    finished: bool,
}

/// 断线重放缓冲池（按响应 id 索引）。可克隆（共享内部状态）。
#[derive(Clone)]
pub struct ReplayStore {
    inner: Arc<Mutex<HashMap<String, Buffer>>>,
    ttl: Duration,
    max_entries_per_response: usize,
    max_responses: usize,
}

impl ReplayStore {
    pub fn new(ttl_secs: u64, max_responses: usize, max_entries_per_response: usize) -> Self {
        ReplayStore {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl: Duration::from_secs(ttl_secs.max(1)),
            max_entries_per_response: max_entries_per_response.max(1),
            max_responses: max_responses.max(1),
        }
    }

    /// 追加一个事件帧，返回其序号。
    pub fn push(&self, resp_id: &str, frame: String) -> u64 {
        let mut m = self.inner.lock().unwrap();
        self.gc(&mut m);
        if m.len() >= self.max_responses && !m.contains_key(resp_id) {
            if let Some(k) = m
                .iter()
                .min_by_key(|(_, b)| b.updated)
                .map(|(k, _)| k.clone())
            {
                m.remove(&k);
            }
        }
        let b = m.entry(resp_id.to_string()).or_insert_with(|| Buffer {
            entries: Vec::new(),
            updated: Instant::now(),
            finished: false,
        });
        let seq = b.entries.len() as u64 + 1;
        if b.entries.len() < self.max_entries_per_response {
            b.entries.push(ReplayEntry { seq, frame });
        }
        b.updated = Instant::now();
        seq
    }

    /// 标记响应结束。
    pub fn finish(&self, resp_id: &str) {
        let mut m = self.inner.lock().unwrap();
        if let Some(b) = m.get_mut(resp_id) {
            b.finished = true;
            b.updated = Instant::now();
        }
    }

    /// 从 `after_seq` 之后回放（不含 `after_seq` 本身）。
    ///
    /// 返回 `None` = 无此响应/已过期（调用方应返回 409）。
    pub fn replay(&self, resp_id: &str, after_seq: u64) -> Option<Vec<ReplayEntry>> {
        let m = self.inner.lock().unwrap();
        let b = m.get(resp_id)?;
        if b.updated.elapsed() >= self.ttl {
            return None;
        }
        Some(
            b.entries
                .iter()
                .filter(|e| e.seq > after_seq)
                .cloned()
                .collect(),
        )
    }

    /// 该响应是否已结束。
    pub fn is_finished(&self, resp_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(resp_id)
            .map(|b| b.finished)
            .unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn gc(&self, m: &mut HashMap<String, Buffer>) {
        let ttl = self.ttl;
        m.retain(|_, b| b.updated.elapsed() < ttl);
    }
}

/// 从 `Last-Event-ID` 头解析上次收到的序号。
pub fn parse_last_event_id(headers: &axum::http::HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_replay() {
        let s = ReplayStore::new(60, 10, 100);
        s.push("r1", "id: 1\ndata: a".into());
        s.push("r1", "id: 2\ndata: b".into());
        s.push("r1", "id: 3\ndata: c".into());
        let r = s.replay("r1", 1).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].seq, 2);
        assert_eq!(r[1].seq, 3);
        // after=0 → 全部
        assert_eq!(s.replay("r1", 0).unwrap().len(), 3);
        // after=3 → 空
        assert!(s.replay("r1", 3).unwrap().is_empty());
    }

    #[test]
    fn unknown_response_returns_none() {
        let s = ReplayStore::new(60, 10, 100);
        assert!(s.replay("nope", 0).is_none());
    }

    #[test]
    fn seq_is_monotonic_per_response() {
        let s = ReplayStore::new(60, 10, 100);
        assert_eq!(s.push("a", "x".into()), 1);
        assert_eq!(s.push("a", "y".into()), 2);
        assert_eq!(s.push("b", "z".into()), 1);
    }

    #[test]
    fn finish_flag() {
        let s = ReplayStore::new(60, 10, 100);
        s.push("r", "x".into());
        assert!(!s.is_finished("r"));
        s.finish("r");
        assert!(s.is_finished("r"));
    }

    #[test]
    fn caps_entries_per_response() {
        let s = ReplayStore::new(60, 10, 3);
        for i in 0..10 {
            s.push("r", format!("id: {i}"));
        }
        // 只保留前 3 条，但 seq 仍递增
        let r = s.replay("r", 0).unwrap();
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn parse_header() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("last-event-id", "42".parse().unwrap());
        assert_eq!(parse_last_event_id(&h), Some(42));
        h.insert("last-event-id", "abc".parse().unwrap());
        assert_eq!(parse_last_event_id(&h), None);
        assert_eq!(parse_last_event_id(&axum::http::HeaderMap::new()), None);
    }
}
