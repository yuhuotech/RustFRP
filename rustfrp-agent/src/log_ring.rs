//! 固定容量日志快照，秘密相关消息整条隐藏。
#[cfg(test)]
use crate::log::Logger;
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Arc;
use std::sync::{Mutex, PoisonError};

/// 最多保留的日志条数。
pub const CAPACITY: usize = 128;
/// 每条 UTF-8 字节上限。
pub const LINE_BYTES: usize = 512;

/// 有界内存日志，不保存流量载荷。
#[derive(Default)]
struct State {
    cursor: u64,
    lines: VecDeque<Entry>,
}

struct Entry {
    cursor: u64,
    line: String,
}

#[derive(Default)]
pub struct LogRing(Mutex<State>);
impl LogRing {
    /// 取一份最多 64 KiB 的快照。
    pub fn snapshot(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .lines
            .iter()
            .map(|entry| entry.line.clone())
            .collect()
    }

    /// 返回给定游标之后的新日志；`gap` 表示游标失效或旧日志已被环形缓冲淘汰。
    pub fn snapshot_after(&self, after: u64) -> (u64, bool, Vec<String>) {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let oldest = state
            .lines
            .front()
            .map_or(state.cursor.saturating_add(1), |entry| entry.cursor);
        let gap = after > state.cursor || after.saturating_add(1) < oldest;
        // A cursor from the previous process must replay this process's bounded tail.
        let start = if after > state.cursor { 0 } else { after };
        let lines = state
            .lines
            .iter()
            .filter(|entry| entry.cursor > start)
            .map(|entry| entry.line.clone())
            .collect();
        (state.cursor, gap, lines)
    }
    pub(crate) fn push(&self, line: &str) {
        // 检查原始文本但不复制整条消息：日志调用方可能传入很长的错误。
        let secret = [
            "token",
            "password",
            "secret",
            "authorization",
            "private key",
        ]
        .iter()
        .any(|word| {
            line.as_bytes()
                .windows(word.len())
                .any(|part| part.eq_ignore_ascii_case(word.as_bytes()))
        });
        let line = if secret {
            "[redacted: secret-related log]"
        } else {
            line.trim_end()
        };
        let mut end = line.len().min(LINE_BYTES);
        while !line.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let bounded = line
            .get(..end)
            .unwrap_or_default()
            .replace(['\r', '\n'], " ");
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.lines.len() == CAPACITY {
            state.lines.pop_front();
        }
        state.cursor = state.cursor.saturating_add(1);
        let cursor = state.cursor;
        state.lines.push_back(Entry {
            cursor,
            line: bounded,
        });
    }
}
#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn bounded_unicode_secret_redaction_and_order() {
        let ring = Arc::new(LogRing::default());
        for n in 0..CAPACITY + 10 {
            ring.push(&n.to_string());
        }
        let lines = ring.snapshot();
        assert_eq!(lines.len(), CAPACITY);
        assert_eq!(lines.first().unwrap(), "10");
        assert_eq!(lines.last().unwrap(), "137");
        ring.push(&"中".repeat(300));
        let lines = ring.snapshot();
        let last = lines.last().unwrap();
        assert!(last.len() <= LINE_BYTES);
        assert_eq!(last.len() % 3, 0);
        for key in [
            "TOKEN",
            "password",
            "secret",
            "Authorization",
            "private key",
        ] {
            ring.push(&format!("{key}=do-not-expose"));
            assert_eq!(
                ring.snapshot().last().unwrap(),
                "[redacted: secret-related log]"
            );
        }
        ring.push("hello\nworld\r\n");
        assert_eq!(ring.snapshot().last().unwrap(), "hello world");
        let logger = Logger::silent().with_ring(ring.clone());
        logger.info("not logged at error level");
        assert_eq!(ring.snapshot().last().unwrap(), "hello world");
        logger.error("logged");
        assert_eq!(ring.snapshot().last().unwrap(), "logged");
    }

    #[test]
    fn tail_cursor_returns_only_new_lines_and_signals_eviction() {
        let ring = LogRing::default();
        ring.push("first");
        let (cursor, gap, first) = ring.snapshot_after(0);
        assert_eq!((cursor, gap, first), (1, false, vec!["first".to_string()]));
        ring.push("second");
        assert_eq!(
            ring.snapshot_after(cursor),
            (2, false, vec!["second".into()])
        );
        assert_eq!(
            ring.snapshot_after(99),
            (2, true, vec!["first".into(), "second".into()])
        );
        for n in 0..CAPACITY {
            ring.push(&n.to_string());
        }
        let (_, gap, items) = ring.snapshot_after(cursor);
        assert!(gap);
        assert_eq!(items.len(), CAPACITY);
    }
}
