//! 命令行解析：兼容 frp 的 `-c` / `--strict_config` / `--allow-unsafe` / `verify`，
//! 并识别被命名为 `frpc` / `frps` 的可执行文件（argv[0]）。

use crate::managed::ManagedOpts;

/// 运行角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 客户端（frpc）。
    Client,
    /// 边缘节点（frps）。
    Edge,
}

/// 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 运行。
    Run(Options),
    /// 仅校验配置并打印兼容性报告。
    Verify(Options),
    /// 将旧版 INI 转换为标准 TOML，输出到 stdout。
    Convert(Options),
    /// 系统服务管理（`service install|uninstall|status|start|stop`）。
    Service(crate::service::ServiceCmd),
    /// 打印版本。
    Version,
    /// 打印帮助。
    Help,
}

/// 运行选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// 角色。
    pub mode: Mode,
    /// 配置文件路径（受管模式下为空）。
    pub config: String,
    /// 受管模式参数（`--controller`）；与 `-c` 二选一。
    pub managed: Option<ManagedOpts>,
    /// 私有本机控制发现文件（仅受管 client）。
    pub local_control: Option<String>,
    /// 严格模式（frp 默认开启）。
    pub strict: bool,
    /// 放行的不安全特性。
    pub allow_unsafe: Vec<String>,
}

/// 帮助文本。
pub const HELP: &str = "\
rustfrp-agent — frp-compatible tunnel agent (client / edge)

USAGE:
  rustfrp-agent --mode <client|edge> -c <config> [options]
  rustfrp-agent --mode client --controller <host[:port]> --enroll-token <token> [--name <n>]
  rustfrp-agent --mode edge --controller <host[:port]> --enroll-token <token> --public-host <name> [-c <frps.toml>]
  rustfrp-agent convert --mode <client|edge> -c <legacy.ini>
  rustfrp-agent verify --mode <client|edge> -c <config>
  rustfrp-agent service <install|uninstall|status|start|stop> [--user] [--name <n>] [--print] [-- <agent args>]
  frpc -c <config>        (when the binary is named frpc)
  frps -c <config>        (when the binary is named frps)

OPTIONS:
  -c, --config <file>          frp configuration file (TOML / YAML / JSON)
      --mode <client|edge>     role (implied by the binary name frpc / frps)
      --strict_config[=bool]   reject unknown fields (default: true)
      --allow-unsafe <a,b>     allow unsafe features, e.g. TokenSourceExec
      --controller <host[:port]>  managed mode: connect to a RustFRP controller (default port 7000)
      --enroll-token <token>   one-time token from the panel (first run only)
      --name <name>            device name shown in the panel (default: host name)
      --state-dir <dir>        where the device identity is stored (default: ~/.rustfrp)
      --local-control <file>   private loopback control discovery (managed client only)
      --public-host <name>     managed edge: host name clients use to reach this node (required at first run)
      --tls-pin <sha256>      fix the server leaf certificate fingerprint (managed only)
      --ca-file <pem>          verify the controller certificate against this CA
  -v, --version                print version
  -h, --help                   print this help
";

