//! Login startup definitions preserve the existing desktop contract on all three platforms.
use crate::{
    config::{AtomicReplace, Replace},
    process, Error,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Linux,
    Windows,
}
impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}
pub struct Startup {
    pub platform: Platform,
    pub home: PathBuf,
    pub executable: PathBuf,
}
pub trait Runner {
    fn run(&self, args: &[String]) -> Result<bool, Error>;
}
pub struct TaskScheduler;
impl Runner for TaskScheduler {
    fn run(&self, args: &[String]) -> Result<bool, Error> {
        Ok(process::run("schtasks.exe", args, Duration::from_secs(5))?.success)
    }
}
impl Startup {
    fn check(&self) -> Result<(), Error> {
        if !self.home.is_absolute()
            || !self.executable.is_absolute()
            || self.executable.as_os_str().len() > 4096
            || self
                .executable
                .to_string_lossy()
                .chars()
                .any(char::is_control)
        {
            return Err(Error::invalid("自启动必须使用当前用户目录和应用的绝对路径"));
        }
        Ok(())
    }
    pub fn path(&self) -> PathBuf {
        if self.platform == Platform::Mac {
            self.home
                .join("Library/LaunchAgents/com.rustfrp.desktop.plist")
        } else {
            self.home.join(".config/autostart/rustfrp.desktop")
        }
    }
    pub fn definition(&self) -> Result<String, Error> {
        self.check()?;
        let executable = self.executable.to_string_lossy();
        if self.platform == Platform::Mac {
            let escaped = executable
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;");
            Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>com.rustfrp.desktop</string><key>ProgramArguments</key><array><string>{escaped}</string><string>--startup-to-tray</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>"))
        } else if self.platform == Platform::Linux {
            // Desktop Entry Exec quoting is distinct from shell quoting; %% is a literal percent.
            let escaped = executable
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('`', "\\`")
                .replace('$', "\\$")
                .replace('%', "%%");
            Ok(format!("[Desktop Entry]\nType=Application\nName=RustFRP\nExec=\"{escaped}\" --startup-to-tray\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"))
        } else {
            Err(Error::invalid("Windows 使用登录计划任务"))
        }
    }
    pub fn enabled(&self, runner: &impl Runner) -> Result<bool, Error> {
        self.check()?;
        if self.platform == Platform::Windows {
            return runner.run(&["/Query".into(), "/TN".into(), "RustFRP-Startup".into()]);
        }
        match std::fs::symlink_metadata(self.path()) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(Error::invalid("自启动定义须为普通文件")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(Error::io("无法读取自启动定义")),
        }
    }
    pub fn set(&self, enabled: bool, runner: &impl Runner) -> Result<bool, Error> {
        self.check()?;
        if self.platform == Platform::Windows {
            if !enabled && !self.enabled(runner)? {
                return Ok(false);
            }
            let args = if enabled {
                vec![
                    "/Create".into(),
                    "/SC".into(),
                    "ONLOGON".into(),
                    "/TN".into(),
                    "RustFRP-Startup".into(),
                    "/TR".into(),
                    format!("\"{}\" --startup-to-tray", self.executable.display()),
                    "/F".into(),
                ]
            } else {
                vec![
                    "/Delete".into(),
                    "/TN".into(),
                    "RustFRP-Startup".into(),
                    "/F".into(),
                ]
            };
            if !runner.run(&args)? {
                return Err(Error::io("Windows 自启动设置失败，请核对计划任务权限"));
            }
            return Ok(enabled);
        }
        let path = self.path();
        if !enabled {
            if self.enabled(runner)? {
                std::fs::remove_file(path).map_err(Error::io_context("无法删除自启动定义"))?;
            }
            return Ok(false);
        }
        self.enabled(runner)?;
        let parent = path
            .parent()
            .ok_or_else(|| Error::invalid("自启动路径无效"))?;
        std::fs::create_dir_all(parent).map_err(Error::io_context("无法创建自启动目录"))?;
        let mut stage = tempfile::NamedTempFile::new_in(parent)
            .map_err(Error::io_context("无法创建私有自启动暂存文件"))?;
        stage
            .write_all(self.definition()?.as_bytes())
            .and_then(|()| stage.as_file().sync_all())
            .map_err(Error::io_context("无法写入自启动定义"))?;
        let stage = stage.into_temp_path();
        AtomicReplace
            .apply(&stage, &path)
            .map_err(Error::io_context("无法原子应用自启动定义"))?;
        Ok(true)
    }
}

