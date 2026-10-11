//! 配置加载：读取 → 模板渲染 → 解析（TOML / YAML / JSON）→ includes → 补全默认值 → 过滤。
//! 流程与 frp `pkg/config/load.go` 保持一致。

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::compat::{check_client, check_server, CompatReport, Severity};
use super::env::{ConfigFs, Env};
use super::error::ConfigError;
use super::strict::{precheck_client, pretty_path};
use super::template::{render, Values};
use super::types::*;

/// 配置文件格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 按 frp 的方式自动识别：先试 TOML，再试 YAML / JSON。
    Auto,
    /// 仅 TOML。
    Toml,
}

/// 根据扩展名推断格式（`.toml` 强制 TOML；其余自动）。
pub fn detect_format(path: &str) -> Format {
    if path.to_lowercase().ends_with(".toml") {
        Format::Toml
    } else {
        Format::Auto
    }
}

/// 是否旧版 INI（含 `[common]` 段）。
pub fn is_legacy_ini(content: &str) -> bool {
    content.lines().any(|l| l.trim() == "[common]")
}

fn parse_value(content: &str, format: Format) -> Result<Value, ConfigError> {
    match toml::from_str::<Value>(content) {
        Ok(v) => Ok(v),
        Err(e) if format == Format::Toml => Err(ConfigError::Parse(format!("toml: {e}"))),
        Err(_) => {
            let mut v: Value =
                serde_yaml_ng::from_str(content).map_err(|e| ConfigError::Parse(e.to_string()))?;
            // frp 允许 YAML 顶层以 `.` 开头的键作为锚点承载，解析前丢弃。
            if let Value::Object(map) = &mut v {
                map.retain(|k, _| !k.starts_with('.'));
            }
            if v.is_null() {
                return Ok(Value::Object(Default::default()));
            }
            Ok(v)
        }
    }
}

/// 解析配置文本，返回类型化结果与未知字段路径。
fn decode<T: DeserializeOwned>(
    content: &str,
    format: Format,
    client_checks: bool,
) -> Result<(T, Vec<String>), ConfigError> {
    let value = parse_value(content, format)?;
    let mut unknown = Vec::new();
    if client_checks {
        precheck_client(&value, &mut unknown)?;
    }
    let typed: T =
        serde_ignored::deserialize(&value, |p| unknown.push(pretty_path(&p.to_string())))
            .map_err(|e| ConfigError::Parse(e.to_string()))?;
    let mut seen = BTreeSet::new();
    unknown.retain(|u| seen.insert(u.clone()));
    Ok((typed, unknown))
}

fn record_unknown(
    unknown: Vec<String>,
    strict: bool,
    report: &mut CompatReport,
) -> Result<(), ConfigError> {
    if strict {
        if let Some(first) = unknown.into_iter().next() {
            return Err(ConfigError::UnknownField(first));
        }
        return Ok(());
    }
    for path in unknown {
        report.add(
            Severity::UnknownField,
            path,
            "unknown field ignored (strict mode would reject it)",
        );
    }
    Ok(())
}

fn render_file(fs: &dyn ConfigFs, env: &dyn Env, path: &str) -> Result<String, ConfigError> {
    let raw = fs
        .read_to_string(path)
        .map_err(|e| ConfigError::Io(format!("{path}: {e}")))?;
    let vars = env.vars();
    render(&raw, &Values { envs: &vars })
}

/// 加载选项。
pub struct LoadOptions<'a> {
    /// 严格模式：出现未知字段即报错（frp 的 `--strict_config`）。
    pub strict: bool,
    /// 文件系统。
    pub fs: &'a dyn ConfigFs,
    /// 环境变量。
    pub env: &'a dyn Env,
}

