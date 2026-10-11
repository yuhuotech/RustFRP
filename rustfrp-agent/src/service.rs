//! 系统服务管理：`rustfrp-agent service install|uninstall|status|start|stop`。
//!
//! 只做三件事：生成各平台的服务定义（纯函数，可逐字测试）、通过 [`ServiceHost`] 写文件 / 执行系统命令、
//! 把结果翻译成退出码。真实的文件与进程操作集中在 [`StdHost`]。
//!
//! - Linux：systemd 单元。系统级使用 `DynamicUser` + `StateDirectory` 与一组沙箱选项（无需创建用户）；`--user` 装到用户级。
//! - macOS：launchd plist（系统级 LaunchDaemon / 用户级 LaunchAgent）。
//! - Windows：任务计划程序（开机 / 登录触发）。**未经实机验证**；崩溃后不会自动重启。

use std::io::Write;

use crate::cli::{parse_args, Command};

/// 服务操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 安装并启动。
    Install,
    /// 停止并卸载。
    Uninstall,
    /// 查询状态。
    Status,
    /// 启动。
    Start,
    /// 停止。
    Stop,
}

/// `service` 子命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCmd {
    /// 操作。
    pub action: Action,
    /// 装到当前用户（不需要管理员权限，登录后才运行）。
    pub user: bool,
    /// 服务名。
    pub name: String,
    /// 只打印将要写入的服务定义，不做任何改动。
    pub print: bool,
    /// 交给 agent 的运行参数（`install` 必填）。
    pub args: Vec<String>,
}

/// 默认服务名。
pub const DEFAULT_NAME: &str = "rustfrp-agent";

/// 解析 `service` 之后的参数。
pub fn parse(args: &[String]) -> Result<ServiceCmd, String> {
    let mut it = args.iter();
    let action = match it.next().map(String::as_str) {
        Some("install") => Action::Install,
        Some("uninstall") => Action::Uninstall,
        Some("status") => Action::Status,
        Some("start") => Action::Start,
        Some("stop") => Action::Stop,
        _ => return Err("usage: service <install|uninstall|status|start|stop> [--user] [--name <n>] [--print] [-- <agent args>]".into()),
    };
    let mut cmd = ServiceCmd {
        action,
        user: false,
        name: DEFAULT_NAME.into(),
        print: false,
        args: Vec::new(),
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--user" => cmd.user = true,
            "--print" => cmd.print = true,
            "--name" => cmd.name = it.next().cloned().ok_or("--name requires a value")?,
            "--" => {
                cmd.args = it.by_ref().cloned().collect();
            }
            other => return Err(format!("unknown service argument: {other}")),
        }
    }
    check_name(&cmd.name)?;
    if action == Action::Install {
        if cmd.args.is_empty() {
            return Err("service install needs the agent arguments after `--`, e.g. -- --mode client --controller host --state-dir <dir>".into());
        }
        // 提前用真正的解析器校验，避免装出一个启动即退出的服务。
        match parse_args("rustfrp-agent", &cmd.args) {
            Ok(Command::Run(_)) => {}
            Ok(_) => {
                return Err(
                    "the agent arguments must start the agent (not verify / convert / help)".into(),
                )
            }
            Err(e) => return Err(format!("invalid agent arguments: {e}")),
        }
    } else if !cmd.args.is_empty() {
        return Err("agent arguments are only accepted by service install".into());
    }
    Ok(cmd)
}

fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err("service name must be 1-48 characters of letters, digits, '-', '_' or '.'".into())
    }
}

/// 目标平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Linux（systemd）。
    Linux,
    /// macOS（launchd）。
    MacOs,
    /// Windows（任务计划程序）。
    Windows,
}

/// 外部命令的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// 退出码。
    pub code: i32,
    /// 标准输出。
    pub stdout: String,
    /// 标准错误。
    pub stderr: String,
}

/// 宿主：所有对系统的读写都经过它，便于测试。
pub trait ServiceHost {
    /// 平台。
    fn os(&self) -> Os;
    /// 当前可执行文件的绝对路径。
    fn exe(&self) -> Result<String, String>;
    /// 用户主目录。
    fn home(&self) -> Option<String>;
    /// 是否拥有管理员 / root 权限。
    fn is_admin(&self) -> bool;
    /// 当前用户 ID（launchd 的 `gui/<uid>` 域需要）。
    fn uid(&self) -> Option<String>;
    /// 写文件（自动创建父目录）。
    fn write(&self, path: &str, contents: &str) -> Result<(), String>;
    /// 删除文件；文件不存在返回 `false`。
    fn remove(&self, path: &str) -> Result<bool, String>;
    /// 文件是否存在。
    fn exists(&self, path: &str) -> bool;
    /// 执行命令。
    fn run(&self, program: &str, args: &[String]) -> Result<Ran, String>;
}

// ───────────── 服务定义（纯函数） ─────────────

