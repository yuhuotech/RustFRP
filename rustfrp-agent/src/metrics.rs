//! 有界代理计数注册表；热更新沿用计数，停止后保留最终值供采样。
use rustfrp_core::pipe::{Traffic, TrafficSnapshot};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// 单节点同时保留的计数条目上限。
pub const MAX_PROXIES: usize = 1024;

struct Entry {
    traffic: Arc<Traffic>,
    active: bool,
    generation: u64,
    kind: String,
}

/// 一次代理实例采样。
pub struct Sample {
    pub name: String,
    pub kind: String,
    pub active: bool,
    pub generation: u64,
    pub traffic: TrafficSnapshot,
}

/// 代理计数注册表。
#[derive(Default)]
pub struct Metrics {
    entries: Mutex<HashMap<String, Entry>>,
    total: Arc<Traffic>,
    next: std::sync::atomic::AtomicU64,
}

impl Metrics {
    /// 节点累计计数，移除代理不会使节点累计值回退。
    pub fn total(&self) -> Arc<Traffic> {
        self.total.clone()
    }
    pub(crate) fn claim(&self, name: &str) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = entries.get_mut(name) {
            if e.active {
                return false;
            }
            e.active = true;
            return true;
        }
        if entries.len() >= MAX_PROXIES {
            return false;
        }
        entries.insert(
            name.into(),
            Entry {
                traffic: Arc::new(Traffic::child(self.total.clone())),
                active: true,
                generation: self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                kind: String::new(),
            },
        );
        true
    }
    pub(crate) fn set_kind(&self, name: &str, kind: &str) {
        if let Some(entry) = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(name)
        {
            entry.kind.clear();
            entry.kind.push_str(kind);
        }
    }
    pub(crate) fn release(&self, name: &str) {
        if let Some(e) = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(name)
        {
            e.active = false;
        }
    }
    pub(crate) fn counter(&self, name: &str) -> Option<Arc<Traffic>> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(|e| e.traffic.clone())
    }
    /// 最多 1024 个条目；停用条目也返回，防止丢失最后一段流量。
    pub fn snapshot(&self) -> Vec<(String, TrafficSnapshot)> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(name, e)| (name.clone(), e.traffic.snapshot()))
            .collect()
    }
    /// 带实例编号的有界采样，同名代理回收后重建不会共享去重游标。
    pub fn samples(&self) -> Vec<Sample> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(name, e)| Sample {
                name: name.clone(),
                kind: e.kind.clone(),
                active: e.active,
                generation: e.generation,
                traffic: e.traffic.snapshot(),
            })
            .collect()
    }
    /// 独立模式无历史消费者，回收无人持有的停止条目；节点累计仍保留。
    pub fn sweep_stopped(&self) -> usize {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let before = entries.len();
        entries.retain(|_, e| e.active || Arc::strong_count(&e.traffic) > 1);
        before - entries.len()
    }
    /// 持久化成功后确认停止条目；仅在无转发持有计数且快照未变化时移除。
    pub fn acknowledge(&self, name: &str, saved: &TrafficSnapshot) {
        self.acknowledge_inner(name, None, saved);
    }
    /// 持久化确认同时校验实例编号，旧样本不会删除新实例。
    pub fn acknowledge_sample(&self, saved: &Sample) {
        self.acknowledge_inner(&saved.name, Some(saved.generation), &saved.traffic);
    }
    fn acknowledge_inner(&self, name: &str, generation: Option<u64>, saved: &TrafficSnapshot) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.get(name).is_some_and(|e| {
            generation.is_none_or(|g| g == e.generation)
                && !e.active
                && Arc::strong_count(&e.traffic) == 1
                && e.traffic.snapshot() == *saved
        }) {
            entries.remove(name);
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn isolated_counts_survive_restart_and_acknowledge_only_final_values() {
        let metrics = Metrics::default();
        assert!(metrics.claim("a"));
        assert!(!metrics.claim("a"));
        assert!(metrics.claim("b"));
        assert!(metrics.counter("missing").is_none());
        metrics.set_kind("missing", "tcp");
        assert!(metrics.counter("missing").is_none());
        let a = metrics.counter("a").unwrap();
        let b = metrics.counter("b").unwrap();
        let connection = a.connection();
        a.record_in(7);
        a.record_out(11);
        b.record_in(3);
        assert_eq!((a.snapshot().bytes_in, b.snapshot().bytes_in), (7, 3));
        assert_eq!(
            (
                metrics.total().snapshot().bytes_in,
                metrics.total().snapshot().bytes_out
            ),
            (10, 11)
        );
        assert_eq!(metrics.total().snapshot().connections, 1);
        let saved = a.snapshot();
        metrics.acknowledge("a", &saved);
        assert!(metrics.counter("a").is_some());
        metrics.release("a");
        metrics.acknowledge("a", &saved);
        assert!(metrics.counter("a").is_some());
        drop(connection);
        assert_eq!(metrics.total().snapshot().connections, 0);
        assert!(metrics.claim("a"));
        assert_eq!(metrics.counter("a").unwrap().snapshot().bytes_in, 7);
        metrics.release("a");
        let final_value = a.snapshot();
        drop(a);
        metrics.acknowledge("a", &saved);
        assert!(metrics.counter("a").is_some());
        metrics.acknowledge("a", &final_value);
        assert!(metrics.counter("a").is_none());
        metrics.acknowledge("missing", &saved);
        metrics.release("missing");
        assert_eq!(metrics.snapshot().len(), 1);
        assert_eq!(metrics.total().snapshot().bytes_in, 10);
    }

    #[test]
    fn capacity_is_bounded_and_reclaimed_after_persistence() {
        let metrics = Metrics::default();
        for n in 0..MAX_PROXIES {
            assert!(metrics.claim(&n.to_string()));
        }
        assert!(!metrics.claim("overflow"));
        assert_eq!(metrics.snapshot().len(), MAX_PROXIES);
        let mut saved = metrics
            .samples()
            .into_iter()
            .find(|s| s.name == "0")
            .unwrap();
        metrics.release("0");
        saved.generation = u64::MAX;
        metrics.acknowledge_sample(&saved);
        assert!(metrics.counter("0").is_some());
        saved.generation = 0;
        metrics.acknowledge_sample(&saved);
        metrics.acknowledge("0", &TrafficSnapshot::default());
        assert!(metrics.claim("overflow"));
        let samples = metrics.samples();
        assert_eq!(samples.len(), MAX_PROXIES);
        assert_eq!(
            samples
                .iter()
                .find(|s| s.name == "overflow")
                .unwrap()
                .generation,
            MAX_PROXIES as u64
        );
        assert_eq!(metrics.snapshot().len(), MAX_PROXIES);
    }
    #[test]
    fn standalone_sweep_preserves_live_transfers_and_node_totals() {
        let metrics = Metrics::default();
        assert!(metrics.claim("live"));
        assert!(metrics.claim("retired"));
        let traffic = metrics.counter("retired").unwrap();
        let connection = traffic.connection();
        traffic.record_in(7);
        traffic.record_out(11);
        metrics.release("retired");
        assert_eq!(metrics.sweep_stopped(), 0);
        drop(connection);
        drop(traffic);
        assert_eq!(metrics.sweep_stopped(), 1);
        assert_eq!(metrics.snapshot().len(), 1);
        assert!(metrics.counter("live").is_some());
        let total = metrics.total().snapshot();
        assert_eq!(
            (
                total.bytes_in,
                total.bytes_out,
                total.connections,
                total.connections_total
            ),
            (7, 11, 0, 1)
        );
        for n in 0..MAX_PROXIES * 2 {
            let name = n.to_string();
            assert!(metrics.claim(&name));
            metrics.release(&name);
            assert_eq!(metrics.sweep_stopped(), 1);
        }
        assert_eq!(metrics.snapshot().len(), 1);
    }
}
