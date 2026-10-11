//! 极简日志：不引入 `tracing`，按级别写到 stderr 或文件（对应 frp 的 `log.to` / `log.level`）。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 最详细。
    Trace,
    /// 调试。
    Debug,
    /// 信息。
    Info,
    /// 警告。
    Warn,
    /// 错误。
    Error,
}

impl Level {
    /// 解析 frp 的级别名；未知值按 `info`。
    pub fn parse(s: &str) -> Level {
        match s {
            "trace" => Level::Trace,
            "debug" => Level::Debug,
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Level::Trace => "T",
            Level::Debug => "D",
            Level::Info => "I",
            Level::Warn => "W",
            Level::Error => "E",
        }
    }
}

/// 日志器，可克隆共享。
#[derive(Clone)]
pub struct Logger {
    level: Level,
    sink: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Logger {
    /// 写到任意 `Write`（测试用）。
    pub fn new(level: Level, sink: Box<dyn Write + Send>) -> Self {
        Self {
            level,
            sink: Arc::new(Mutex::new(sink)),
        }
    }

    /// 按 frp 的 `log.to` 创建：`console` 写 stderr，否则追加到文件。
    pub fn from_target(to: &str, level: Level) -> std::io::Result<Self> {
        let sink: Box<dyn Write + Send> = if to == "console" {
            Box::new(std::io::stderr())
        } else {
            Box::new(OpenOptions::new().create(true).append(true).open(to)?)
        };
        Ok(Self::new(level, sink))
    }

    /// 丢弃所有输出。
    pub fn silent() -> Self {
        Self::new(Level::Error, Box::new(std::io::sink()))
    }

    /// 输出一行。
    pub fn log(&self, level: Level, msg: &str) {
        if level < self.level {
            return;
        }
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(sink, "[{}] {msg}", level.tag());
    }

    /// 信息。
    pub fn info(&self, msg: &str) {
        self.log(Level::Info, msg);
    }
    /// 警告。
    pub fn warn(&self, msg: &str) {
        self.log(Level::Warn, msg);
    }
    /// 错误。
    pub fn error(&self, msg: &str) {
        self.log(Level::Error, msg);
    }
    /// 调试。
    pub fn debug(&self, msg: &str) {
        self.log(Level::Debug, msg);
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn level_parsing_and_filtering() {
        for (s, l) in [
            ("trace", Level::Trace),
            ("debug", Level::Debug),
            ("info", Level::Info),
            ("warn", Level::Warn),
            ("error", Level::Error),
            ("???", Level::Info),
        ] {
            assert_eq!(Level::parse(s), l);
        }
        let buf = Buf::default();
        let log = Logger::new(Level::Warn, Box::new(buf.clone()));
        log.debug("d");
        log.info("i");
        log.warn("w");
        log.error("e");
        log.log(Level::Trace, "t");
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(out, "[W] w\n[E] e\n");
    }

    #[test]
    fn all_tags() {
        let buf = Buf::default();
        let log = Logger::new(Level::Trace, Box::new(buf.clone()));
        for l in [
            Level::Trace,
            Level::Debug,
            Level::Info,
            Level::Warn,
            Level::Error,
        ] {
            log.log(l, "x");
        }
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(out, "[T] x\n[D] x\n[I] x\n[W] x\n[E] x\n");
    }

    #[test]
    fn targets() {
        Logger::from_target("console", Level::Info)
            .unwrap()
            .info("to stderr");
        let path = std::env::temp_dir().join(format!("rustfrp-log-{}.txt", std::process::id()));
        let log = Logger::from_target(path.to_str().unwrap(), Level::Info).unwrap();
        log.info("hello");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[I] hello\n");
        std::fs::remove_file(&path).unwrap();
        assert!(Logger::from_target("/nonexistent-dir/x/y.log", Level::Info).is_err());
        Logger::silent().error("nothing");
    }
}