/// systemd 的 ExecStart 引号规则：含空白 / 引号 / 反斜杠的参数加双引号并转义，`%` 与 `$` 翻倍。
pub fn systemd_quote(arg: &str) -> String {
    let escaped = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    if arg.is_empty()
        || arg
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '\\')
    {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// 给托管模式补上状态目录（没有 HOME 的系统服务账号需要显式目录）。
fn with_state_dir(args: &[String], default_dir: &str) -> Vec<String> {
    let managed = args
        .iter()
        .any(|a| a == "--controller" || a.starts_with("--controller="));
    let has_dir = args
        .iter()
        .any(|a| a == "--state-dir" || a.starts_with("--state-dir="));
    let mut out = args.to_vec();
    if managed && !has_dir {
        out.push("--state-dir".into());
        out.push(default_dir.into());
    }
    out
}

/// systemd 单元文件内容。
pub fn systemd_unit(name: &str, exe: &str, args: &[String], user: bool) -> String {
    let mut exec = systemd_quote(exe);
    for a in args {
        exec.push(' ');
        exec.push_str(&systemd_quote(a));
    }
    let mut s = format!(
        "[Unit]\nDescription=RustFRP agent ({name})\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={exec}\nRestart=always\nRestartSec=3\nEnvironment=RUSTFRP_SUPERVISED=1\nNoNewPrivileges=yes\nPrivateTmp=yes\n"
    );
    if !user {
        // 系统级：无需预先创建账号；状态目录由 systemd 创建并归该动态用户所有。
        s.push_str(&format!(
            "DynamicUser=yes\nStateDirectory={name}\nProtectSystem=strict\nProtectHome=yes\nProtectKernelTunables=yes\nProtectControlGroups=yes\nAmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\n"
        ));
    }
    s.push_str(&format!(
        "\n[Install]\nWantedBy={}\n",
        if user {
            "default.target"
        } else {
            "multi-user.target"
        }
    ));
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// launchd plist 内容。
pub fn launchd_plist(label: &str, exe: &str, args: &[String], log: &str) -> String {
    let mut items = format!("    <string>{}</string>\n", xml_escape(exe));
    for a in args {
        items.push_str(&format!("    <string>{}</string>\n", xml_escape(a)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{label}</string>\n  <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <true/>\n  <key>EnvironmentVariables</key>\n  <dict>\n    <key>RUSTFRP_SUPERVISED</key>\n    <string>1</string>\n  </dict>\n  <key>StandardOutPath</key>\n  <string>{log}</string>\n  <key>StandardErrorPath</key>\n  <string>{log}</string>\n</dict>\n</plist>\n",
        label = xml_escape(label),
        log = xml_escape(log),
    )
}

/// Windows 命令行引用（CreateProcess 规则的保守子集）：含空白 / 引号时加双引号并转义内部引号。
pub fn windows_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('"', "\\\""))
}

// ───────────── 执行 ─────────────

struct Paths {
    /// 服务定义文件（Windows 用任务计划，无文件，为空串）。
    file: String,
    /// launchd 标签。
    label: String,
    /// 日志文件（macOS）。
    log: String,
    /// 托管模式默认状态目录（用户级为 `None`，沿用 agent 自己的默认）。
    state_dir: Option<String>,
}

fn paths(host: &dyn ServiceHost, cmd: &ServiceCmd) -> Result<Paths, String> {
    let home = host.home();
    let need_home = || home.clone().ok_or("cannot determine the home directory");
    let name = &cmd.name;
    Ok(match (host.os(), cmd.user) {
        (Os::Linux, false) => Paths {
            file: format!("/etc/systemd/system/{name}.service"),
            label: name.clone(),
            log: String::new(),
            state_dir: Some(format!("/var/lib/{name}")),
        },
        (Os::Linux, true) => Paths {
            file: format!("{}/.config/systemd/user/{name}.service", need_home()?),
            label: name.clone(),
            log: String::new(),
            state_dir: None,
        },
        (Os::MacOs, false) => Paths {
            file: format!("/Library/LaunchDaemons/com.rustfrp.{name}.plist"),
            label: format!("com.rustfrp.{name}"),
            log: format!("/var/log/{name}.log"),
            state_dir: Some(format!("/Library/Application Support/RustFRP/{name}")),
        },
        (Os::MacOs, true) => {
            let h = need_home()?;
            Paths {
                file: format!("{h}/Library/LaunchAgents/com.rustfrp.{name}.plist"),
                label: format!("com.rustfrp.{name}"),
                log: format!("{h}/Library/Logs/{name}.log"),
                state_dir: None,
            }
        }
        (Os::Windows, user) => Paths {
            file: String::new(),
            label: name.clone(),
            log: String::new(),
            state_dir: (!user).then(|| format!("C:\\ProgramData\\RustFRP\\{name}")),
        },
    })
}

fn sv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

/// 执行 `service` 子命令，返回退出码。
pub fn run(
    cmd: &ServiceCmd,
    host: &dyn ServiceHost,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    match execute(cmd, host, out) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "service {}: {e}", action_name(cmd.action));
            1
        }
    }
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Install => "install",
        Action::Uninstall => "uninstall",
        Action::Status => "status",
        Action::Start => "start",
        Action::Stop => "stop",
    }
}