pub fn current(executable: &Path) -> Result<Startup, Error> {
    let platform = Platform::current();
    let home = std::env::var_os(if platform == Platform::Windows {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .ok_or_else(|| Error::io("无法定位当前用户目录"))?;
    let startup = Startup {
        platform,
        home: PathBuf::from(home),
        executable: executable.into(),
    };
    startup.check()?;
    Ok(startup)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Fake {
        calls: RefCell<Vec<Vec<String>>>,
        result: bool,
    }
    impl Runner for Fake {
        fn run(&self, args: &[String]) -> Result<bool, Error> {
            self.calls.borrow_mut().push(args.to_vec());
            Ok(self.result)
        }
    }
    #[test]
    fn unix_definitions_are_private_escaped_atomic_and_disable_without_terminating_the_app() {
        let home = tempfile::tempdir().unwrap();
        let fake = Fake {
            calls: RefCell::new(vec![]),
            result: true,
        };
        for platform in [Platform::Mac, Platform::Linux] {
            let startup = Startup {
                platform,
                home: home.path().into(),
                executable: home.path().join("R&<\"'$`%\\ app"),
            };
            assert!(!startup.enabled(&fake).unwrap());
            assert!(startup.set(true, &fake).unwrap());
            let content = std::fs::read_to_string(startup.path()).unwrap();
            assert!(content.contains("--startup-to-tray"));
            if platform == Platform::Mac {
                assert!(content.contains("R&amp;&lt;&quot;&apos;"));
            } else {
                assert!(content.contains("\\$\\`%%\\\\ app"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(startup.path())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            assert!(startup.enabled(&fake).unwrap());
            startup.set(true, &fake).unwrap();
            assert!(!startup.set(false, &fake).unwrap());
            assert!(!startup.set(false, &fake).unwrap());
            std::fs::create_dir(startup.path()).unwrap();
            assert!(startup.set(true, &fake).is_err());
            assert!(startup.enabled(&fake).is_err());
        }
        assert!(fake.calls.borrow().is_empty());
    }
    #[test]
    fn windows_task_is_typed_and_failures_do_not_report_success() {
        let home = tempfile::tempdir().unwrap();
        let startup = Startup {
            platform: Platform::Windows,
            home: home.path().into(),
            executable: home.path().join("app with spaces.exe"),
        };
        let fake = Fake {
            calls: RefCell::new(vec![]),
            result: true,
        };
        assert!(startup.enabled(&fake).unwrap());
        startup.set(true, &fake).unwrap();
        startup.set(false, &fake).unwrap();
        assert!(fake
            .calls
            .borrow()
            .iter()
            .any(|args| args.contains(&format!(
                "\"{}\" --startup-to-tray",
                startup.executable.display()
            ))));
        let fail = Fake {
            calls: RefCell::new(vec![]),
            result: false,
        };
        assert!(startup.set(true, &fail).is_err());
        assert!(!startup.set(false, &fail).unwrap());
        assert!(startup.definition().is_err());
        let invalid = Startup {
            executable: PathBuf::from("relative"),
            ..startup
        };
        assert!(invalid.enabled(&fake).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn inaccessible_autostart_directory_preserves_an_existing_definition_and_never_calls_a_scheduler(
    ) {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let fake = Fake {
            calls: RefCell::new(vec![]),
            result: true,
        };
        let startup = Startup {
            platform: Platform::Linux,
            home: home.path().into(),
            executable: home.path().join("app"),
        };
        std::fs::write(home.path().join(".config"), b"unrelated config").unwrap();
        assert!(startup.set(true, &fake).is_err());
        assert_eq!(
            std::fs::read(home.path().join(".config")).unwrap(),
            b"unrelated config"
        );
        std::fs::remove_file(home.path().join(".config")).unwrap();
        startup.set(true, &fake).unwrap();
        let path = startup.path();
        let parent = path.parent().unwrap();
        let original = std::fs::read(&path).unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(startup.set(false, &fake).is_err());
        assert!(startup.set(true, &fake).is_err());
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert!(fake.calls.borrow().is_empty());
        let current = current(&startup.executable).unwrap();
        assert!(current.platform == Platform::current());
        assert_eq!(current.executable, startup.executable);
    }
}