fn stem(argv0: &str) -> &str {
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// 解析命令行（`args` 不含 argv[0]）。
pub fn parse_args(argv0: &str, args: &[String]) -> Result<Command, String> {
    let mut mode = match stem(argv0) {
        "frpc" => Some(Mode::Client),
        "frps" => Some(Mode::Edge),
        _ => None,
    };
    if args.first().map(String::as_str) == Some("service") {
        return crate::service::parse(args.get(1..).unwrap_or(&[])).map(Command::Service);
    }
    let mut local_control = None;
    let mut public_host = None;
    let mut tls_pin = None;
    let (mut config, mut strict, mut allow_unsafe) = (None::<String>, true, Vec::<String>::new());
    let (mut controller, mut token, mut name, mut state_dir, mut ca_file) = (
        None::<String>,
        None::<String>,
        None::<String>,
        None::<String>,
        None::<String>,
    );
    let mut convert = false;
    let (mut verify, mut version, mut help) = (false, false, false);

    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => it
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value")),
            }
        };
        match key {
            "verify" => verify = true,
            "convert" => convert = true,
            "-v" | "--version" | "version" => version = true,
            "-h" | "--help" | "help" => help = true,
            "-c" | "--config" => config = Some(value(key)?),
            "--mode" => {
                mode = Some(match value(key)?.as_str() {
                    "client" => Mode::Client,
                    "edge" | "server" => Mode::Edge,
                    other => {
                        return Err(format!("unknown mode \"{other}\", expected client or edge"))
                    }
                });
            }
            "--strict_config" => {
                strict = match inline.as_deref() {
                    None | Some("true") => true,
                    Some("false") => false,
                    Some(other) => {
                        return Err(format!("invalid value for --strict_config: {other}"))
                    }
                }
            }
            "--local-control" => local_control = Some(value(key)?),
            "--controller" => controller = Some(value(key)?),
            "--enroll-token" => token = Some(value(key)?),
            "--name" => name = Some(value(key)?),
            "--state-dir" => state_dir = Some(value(key)?),
            "--ca-file" => ca_file = Some(value(key)?),
            "--tls-pin" => tls_pin = Some(value(key)?),
            "--public-host" => public_host = Some(value(key)?),
            "--allow-unsafe" => allow_unsafe.extend(
                value(key)?
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            ),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if help {
        return Ok(Command::Help);
    }
    if version {
        return Ok(Command::Version);
    }
    let mode = mode.ok_or("missing --mode <client|edge> (or run as frpc / frps)")?;
    if tls_pin.is_some() && controller.is_none() {
        return Err("--tls-pin requires --controller".into());
    }
    if public_host.is_some() && !(controller.is_some() && mode == Mode::Edge) {
        return Err("--public-host is only valid with --mode edge --controller".into());
    }
    let managed = match controller {
        Some(controller) => {
            if mode == Mode::Client && config.is_some() {
                return Err("--controller and -c cannot be combined".into());
            }
            Some(ManagedOpts {
                controller,
                enroll_token: token,
                name,
                state_dir,
                ca_file,
                tls_pin,
                public_host,
            })
        }
        None => None,
    };
    if convert && (verify || managed.is_some()) {
        return Err(
            "convert requires a standalone configuration and cannot combine with verify".into(),
        );
    }
    // 受管客户端没有配置文件；受管 Edge 的 `-c` 是可选的监听参数（frps.toml）。
    let config = match (&managed, config) {
        (Some(_), c) if mode == Mode::Edge => c.unwrap_or_default(),
        (Some(_), _) => String::new(),
        (None, Some(c)) => c,
        (None, None) => return Err("missing -c <config>".into()),
    };
    if local_control.is_some() && (managed.is_none() || verify) {
        return Err("--local-control requires a managed client run".into());
    }
    let opts = Options {
        mode,
        config,
        managed,
        local_control,
        strict,
        allow_unsafe,
    };
    Ok(if convert {
        Command::Convert(opts)
    } else if verify {
        Command::Verify(opts)
    } else {
        Command::Run(opts)
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn local_control_options() {
        let Command::Run(o) = p(
            "x",
            &[
                "--mode",
                "client",
                "--controller",
                "h",
                "--local-control",
                "/s/control.json",
            ],
        )
        .unwrap() else {
            panic!("run")
        };
        assert_eq!(o.local_control.as_deref(), Some("/s/control.json"));
        assert!(p(
            "x",
            &["--mode", "client", "-c", "x", "--local-control", "p"]
        )
        .is_err());
        assert!(p(
            "x",
            &[
                "verify",
                "--mode",
                "client",
                "--controller",
                "h",
                "--local-control",
                "p"
            ]
        )
        .is_err());
        assert!(p("x", &["--local-control"]).is_err());
    }

    fn p(argv0: &str, args: &[&str]) -> Result<Command, String> {
        parse_args(
            argv0,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    fn opts(mode: Mode, strict: bool, unsafe_: &[&str]) -> Options {
        Options {
            mode,
            config: "a.toml".into(),
            managed: None,
            local_control: None,
            strict,
            allow_unsafe: unsafe_.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn modes_and_argv0() {
        assert_eq!(
            p("rustfrp-agent", &["--mode", "client", "-c", "a.toml"]),
            Ok(Command::Run(opts(Mode::Client, true, &[])))
        );
        assert_eq!(
            p("rustfrp-agent", &["--mode=edge", "--config=a.toml"]),
            Ok(Command::Run(opts(Mode::Edge, true, &[])))
        );
        assert_eq!(
            p("rustfrp-agent", &["--mode", "server", "-c", "a.toml"]),
            Ok(Command::Run(opts(Mode::Edge, true, &[])))
        );
        assert_eq!(
            p("/usr/bin/frpc", &["-c", "a.toml"]),
            Ok(Command::Run(opts(Mode::Client, true, &[])))
        );
        assert_eq!(
            p("C:\\bin\\frps.exe", &["-c", "a.toml"]),
            Ok(Command::Run(opts(Mode::Edge, true, &[])))
        );
    }

    #[test]
    fn verify_strict_and_unsafe() {
        assert_eq!(
            p(
                "frpc",
                &[
                    "verify",
                    "-c",
                    "a.toml",
                    "--strict_config=false",
                    "--allow-unsafe",
                    "A,B",
                    "--allow-unsafe=C"
                ]
            ),
            Ok(Command::Verify(opts(Mode::Client, false, &["A", "B", "C"])))
        );
        assert_eq!(
            p("frpc", &["-c", "a.toml", "--strict_config"]),
            Ok(Command::Run(opts(Mode::Client, true, &[])))
        );
        assert_eq!(
            p("frpc", &["-c", "a.toml", "--strict_config=true"]),
            Ok(Command::Run(opts(Mode::Client, true, &[])))
        );
    }

    #[test]
    fn version_and_help_win() {
        assert_eq!(p("x", &["-v"]), Ok(Command::Version));
        assert_eq!(p("x", &["--version"]), Ok(Command::Version));
        assert_eq!(p("x", &["version"]), Ok(Command::Version));
        assert_eq!(p("x", &["-h"]), Ok(Command::Help));
        assert_eq!(p("x", &["help", "-v"]), Ok(Command::Help));
        assert!(HELP.contains("USAGE"));
    }

    #[test]
    fn managed_mode() {
        let got = p(
            "rustfrp-agent",
            &[
                "--mode",
                "client",
                "--controller",
                "h:7100",
                "--enroll-token",
                "t",
                "--name",
                "n",
                "--state-dir",
                "/s",
                "--ca-file",
                "/c.pem",
            ],
        )
        .unwrap();
        let Command::Run(o) = got else { panic!() };
        assert_eq!(o.config, "");
        assert_eq!(
            o.managed,
            Some(ManagedOpts {
                controller: "h:7100".into(),
                enroll_token: Some("t".into()),
                name: Some("n".into()),
                state_dir: Some("/s".into()),
                ca_file: Some("/c.pem".into()),
                tls_pin: None,
                public_host: None
            })
        );
        let Command::Run(o) = p("x", &["--mode", "client", "--controller", "h"]).unwrap() else {
            panic!()
        };
        assert_eq!(o.managed.unwrap().enroll_token, None);
        // 受管 Edge：-c 可选；--public-host 只属于它。
        let Command::Run(o) = p(
            "x",
            &[
                "--mode",
                "edge",
                "--controller",
                "h:7200",
                "--public-host",
                "edge.example.com",
            ],
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(o.config, "");
        assert_eq!(
            o.managed.unwrap().public_host.as_deref(),
            Some("edge.example.com")
        );
        let Command::Run(o) = p(
            "x",
            &["--mode", "edge", "--controller", "h", "-c", "frps.toml"],
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(o.config, "frps.toml");
        for bad in [
            &[
                "--mode",
                "client",
                "--controller",
                "h",
                "--public-host",
                "x",
            ][..],
            &["--mode", "edge", "-c", "a.toml", "--public-host", "x"][..],
        ] {
            assert!(
                p("x", bad).unwrap_err().contains("--public-host"),
                "{bad:?}"
            );
        }
        assert!(p(
            "x",
            &["--mode", "client", "--controller", "h", "-c", "a.toml"]
        )
        .unwrap_err()
        .contains("cannot be combined"));
    }

    #[test]
    fn errors() {
        assert!(p("x", &["-c", "a"]).unwrap_err().contains("missing --mode"));
        assert!(p("x", &["--mode", "client"])
            .unwrap_err()
            .contains("missing -c"));
        assert!(p("x", &["--mode", "other", "-c", "a"])
            .unwrap_err()
            .contains("unknown mode"));
        assert!(p("x", &["--bogus"])
            .unwrap_err()
            .contains("unknown argument"));
        assert!(p("x", &["-c"]).unwrap_err().contains("requires a value"));
        assert!(p("x", &["--mode"])
            .unwrap_err()
            .contains("requires a value"));
        assert!(p("x", &["--strict_config=maybe"])
            .unwrap_err()
            .contains("invalid value"));
    }
    #[test]
    fn managed_certificate_pin_is_explicit() {
        let Command::Run(o) = p(
            "x",
            &["--mode", "client", "--controller", "h", "--tls-pin", "abc"],
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(o.managed.unwrap().tls_pin, Some("abc".into()));
        assert!(p(
            "x",
            &["--mode", "client", "-c", "x.toml", "--tls-pin", "abc"]
        )
        .is_err());
    }
    #[test]
    fn ini_convert_command_is_unambiguous() {
        assert!(matches!(
            p("frpc", &["convert", "-c", "old.ini"]),
            Ok(Command::Convert(_))
        ));
        assert!(p("frpc", &["convert", "verify", "-c", "old.ini"]).is_err());
        assert!(p("frpc", &["convert", "--controller", "h"]).is_err());
    }
}