/// 以 `systemctl` / `launchctl` / `schtasks` 的实际返回为准，非零即失败并带上它的输出。
fn must(host: &dyn ServiceHost, program: &str, args: &[String]) -> Result<Ran, String> {
    let r = host.run(program, args)?;
    if r.code == 0 {
        return Ok(r);
    }
    let detail = if r.stderr.trim().is_empty() {
        r.stdout.trim()
    } else {
        r.stderr.trim()
    };
    let line = args.join(" ");
    Err(format!("`{program} {line}` failed ({}): {detail}", r.code))
}

/// 一次执行共用的上下文。
struct Ctx<'a> {
    cmd: &'a ServiceCmd,
    host: &'a dyn ServiceHost,
    os: Os,
    p: Paths,
    /// launchd 目标：`system/<label>` 或 `gui/<uid>/<label>`。
    domain: String,
}

impl Ctx<'_> {
    fn name(&self) -> &str {
        &self.cmd.name
    }

    /// systemctl 的参数，用户级时加 `--user`。
    fn ctl(&self, rest: &[&str]) -> Vec<String> {
        let mut v = if self.os == Os::Linux && self.cmd.user {
            sv(&["--user"])
        } else {
            Vec::new()
        };
        v.extend(sv(rest));
        v
    }

    fn target(&self) -> String {
        format!("{}/{}", self.domain, self.p.label)
    }

    fn installed(&self) -> bool {
        if self.os == Os::Windows {
            let query = sv(&["/Query", "/TN", self.name()]);
            return self.host.run("schtasks", &query).is_ok_and(|r| r.code == 0);
        }
        self.host.exists(&self.p.file)
    }

    fn need_admin(&self, how: &str) -> Result<(), String> {
        if self.cmd.user || self.host.is_admin() {
            return Ok(());
        }
        Err(format!("administrator rights are required{how}"))
    }
}

/// 向用户输出一行；终端写失败（管道已关闭）不影响服务操作本身的结果。
fn say(out: &mut dyn Write, msg: String) {
    let _ = writeln!(out, "{msg}");
}

fn execute(cmd: &ServiceCmd, host: &dyn ServiceHost, out: &mut dyn Write) -> Result<i32, String> {
    let ctx = Ctx {
        cmd,
        host,
        os: host.os(),
        p: paths(host, cmd)?,
        domain: if cmd.user {
            format!("gui/{}", host.uid().unwrap_or_default())
        } else {
            "system".into()
        },
    };
    match cmd.action {
        Action::Install => install(&ctx, out),
        Action::Uninstall => uninstall(&ctx, out),
        Action::Status => status(&ctx, out),
        Action::Start | Action::Stop => start_stop(&ctx, out),
    }
}

fn install(c: &Ctx, out: &mut dyn Write) -> Result<i32, String> {
    let exe = c.host.exe()?;
    let args = match &c.p.state_dir {
        Some(dir) => with_state_dir(&c.cmd.args, dir),
        None => c.cmd.args.clone(),
    };
    let text = match c.os {
        Os::Linux => systemd_unit(c.name(), &exe, &args, c.cmd.user),
        Os::MacOs => launchd_plist(&c.p.label, &exe, &args, &c.p.log),
        Os::Windows => std::iter::once(exe.as_str())
            .chain(args.iter().map(String::as_str))
            .map(windows_quote)
            .collect::<Vec<_>>()
            .join(" "),
    };
    if c.cmd.print {
        say(out, text);
        return Ok(0);
    }
    let hint = if c.os == Os::Windows {
        "; run from an elevated prompt or add --user"
    } else {
        "; root is required for a system service, use sudo or add --user"
    };
    c.need_admin(hint)?;
    if c.installed() {
        return Err(format!(
            "`{}` is already installed; uninstall it first",
            c.name()
        ));
    }
    match c.os {
        Os::Linux => {
            c.host.write(&c.p.file, &text)?;
            must(c.host, "systemctl", &c.ctl(&["daemon-reload"]))?;
            must(c.host, "systemctl", &c.ctl(&["enable", "--now", c.name()]))?;
            if c.cmd.user {
                say(out, format!("installed user service {}; run `loginctl enable-linger $USER` to keep it running after logout", c.name()));
            }
        }
        Os::MacOs => {
            c.host.write(&c.p.file, &text)?;
            let bootstrap = sv(&["bootstrap", &c.domain, &c.p.file]);
            must(c.host, "launchctl", &bootstrap)?;
        }
        Os::Windows => {
            let trigger = if c.cmd.user {
                sv(&["/SC", "ONLOGON"])
            } else {
                sv(&["/SC", "ONSTART", "/RU", "SYSTEM", "/RL", "HIGHEST"])
            };
            let mut create = sv(&["/Create", "/TN", c.name(), "/TR", &text, "/F"]);
            create.extend(trigger);
            must(c.host, "schtasks", &create)?;
            must(c.host, "schtasks", &sv(&["/Run", "/TN", c.name()]))?;
        }
    }
    say(out, format!("service {} installed and started", c.name()));
    Ok(0)
}