/// 加载结果（客户端）。
#[derive(Debug, Clone)]
pub struct LoadedClient {
    /// 解析后的原始配置（未补全默认值，已合并 includes），用于无损导出。
    pub source: ClientConfig,
    /// 补全默认值后的公共配置（`proxies` / `visitors` 为空，见下方两个字段）。
    pub common: ClientConfig,
    /// 生效的代理（已按 `start` / `enabled` 过滤并补全默认值）。
    pub proxies: Vec<Proxy>,
    /// 生效的访问者。
    pub visitors: Vec<Visitor>,
    /// 兼容性报告。
    pub report: CompatReport,
}

/// 加载结果（服务端）。
#[derive(Debug, Clone)]
pub struct LoadedServer {
    /// 解析后的原始配置（未补全默认值）。
    pub source: ServerConfig,
    /// 补全默认值后的配置。
    pub config: ServerConfig,
    /// 兼容性报告。
    pub report: CompatReport,
}

/// 简单通配：支持 `*`（任意串）与 `?`（单字符）。
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < n.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some(pi);
                mark = ni;
                pi += 1;
            }
            Some(&c) if c == '?' || Some(&c) == n.get(ni) => {
                pi += 1;
                ni += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ni = mark;
                }
                None => return false,
            },
        }
    }
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
}

fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (path.get(..i).unwrap_or(""), path.get(i + 1..).unwrap_or("")),
        None => (".", path),
    }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir == "." {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn load_includes(
    patterns: &[String],
    opts: &LoadOptions<'_>,
    report: &mut CompatReport,
) -> Result<(Vec<Proxy>, Vec<Visitor>), ConfigError> {
    let (mut proxies, mut visitors) = (Vec::new(), Vec::new());
    for pattern in patterns {
        let (dir, file_pat) = split_path(pattern);
        if !opts.fs.dir_exists(dir) {
            return Err(ConfigError::Io(format!(
                "include: directory of {pattern} not exist"
            )));
        }
        let names = opts
            .fs
            .list_files(dir)
            .map_err(|e| ConfigError::Io(format!("{dir}: {e}")))?;
        for name in names.into_iter().filter(|n| glob_match(file_pat, n)) {
            let path = join_path(dir, &name);
            let content = render_file(opts.fs, opts.env, &path)?;
            let (cfg, unknown): (ClientConfig, _) = decode(&content, detect_format(&path), true)?;
            record_unknown(unknown, opts.strict, report)?;
            proxies.extend(cfg.proxies);
            visitors.extend(cfg.visitors);
        }
    }
    Ok((proxies, visitors))
}

fn check_duplicates(proxies: &[Proxy], visitors: &[Visitor]) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for p in proxies {
        if !seen.insert(p.name.as_str()) {
            return Err(ConfigError::Duplicated {
                kind: "proxy",
                name: p.name.clone(),
            });
        }
    }
    let mut seen = BTreeSet::new();
    for v in visitors {
        if !seen.insert(v.name.as_str()) {
            return Err(ConfigError::Duplicated {
                kind: "visitor",
                name: v.name.clone(),
            });
        }
    }
    Ok(())
}

/// 加载 frpc 配置文件。
pub fn load_client(path: &str, opts: &LoadOptions<'_>) -> Result<LoadedClient, ConfigError> {
    let raw = opts
        .fs
        .read_to_string(path)
        .map_err(|e| ConfigError::Io(format!("{path}: {e}")))?;
    if is_legacy_ini(&raw) {
        return Err(ConfigError::LegacyIni);
    }
    load_client_str(
        &render_file(opts.fs, opts.env, path)?,
        detect_format(path),
        opts,
    )
}

