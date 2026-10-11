//! 显式 INI 迁移：先渲染环境模板，再转换并展开 includes，输出 frp TOML。
use crate::cli::{Mode, Options};
use rustfrp_core::frpcfg::{self, CompatReport, ConfigFs, Env, ExportFormat, LoadOptions};
use std::io;

fn config_error(error: frpcfg::ConfigError) -> String {
    error.to_string()
}

fn rendered(fs: &dyn ConfigFs, env: &dyn Env, path: &str) -> Result<String, String> {
    let raw = fs
        .read_to_string(path)
        .map_err(|e| format!("read {path}: {e}"))?;
    if raw.len() > 1024 * 1024 {
        return Err("conversion input exceeds 1 MiB".into());
    }
    frpcfg::template::render(&raw, &frpcfg::template::Values { envs: &env.vars() })
        .map_err(config_error)
}
struct IncludeFs<'a> {
    fs: &'a dyn ConfigFs,
    env: &'a dyn Env,
}
impl ConfigFs for IncludeFs<'_> {
    fn read_to_string(&self, path: &str) -> io::Result<String> {
        if !path.ends_with(".ini") {
            return self.fs.read_to_string(path);
        }
        let mut raw = rendered(self.fs, self.env, path).map_err(io::Error::other)?;
        if !frpcfg::load::is_legacy_ini(&raw) {
            raw = format!("[common]\n{raw}");
        }
        frpcfg::ini::convert(&raw, true)
            .map(|v| v.to_string())
            .map_err(io::Error::other)
    }
    fn list_files(&self, dir: &str) -> io::Result<Vec<String>> {
        self.fs.list_files(dir)
    }
    fn dir_exists(&self, dir: &str) -> bool {
        self.fs.dir_exists(dir)
    }
    fn write_private(&self, _: &str, _: &str) -> io::Result<()> {
        Err(io::Error::other("conversion filesystem is read-only"))
    }
    fn create_dir_all(&self, _: &str) -> io::Result<()> {
        Err(io::Error::other("conversion filesystem is read-only"))
    }
}
/// 返回标准 TOML 与运行能力报告，源文件保持不变；includes 展开到输出中。
pub fn convert(
    o: &Options,
    fs: &dyn ConfigFs,
    env: &dyn Env,
) -> Result<(String, CompatReport), String> {
    let raw = rendered(fs, env, &o.config)?;
    let converted = frpcfg::ini::convert(&raw, o.mode == Mode::Client).map_err(config_error)?;
    let includes = IncludeFs { fs, env };
    let opts = LoadOptions {
        strict: true,
        fs: &includes,
        env,
    };
    match o.mode {
        Mode::Client => {
            let mut loaded =
                frpcfg::load_client_str(&converted.to_string(), frpcfg::Format::Auto, &opts)
                    .map_err(config_error)?;
            loaded.source.includes.clear();
            let warnings = frpcfg::validate::validate_client_all(
                &loaded.common,
                &loaded.proxies,
                &loaded.visitors,
                &o.allow_unsafe,
            )
            .map_err(config_error)?;
            for warning in warnings {
                loaded.report.issues.push(frpcfg::Issue {
                    severity: frpcfg::Severity::Ignored,
                    path: "legacy-conversion".into(),
                    message: warning,
                });
            }
            for proxy in &loaded.source.proxies {
                for option in crate::plan::ignored_options(proxy) {
                    loaded.report.issues.push(frpcfg::Issue {
                        severity: frpcfg::Severity::Unsupported,
                        path: format!("proxy [{}].{option}", proxy.name),
                        message: "preserved in export; runtime option is not implemented yet"
                            .into(),
                    });
                }
                if !["tcp", "udp", "http", "https"].contains(&proxy.kind.as_str())
                    || proxy.plugin.is_some()
                {
                    loaded.report.issues.push(frpcfg::Issue {
                        severity: frpcfg::Severity::Unsupported,
                        path: format!("proxy [{}]", proxy.name),
                        message: "preserved in export; proxy type or plugin is not implemented yet"
                            .into(),
                    });
                }
            }
            for visitor in &loaded.source.visitors {
                loaded.report.issues.push(frpcfg::Issue {
                    severity: frpcfg::Severity::Unsupported,
                    path: format!("visitor [{}]", visitor.name),
                    message: "preserved in export; visitors are not implemented yet".into(),
                });
            }
            Ok((
                frpcfg::export(&loaded.source, ExportFormat::Toml).map_err(config_error)?,
                loaded.report,
            ))
        }
        Mode::Edge => {
            let loaded =
                frpcfg::load_server_str(&converted.to_string(), frpcfg::Format::Auto, &opts)
                    .map_err(config_error)?;
            frpcfg::validate::validate_server(&loaded.config, &o.allow_unsafe)
                .map_err(config_error)?;
            Ok((
                frpcfg::export(&loaded.source, ExportFormat::Toml).map_err(config_error)?,
                loaded.report,
            ))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use frpcfg::{MapEnv, MemFs};
    fn options(mode: Mode) -> Options {
        Options {
            mode,
            config: "legacy.ini".into(),
            managed: None,
            local_control: None,
            strict: true,
            allow_unsafe: vec![],
        }
    }
    #[test]
    fn converts_client_server_and_expands_ini_includes() {
        let fs=MemFs::new().with("legacy.ini","[common]\nserver_addr=localhost\nincludes=extra/*.ini\n[ssh]\nlocal_port=22\nremote_port=6000\n").with("extra/http.ini","[web]\ntype=http\nlocal_port=80\ncustom_domains=web.example.com\n");
        let env = MapEnv::default();
        let (text, _) = convert(&options(Mode::Client), &fs, &env).unwrap();
        assert!(text.contains("[[proxies]]"));
        assert!(text.contains("web.example.com"));
        assert!(!text.contains("includes"));
        assert!(fs
            .read_to_string("legacy.ini")
            .unwrap()
            .contains("[common]"));
        let fs = MemFs::new().with(
            "legacy.ini",
            "[common]\nbind_port=7000\nvhost_http_port=80\n",
        );
        let (text, _) = convert(&options(Mode::Edge), &fs, &env).unwrap();
        assert!(text.contains("vhostHTTPPort = 80"));
        let wrapper = IncludeFs { fs: &fs, env: &env };
        assert!(wrapper.dir_exists("."));
        assert!(wrapper.list_files(".").is_ok());
        assert!(wrapper.write_private("x", "y").is_err());
        assert!(wrapper.create_dir_all("x").is_err());
    }
    #[test]
    fn explicit_conversion_errors_leave_source_untouched() {
        let env = MapEnv::default();
        for text in [
            "not-ini",
            "[common]\nserver_port=oops",
            "[common]\nserver_addr=localhost\n[bad]\nlocal_port=70000",
            "[common]\nserver_addr=localhost\nincludes=missing/*.ini",
            "[common]\nserver_addr={{ missing }}",
        ] {
            let fs = MemFs::new().with("legacy.ini", text);
            assert!(
                convert(&options(Mode::Client), &fs, &env).is_err(),
                "{text}"
            );
            assert_eq!(fs.read_to_string("legacy.ini").unwrap(), text);
        }
        assert!(convert(&options(Mode::Client), &MemFs::new(), &env).is_err());
        let fs = MemFs::new().with("legacy.ini", &"x".repeat(1024 * 1024 + 1));
        assert!(convert(&options(Mode::Client), &fs, &env).is_err());
        let fs = MemFs::new().with("legacy.ini", "[common]\nbind_port=70000");
        assert!(convert(&options(Mode::Edge), &fs, &env).is_err());
        let fs = MemFs::new().with("legacy.ini", "[common]\nunknown=1");
        assert!(convert(&options(Mode::Edge), &fs, &env).is_err());
        let fs = MemFs::new()
            .with(
                "legacy.ini",
                "[common]\nserver_addr=localhost\nincludes=extra/*.toml",
            )
            .with(
                "extra/x.toml",
                "[[proxies]]\nname='x'\ntype='tcp'\nlocalPort=22\nremotePort=6000",
            );
        assert!(convert(&options(Mode::Client), &fs, &env)
            .unwrap()
            .0
            .contains("name = \"x\""));
        let fs = MemFs::new()
            .with(
                "legacy.ini",
                "[common]\nserver_addr=localhost\nincludes=extra/*.ini",
            )
            .with(
                "extra/x.ini",
                "[common]\n[x]\nlocal_port=22\nremote_port=6000",
            );
        assert!(convert(&options(Mode::Client), &fs, &env).is_ok());
    }
    #[test]
    fn conversion_preserves_unsupported_proxy_and_visitor_options() {
        let fs=MemFs::new().with("legacy.ini","[common]\nserver_addr=localhost\n[private]\ntype=stcp\nsk=key\nlocal_port=22\n[visitor]\ntype=stcp\nrole=visitor\nserver_name=private\nbind_port=6000\nsk=key\n");
        let (text, report) = convert(&options(Mode::Client), &fs, &MapEnv::default()).unwrap();
        assert!(text.contains("[[visitors]]"));
        assert!(report.issues.iter().any(|i| i.path == "proxy [private]"));
        assert!(report.issues.iter().any(|i| i.path == "visitor [visitor]"));
    }
}

#[cfg(test)]
mod coverage_edges {
    use super::*;
    use rustfrp_core::frpcfg::{MapEnv, MemFs};
    #[test]
    fn unsupported_plugin_and_valid_visitor_are_reported() {
        let fs=MemFs::new().with("legacy.ini","[common]\nserver_addr=localhost\n[plugin]\ntype=tcp\nplugin=unix_domain_socket\nplugin_unix_path=/tmp/x\n[private]\ntype=stcp\nrole=server\nsk=secret\nlocal_ip=127.0.0.1\nlocal_port=22\n[private_visitor]\ntype=stcp\nrole=visitor\nserver_name=private\nsk=secret\nbind_addr=127.0.0.1\nbind_port=6000\n");
        let (text, report) = convert(
            &Options {
                mode: Mode::Client,
                config: "legacy.ini".into(),
                managed: None,
                local_control: None,
                strict: true,
                allow_unsafe: vec![],
            },
            &fs,
            &MapEnv::default(),
        )
        .unwrap();
        assert!(text.contains("[[visitors]]"));
        assert!(report.issues.iter().any(|i| i.path.contains("plugin")));
        assert!(report.issues.iter().any(|i| i.path.contains("visitor")));
    }
}