fn uninstall(c: &Ctx, out: &mut dyn Write) -> Result<i32, String> {
    c.need_admin(" (or add --user)")?;
    if !c.installed() {
        say(out, format!("service {} is not installed", c.name()));
        return Ok(0);
    }
    match c.os {
        Os::Linux => {
            // 已停止的服务 disable 会失败，不影响卸载。
            let _ = c
                .host
                .run("systemctl", &c.ctl(&["disable", "--now", c.name()]));
            c.host.remove(&c.p.file)?;
            must(c.host, "systemctl", &c.ctl(&["daemon-reload"]))?;
        }
        Os::MacOs => {
            let _ = c.host.run("launchctl", &sv(&["bootout", &c.target()]));
            c.host.remove(&c.p.file)?;
        }
        Os::Windows => {
            let _ = c.host.run("schtasks", &sv(&["/End", "/TN", c.name()]));
            must(c.host, "schtasks", &sv(&["/Delete", "/TN", c.name(), "/F"]))?;
        }
    }
    say(out, format!("service {} uninstalled", c.name()));
    Ok(0)
}

fn status(c: &Ctx, out: &mut dyn Write) -> Result<i32, String> {
    if !c.installed() {
        say(out, format!("service {} is not installed", c.name()));
        return Ok(4);
    }
    let (program, args) = match c.os {
        Os::Linux => ("systemctl", c.ctl(&["is-active", c.name()])),
        Os::MacOs => ("launchctl", sv(&["print", &c.target()])),
        Os::Windows => ("schtasks", sv(&["/Query", "/TN", c.name(), "/FO", "LIST"])),
    };
    let r = c.host.run(program, &args)?;
    let running = r.code == 0
        && match c.os {
            Os::Linux => true,
            Os::MacOs => r.stdout.contains("state = running"),
            Os::Windows => r.stdout.contains("Running"),
        };
    let word = if running {
        "running"
    } else {
        "installed but not running"
    };
    say(out, format!("service {} is {word}", c.name()));
    Ok(if running { 0 } else { 3 })
}

fn start_stop(c: &Ctx, out: &mut dyn Write) -> Result<i32, String> {
    if !c.installed() {
        return Err(format!("service {} is not installed", c.name()));
    }
    let start = c.cmd.action == Action::Start;
    let (program, args) = match c.os {
        Os::Linux => (
            "systemctl",
            c.ctl(&[if start { "start" } else { "stop" }, c.name()]),
        ),
        Os::MacOs if start => ("launchctl", sv(&["kickstart", "-k", &c.target()])),
        Os::MacOs => ("launchctl", sv(&["kill", "SIGTERM", &c.target()])),
        Os::Windows => (
            "schtasks",
            sv(&[if start { "/Run" } else { "/End" }, "/TN", c.name()]),
        ),
    };
    must(c.host, program, &args)?;
    let word = if start { "started" } else { "stopped" };
    say(out, format!("service {} {word}", c.name()));
    Ok(0)
}