/// 从文本加载 frpc 配置（已完成模板渲染；面板「导入」使用）。
pub fn load_client_str(
    content: &str,
    format: Format,
    opts: &LoadOptions<'_>,
) -> Result<LoadedClient, ConfigError> {
    let mut report = CompatReport::default();
    let (mut source, unknown): (ClientConfig, _) = decode(content, format, true)?;
    record_unknown(unknown, opts.strict, &mut report)?;

    if !source.includes.is_empty() {
        let (p, v) = load_includes(&source.includes, opts, &mut report)?;
        source.proxies.extend(p);
        source.visitors.extend(v);
    }

    let mut common = source.clone();
    common.proxies = Vec::new();
    common.visitors = Vec::new();
    common.complete(opts.env);

    check_duplicates(&source.proxies, &source.visitors)?;

    let started = |name: &str| common.start.is_empty() || common.start.iter().any(|s| s == name);
    let mut proxies: Vec<Proxy> = source
        .proxies
        .iter()
        .filter(|p| started(&p.name) && p.is_enabled())
        .cloned()
        .collect();
    let mut visitors: Vec<Visitor> = source
        .visitors
        .iter()
        .filter(|v| started(&v.name) && v.is_enabled())
        .cloned()
        .collect();
    proxies.iter_mut().for_each(Proxy::complete);
    visitors.iter_mut().for_each(Visitor::complete);

    check_client(&common, &proxies, &visitors, &mut report);
    Ok(LoadedClient {
        source,
        common,
        proxies,
        visitors,
        report,
    })
}

/// 加载 frps 配置文件。
pub fn load_server(path: &str, opts: &LoadOptions<'_>) -> Result<LoadedServer, ConfigError> {
    let raw = opts
        .fs
        .read_to_string(path)
        .map_err(|e| ConfigError::Io(format!("{path}: {e}")))?;
    if is_legacy_ini(&raw) {
        return Err(ConfigError::LegacyIni);
    }
    load_server_str(
        &render_file(opts.fs, opts.env, path)?,
        detect_format(path),
        opts,
    )
}

