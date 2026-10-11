//! frp 配置模板（Go `text/template` 的受支持子集）。
//!
//! frp 在解析前用 `text/template` 渲染配置，常见用法只有三类：
//! 1. `{{ .Envs.NAME }}` 读取环境变量；
//! 2. `{{ range $i, $v := parseNumberRange "6000-6006,6007" }} … {{ end }}` 批量生成代理；
//! 3. `{{ range $_, $v := parseNumberRangePair "a" "b" }}` + `{{ $v.First }}` / `{{ $v.Second }}`。
//!
//! 另支持 `{{-` / `-}}` 空白裁剪与 `{{/* 注释 */}}`。其余动作一律报错（不会静默输出错误内容）。

use std::collections::BTreeMap;

use super::error::ConfigError;

const MAX_RANGE_ITEMS: usize = 65_536;
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_NESTING: usize = 8;

/// 模板里可见的变量。
pub struct Values<'a> {
    /// `.Envs`。
    pub envs: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Item {
    Text(String),
    Action(String),
}

fn terr(msg: impl Into<String>) -> ConfigError {
    ConfigError::Template(msg.into())
}

fn tokenize(src: &str) -> Result<Vec<Item>, ConfigError> {
    let mut items = Vec::new();
    let mut rest = src;
    let mut trim_next = false;
    while let Some(start) = rest.find("{{") {
        let mut text = rest.get(..start).unwrap_or("");
        if trim_next {
            text = text.trim_start();
            trim_next = false;
        }
        let after = rest.get(start + 2..).unwrap_or("");
        let (trim_left, after) = match after.strip_prefix('-') {
            Some(a) if a.starts_with(char::is_whitespace) => (true, a),
            _ => (false, after),
        };
        let end = after.find("}}").ok_or_else(|| terr("unclosed action"))?;
        let mut body = after.get(..end).unwrap_or("");
        if trim_left {
            text = text.trim_end();
        }
        if let Some(b) = body.strip_suffix('-') {
            if b.ends_with(char::is_whitespace) {
                body = b;
                trim_next = true;
            }
        }
        if !text.is_empty() {
            items.push(Item::Text(text.to_string()));
        }
        items.push(Item::Action(body.trim().to_string()));
        rest = after.get(end + 2..).unwrap_or("");
    }
    let tail = if trim_next { rest.trim_start() } else { rest };
    if !tail.is_empty() {
        items.push(Item::Text(tail.to_string()));
    }
    Ok(items)
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Print(String),
    Range {
        key: Option<String>,
        val: String,
        call: String,
        body: Vec<Node>,
    },
}

fn parse(items: &[Item]) -> Result<Vec<Node>, ConfigError> {
    let mut pos = 0;
    let nodes = parse_block(items, &mut pos, 0)?;
    if pos < items.len() {
        return Err(terr("unexpected {{end}}"));
    }
    Ok(nodes)
}

fn parse_block(items: &[Item], pos: &mut usize, depth: usize) -> Result<Vec<Node>, ConfigError> {
    if depth > MAX_NESTING {
        return Err(terr("range nesting too deep"));
    }
    let mut out = Vec::new();
    while let Some(item) = items.get(*pos) {
        match item {
            Item::Text(t) => {
                out.push(Node::Text(t.clone()));
                *pos += 1;
            }
            Item::Action(a) if a == "end" => return Ok(out),
            Item::Action(a) if a.starts_with("/*") && a.ends_with("*/") => *pos += 1,
            Item::Action(a)
                if a == "range" || a.starts_with("range ") || a.starts_with("range\t") =>
            {
                *pos += 1;
                let (key, val, call) = parse_range_head(a.get(5..).unwrap_or("").trim())?;
                let body = parse_block(items, pos, depth + 1)?;
                match items.get(*pos) {
                    Some(Item::Action(e)) if e == "end" => *pos += 1,
                    _ => return Err(terr("range without {{end}}")),
                }
                out.push(Node::Range {
                    key,
                    val,
                    call,
                    body,
                });
            }
            Item::Action(a) => {
                out.push(Node::Print(a.clone()));
                *pos += 1;
            }
        }
    }
    Ok(out)
}

