//! 请求级响应缓存（P3-2）：相同 (model, prompt, session) 短时复用，降低上游压力。
//!
//! 设计：
//! - 内存 LRU + TTL（不持久化；进程重启即失效）。
//! - key = FNV-1a(model + '\x1f' + prompt)；**不包含** conversation_uuid，
//!   因为上游多轮上下文由 conversation_uuid 维护，缓存命中必须保证语义等价——
//!   故仅缓存**无历史**（单轮）请求，且调用方需传入 `session=None` 场景。
//! - 仅缓存成功且长度 ≥ `min_chars` 的结果，避免缓存噪声/错误。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 一条缓存条目。
struct Entry {
    value: String,
    stored: Instant,
    /// 最近访问（LRU 用）
    last_hit: Instant,
}

/// 响应缓存。
pub struct ResponseCache {
    inner: Mutex<HashMap<u64, Entry>>,
    ttl: Duration,
    max: usize,
    min_chars: usize,
}

impl ResponseCache {
    /// `ttl_secs = 0` → 禁用（`get` 恒 None）。
    pub fn new(ttl_secs: u64, max: usize, min_chars: usize) -> Self {
        ResponseCache {
            inner: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
            max: max.max(1),
            min_chars,
        }
    }

    pub fn enabled(&self) -> bool {
        !self.ttl.is_zero()
    }

    /// 计算缓存键（FNV-1a 64 位）。
    pub fn key(model: &str, prompt: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in model
            .as_bytes()
            .iter()
            .chain(b"\x1f")
            .chain(prompt.as_bytes())
        {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// 查询（未过期则返回并刷新 LRU）。
    pub fn get(&self, key: u64) -> Option<String> {
        if !self.enabled() {
            return None;
        }
        let mut m = self.inner.lock().unwrap();
        let e = m.get_mut(&key)?;
        if e.stored.elapsed() >= self.ttl {
            m.remove(&key);
            return None;
        }
        e.last_hit = Instant::now();
        Some(e.value.clone())
    }

    /// 写入（长度不足或禁用则忽略）。
    pub fn put(&self, key: u64, value: String) {
        if !self.enabled() || value.chars().count() < self.min_chars {
            return;
        }
        let mut m = self.inner.lock().unwrap();
        // 过期清理 + 容量控制（淘汰最久未访问）
        let now = Instant::now();
        m.retain(|_, e| now.duration_since(e.stored) < self.ttl);
        if m.len() >= self.max && !m.contains_key(&key) {
            if let Some(k) = m.iter().min_by_key(|(_, e)| e.last_hit).map(|(k, _)| *k) {
                m.remove(&k);
            }
        }
        m.insert(
            key,
            Entry {
                value,
                stored: now,
                last_hit: now,
            },
        );
    }

    /// 当前条目数（测试/诊断）。
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_ttl_zero() {
        let c = ResponseCache::new(0, 10, 0);
        assert!(!c.enabled());
        c.put(ResponseCache::key("m", "p"), "hello".into());
        assert!(c.get(ResponseCache::key("m", "p")).is_none());
    }

    #[test]
    fn hit_and_miss() {
        let c = ResponseCache::new(60, 10, 0);
        let k = ResponseCache::key("m", "hi");
        assert!(c.get(k).is_none());
        c.put(k, "world".into());
        assert_eq!(c.get(k).as_deref(), Some("world"));
    }

    #[test]
    fn key_distinguishes_model_and_prompt() {
        assert_ne!(
            ResponseCache::key("a", "bc"),
            ResponseCache::key("ab", "c"),
            "分隔符必须防止拼接歧义"
        );
        assert_eq!(ResponseCache::key("m", "p"), ResponseCache::key("m", "p"));
    }

    #[test]
    fn min_chars_skips_short() {
        let c = ResponseCache::new(60, 10, 10);
        let k = ResponseCache::key("m", "p");
        c.put(k, "short".into());
        assert!(c.get(k).is_none(), "过短结果不应缓存");
        c.put(k, "this is long enough".into());
        assert!(c.get(k).is_some());
    }

    #[test]
    fn evicts_when_full() {
        let c = ResponseCache::new(60, 2, 0);
        c.put(1, "a".into());
        c.put(2, "b".into());
        // 访问 1 使其更新
        let _ = c.get(1);
        c.put(3, "c".into());
        assert!(c.len() <= 2, "容量未受限: {}", c.len());
        assert!(c.get(2).is_none(), "最久未访问的应被淘汰");
    }

    #[test]
    fn ttl_expiry() {
        let c = ResponseCache::new(1, 10, 0);
        let k = ResponseCache::key("m", "p");
        c.put(k, "v".into());
        assert!(c.get(k).is_some());
        // 直接构造过期：用极小 ttl 不便等待，改测 retain 逻辑
        std::thread::sleep(Duration::from_millis(1100));
        assert!(c.get(k).is_none(), "TTL 到期应失效");
    }
}