/// 把 I/O 等错误转成给用户看的文字。
pub fn describe(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 主目录：`HOME` 优先，其次 `USERPROFILE`（Windows），忽略空值。
pub fn pick_home(home: Option<String>, profile: Option<String>) -> Option<String> {
    [home, profile]
        .into_iter()
        .flatten()
        .find(|h| !h.is_empty())
}

/// `std::env::consts::OS` → 平台（未知系统按 Linux 处理，使用 systemd 路径）。
pub fn os_from(name: &str) -> Os {
    match name {
        "macos" => Os::MacOs,
        "windows" => Os::Windows,
        _ => Os::Linux,
    }
}

/// 判断管理员身份所用的命令：Unix 用 `id -u`（0 即 root），Windows 用 `net session`（仅管理员能成功）。
pub fn admin_probe(os: Os) -> (&'static str, Vec<String>) {
    match os {
        Os::Windows => ("net", sv(&["session"])),
        _ => ("id", sv(&["-u"])),
    }
}

/// 解读 [`admin_probe`] 的结果。
pub fn is_admin_result(os: Os, r: &Ran) -> bool {
    match os {
        Os::Windows => r.code == 0,
        _ => r.stdout.trim() == "0",
    }
}

// ───────────── 真实宿主 ─────────────

/// 使用真实文件系统与外部命令的宿主。
pub struct StdHost;

impl ServiceHost for StdHost {
    fn os(&self) -> Os {
        os_from(std::env::consts::OS)
    }
    fn exe(&self) -> Result<String, String> {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(describe)
    }
    fn home(&self) -> Option<String> {
        pick_home(
            std::env::var("HOME").ok(),
            std::env::var("USERPROFILE").ok(),
        )
    }
    fn is_admin(&self) -> bool {
        let (program, args) = admin_probe(self.os());
        self.run(program, &args)
            .is_ok_and(|r| is_admin_result(self.os(), &r))
    }
    fn uid(&self) -> Option<String> {
        self.run("id", &sv(&["-u"]))
            .ok()
            .map(|r| r.stdout.trim().to_string())
            .filter(|u| !u.is_empty())
    }
    fn write(&self, path: &str, contents: &str) -> Result<(), String> {
        let p = std::path::Path::new(path);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(describe)?;
        }
        std::fs::write(p, contents).map_err(describe)
    }
    fn remove(&self, path: &str) -> Result<bool, String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("remove {path}: {e}")),
        }
    }
    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    fn run(&self, program: &str, args: &[String]) -> Result<Ran, String> {
        let o = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        Ok(Ran {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    struct Fake {
        os: Os,
        admin: bool,
        home: Option<String>,
        files: RefCell<BTreeMap<String, String>>,
        ran: RefCell<Vec<String>>,
        /// 命令行前缀 → 结果；未列出的命令返回成功。
        results: RefCell<Vec<(String, Ran)>>,
    }

    fn ran(code: i32, stdout: &str, stderr: &str) -> Ran {
        Ran {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    impl Fake {
        fn new(os: Os) -> Self {
            Self {
                os,
                admin: true,
                home: Some("/home/u".into()),
                files: RefCell::default(),
                ran: RefCell::default(),
                results: RefCell::default(),
            }
        }
        fn on(&self, prefix: &str, r: Ran) {
            self.results.borrow_mut().push((prefix.into(), r));
        }
        fn commands(&self) -> Vec<String> {
            self.ran.borrow().clone()
        }
    }

    impl ServiceHost for Fake {
        fn os(&self) -> Os {
            self.os
        }
        fn exe(&self) -> Result<String, String> {
            Ok("/opt/rustfrp/rustfrp-agent".into())
        }
        fn home(&self) -> Option<String> {
            self.home.clone()
        }
        fn is_admin(&self) -> bool {
            self.admin
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
            self.ran.borrow_mut().push(line.clone());
            let found = self
                .results
                .borrow()
                .iter()
                .rev()
                .find(|(p, _)| line.starts_with(p.as_str()))
                .map(|(_, r)| r.clone());
            Ok(found.unwrap_or_else(|| ran(0, "", "")))
        }
    }

    fn cmd(s: &str) -> ServiceCmd {
        let args: Vec<String> = s.split_whitespace().map(String::from).collect();
        parse(&args).unwrap()
    }

    fn go(c: &ServiceCmd, h: &Fake) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(c, h, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    const MANAGED: &str =
        "install -- --mode client --controller panel.example.com --enroll-token t";

    #[test]
    fn parses_actions_flags_and_validates_agent_arguments() {
        let c = cmd(MANAGED);
        assert_eq!(c.action, Action::Install);
        assert_eq!(
            (c.name.as_str(), c.user, c.print),
            (DEFAULT_NAME, false, false)
        );
        assert_eq!(c.args.len(), 6);
        let c = cmd("install --user --name edge-1 --print -- --mode edge -c /etc/frps.toml");
        assert_eq!((c.user, c.print, c.name.as_str()), (true, true, "edge-1"));
        for (word, action) in [
            ("uninstall", Action::Uninstall),
            ("status", Action::Status),
            ("start", Action::Start),
            ("stop", Action::Stop),
        ] {
            assert_eq!(cmd(word).action, action);
        }
        for bad in [
            "",
            "restart",
            "install",
            "install --",
            "install -- --mode client",
            "install -- verify --mode client -c x.toml",
            "install -- --help",
            "install --bogus -- --mode edge -c x",
            "install --name -- --mode edge -c x",
            "install --name bad/name -- --mode edge -c x",
            "install --name .hidden -- --mode edge -c x",
            "status -- --mode edge -c x",
        ] {
            let args: Vec<String> = bad.split_whitespace().map(String::from).collect();
            assert!(parse(&args).is_err(), "{bad}");
        }
        assert!(parse(&[
            "install".into(),
            "--name".into(),
            "x".repeat(49),
            "--".into()
        ])
        .is_err());
        // 顶层命令行：service 之后的内容整体交给 service 解析。
        let top = parse_args("rustfrp-agent", &["service".into(), "status".into()]).unwrap();
        assert!(matches!(top, Command::Service(c) if c.action == Action::Status));
        assert!(parse_args("rustfrp-agent", &["service".into()]).is_err());
    }

    #[test]
    fn quoting_rules() {
        assert_eq!(systemd_quote("plain-1.2"), "plain-1.2");
        assert_eq!(systemd_quote("a b"), "\"a b\"");
        assert_eq!(systemd_quote("100%"), "100%%");
        assert_eq!(systemd_quote("$HOME"), "$$HOME");
        assert_eq!(systemd_quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(systemd_quote(""), "\"\"");
        assert_eq!(windows_quote("plain"), "plain");
        assert_eq!(
            windows_quote("C:\\Program Files\\a.exe"),
            "\"C:\\Program Files\\a.exe\""
        );
        assert_eq!(windows_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(windows_quote(""), "\"\"");
        assert_eq!(xml_escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
    }

    #[test]
    fn linux_system_service_lifecycle() {
        let h = Fake::new(Os::Linux);
        let (code, out, _) = go(&cmd(MANAGED), &h);
        assert_eq!(code, 0, "{out}");
        let unit = h.files.borrow()["/etc/systemd/system/rustfrp-agent.service"].clone();
        for want in [
            "ExecStart=/opt/rustfrp/rustfrp-agent --mode client --controller panel.example.com --enroll-token t --state-dir /var/lib/rustfrp-agent\n",
            "DynamicUser=yes",
            "StateDirectory=rustfrp-agent",
            "NoNewPrivileges=yes",
            "Restart=always",
            "WantedBy=multi-user.target",
        ] {
            assert!(unit.contains(want), "{want}\n{unit}");
        }
        assert_eq!(
            h.commands(),
            [
                "systemctl daemon-reload",
                "systemctl enable --now rustfrp-agent"
            ]
        );
        // 重复安装被拒绝。
        let (code, _, err) = go(&cmd(MANAGED), &h);
        assert_eq!(code, 1);
        assert!(err.contains("already installed"), "{err}");
        // 状态 / 启停。
        h.on("systemctl is-active", ran(0, "active\n", ""));
        let (code, out, _) = go(&cmd("status"), &h);
        assert_eq!((code, out.trim()), (0, "service rustfrp-agent is running"));
        h.on("systemctl is-active", ran(3, "inactive\n", ""));
        assert_eq!(go(&cmd("status"), &h).0, 3);
        assert_eq!(go(&cmd("stop"), &h).0, 0);
        assert_eq!(go(&cmd("start"), &h).0, 0);
        assert!(h.commands().ends_with(&[
            "systemctl stop rustfrp-agent".into(),
            "systemctl start rustfrp-agent".into()
        ]));
        // 卸载。
        let (code, out, _) = go(&cmd("uninstall"), &h);
        assert_eq!((code, out.trim()), (0, "service rustfrp-agent uninstalled"));
        assert!(h.files.borrow().is_empty());
        assert!(h
            .commands()
            .iter()
            .any(|c| c == "systemctl disable --now rustfrp-agent"));
        // 已卸载：幂等；状态 4；启停报错。
        assert_eq!(go(&cmd("uninstall"), &h).0, 0);
        let (code, out, _) = go(&cmd("status"), &h);
        assert_eq!((code, out.contains("not installed")), (4, true));
        assert_eq!(go(&cmd("start"), &h).0, 1);
    }

    #[test]
    fn explicit_state_dir_is_respected_and_standalone_args_are_untouched() {
        let h = Fake::new(Os::Linux);
        let c = cmd("install -- --mode client --controller p --state-dir /data/x");
        assert_eq!(go(&c, &h).0, 0);
        let unit = h.files.borrow()["/etc/systemd/system/rustfrp-agent.service"].clone();
        assert!(
            unit.contains("--state-dir /data/x\n") && !unit.contains("/var/lib"),
            "{unit}"
        );
        let h = Fake::new(Os::Linux);
        let c = cmd("install --name edge -- --mode edge -c /etc/frp/frps.toml");
        assert_eq!(go(&c, &h).0, 0);
        let unit = h.files.borrow()["/etc/systemd/system/edge.service"].clone();
        assert!(
            unit.contains(
                "ExecStart=/opt/rustfrp/rustfrp-agent --mode edge -c /etc/frp/frps.toml\n"
            ),
            "{unit}"
        );
        assert!(unit.contains("CAP_NET_BIND_SERVICE"), "{unit}");
    }

    #[test]
    fn linux_user_service_and_permission_errors() {
        let h = Fake::new(Os::Linux);
        let (code, out, _) = go(&cmd("install --user -- --mode client --controller p"), &h);
        assert_eq!(code, 0);
        assert!(out.contains("enable-linger"), "{out}");
        let unit = h.files.borrow()["/home/u/.config/systemd/user/rustfrp-agent.service"].clone();
        assert!(
            !unit.contains("DynamicUser") && unit.contains("WantedBy=default.target"),
            "{unit}"
        );
        assert!(
            !unit.contains("--state-dir"),
            "user services use the agent default state dir"
        );
        assert_eq!(
            h.commands(),
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now rustfrp-agent"
            ]
        );
        assert_eq!(go(&cmd("uninstall --user"), &h).0, 0);
        // 非 root 装系统级 / 卸载系统级：拒绝，且不写任何东西。
        let mut h = Fake::new(Os::Linux);
        h.admin = false;
        for c in [cmd(MANAGED), cmd("uninstall")] {
            let (code, _, err) = go(&c, &h);
            assert_eq!(code, 1);
            assert!(
                err.contains("root") || err.contains("administrator"),
                "{err}"
            );
        }
        assert!(h.files.borrow().is_empty() && h.commands().is_empty());
        // 无主目录的用户级安装。
        h.home = None;
        let (code, _, err) = go(&cmd("install --user -- --mode edge -c x"), &h);
        assert_eq!(code, 1);
        assert!(err.contains("home directory"), "{err}");
    }

    #[test]
    fn print_mode_changes_nothing_and_command_failures_surface() {
        let mut h = Fake::new(Os::Linux);
        h.admin = false;
        let (code, out, _) = go(&cmd("install --print -- --mode edge -c /etc/frps.toml"), &h);
        assert_eq!(code, 0);
        assert!(
            out.starts_with("[Unit]") && out.contains("ExecStart="),
            "{out}"
        );
        assert!(h.files.borrow().is_empty() && h.commands().is_empty());
        let h = Fake::new(Os::Linux);
        h.on(
            "systemctl enable",
            ran(1, "", "Failed to enable unit: Access denied"),
        );
        let (code, _, err) = go(&cmd(MANAGED), &h);
        assert_eq!(code, 1);
        assert!(
            err.contains("Access denied") && err.contains("systemctl enable --now"),
            "{err}"
        );
        let h = Fake::new(Os::Linux);
        h.on("systemctl enable", ran(2, "only stdout detail", ""));
        assert!(go(&cmd(MANAGED), &h).2.contains("only stdout detail"));
    }

    #[test]
    fn macos_launchd_system_and_user() {
        let h = Fake::new(Os::MacOs);
        let c = cmd("install -- --mode client --controller p --name A&B");
        assert_eq!(go(&c, &h).0, 0);
        let plist =
            h.files.borrow()["/Library/LaunchDaemons/com.rustfrp.rustfrp-agent.plist"].clone();
        for want in [
            "<string>com.rustfrp.rustfrp-agent</string>",
            "<string>/opt/rustfrp/rustfrp-agent</string>",
            "<string>A&amp;B</string>",
            "<string>/Library/Application Support/RustFRP/rustfrp-agent</string>",
            "<key>KeepAlive</key>",
            "/var/log/rustfrp-agent.log",
        ] {
            assert!(plist.contains(want), "{want}\n{plist}");
        }
        assert_eq!(
            h.commands(),
            ["launchctl bootstrap system /Library/LaunchDaemons/com.rustfrp.rustfrp-agent.plist"]
        );
        h.on("launchctl print", ran(0, "state = running\n", ""));
        assert_eq!(go(&cmd("status"), &h).0, 0);
        h.on("launchctl print", ran(0, "state = waiting\n", ""));
        assert_eq!(go(&cmd("status"), &h).0, 3);
        assert_eq!(go(&cmd("start"), &h).0, 0);
        assert_eq!(go(&cmd("stop"), &h).0, 0);
        let cmds = h.commands();
        assert!(
            cmds.contains(&"launchctl kickstart -k system/com.rustfrp.rustfrp-agent".to_string())
        );
        assert!(
            cmds.contains(&"launchctl kill SIGTERM system/com.rustfrp.rustfrp-agent".to_string())
        );
        assert_eq!(go(&cmd("uninstall"), &h).0, 0);
        assert!(h
            .commands()
            .contains(&"launchctl bootout system/com.rustfrp.rustfrp-agent".to_string()));
        assert!(h.files.borrow().is_empty());

        let h = Fake::new(Os::MacOs);
        assert_eq!(
            go(
                &cmd("install --user --name me -- --mode client --controller p"),
                &h
            )
            .0,
            0
        );
        assert!(h
            .files
            .borrow()
            .contains_key("/home/u/Library/LaunchAgents/com.rustfrp.me.plist"));
        let plist = h.files.borrow()["/home/u/Library/LaunchAgents/com.rustfrp.me.plist"].clone();
        assert!(
            plist.contains("/home/u/Library/Logs/me.log") && !plist.contains("--state-dir"),
            "{plist}"
        );
        assert_eq!(
            h.commands(),
            ["launchctl bootstrap gui/501 /home/u/Library/LaunchAgents/com.rustfrp.me.plist"]
        );
    }

    #[test]
    fn windows_scheduled_task() {
        let h = Fake::new(Os::Windows);
        // 任务不存在 → 视为未安装。
        h.on("schtasks /Query", ran(1, "", "not found"));
        let c = cmd("install -- --mode client --controller panel.example.com --name D");
        assert_eq!(go(&c, &h).0, 0);
        let cmds = h.commands();
        let create = cmds.iter().find(|c| c.contains("/Create")).unwrap();
        assert!(
            create.contains("/SC ONSTART") && create.contains("/RU SYSTEM"),
            "{create}"
        );
        assert!(
            create.contains("--state-dir C:\\ProgramData\\RustFRP\\rustfrp-agent"),
            "{create}"
        );
        assert!(cmds.contains(&"schtasks /Run /TN rustfrp-agent".to_string()));
        // 用户级：登录触发，没有默认状态目录。
        let h = Fake::new(Os::Windows);
        h.on("schtasks /Query", ran(1, "", ""));
        assert_eq!(
            go(&cmd("install --user -- --mode client --controller p"), &h).0,
            0
        );
        let create = h
            .commands()
            .into_iter()
            .find(|c| c.contains("/Create"))
            .unwrap();
        assert!(
            create.contains("/SC ONLOGON") && !create.contains("--state-dir"),
            "{create}"
        );
        // 已安装：状态 / 启停 / 卸载。
        let h = Fake::new(Os::Windows);
        h.on(
            "schtasks /Query /TN rustfrp-agent /FO LIST",
            ran(0, "Status: Running\n", ""),
        );
        assert_eq!(go(&cmd("status"), &h).0, 0);
        h.on(
            "schtasks /Query /TN rustfrp-agent /FO LIST",
            ran(0, "Status: Ready\n", ""),
        );
        assert_eq!(go(&cmd("status"), &h).0, 3);
        assert_eq!(go(&cmd("start"), &h).0, 0);
        assert_eq!(go(&cmd("stop"), &h).0, 0);
        assert_eq!(go(&cmd("uninstall"), &h).0, 0);
        let cmds = h.commands();
        assert!(cmds.contains(&"schtasks /End /TN rustfrp-agent".to_string()));
        assert!(cmds.contains(&"schtasks /Delete /TN rustfrp-agent /F".to_string()));
        // 非管理员装系统级。
        let mut h = Fake::new(Os::Windows);
        h.admin = false;
        h.on("schtasks /Query", ran(1, "", ""));
        let (code, _, err) = go(&cmd(MANAGED), &h);
        assert_eq!(code, 1);
        assert!(err.contains("elevated"), "{err}");
    }

    #[test]
    fn platform_and_privilege_probes() {
        assert_eq!(os_from("macos"), Os::MacOs);
        assert_eq!(os_from("windows"), Os::Windows);
        assert_eq!(os_from("linux"), Os::Linux);
        assert_eq!(os_from("freebsd"), Os::Linux);
        assert_eq!(admin_probe(Os::Windows).0, "net");
        assert_eq!(admin_probe(Os::Linux).0, "id");
        assert!(is_admin_result(Os::Linux, &ran(0, "0\n", "")));
        assert!(!is_admin_result(Os::MacOs, &ran(0, "501\n", "")));
        assert!(is_admin_result(Os::Windows, &ran(0, "", "")));
        assert!(!is_admin_result(Os::Windows, &ran(5, "", "")));
    }

    #[test]
    fn host_errors_are_reported_with_the_action_name() {
        struct Broken;
        impl ServiceHost for Broken {
            fn os(&self) -> Os {
                Os::Linux
            }
            fn exe(&self) -> Result<String, String> {
                Err("no exe".into())
            }
            fn home(&self) -> Option<String> {
                None
            }
            fn is_admin(&self) -> bool {
                true
            }
            fn uid(&self) -> Option<String> {
                None
            }
            fn write(&self, _: &str, _: &str) -> Result<(), String> {
                Err("disk full".into())
            }
            fn remove(&self, _: &str) -> Result<bool, String> {
                Err("busy".into())
            }
            fn exists(&self, _: &str) -> bool {
                true
            }
            fn run(&self, p: &str, _: &[String]) -> Result<Ran, String> {
                Err(format!("cannot run {p}"))
            }
        }
        for (c, want) in [
            (cmd(MANAGED), "service install: no exe"),
            (cmd("uninstall"), "service uninstall: "),
            (cmd("status"), "service status: cannot run systemctl"),
            (cmd("start"), "service start: cannot run systemctl"),
            (cmd("stop"), "service stop: cannot run systemctl"),
        ] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            assert_eq!(run(&c, &Broken, &mut out, &mut err), 1);
            let err = String::from_utf8(err).unwrap();
            assert!(err.contains(want), "{want}: {err}");
        }
    }

    #[test]
    fn std_host_files_commands_and_environment() {
        let h = StdHost;
        let dir = std::env::temp_dir().join(format!("rfrp-svc-{}", std::process::id()));
        let file = dir.join("nested/dir/unit.service");
        let file = file.to_str().unwrap();
        assert!(!h.exists(file));
        h.write(file, "hello").unwrap();
        assert!(h.exists(file));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "hello");
        assert_eq!(h.remove(file), Ok(true));
        assert_eq!(h.remove(file), Ok(false));
        // 路径被文件占用 → 报错而不是 panic。
        std::fs::write(dir.join("blocker"), "x").unwrap();
        assert!(h
            .write(dir.join("blocker/child").to_str().unwrap(), "x")
            .is_err());
        assert!(
            h.remove(dir.to_str().unwrap()).is_err(),
            "removing a directory as a file fails"
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(h.exe().unwrap().len() > 3);
        assert!(h.home().is_some());
        let _ = h.is_admin();
        if cfg!(unix) {
            let r = h
                .run(
                    "sh",
                    &["-c".into(), "echo out; echo err >&2; exit 3".into()],
                )
                .unwrap();
            assert_eq!(
                (r.code, r.stdout.trim(), r.stderr.trim()),
                (3, "out", "err")
            );
            assert!(h.uid().is_some());
            assert!(h.run("/definitely/not/a/program", &[]).is_err());
        }
        assert_eq!(
            h.os(),
            if cfg!(target_os = "macos") {
                Os::MacOs
            } else if cfg!(windows) {
                Os::Windows
            } else {
                Os::Linux
            }
        );
    }
}