fn parse_range_head(head: &str) -> Result<(Option<String>, String, String), ConfigError> {
    let (vars, call) = head
        .split_once(":=")
        .ok_or_else(|| terr("range requires variable declaration, e.g. `range $i, $v := ...`"))?;
    let names: Vec<&str> = vars.split(',').map(str::trim).collect();
    let valid = |n: &str| n.starts_with('$') && n.len() > 1;
    match names.as_slice() {
        [v] if valid(v) => Ok((None, (*v).to_string(), call.trim().to_string())),
        [k, v] if valid(k) && valid(v) => Ok((
            Some((*k).to_string()),
            (*v).to_string(),
            call.trim().to_string(),
        )),
        _ => Err(terr("invalid range variables")),
    }
}

/// 一个值：字符串、整数或数对。
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Str(String),
    Int(i64),
    Pair(i64, i64),
}

impl Val {
    fn render(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            Val::Int(i) => i.to_string(),
            Val::Pair(a, b) => format!("{{{a} {b}}}"),
        }
    }
}

fn split_args(s: &str) -> Result<Vec<String>, ConfigError> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                in_str = !in_str;
                cur.push(ch);
            }
            c if c.is_whitespace() && !in_str => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_str {
        return Err(terr("unterminated string"));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

struct Ctx<'a> {
    values: &'a Values<'a>,
    vars: BTreeMap<String, Val>,
}

fn eval_arg(arg: &str, ctx: &Ctx<'_>) -> Result<Val, ConfigError> {
    if let Some(inner) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
        return Ok(Val::Str(inner.to_string()));
    }
    if let Some(name) = arg.strip_prefix(".Envs.") {
        return Ok(Val::Str(
            ctx.values.envs.get(name).cloned().unwrap_or_default(),
        ));
    }
    if arg.starts_with('$') {
        let (name, field) = match arg.split_once('.') {
            Some((n, f)) => (n, Some(f)),
            None => (arg, None),
        };
        let v = ctx
            .vars
            .get(name)
            .ok_or_else(|| terr(format!("undefined variable {name}")))?;
        return match (v, field) {
            (v, None) => Ok(v.clone()),
            (Val::Pair(a, _), Some("First")) => Ok(Val::Int(*a)),
            (Val::Pair(_, b), Some("Second")) => Ok(Val::Int(*b)),
            _ => Err(terr(format!("cannot evaluate field in {arg}"))),
        };
    }
    Err(terr(format!("unsupported expression: {arg}")))
}

/// 解析 `1000-2000,2001`；与 frp 的 `ParseRangeNumbers` 一致。
pub fn parse_range_numbers(s: &str) -> Result<Vec<i64>, String> {
    let mut out = Vec::new();
    for part in s.trim().split(',') {
        let bad = |e: std::num::ParseIntError| format!("range number is invalid, {e}");
        let pieces: Vec<&str> = part.split('-').collect();
        match pieces.as_slice() {
            [one] => out.push(one.trim().parse::<i64>().map_err(bad)?),
            [lo, hi] => {
                let lo = lo.trim().parse::<i64>().map_err(bad)?;
                let hi = hi.trim().parse::<i64>().map_err(bad)?;
                if hi < lo {
                    return Err("range number is invalid".to_string());
                }
                if (hi - lo) as usize >= MAX_RANGE_ITEMS {
                    return Err("range is too large".to_string());
                }
                out.extend(lo..=hi);
            }
            _ => return Err("range number is invalid".to_string()),
        }
        if out.len() > MAX_RANGE_ITEMS {
            return Err("range is too large".to_string());
        }
    }
    Ok(out)
}

