//! 命令行解析：兼容 frp 的 `-c` / `--strict_config` / `--allow-unsafe` / `verify`，
//! 并识别被命名为 `frpc` / `frps` 的可执行文件（argv[0]）。

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
    /// 配置文件路径。
    pub config: String,
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
  rustfrp-agent verify --mode <client|edge> -c <config>
  frpc -c <config>        (when the binary is named frpc)
  frps -c <config>        (when the binary is named frps)

OPTIONS:
  -c, --config <file>          frp configuration file (TOML / YAML / JSON)
      --mode <client|edge>     role (implied by the binary name frpc / frps)
      --strict_config[=bool]   reject unknown fields (default: true)
      --allow-unsafe <a,b>     allow unsafe features, e.g. TokenSourceExec
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
    let (mut config, mut strict, mut allow_unsafe) = (None::<String>, true, Vec::<String>::new());
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
    let config = config.ok_or("missing -c <config>")?;
    let opts = Options {
        mode,
        config,
        strict,
        allow_unsafe,
    };
    Ok(if verify {
        Command::Verify(opts)
    } else {
        Command::Run(opts)
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

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
}
