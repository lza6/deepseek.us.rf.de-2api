//! 会话管理：下游会话（如 OpenAI 的 user / 无状态）→ 上游 session_id / conversation_uuid。
//!
//! 上游多轮上下文按 `conversation_uuid` 维护。映射策略：
//! - 下游提供稳定 key（`user` 字段或请求头 `x-session-id`）→ 用其哈希固定 conversation_uuid
//! - 否则每次请求新建（无状态）

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 简单的内存会话表（key → conversation_uuid + 最近活跃时间）。
pub struct SessionStore {
    inner: Mutex<HashMap<String, SessionEntry>>,
    ttl: Duration,
}

struct SessionEntry {
    conversation_uuid: String,
    last_active: Instant,
}

impl SessionStore {
    pub fn new(ttl: Duration) -> Self {
        SessionStore {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// 获取或创建会话。key 为空时返回一个新的临时会话（不存储）。
    pub fn get_or_create(&self, key: Option<&str>) -> (String, String) {
        let Some(key) = key.filter(|k| !k.trim().is_empty()) else {
            let id = uuid::Uuid::new_v4().to_string();
            return (id.clone(), id);
        };
        let mut map = self.inner.lock().unwrap();
        self.gc(&mut map);
        if let Some(e) = map.get_mut(key) {
            e.last_active = Instant::now();
            return (key.to_string(), e.conversation_uuid.clone());
        }
        let conv = deterministic_uuid(key);
        map.insert(
            key.to_string(),
            SessionEntry {
                conversation_uuid: conv.clone(),
                last_active: Instant::now(),
            },
        );
        (key.to_string(), conv)
    }

    fn gc(&self, map: &mut HashMap<String, SessionEntry>) {
        let ttl = self.ttl;
        map.retain(|_, e| e.last_active.elapsed() < ttl);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

/// 由稳定 key 派生确定性 UUID（同 key 永远同 UUID，实现跨请求上下文）。
fn deterministic_uuid(key: &str) -> String {
    // 用 FNV-1a 生成 128 位确定性数据，再格式化为 UUID v4 形态
    let mut h1: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        h1 ^= *b as u64;
        h1 = h1.wrapping_mul(0x100000001b3);
    }
    let mut h2: u64 = 0x84222325cbf29ce4;
    for b in key.bytes().rev() {
        h2 ^= b as u64;
        h2 = h2.wrapping_mul(0x100000001b3);
    }
    let hex = format!("{:016x}{:016x}", h1, h2);
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[16..19],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_key() {
        let s = SessionStore::new(Duration::from_secs(60));
        let (sid1, c1) = s.get_or_create(Some("user-1"));
        let (sid2, c2) = s.get_or_create(Some("user-1"));
        assert_eq!(c1, c2);
        assert_eq!(sid1, sid2);
    }

    #[test]
    fn different_keys_different_conversations() {
        let s = SessionStore::new(Duration::from_secs(60));
        let (_, c1) = s.get_or_create(Some("user-1"));
        let (_, c2) = s.get_or_create(Some("user-2"));
        assert_ne!(c1, c2);
    }

    #[test]
    fn empty_key_random() {
        let s = SessionStore::new(Duration::from_secs(60));
        let (_, c1) = s.get_or_create(None);
        let (_, c2) = s.get_or_create(None);
        assert_ne!(c1, c2);
        assert_eq!(s.len(), 0); // 不存储
    }

    #[test]
    fn deterministic_uuid_format() {
        let u = deterministic_uuid("abc");
        assert_eq!(u.len(), 36);
        assert_eq!(u.chars().filter(|c| *c == '-').count(), 4);
    }
}
