//! `service` 子命令的公开接口：用假宿主走一遍三个平台，并用真实宿主在临时目录里做文件操作。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::collections::BTreeMap;

use rustfrp_agent::cli::{parse_args, Command};
use rustfrp_agent::service::{
    admin_probe, describe, is_admin_result, launchd_plist, os_from, parse, pick_home, run,
    systemd_quote, systemd_unit, windows_quote, Os, Ran, ServiceHost, StdHost,
};

struct Fake {
    os: Os,
    files: RefCell<BTreeMap<String, String>>,
    ran: RefCell<Vec<String>>,
    /// 查询类命令的返回码（`schtasks /Query` 等）。
    query_code: i32,
}

impl Fake {
    fn new(os: Os, query_code: i32) -> Self {
        Self {
            os,
            files: RefCell::default(),
            ran: RefCell::default(),
            query_code,
        }
    }
}

impl ServiceHost for Fake {
    fn os(&self) -> Os {
        self.os
    }
    fn exe(&self) -> Result<String, String> {
        Ok("/bin/rustfrp-agent".into())
    }
    fn home(&self) -> Option<String> {
        Some("/home/u".into())
    }
    fn is_admin(&self) -> bool {
        true
    }
    fn uid(&self) -> Option<String> {
        Some("501".into())
    }
    fn write(&self, path: &str, contents: &str) -> Result<(), String> {
        self.files.borrow_mut().insert(path.into(), contents.into());
        Ok(())
    }
    fn remove(&self, path: &str) -> Result<bool, String> {
        Ok(self.files.borrow_mut().remove(path).is_some())
    }
    fn exists(&self, path: &str) -> bool {
        self.files.borrow().contains_key(path)
    }
    fn run(&self, program: &str, args: &[String]) -> Result<Ran, String> {
        let line = format!("{program} {}", args.join(" "));
        let code = if line.contains("/Query") {
            self.query_code
        } else {
            0
        };
        let stdout =
            if line.contains("is-active") || line.contains("print") || line.contains("/Query") {
                "state = running Running".to_string()
            } else {
                String::new()
            };
        self.ran.borrow_mut().push(line);
        Ok(Ran {
            code,
            stdout,
            stderr: String::new(),
        })
    }
}

fn command(s: &str) -> rustfrp_agent::service::ServiceCmd {
    let args: Vec<String> = s.split_whitespace().map(String::from).collect();
    parse(&args).unwrap()
}

fn exec(host: &Fake, s: &str) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&command(s), host, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap() + &String::from_utf8(err).unwrap(),
    )
}

const INSTALL: &str = "install -- --mode client --controller panel.example.com --enroll-token t";

#[test]
fn every_platform_installs_reports_and_removes() {
    for (os, query_code) in [(Os::Linux, 0), (Os::MacOs, 0), (Os::Windows, 1)] {
        let h = Fake::new(os, query_code);
        assert_eq!(exec(&h, INSTALL).0, 0, "{os:?}");
        if os != Os::Windows {
            assert_eq!(h.files.borrow().len(), 1);
        }
        // 装好后的行为（Windows 的「已安装」由查询码决定）。
        let h2 = Fake::new(os, 0);
        if os != Os::Windows {
            *h2.files.borrow_mut() = h.files.borrow().clone();
        }
        assert_eq!(exec(&h2, "status").0, 0, "{os:?}");
        assert_eq!(exec(&h2, "stop").0, 0);
        assert_eq!(exec(&h2, "start").0, 0);
        assert_eq!(exec(&h2, "uninstall").0, 0);
        assert!(h2.files.borrow().is_empty());
    }
    let user = Fake::new(Os::Linux, 0);
    assert_eq!(
        exec(
            &user,
            "install --user --print -- --mode edge -c /etc/frps.toml"
        )
        .0,
        0
    );
    assert_eq!(
        exec(&user, "install --user -- --mode edge -c /etc/frps.toml").0,
        0
    );
    assert_eq!(exec(&user, "status --user").0, 0);
    assert_eq!(exec(&Fake::new(Os::Linux, 0), "status").0, 4);
    assert_eq!(exec(&Fake::new(Os::Linux, 0), "start").0, 1);
}

#[test]
fn definitions_and_quoting_are_available_to_callers() {
    let args: Vec<String> = ["--mode", "edge", "-c", "/etc/my frps.toml"]
        .map(String::from)
        .to_vec();
    let unit = systemd_unit("edge", "/bin/rustfrp-agent", &args, false);
    assert!(
        unit.contains("ExecStart=/bin/rustfrp-agent --mode edge -c \"/etc/my frps.toml\"\n"),
        "{unit}"
    );
    let plist = launchd_plist("com.rustfrp.edge", "/bin/a", &args, "/tmp/log");
    assert!(
        plist.contains("<string>/etc/my frps.toml</string>"),
        "{plist}"
    );
    assert_eq!(systemd_quote("100%"), "100%%");
    assert_eq!(windows_quote("a b"), "\"a b\"");
    assert_eq!(os_from("macos"), Os::MacOs);
    assert_eq!(describe("boom"), "boom");
    assert_eq!(
        pick_home(Some(String::new()), Some("C:\\Users\\u".into())).as_deref(),
        Some("C:\\Users\\u")
    );
    assert_eq!(
        pick_home(Some("/h".into()), Some("x".into())).as_deref(),
        Some("/h")
    );
    assert_eq!(pick_home(None, None), None);
    assert_eq!(admin_probe(Os::Windows).0, "net");
    assert!(is_admin_result(
        Os::Linux,
        &Ran {
            code: 0,
            stdout: "0".into(),
            stderr: String::new()
        }
    ));
    assert!(matches!(
        parse_args("rustfrp-agent", &["service".into(), "status".into()]),
        Ok(Command::Service(_))
    ));
    assert!(parse(&["bogus".into()]).is_err());
}

#[test]
fn the_real_host_reads_and_writes_files_and_runs_commands() {
    let h = StdHost;
    let dir = std::env::temp_dir().join(format!("rfrp-svc-it-{}", std::process::id()));
    let file = dir.join("a/b/unit");
    let path = file.to_str().unwrap();
    assert!(!h.exists(path));
    h.write(path, "x").unwrap();
    assert!(h.exists(path));
    assert_eq!(h.remove(path), Ok(true));
    assert_eq!(h.remove(path), Ok(false));
    // 父路径被文件占用：写入失败而不是 panic。
    std::fs::write(dir.join("blocker"), "x").unwrap();
    assert!(h
        .write(dir.join("blocker/child").to_str().unwrap(), "x")
        .is_err());
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(h.exe().unwrap().len() > 3);
    assert!(h.home().is_some());
    let _ = h.is_admin();
    let _ = h.uid();
    let _ = h.os();
    if cfg!(unix) {
        assert_eq!(
            h.run("sh", &["-c".into(), "exit 7".into()]).unwrap().code,
            7
        );
        assert!(h.run("/no/such/program", &[]).is_err());
    }
}