fn eval_range(call: &str, ctx: &Ctx<'_>) -> Result<Vec<Val>, ConfigError> {
    let parts = split_args(call)?;
    let (name, args) = parts
        .split_first()
        .ok_or_else(|| terr("empty range expression"))?;
    let strs = args
        .iter()
        .map(|a| match eval_arg(a, ctx)? {
            Val::Str(s) => Ok(s),
            other => Err(terr(format!(
                "expected string argument, got {}",
                other.render()
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    match (name.as_str(), strs.as_slice()) {
        ("parseNumberRange", [a]) => Ok(parse_range_numbers(a)
            .map_err(terr)?
            .into_iter()
            .map(Val::Int)
            .collect()),
        ("parseNumberRangePair", [a, b]) => {
            let first = parse_range_numbers(a).map_err(terr)?;
            let second = parse_range_numbers(b).map_err(terr)?;
            if first.len() != second.len() {
                return Err(terr("first and second range numbers are not in pairs"));
            }
            Ok(first
                .into_iter()
                .zip(second)
                .map(|(x, y)| Val::Pair(x, y))
                .collect())
        }
        ("parseNumberRange" | "parseNumberRangePair", _) => {
            Err(terr(format!("wrong number of args for {name}")))
        }
        _ => Err(terr(format!("function \"{name}\" not defined"))),
    }
}

fn render_nodes(nodes: &[Node], ctx: &mut Ctx<'_>, out: &mut String) -> Result<(), ConfigError> {
    for node in nodes {
        if out.len() > MAX_OUTPUT_BYTES {
            return Err(terr("rendered output too large"));
        }
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Print(expr) => {
                let parts = split_args(expr)?;
                match parts.as_slice() {
                    [one] => out.push_str(&eval_arg(one, ctx)?.render()),
                    _ => return Err(terr(format!("unsupported action: {expr}"))),
                }
            }
            Node::Range {
                key,
                val,
                call,
                body,
            } => {
                for (i, item) in eval_range(call, ctx)?.into_iter().enumerate() {
                    if let Some(k) = key {
                        ctx.vars.insert(k.clone(), Val::Int(i as i64));
                    }
                    ctx.vars.insert(val.clone(), item);
                    render_nodes(body, ctx, out)?;
                }
            }
        }
    }
    Ok(())
}

/// 渲染模板。不含 `{{` 的输入原样返回。
pub fn render(src: &str, values: &Values<'_>) -> Result<String, ConfigError> {
    if !src.contains("{{") {
        return Ok(src.to_string());
    }
    let nodes = parse(&tokenize(src)?)?;
    let mut ctx = Ctx {
        values,
        vars: BTreeMap::new(),
    };
    let mut out = String::with_capacity(src.len());
    render_nodes(&nodes, &mut ctx, &mut out)?;
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn r(src: &str) -> Result<String, ConfigError> {
        let mut envs = BTreeMap::new();
        envs.insert("PORT".to_string(), "7000".to_string());
        envs.insert("RANGE".to_string(), "10-12".to_string());
        render(src, &Values { envs: &envs })
    }

    fn err(src: &str) -> String {
        r(src).unwrap_err().to_string()
    }

    #[test]
    fn plain_text_passthrough() {
        assert_eq!(r("a = 1\n").unwrap(), "a = 1\n");
    }

    #[test]
    fn env_variable() {
        assert_eq!(r("p = {{ .Envs.PORT }}").unwrap(), "p = 7000");
    }

    #[test]
    fn missing_env_renders_empty() {
        assert_eq!(r("x{{ .Envs.NOPE }}y").unwrap(), "xy");
    }

    #[test]
    fn range_single_variable() {
        let out = r(r#"{{ range $v := parseNumberRange "1-3" }}[{{ $v }}]{{ end }}"#).unwrap();
        assert_eq!(out, "[1][2][3]");
    }

    #[test]
    fn range_with_index_and_env_argument() {
        let out =
            r("{{ range $i, $v := parseNumberRange .Envs.RANGE }}{{ $i }}={{ $v }};{{ end }}")
                .unwrap();
        assert_eq!(out, "0=10;1=11;2=12;");
    }

    #[test]
    fn range_pair_fields() {
        let out = r(r#"{{ range $_, $v := parseNumberRangePair "1-2" "5,6" }}{{ $v.First }}:{{ $v.Second }} {{ end }}"#)
            .unwrap();
        assert_eq!(out, "1:5 2:6 ");
    }

    #[test]
    fn pair_prints_as_go_struct() {
        let out = r(r#"{{ range $v := parseNumberRangePair "1" "2" }}{{ $v }}{{ end }}"#).unwrap();
        assert_eq!(out, "{1 2}");
    }

    #[test]
    fn trim_markers() {
        let out = r("a  \n{{- .Envs.PORT -}}  \n b").unwrap();
        assert_eq!(out, "a7000b");
    }

    #[test]
    fn trim_marker_requires_whitespace() {
        // `{{-1}}` 不是裁剪标记，应按表达式处理并报错。
        assert!(err("{{-1}}").contains("unsupported expression"));
    }

    #[test]
    fn comments_are_dropped() {
        assert_eq!(r("a{{/* note */}}b").unwrap(), "ab");
    }

    #[test]
    fn nested_range() {
        let out = r(r#"{{ range $a := parseNumberRange "1-2" }}{{ range $b := parseNumberRange "7-8" }}{{ $a }}{{ $b }},{{ end }}{{ end }}"#)
            .unwrap();
        assert_eq!(out, "17,18,27,28,");
    }

    #[test]
    fn trailing_text_after_trim() {
        assert_eq!(r("{{ .Envs.PORT -}}   \n tail").unwrap(), "7000tail");
    }

    #[test]
    fn unclosed_action() {
        assert!(err("{{ .Envs.PORT").contains("unclosed action"));
    }

    #[test]
    fn range_without_end() {
        assert!(err(r#"{{ range $v := parseNumberRange "1" }}x"#).contains("without {{end}}"));
    }

    #[test]
    fn stray_end() {
        assert!(err("a{{ end }}").contains("unexpected {{end}}"));
    }

    #[test]
    fn range_requires_declaration() {
        assert!(
            err(r#"{{ range parseNumberRange "1" }}{{ end }}"#).contains("variable declaration")
        );
    }

    #[test]
    fn range_rejects_bad_variables() {
        assert!(err(r#"{{ range v := parseNumberRange "1" }}{{ end }}"#)
            .contains("invalid range variables"));
        assert!(
            err(r#"{{ range $a, $b, $c := parseNumberRange "1" }}{{ end }}"#)
                .contains("invalid range variables")
        );
        assert!(err(r#"{{ range $, $v := parseNumberRange "1" }}{{ end }}"#)
            .contains("invalid range variables"));
    }

    #[test]
    fn undefined_variable() {
        assert!(err("{{ $x }}").contains("undefined variable $x"));
    }

    #[test]
    fn unsupported_expression() {
        assert!(err("{{ foo }}").contains("unsupported expression: foo"));
    }

    #[test]
    fn unsupported_multi_arg_action() {
        assert!(err(r#"{{ printf "%d" 1 }}"#).contains("unsupported action"));
    }

    #[test]
    fn unknown_function_and_arity() {
        assert!(
            err(r#"{{ range $v := nope "1" }}{{ end }}"#).contains("function \"nope\" not defined")
        );
        assert!(
            err(r#"{{ range $v := parseNumberRange }}{{ end }}"#).contains("wrong number of args")
        );
        assert!(
            err(r#"{{ range $v := parseNumberRangePair "1" }}{{ end }}"#)
                .contains("wrong number of args")
        );
        assert!(err("{{ range $v := }}{{ end }}").contains("empty range expression"));
    }

    #[test]
    fn pair_length_mismatch() {
        assert!(
            err(r#"{{ range $v := parseNumberRangePair "1-3" "1" }}{{ end }}"#)
                .contains("not in pairs")
        );
    }

    #[test]
    fn bad_field_access() {
        assert!(
            err(r#"{{ range $v := parseNumberRange "1" }}{{ $v.First }}{{ end }}"#)
                .contains("cannot evaluate field")
        );
        assert!(
            err(r#"{{ range $v := parseNumberRangePair "1" "2" }}{{ $v.Third }}{{ end }}"#)
                .contains("cannot evaluate field")
        );
    }

    #[test]
    fn non_string_argument_is_rejected() {
        let src = r#"{{ range $v := parseNumberRange "1-2" }}{{ range $w := parseNumberRange $v }}{{ end }}{{ end }}"#;
        assert!(err(src).contains("expected string argument"));
    }

    #[test]
    fn unterminated_string() {
        assert!(err(r#"{{ range $v := parseNumberRange "1 }}{{ end }}"#)
            .contains("unterminated string"));
    }

    #[test]
    fn nesting_limit() {
        let mut src = String::new();
        for _ in 0..=MAX_NESTING + 1 {
            src.push_str(r#"{{ range $v := parseNumberRange "1" }}"#);
        }
        for _ in 0..=MAX_NESTING + 1 {
            src.push_str("{{ end }}");
        }
        assert!(err(&src).contains("nesting too deep"));
    }

    #[test]
    fn output_limit() {
        let src = r#"{{ range $a := parseNumberRange "1-60000" }}{{ range $b := parseNumberRange "1-60000" }}xxxxxxxxxxxxxxxx{{ end }}{{ end }}"#;
        assert!(err(src).contains("too large"));
    }

    #[test]
    fn parse_range_numbers_cases() {
        assert_eq!(
            parse_range_numbers("1000-1002, 2001").unwrap(),
            vec![1000, 1001, 1002, 2001]
        );
        assert!(parse_range_numbers("a")
            .unwrap_err()
            .contains("range number is invalid"));
        assert!(parse_range_numbers("1-b")
            .unwrap_err()
            .contains("range number is invalid"));
        assert!(parse_range_numbers("x-2")
            .unwrap_err()
            .contains("range number is invalid"));
        assert_eq!(
            parse_range_numbers("5-1").unwrap_err(),
            "range number is invalid"
        );
        assert_eq!(
            parse_range_numbers("1-2-3").unwrap_err(),
            "range number is invalid"
        );
        assert_eq!(
            parse_range_numbers("1-70000").unwrap_err(),
            "range is too large"
        );
        assert_eq!(
            parse_range_numbers("1-60000,1-60000").unwrap_err(),
            "range is too large"
        );
    }

    #[test]
    fn val_render() {
        assert_eq!(Val::Str("a".into()).render(), "a");
        assert_eq!(Val::Int(3).render(), "3");
        assert_eq!(Val::Pair(1, 2).render(), "{1 2}");
    }

    #[test]
    fn consecutive_trim_markers() {
        assert_eq!(
            r("{{ .Envs.PORT -}}\n  {{ .Envs.PORT }}").unwrap(),
            "70007000"
        );
        assert_eq!(r("a {{- .Envs.PORT }}").unwrap(), "a7000");
    }

    #[test]
    fn range_keyword_with_tab_and_bare_range() {
        let out = r("{{ range\t$v := parseNumberRange \"1\" }}{{ $v }}{{ end }}").unwrap();
        assert_eq!(out, "1");
        assert!(err("{{ range }}{{ end }}").contains("variable declaration"));
    }

    #[test]
    fn range_body_followed_by_non_end_action_is_an_error() {
        // 缺少 end 时，块解析在输入末尾结束。
        assert!(
            err(r#"{{ range $v := parseNumberRange "1" }}{{ $v }}"#).contains("without {{end}}")
        );
    }

    #[test]
    fn arguments_with_spaces_inside_strings() {
        let out = r(r#"{{ range $v := parseNumberRange "1, 2" }}{{ $v }}{{ end }}"#).unwrap();
        assert_eq!(out, "12");
        assert_eq!(split_args("a  b").unwrap(), vec!["a", "b"]);
        assert_eq!(split_args("").unwrap(), Vec::<String>::new());
    }
}