/// 从文本加载 frps 配置。
pub fn load_server_str(
    content: &str,
    format: Format,
    opts: &LoadOptions<'_>,
) -> Result<LoadedServer, ConfigError> {
    let mut report = CompatReport::default();
    let (source, unknown): (ServerConfig, _) = decode(content, format, false)?;
    record_unknown(unknown, opts.strict, &mut report)?;
    let mut config = source.clone();
    config.complete();
    check_server(&config, &mut report);
    Ok(LoadedServer {
        source,
        config,
        report,
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::frpcfg::env::{MapEnv, MemFs};

    fn opts<'a>(strict: bool, fs: &'a MemFs, env: &'a MapEnv) -> LoadOptions<'a> {
        LoadOptions { strict, fs, env }
    }

    fn client(content: &str, strict: bool) -> Result<LoadedClient, ConfigError> {
        let (fs, env) = (MemFs::new(), MapEnv::default());
        load_client_str(content, Format::Auto, &opts(strict, &fs, &env))
    }

    fn server(content: &str, strict: bool) -> Result<LoadedServer, ConfigError> {
        let (fs, env) = (MemFs::new(), MapEnv::default());
        load_server_str(content, Format::Auto, &opts(strict, &fs, &env))
    }

    const BASIC: &str = r#"
serverAddr = "1.2.3.4"
serverPort = 7000
[[proxies]]
name = "ssh"
type = "tcp"
localPort = 22
remotePort = 6000
"#;

    #[test]
    fn toml_client_with_defaults() {
        let l = client(BASIC, true).unwrap();
        assert_eq!(l.common.server_addr, "1.2.3.4");
        assert_eq!(l.proxies.len(), 1);
        assert_eq!(l.proxies[0].local_ip, "127.0.0.1");
        assert_eq!(l.common.transport.protocol, "tcp");
        assert_eq!(l.source.transport.protocol, "", "source stays pristine");
        assert!(l.report.issues.is_empty());
    }

    #[test]
    fn yaml_and_json_clients() {
        let y = "serverAddr: 9.9.9.9\nproxies:\n  - name: a\n    type: udp\n    localPort: 53\n    remotePort: 5353\n";
        let l = client(y, true).unwrap();
        assert_eq!(
            (l.common.server_addr.as_str(), l.proxies[0].kind.as_str()),
            ("9.9.9.9", "udp")
        );
        let j = r#"{"serverAddr":"8.8.8.8","proxies":[{"name":"a","type":"tcp","localPort":1}]}"#;
        assert_eq!(client(j, true).unwrap().common.server_addr, "8.8.8.8");
    }

    #[test]
    fn empty_and_null_documents_are_default_configs() {
        assert_eq!(client("", true).unwrap().common.server_port, 7000);
        assert_eq!(client("~\n", true).unwrap().common.server_port, 7000);
        assert_eq!(server("", true).unwrap().config.bind_port, 7000);
    }

    #[test]
    fn yaml_dot_prefixed_top_level_keys_are_dropped() {
        let y = ".anchors:\n  x: &x 1\nserverAddr: 5.5.5.5\n";
        assert_eq!(client(y, true).unwrap().common.server_addr, "5.5.5.5");
    }

    #[test]
    fn syntax_errors() {
        let (fs, env) = (MemFs::new(), MapEnv::default());
        let e = load_client_str("a = ", Format::Toml, &opts(true, &fs, &env)).unwrap_err();
        assert!(e.to_string().starts_with("toml:"), "{e}");
        let e = client("{ not: [valid", true).unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)));
        let e = client("serverPort = \"abc\"", true).unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)), "{e}");
    }

    #[test]
    fn unknown_fields_strict_vs_lenient() {
        let src = format!("{BASIC}\nbogus = 1\n[log]\nlevel = \"info\"\nnope = true\n");
        // bogus 位于 [[proxies]] 表内；nope 位于 [log] 表内。
        let e = client(&src, true).unwrap_err();
        assert_eq!(e, ConfigError::UnknownField("proxies[0].bogus".into()));
        let l = client(&src, false).unwrap();
        let unknown: Vec<_> = l
            .report
            .of(Severity::UnknownField)
            .map(|i| i.path.as_str())
            .collect();
        assert_eq!(unknown, vec!["proxies[0].bogus", "log.nope"]);
    }

    #[test]
    fn nested_unknown_field_and_type_specific_field() {
        let e = client("log.nope = 1\n", true).unwrap_err();
        assert_eq!(e, ConfigError::UnknownField("log.nope".into()));
        let src = "[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\ncustomDomains=['x']\n";
        assert_eq!(
            client(src, true).unwrap_err(),
            ConfigError::UnknownField("proxies[0].customDomains".into())
        );
        // 同一路径被多处发现时只报一次。
        let l = client(
            "[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nzzz=1\n",
            false,
        )
        .unwrap();
        assert_eq!(l.report.of(Severity::UnknownField).count(), 1);
    }

    #[test]
    fn unknown_proxy_type_errors_even_when_lenient() {
        let e = client("[[proxies]]\nname='a'\ntype='ftp'\n", false).unwrap_err();
        assert_eq!(
            e.to_string(),
            "decode proxy at index 0: unknown proxy type: ftp"
        );
    }

    #[test]
    fn server_unknown_fields() {
        assert_eq!(
            server("bindPort = 1\nzzz = 2", true).unwrap_err(),
            ConfigError::UnknownField("zzz".into())
        );
        let l = server("bindPort = 1\nzzz = 2", false).unwrap();
        assert_eq!(l.report.of(Severity::UnknownField).count(), 1);
    }

    #[test]
    fn start_and_enabled_filter() {
        let src = r#"
start = ["a", "c"]
[[proxies]]
name = "a"
type = "tcp"
localPort = 1
[[proxies]]
name = "b"
type = "tcp"
localPort = 2
[[proxies]]
name = "c"
type = "tcp"
localPort = 3
enabled = false
[[visitors]]
name = "a"
type = "stcp"
serverName = "s"
bindPort = 9
[[visitors]]
name = "b"
type = "stcp"
serverName = "s"
bindPort = 10
"#;
        let l = client(src, true).unwrap();
        assert_eq!(
            l.proxies
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        assert_eq!(
            l.visitors
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        assert_eq!(l.source.proxies.len(), 3, "source keeps everything");
        assert_eq!(l.visitors[0].bind_addr, "127.0.0.1");
    }

    #[test]
    fn disabled_without_start_list() {
        let src = "[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nenabled=false\n[[visitors]]\nname='v'\ntype='stcp'\nenabled=false\n";
        let l = client(src, true).unwrap();
        assert!(l.proxies.is_empty() && l.visitors.is_empty());
    }

    #[test]
    fn duplicate_names() {
        let p = "[[proxies]]\nname='a'\ntype='tcp'\n[[proxies]]\nname='a'\ntype='udp'\n";
        assert_eq!(
            client(p, true).unwrap_err().to_string(),
            "proxy name [a] is duplicated"
        );
        let v = "[[visitors]]\nname='a'\ntype='stcp'\n[[visitors]]\nname='a'\ntype='stcp'\n";
        assert_eq!(
            client(v, true).unwrap_err().to_string(),
            "visitor name [a] is duplicated"
        );
        let ok = "[[proxies]]\nname='a'\ntype='stcp'\n[[visitors]]\nname='a'\ntype='stcp'\n";
        assert!(
            client(ok, true).is_ok(),
            "proxy and visitor namespaces are independent"
        );
    }

    #[test]
    fn includes_merge_proxies_and_visitors() {
        let fs = MemFs::new()
            .with(
                "conf/frpc.toml",
                "serverAddr='1.1.1.1'\nincludes=['conf/extra/*.toml']\n",
            )
            .with(
                "conf/extra/a.toml",
                "[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\n",
            )
            .with(
                "conf/extra/b.toml",
                "[[visitors]]\nname='v'\ntype='stcp'\nserverName='s'\nbindPort=5\n",
            )
            .with(
                "conf/extra/skip.yaml",
                "proxies:\n - {name: z, type: tcp, localPort: 9}\n",
            )
            .with("conf/extra/note.txt", "ignored");
        let env = MapEnv::default();
        let l = load_client("conf/frpc.toml", &opts(true, &fs, &env)).unwrap();
        let names: Vec<_> = l.proxies.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
        assert_eq!(l.visitors.len(), 1);
        assert_eq!(l.source.proxies.len(), 1);
    }

    #[test]
    fn includes_glob_matches_other_extensions() {
        let fs = MemFs::new().with("frpc.toml", "includes=['inc/*']\n").with(
            "inc/one.yaml",
            "proxies:\n - {name: y, type: tcp, localPort: 9}\n",
        );
        let env = MapEnv::default();
        let l = load_client("frpc.toml", &opts(true, &fs, &env)).unwrap();
        assert_eq!(l.proxies[0].name, "y");
    }

    #[test]
    fn includes_errors() {
        let env = MapEnv::default();
        let fs = MemFs::new().with("frpc.toml", "includes=['nodir/*.toml']\n");
        let e = load_client("frpc.toml", &opts(true, &fs, &env)).unwrap_err();
        assert!(e
            .to_string()
            .contains("include: directory of nodir/*.toml not exist"));

        let fs = MemFs::new()
            .with("frpc.toml", "includes=['inc/*.toml']\n")
            .with("inc/bad.toml", "zzz = 1\n");
        assert_eq!(
            load_client("frpc.toml", &opts(true, &fs, &env)).unwrap_err(),
            ConfigError::UnknownField("zzz".into())
        );
        // 非严格模式：include 文件里的未知字段进报告。
        let l = load_client("frpc.toml", &opts(false, &fs, &env)).unwrap();
        assert_eq!(l.report.of(Severity::UnknownField).count(), 1);
    }

    #[test]
    fn load_from_file_paths() {
        let env = MapEnv::default();
        let fs = MemFs::new()
            .with("frpc.toml", BASIC)
            .with("frps.toml", "bindPort = 7100\n");
        assert_eq!(
            load_client("frpc.toml", &opts(true, &fs, &env))
                .unwrap()
                .proxies
                .len(),
            1
        );
        assert_eq!(
            load_server("frps.toml", &opts(true, &fs, &env))
                .unwrap()
                .config
                .bind_port,
            7100
        );
        let e = load_client("none.toml", &opts(true, &fs, &env)).unwrap_err();
        assert!(matches!(e, ConfigError::Io(m) if m.starts_with("none.toml:")));
        assert!(matches!(
            load_server("none.toml", &opts(true, &fs, &env)),
            Err(ConfigError::Io(_))
        ));
    }

    #[test]
    fn legacy_ini_is_detected() {
        let env = MapEnv::default();
        let fs = MemFs::new().with("old.ini", "[common]\nserver_addr = 1.1.1.1\n");
        assert_eq!(
            load_client("old.ini", &opts(true, &fs, &env)).unwrap_err(),
            ConfigError::LegacyIni
        );
        assert_eq!(
            load_server("old.ini", &opts(true, &fs, &env)).unwrap_err(),
            ConfigError::LegacyIni
        );
        assert!(is_legacy_ini("# c\n  [common]  \nx=1"));
        assert!(!is_legacy_ini("[commonx]\n"));
    }

    #[test]
    fn templates_run_before_parsing() {
        let mut envs = std::collections::BTreeMap::new();
        envs.insert("TOKEN".to_string(), "s3cret".to_string());
        let env = MapEnv(envs);
        let src = "auth.token = \"{{ .Envs.TOKEN }}\"\n{{- range $_, $v := parseNumberRangePair \"6000-6001\" \"7000-7001\" }}\n[[proxies]]\nname = \"p-{{ $v.First }}\"\ntype = \"tcp\"\nlocalPort = {{ $v.Second }}\nremotePort = {{ $v.First }}\n{{- end }}\n";
        let fs = MemFs::new().with("frpc.toml", src);
        let l = load_client("frpc.toml", &opts(true, &fs, &env)).unwrap();
        assert_eq!(l.common.auth.token, "s3cret");
        assert_eq!(l.proxies.len(), 2);
        assert_eq!(
            (
                l.proxies[1].name.as_str(),
                l.proxies[1].local_port,
                l.proxies[1].remote_port
            ),
            ("p-6001", 7001, 6001)
        );
        let fs = MemFs::new().with("frpc.toml", "x = {{ nope }}");
        assert!(matches!(
            load_client("frpc.toml", &opts(true, &fs, &env)),
            Err(ConfigError::Template(_))
        ));
    }

    #[test]
    fn compat_report_is_attached() {
        let l = client("transport.protocol = \"quic\"\n", true).unwrap();
        assert!(l.report.has_unsupported());
        let s = server("kcpBindPort = 7000\n", true).unwrap();
        assert!(s.report.has_unsupported());
    }

    #[test]
    fn detect_format_by_extension() {
        assert_eq!(detect_format("a.TOML"), Format::Toml);
        assert_eq!(detect_format("a.yaml"), Format::Auto);
        assert_eq!(detect_format("a"), Format::Auto);
    }

    #[test]
    fn glob_matching() {
        for (p, n, want) in [
            ("*.toml", "a.toml", true),
            ("*.toml", "a.yaml", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("*", "", true),
            ("*", "anything", true),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "aXXbYY", false),
            ("abc", "abc", true),
            ("abc", "abd", false),
            ("", "", true),
            ("", "a", false),
            ("a*", "a", true),
            ("*a", "ba", true),
            ("*a", "bab", false),
        ] {
            assert_eq!(glob_match(p, n), want, "{p} vs {n}");
        }
    }

    #[test]
    fn split_and_join_paths() {
        assert_eq!(split_path("a/b/c.toml"), ("a/b", "c.toml"));
        assert_eq!(split_path("c.toml"), (".", "c.toml"));
        assert_eq!(join_path(".", "x"), "x");
        assert_eq!(join_path("d", "x"), "d/x");
    }
}
