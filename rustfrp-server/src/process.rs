//! Controller 自身的资源指标（运行时长 / 常驻内存 / CPU 累计时间）。
//!
//! 常驻内存与 CPU 从 `/proc/self` 读取（Linux）；其他平台返回 `None`，由界面显示「—」，
//! 不用外部命令或 `unsafe` 去凑数。

use serde::Serialize;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// 记录进程启动时刻（重复调用无副作用）。
pub fn mark_start() {
    let _ = START.get_or_init(Instant::now);
}

/// 进程资源快照。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessStats {
    /// 运行秒数。
    pub uptime_seconds: u64,
    /// 常驻内存（字节）。
    pub rss_bytes: Option<u64>,
    /// 进程累计 CPU 时间（秒，用户态 + 内核态）；两次读数相除即得利用率。
    pub cpu_seconds: Option<f64>,
}

/// 从 `/proc/<pid>/status` 文本取 `VmRSS`（kB → 字节）。
pub fn parse_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kb.checked_mul(1024)
}

/// 从 `/proc/<pid>/stat` 文本取 utime + stime（时钟节拍，Linux 的 USER_HZ 恒为 100）。
pub fn parse_cpu_seconds(stat: &str) -> Option<f64> {
    // comm 字段可能带空格和括号，必须从最后一个 ')' 之后开始切分。
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let mut f = rest.split_whitespace();
    // ')' 之后依次是 state(3) ppid(4) ... utime(14) stime(15)。
    let utime: u64 = f.nth(11)?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    Some((utime + stime) as f64 / 100.0)
}

/// 采样当前进程。
pub fn sample() -> ProcessStats {
    mark_start();
    let uptime = START.get().map_or(0, |t| t.elapsed().as_secs());
    ProcessStats {
        uptime_seconds: uptime,
        rss_bytes: std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| parse_rss(&s)),
        cpu_seconds: std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|s| parse_cpu_seconds(&s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_files() {
        let status = "Name:\tx\nVmPeak:\t 9 kB\nVmRSS:\t    4096 kB\nThreads:\t3\n";
        assert_eq!(parse_rss(status), Some(4096 * 1024));
        assert_eq!(parse_rss("Name:\tx\n"), None);
        assert_eq!(parse_rss("VmRSS:\tabc kB\n"), None);
        assert_eq!(parse_rss("VmRSS:\n"), None);
        let stat = "123 (my (weird) app) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0 20";
        assert_eq!(parse_cpu_seconds(stat), Some(3.0));
        for bad in [
            "",
            "123 no-paren",
            "1 (a) S 1 2",
            "1 (a) S 1 2 3 4 5 6 7 8 9 10 x 5",
        ] {
            assert_eq!(parse_cpu_seconds(bad), None, "{bad}");
        }
    }

    #[test]
    fn sample_is_consistent_with_the_platform() {
        let s = sample();
        if cfg!(target_os = "linux") {
            assert!(s.rss_bytes.is_some_and(|b| b > 0));
            assert!(s.cpu_seconds.is_some_and(|c| c >= 0.0));
        } else {
            assert_eq!((s.rss_bytes, s.cpu_seconds), (None, None));
        }
        assert!(sample().uptime_seconds >= s.uptime_seconds);
    }
}
