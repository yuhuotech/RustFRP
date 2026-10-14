//! Infrequent native commands have bounded output, deadlines and owned child cleanup.
use crate::Error;
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
}

pub fn run(program: &str, args: &[String], deadline: Duration) -> Result<Output, Error> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(Error::io_context("无法执行系统集成命令"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::io("系统集成命令超时或失败，请核对实际状态"));
            }
        }
    };
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(Error::io_context("无法读取系统命令结果"))?;
    }
    if bytes.len() > 4096 {
        return Err(Error::invalid("系统命令输出超过限制"));
    }
    Ok(Output {
        success: status.success(),
        code: status.code(),
        stdout: String::from_utf8(bytes)
            .map_err(Error::invalid_context("系统命令结果不是有效文本"))?,
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn command_results_are_bounded_and_our_timed_out_child_is_reaped() {
        let run_sh = |script: &str, timeout| run("/bin/sh", &["-c".into(), script.into()], timeout);
        let good = run_sh("printf '42'", Duration::from_secs(2)).unwrap();
        assert!(good.success);
        assert_eq!(good.code, Some(0));
        assert_eq!(good.stdout, "42");
        assert_eq!(
            run_sh("exit 7", Duration::from_secs(2)).unwrap().code,
            Some(7)
        );
        assert!(run_sh("printf '%05000d' 0", Duration::from_secs(2)).is_err());
        assert!(run_sh("exec /bin/sleep 2", Duration::from_millis(30)).is_err());
        assert!(run("/not/a/program", &[], Duration::from_secs(1)).is_err());
        assert!(run_sh("printf '\\377'", Duration::from_secs(2)).is_err());
    }
}
