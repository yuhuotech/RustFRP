//! HTTP/1.x 请求侧的严格解析与分帧：请求头、Basic 凭据、`Content-Length` / `chunked` 正文边界。
//!
//! 只服务于 Edge 的 HTTP 虚拟主机：逐请求路由 / 鉴权 / 改写请求头，需要知道每个请求在字节流中的
//! 起止，但**不**缓冲正文（内存与正文大小无关）。对会引发请求走私的歧义一律拒绝。
//! 全部是无副作用的字节处理，面对任意输入不得 panic。

use std::fmt::Write as _;

/// 请求头最多允许的头字段数。
pub const MAX_HEADERS: usize = 64;
/// 单个分块大小行（含扩展）与 trailer 的字节上限。
const MAX_CHUNK_LINE: usize = 4096;
const MAX_TRAILERS: usize = 8 * 1024;

/// 正文分帧方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// 无正文。
    None,
    /// 固定长度。
    Length(u64),
    /// 分块传输。
    Chunked,
}

/// 请求头解析失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadError {
    /// 语法错误或存在走私风险的歧义。
    Invalid(&'static str),
    /// 暂不支持（如 CONNECT）。
    Unsupported(&'static str),
}

/// 已解析的请求头（不含正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    /// 请求头总长度（含结尾空行）。
    pub head_len: usize,
    /// 方法。
    pub method: String,
    /// 路径（不含查询串；绝对形式的目标会去掉 scheme 与 authority）。
    pub path: String,
    /// 小写、无端口的 Host。
    pub host: Option<String>,
    /// 正文分帧。
    pub framing: Framing,
    /// 是否是协议升级（WebSocket 等）：头结束后转为原样透传。
    pub upgrade: bool,
    /// Basic 凭据（用户名, 密码）。
    pub basic: Option<(String, String)>,
}

fn token_list_has(value: &[u8], want: &str) -> bool {
    value
        .split(|b| *b == b',')
        .any(|t| t.trim_ascii().eq_ignore_ascii_case(want.as_bytes()))
}

/// 解析请求头；数据不足返回 `Ok(None)`。
pub fn parse_request(buf: &[u8]) -> Result<Option<RequestHead>, HeadError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    let head_len = match req.parse(buf) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(httparse::Error::TooManyHeaders) => return Err(HeadError::Invalid("too many headers")),
        Err(_) => return Err(HeadError::Invalid("malformed request head")),
    };
    // `Complete` 时 httparse 保证这三项都有值，默认值只是类型上的兜底。
    let (method, target, minor) = (
        req.method.unwrap_or_default(),
        req.path.unwrap_or("/"),
        req.version.unwrap_or(1),
    );
    if method.eq_ignore_ascii_case("CONNECT") {
        return Err(HeadError::Unsupported("CONNECT"));
    }
    let mut host = None;
    let mut length: Option<u64> = None;
    let mut chunked = false;
    let (mut has_te, mut conn_upgrade, mut has_upgrade) = (false, false, false);
    let mut basic = None;
    for h in req.headers.iter() {
        let name = h.name;
        if name.eq_ignore_ascii_case("host") {
            if host.is_some() {
                return Err(HeadError::Invalid("duplicate Host"));
            }
            host = Some(std::str::from_utf8(h.value).ok().and_then(host_only));
        } else if name.eq_ignore_ascii_case("content-length") {
            let v = std::str::from_utf8(h.value)
                .ok()
                .map(str::trim)
                .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or(HeadError::Invalid("invalid Content-Length"))?;
            if length.is_some_and(|l| l != v) {
                return Err(HeadError::Invalid("conflicting Content-Length"));
            }
            length = Some(v);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            has_te = true;
            // 只接受以 chunked 结尾的编码列表；其他一律拒绝（无法确定边界）。
            let last = h.value.rsplit(|b| *b == b',').next().unwrap_or(&[]);
            if !last.trim_ascii().eq_ignore_ascii_case(b"chunked") {
                return Err(HeadError::Invalid("unsupported Transfer-Encoding"));
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case("connection") {
            conn_upgrade |= token_list_has(h.value, "upgrade");
        } else if name.eq_ignore_ascii_case("upgrade") {
            has_upgrade = true;
        } else if name.eq_ignore_ascii_case("authorization") && basic.is_none() {
            basic = std::str::from_utf8(h.value).ok().and_then(parse_basic);
        }
    }
    if has_te && length.is_some() {
        return Err(HeadError::Invalid(
            "both Content-Length and Transfer-Encoding",
        ));
    }
    if has_te && minor == 0 {
        return Err(HeadError::Invalid("Transfer-Encoding in HTTP/1.0"));
    }
    let framing = match (chunked, length) {
        (true, _) => Framing::Chunked,
        (false, Some(n)) if n > 0 => Framing::Length(n),
        _ => Framing::None,
    };
    let (target_host, path) = split_target(target);
    let host = match host {
        Some(Some(h)) => Some(h),
        Some(None) => return Err(HeadError::Invalid("invalid Host")),
        None => target_host.and_then(host_only),
    };
    Ok(Some(RequestHead {
        head_len,
        method: method.to_string(),
        path,
        host,
        framing,
        upgrade: conn_upgrade && has_upgrade,
        basic,
    }))
}

/// 去掉端口并小写；IPv6 字面量保留方括号内的内容。
fn host_only(raw: &str) -> Option<String> {
    let t = raw.trim();
    let host = if let Some(rest) = t.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        t.rsplit_once(':').map_or(t, |(h, p)| {
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
                h
            } else {
                t
            }
        })
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 请求目标 → （绝对形式里的 authority, 路径）。查询串与片段不参与路由。
fn split_target(target: &str) -> (Option<&str>, String) {
    let (authority, rest) = match target.split_once("://") {
        Some((_, r)) => match r.find('/') {
            Some(i) => (Some(r.get(..i).unwrap_or("")), r.get(i..).unwrap_or("/")),
            None => (Some(r), "/"),
        },
        None => (None, target),
    };
    let path = rest.split(['?', '#']).next().unwrap_or("/");
    (
        authority,
        if path.is_empty() {
            "/".into()
        } else {
            path.into()
        },
    )
}

/// 路径是否落在 location 前缀内（frp 语义：`HasPrefix(path, loc) || HasPrefix(path+"/", loc)`）。
pub fn location_matches(path: &str, location: &str) -> bool {
    path.starts_with(location) || location.strip_prefix(path) == Some("/")
}

fn b64(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// 解析 `Authorization: Basic ...` 的值。
pub fn parse_basic(value: &str) -> Option<(String, String)> {
    let (scheme, token) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let data = token.trim().trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in data {
        acc = (acc << 6) | u32::from(b64(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    let text = String::from_utf8(out).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// 定时无关的字节串比较（长度不同直接不等，长度本身不是秘密）。
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 头字段名 / 值是否可安全写入（无控制字符，名为 token）。
pub fn valid_header(name: &str, value: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        && value
            .bytes()
            .all(|b| b == b'\t' || (b >= 0x20 && b != 0x7f))
}

/// 重写请求头：`rewrite_host` 非空且合法时替换 Host；`set` 中的头字段替换同名头（不存在则追加）。
/// 其余头字段与请求行原样保留；非法的 `set` 项被忽略。
pub fn rewrite_request(raw: &[u8], rewrite_host: &str, set: &[(String, String)]) -> Vec<u8> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    let Ok(httparse::Status::Complete(head_len)) = req.parse(raw) else {
        return raw.to_vec();
    };
    let (method, target, minor) = (
        req.method.unwrap_or_default(),
        req.path.unwrap_or("/"),
        req.version.unwrap_or(1),
    );
    let mut out = String::with_capacity(head_len + 64);
    let _ = write!(out, "{method} {target} HTTP/1.{minor}\r\n");
    let host =
        (!rewrite_host.is_empty() && valid_header("Host", rewrite_host)).then_some(rewrite_host);
    let mut host_done = false;
    let mut used: Vec<usize> = Vec::new();
    for h in req.headers.iter() {
        let value = String::from_utf8_lossy(h.value);
        if let Some(new_host) = host.filter(|_| h.name.eq_ignore_ascii_case("host")) {
            if !host_done {
                host_done = true;
                let _ = write!(out, "Host: {new_host}\r\n");
            }
            continue;
        }
        let replacement = set
            .iter()
            .enumerate()
            .find(|(_, (n, v))| n.eq_ignore_ascii_case(h.name) && valid_header(n, v));
        if let Some((i, (n, v))) = replacement {
            if !used.contains(&i) {
                used.push(i);
                let _ = write!(out, "{n}: {v}\r\n");
            }
            continue;
        }
        let _ = write!(out, "{}: {value}\r\n", h.name);
    }
    for (i, (n, v)) in set.iter().enumerate() {
        if !used.contains(&i) && valid_header(n, v) {
            let _ = write!(out, "{n}: {v}\r\n");
        }
    }
    out.push_str("\r\n");
    out.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chunk {
    /// 读分块大小（十六进制）。
    Size,
    /// 分块扩展，直到行尾。
    Ext,
    /// 大小行的 CR 之后，等待 LF。
    SizeLf,
    /// 分块数据剩余字节。
    Data,
    /// 数据后的 CR。
    DataCr,
    /// 数据后的 LF。
    DataLf,
    /// trailer 行首（空行表示结束）。
    TrailerStart,
    /// trailer 行内。
    Trailer,
    /// trailer 行的 CR 之后。
    TrailerLf,
    /// 结尾空行的 LF。
    EndLf,
    /// 结束。
    Done,
}

/// 分块正文边界扫描器：只告诉调用方有多少字节属于当前正文，不缓冲、不改写。
#[derive(Debug)]
pub struct ChunkedScanner {
    state: Chunk,
    size: u64,
    digits: u8,
    line: usize,
}

impl Default for ChunkedScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkedScanner {
    /// 新建（位于第一个分块的大小行）。
    pub fn new() -> Self {
        Self {
            state: Chunk::Size,
            size: 0,
            digits: 0,
            line: 0,
        }
    }

    /// 喂入字节，返回（属于本正文的字节数, 是否已结束）。语法错误返回 `Err`。
    pub fn feed(&mut self, data: &[u8]) -> Result<(usize, bool), HeadError> {
        let bad = HeadError::Invalid("malformed chunked body");
        let mut i = 0;
        while i < data.len() && self.state != Chunk::Done {
            if self.state == Chunk::Data {
                let left = usize::try_from(self.size).unwrap_or(usize::MAX);
                let take = left.min(data.len() - i);
                i += take;
                self.size -= take as u64;
                if self.size == 0 {
                    self.state = Chunk::DataCr;
                }
                continue;
            }
            let b = *data.get(i).ok_or(bad)?;
            i += 1;
            match self.state {
                Chunk::Size => match b {
                    b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => {
                        let d = u64::from((b as char).to_digit(16).ok_or(bad)?);
                        self.digits += 1;
                        self.size = self
                            .size
                            .checked_mul(16)
                            .and_then(|s| s.checked_add(d))
                            .ok_or(bad)?;
                        if self.digits > 32 {
                            return Err(bad);
                        }
                    }
                    b';' | b' ' | b'\t' if self.digits > 0 => {
                        self.state = Chunk::Ext;
                        self.line = 0;
                    }
                    b'\r' if self.digits > 0 => self.state = Chunk::SizeLf,
                    _ => return Err(bad),
                },
                Chunk::Ext => {
                    self.line += 1;
                    if self.line > MAX_CHUNK_LINE {
                        return Err(bad);
                    }
                    if b == b'\r' {
                        self.state = Chunk::SizeLf;
                    } else if b == b'\n' || b == 0 {
                        return Err(bad);
                    }
                }
                Chunk::SizeLf => {
                    if b != b'\n' {
                        return Err(bad);
                    }
                    self.digits = 0;
                    if self.size == 0 {
                        self.state = Chunk::TrailerStart;
                        self.line = 0;
                    } else {
                        self.state = Chunk::Data;
                    }
                }
                Chunk::DataCr => {
                    if b != b'\r' {
                        return Err(bad);
                    }
                    self.state = Chunk::DataLf;
                }
                Chunk::DataLf => {
                    if b != b'\n' {
                        return Err(bad);
                    }
                    self.size = 0;
                    self.state = Chunk::Size;
                }
                Chunk::TrailerStart => {
                    self.line += 1;
                    match b {
                        b'\r' => self.state = Chunk::EndLf,
                        b'\n' | 0 => return Err(bad),
                        _ => self.state = Chunk::Trailer,
                    }
                }
                Chunk::Trailer => {
                    self.line += 1;
                    if self.line > MAX_TRAILERS {
                        return Err(bad);
                    }
                    match b {
                        b'\r' => self.state = Chunk::TrailerLf,
                        b'\n' | 0 => return Err(bad),
                        _ => {}
                    }
                }
                Chunk::TrailerLf => {
                    if b != b'\n' {
                        return Err(bad);
                    }
                    self.state = Chunk::TrailerStart;
                }
                Chunk::EndLf => {
                    if b != b'\n' {
                        return Err(bad);
                    }
                    self.state = Chunk::Done;
                }
                Chunk::Data | Chunk::Done => {}
            }
        }
        Ok((i, self.state == Chunk::Done))
    }
}

/// 响应分帧状态。
#[derive(Debug)]
enum RespState {
    Head,
    Length(u64),
    Chunked(ChunkedScanner),
    /// 以连接关闭为界的响应体。
    UntilClose,
    /// 无法继续跟踪（协议升级、畸形响应、请求排队过多）。
    Opaque,
}

/// 观察（不改写）一条后端流上的响应，数出「已完成几个响应」，用来在请求切换后端时保持响应顺序。
/// 对应请求通过 [`ResponseTracker::on_request`] 登记；`HEAD` 与 1xx / 204 / 304 没有响应体。
#[derive(Debug)]
pub struct ResponseTracker {
    state: RespState,
    closing: bool,
    head: Vec<u8>,
    queue: std::collections::VecDeque<bool>,
}

impl Default for ResponseTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// 同一条流上最多同时跟踪的未完成请求数（流水线深度）。
const MAX_OUTSTANDING: usize = 64;

impl ResponseTracker {
    /// 新建。
    pub fn new() -> Self {
        Self {
            state: RespState::Head,
            closing: false,
            head: Vec::new(),
            queue: std::collections::VecDeque::new(),
        }
    }

    /// 登记一个已转发的请求（`head_only` = HEAD 方法）。
    pub fn on_request(&mut self, head_only: bool) {
        if self.queue.len() >= MAX_OUTSTANDING {
            self.state = RespState::Opaque;
        } else {
            self.queue.push_back(head_only);
        }
    }

    /// 后端流已结束：不会再有响应，丢弃未完成的登记。
    pub fn close(&mut self) {
        self.queue.clear();
    }

    /// 尚未收到完整响应的请求数。
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// 是否见过要求关闭连接的最终响应（`Connection: close`，或 HTTP/1.0 且未声明 keep-alive）。
    pub fn closing(&self) -> bool {
        self.closing
    }

    /// 是否已无法判定响应边界。
    pub fn is_opaque(&self) -> bool {
        matches!(self.state, RespState::Opaque)
    }

    fn complete(&mut self) {
        self.queue.pop_front();
        self.state = RespState::Head;
    }

    /// 喂入后端返回的字节。
    pub fn feed(&mut self, data: &[u8]) {
        let mut data = data;
        while !data.is_empty() {
            match &mut self.state {
                RespState::Opaque | RespState::UntilClose => return,
                RespState::Length(left) => {
                    let take = usize::try_from(*left).unwrap_or(usize::MAX).min(data.len());
                    *left -= take as u64;
                    data = data.get(take..).unwrap_or(&[]);
                    if *left == 0 {
                        self.complete();
                    }
                }
                RespState::Chunked(sc) => match sc.feed(data) {
                    Ok((n, done)) => {
                        data = data.get(n..).unwrap_or(&[]);
                        if done {
                            self.complete();
                        }
                    }
                    Err(_) => self.state = RespState::Opaque,
                },
                RespState::Head => {
                    let carried = !self.head.is_empty();
                    let parsed = if carried {
                        self.head.extend_from_slice(data);
                        parse_response_head(&self.head)
                    } else {
                        parse_response_head(data)
                    };
                    let (n, code, length, chunked, close) = match parsed {
                        RespHead::Complete {
                            n,
                            code,
                            length,
                            chunked,
                            close,
                        } => (n, code, length, chunked, close),
                        RespHead::Partial => {
                            if !carried {
                                self.head.extend_from_slice(data);
                            }
                            if self.head.len() >= crate::vhost::MAX_HEAD {
                                self.state = RespState::Opaque;
                            }
                            return;
                        }
                        RespHead::Bad => {
                            self.state = RespState::Opaque;
                            return;
                        }
                    };
                    // 头之后的字节一定来自本次 `data` 的末尾。
                    let total = if carried { self.head.len() } else { data.len() };
                    let remainder = total.saturating_sub(n);
                    data = data
                        .get(data.len().saturating_sub(remainder)..)
                        .unwrap_or(&[]);
                    self.head.clear();
                    if code == 101 {
                        self.state = RespState::Opaque;
                        return;
                    }
                    // 1xx 是临时响应，最终响应还在后面；只有最终响应才可能要求关闭连接。
                    self.closing |= close && !(100..200).contains(&code);
                    if (100..200).contains(&code) {
                    } else if self.queue.front().copied().unwrap_or(false)
                        || code == 204
                        || code == 304
                        || (length == Some(0) && !chunked)
                    {
                        self.complete();
                    } else if chunked {
                        self.state = RespState::Chunked(ChunkedScanner::new());
                    } else if let Some(n) = length {
                        self.state = RespState::Length(n);
                    } else {
                        self.state = RespState::UntilClose;
                    }
                }
            }
        }
    }
}

enum RespHead {
    Complete {
        n: usize,
        code: u16,
        length: Option<u64>,
        chunked: bool,
        close: bool,
    },
    Partial,
    Bad,
}

fn parse_response_head(buf: &[u8]) -> RespHead {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut resp = httparse::Response::new(&mut headers);
    let n = match resp.parse(buf) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return RespHead::Partial,
        Err(_) => return RespHead::Bad,
    };
    let (mut length, mut chunked) = (None, false);
    let (mut conn_close, mut keep_alive) = (false, false);
    for h in resp.headers.iter() {
        if h.name.eq_ignore_ascii_case("content-length") {
            length = std::str::from_utf8(h.value)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok());
        } else if h.name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = h
                .value
                .rsplit(|b| *b == b',')
                .next()
                .is_some_and(|l| l.trim_ascii().eq_ignore_ascii_case(b"chunked"));
        } else if h.name.eq_ignore_ascii_case("connection") {
            conn_close |= token_list_has(h.value, "close");
            keep_alive |= token_list_has(h.value, "keep-alive");
        }
    }
    RespHead::Complete {
        n,
        code: resp.code.unwrap_or(0),
        length,
        chunked,
        // HTTP/1.0 默认关闭连接，除非明确 keep-alive。
        close: conn_close || (resp.version == Some(0) && !keep_alive),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(s: &str) -> Result<Option<RequestHead>, HeadError> {
        parse_request(s.as_bytes())
    }
    fn ok(s: &str) -> RequestHead {
        head(s).unwrap().unwrap()
    }

    #[test]
    fn parses_basic_requests() {
        let h = ok("GET /a/b?x=1#f HTTP/1.1\r\nHost: Example.COM:8080\r\n\r\nBODY");
        assert_eq!(h.method, "GET");
        assert_eq!(h.path, "/a/b");
        assert_eq!(h.host.as_deref(), Some("example.com"));
        assert_eq!(
            (h.framing, h.upgrade, h.head_len),
            (Framing::None, false, 51)
        );
        assert!(h.basic.is_none());
        // 绝对形式：Host 头优先，缺 Host 时取 authority。
        let h = ok("GET http://Other.org:81/p?q HTTP/1.1\r\nHost: a.com\r\n\r\n");
        assert_eq!((h.path.as_str(), h.host.as_deref()), ("/p", Some("a.com")));
        let h = ok("GET http://other.org HTTP/1.0\r\n\r\n");
        assert_eq!(
            (h.path.as_str(), h.host.as_deref()),
            ("/", Some("other.org"))
        );
        let h = ok("GET * HTTP/1.1\r\nHost: [::1]:80\r\n\r\n");
        assert_eq!((h.path.as_str(), h.host.as_deref()), ("*", Some("::1")));
        assert_eq!(ok("GET / HTTP/1.1\r\n\r\n").host, None);
        assert_eq!(ok("GET ? HTTP/1.1\r\nHost: a:b\r\n\r\n").path, "/");
        assert_eq!(
            ok("GET / HTTP/1.1\r\nHost: a:\r\n\r\n").host.as_deref(),
            Some("a:")
        );
        assert_eq!(
            ok("GET / HTTP/1.1\r\nHost: a:b\r\n\r\n").host.as_deref(),
            Some("a:b")
        );
        assert_eq!(ok("GET /?q HTTP/1.1\r\nHost: a\r\n\r\n").path, "/");
        assert_eq!(head("GET / HTTP/1.1\r\nHost: a\r\n"), Ok(None));
    }

    #[test]
    fn framing_and_smuggling_guards() {
        let f = |extra: &str| head(&format!("POST / HTTP/1.1\r\nHost: a\r\n{extra}\r\n"));
        assert_eq!(
            f("Content-Length: 5\r\n").unwrap().unwrap().framing,
            Framing::Length(5)
        );
        assert_eq!(
            f("Content-Length: 0\r\n").unwrap().unwrap().framing,
            Framing::None
        );
        assert_eq!(
            f("Content-Length: 7\r\nContent-Length: 7\r\n")
                .unwrap()
                .unwrap()
                .framing,
            Framing::Length(7)
        );
        assert_eq!(
            f("Transfer-Encoding: gzip, Chunked\r\n")
                .unwrap()
                .unwrap()
                .framing,
            Framing::Chunked
        );
        for bad in [
            "Content-Length: 5\r\nContent-Length: 6\r\n",
            "Content-Length: -1\r\n",
            "Content-Length: 1x\r\n",
            "Content-Length: \r\n",
            "Content-Length: 99999999999999999999999\r\n",
            "Content-Length: 5\r\nTransfer-Encoding: chunked\r\n",
            "Transfer-Encoding: chunked, gzip\r\n",
            "Transfer-Encoding: identity\r\n",
        ] {
            assert!(matches!(f(bad), Err(HeadError::Invalid(_))), "{bad}");
        }
        assert!(matches!(
            head("POST / HTTP/1.0\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(HeadError::Invalid(_))
        ));
        assert_eq!(
            head("GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n"),
            Err(HeadError::Invalid("duplicate Host"))
        );
        assert!(matches!(
            head("GET / HTTP/1.1\r\nHost: :80\r\n\r\n"),
            Err(HeadError::Invalid(_))
        ));
        assert_eq!(
            head("CONNECT a:443 HTTP/1.1\r\nHost: a\r\n\r\n"),
            Err(HeadError::Unsupported("CONNECT"))
        );
        assert!(matches!(
            head("garbage\r\n\r\n"),
            Err(HeadError::Invalid(_))
        ));
        let many: String = (0..70).map(|i| format!("X{i}: v\r\n")).collect();
        assert_eq!(
            head(&format!("GET / HTTP/1.1\r\nHost: a\r\n{many}\r\n")),
            Err(HeadError::Invalid("too many headers"))
        );
    }

    #[test]
    fn upgrade_and_basic_credentials() {
        let h = ok("GET /ws HTTP/1.1\r\nHost: a\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\nAuthorization: Basic dXNlcjpwYTpzcw==\r\n\r\n");
        assert!(h.upgrade);
        assert_eq!(h.basic, Some(("user".into(), "pa:ss".into())));
        assert!(!ok("GET / HTTP/1.1\r\nHost: a\r\nConnection: Upgrade\r\n\r\n").upgrade);
        assert!(!ok("GET / HTTP/1.1\r\nHost: a\r\nUpgrade: x\r\n\r\n").upgrade);
        assert_eq!(parse_basic("basic dTpw"), Some(("u".into(), "p".into())));
        for bad in [
            "Bearer abc",
            "Basic",
            "Basic !!!!",
            "Basic dXNlcg==",
            "Basic /w==",
        ] {
            assert_eq!(parse_basic(bad), None, "{bad}");
        }
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        // 第二个 Authorization 头被忽略；Base64 的数字、+ 与 / 都能解。
        let h = ok("GET / HTTP/1.1\r\nHost: a\r\nAuthorization: Basic dSsvOnA/Pj4=\r\nAuthorization: Basic eDp5\r\n\r\n");
        assert_eq!(h.basic, Some(("u+/".into(), "p?>>".into())));
        assert_eq!(parse_basic("Basic +/+/Ong="), None, "invalid utf-8 bytes");
    }

    #[test]
    fn location_prefix_semantics() {
        assert!(location_matches("/", "/"));
        assert!(location_matches("/api/v1", "/api"));
        assert!(location_matches("/api", "/api/"), "path + '/' counts");
        assert!(!location_matches("/ap", "/api"));
        assert!(!location_matches("/other", "/api"));
    }

    #[test]
    fn rewrites_host_and_sets_headers() {
        let raw = b"GET /x HTTP/1.1\r\nHost: a.com\r\nX-Old: 1\r\nx-set: old\r\nAccept: */*\r\nX-Set: dup\r\n\r\n";
        let out = rewrite_request(
            raw,
            "inner.local",
            &[
                ("X-Set".into(), "new".into()),
                ("X-Added".into(), "yes".into()),
                ("Bad\r\nName".into(), "v".into()),
                ("X-Bad".into(), "a\r\nb".into()),
            ],
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "GET /x HTTP/1.1\r\nHost: inner.local\r\nX-Old: 1\r\nX-Set: new\r\nAccept: */*\r\nX-Added: yes\r\n\r\n"
        );
        // 非 UTF-8 的头值按原样（有损）保留；非法的改写目标不会丢掉原 Host。
        let odd = rewrite_request(
            b"GET / HTTP/1.1\r\nHost: a\r\nX: \xff\r\n\r\n",
            "bad\r\nhost",
            &[],
        );
        assert_eq!(
            odd,
            "GET / HTTP/1.1\r\nHost: a\r\nX: \u{fffd}\r\n\r\n".as_bytes()
        );
        let two = rewrite_request(b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n", "n", &[]);
        assert_eq!(two, b"GET / HTTP/1.1\r\nHost: n\r\n\r\n".to_vec());
        // 不改写 Host 时保持原样；不完整的头原样返回。
        let same = rewrite_request(raw, "", &[]);
        assert_eq!(same, raw.to_vec());
        assert_eq!(rewrite_request(b"GET / HT", "x", &[]), b"GET / HT".to_vec());
        assert!(valid_header("X-A", "v"));
        assert!(!valid_header("", "v"));
        assert!(!valid_header("A B", "v"));
        assert!(!valid_header("A", "\0"));
        assert!(valid_header("A", "tab\tok"));
    }

    fn scan(chunks: &[&[u8]]) -> Result<(usize, bool), HeadError> {
        let mut s = ChunkedScanner::new();
        let (mut total, mut done) = (0, false);
        for c in chunks {
            let (n, d) = s.feed(c)?;
            total += n;
            done = d;
            if d {
                break;
            }
        }
        Ok((total, done))
    }

    #[test]
    fn chunked_boundaries_across_arbitrary_splits() {
        let body: &[u8] = b"5\r\nhello\r\n0\r\n\r\nNEXT";
        assert_eq!(scan(&[body]), Ok((15, true)));
        // 逐字节喂入结果一致。
        let bytes: Vec<&[u8]> = body.chunks(1).collect();
        assert_eq!(scan(&bytes), Ok((15, true)));
        // 扩展与 trailer。
        let t: &[u8] = b"A;ext=1\r\n0123456789\r\n0\r\nX-T: v\r\nY: z\r\n\r\n";
        assert_eq!(scan(&[t]), Ok((t.len(), true)));
        // 大写十六进制、空格扩展。
        assert!(
            scan(&[b"1F \r\n0123456789012345678901234567890\r\n0\r\n\r\n"])
                .unwrap()
                .1
        );
        // 不完整。
        assert_eq!(scan(&[b"5\r\nhel"]), Ok((6, false)));
        assert_eq!(scan(&[b""]), Ok((0, false)));
    }

    #[test]
    fn chunked_rejects_malformed_input() {
        // 每个状态各一个非法字节。
        let zeros = "0".repeat(33);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("size", b"x".to_vec()),
            ("size: bare CR", b"\r\n".to_vec()),
            ("size: too many digits", zeros.into_bytes()),
            ("size: overflow", b"FFFFFFFFFFFFFFFFF\r\n".to_vec()),
            ("ext LF", b"1;a\n".to_vec()),
            ("ext NUL", b"1;a\0".to_vec()),
            ("size LF", b"1\rX".to_vec()),
            ("data CR", b"1\r\nzX".to_vec()),
            ("data LF", b"1\r\nz\rX".to_vec()),
            ("trailer start LF", b"0\r\n\n".to_vec()),
            ("trailer start NUL", b"0\r\n\0".to_vec()),
            ("trailer LF", b"0\r\nA\n".to_vec()),
            ("trailer NUL", b"0\r\nA\0".to_vec()),
            ("trailer CR", b"0\r\nA\rX".to_vec()),
            ("end LF", b"0\r\n\rX".to_vec()),
        ];
        for (name, bad) in cases {
            for cut in [1, 3, 100] {
                let mut s = ChunkedScanner::default();
                let mut failed = false;
                for part in bad.chunks(cut) {
                    failed |= s.feed(part).is_err();
                    if failed {
                        break;
                    }
                }
                assert!(failed, "{name} (cut {cut})");
            }
        }
        // 超长扩展 / trailer。
        let mut ext = b"1;".to_vec();
        ext.extend(std::iter::repeat_n(b'a', MAX_CHUNK_LINE + 1));
        assert!(ChunkedScanner::new().feed(&ext).is_err());
        let mut tr = b"0\r\nX: ".to_vec();
        tr.extend(std::iter::repeat_n(b'a', MAX_TRAILERS + 1));
        assert!(ChunkedScanner::new().feed(&tr).is_err());
        // 结束后再喂数据：不消费任何字节。
        let mut s = ChunkedScanner::new();
        assert_eq!(s.feed(b"0\r\n\r\nXYZ"), Ok((5, true)));
        assert_eq!(s.feed(b"XYZ"), Ok((0, true)));
    }

    #[test]
    fn response_tracker_counts_completed_responses() {
        let mut t = ResponseTracker::new();
        for head_only in [false, false, false, true] {
            t.on_request(head_only);
        }
        assert_eq!(t.pending(), 4);
        // 定长 + 紧跟的分块响应，在任意切分点结果一致。
        let wire: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\nHTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n";
        for cut in [1usize, 2, 3, 7, 1000] {
            let mut t = ResponseTracker::new();
            for head_only in [false, false, false, true] {
                t.on_request(head_only);
            }
            for c in wire.chunks(cut) {
                t.feed(c);
            }
            assert_eq!(t.pending(), 0, "cut {cut}");
            assert!(!t.is_opaque());
        }
        t.feed(wire);
        for (wire, closing) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                true,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n",
                false,
            ),
            ("HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n", true),
            (
                "HTTP/1.0 200 OK\r\nContent-Length: 0\r\nConnection: Keep-Alive\r\n\r\n",
                false,
            ),
            ("HTTP/1.1 100 Continue\r\nConnection: close\r\n\r\n", false),
        ] {
            let mut x = ResponseTracker::new();
            x.on_request(false);
            x.feed(wire.as_bytes());
            assert_eq!(x.closing(), closing, "{wire}");
        }
        let mut closed = ResponseTracker::new();
        closed.on_request(false);
        closed.close();
        assert_eq!(closed.pending(), 0);
        for status in ["304 Not Modified", "200 OK\r\nContent-Length: 0"] {
            let mut x = ResponseTracker::new();
            x.on_request(false);
            x.feed(format!("HTTP/1.1 {status}\r\n\r\n").as_bytes());
            assert_eq!(x.pending(), 0, "{status}");
        }
        assert_eq!(
            t.pending(),
            0,
            "HEAD response has no body even with Content-Length"
        );
    }

    #[test]
    fn response_tracker_gives_up_when_boundaries_are_unknowable() {
        let mut t = ResponseTracker::new();
        t.on_request(false);
        t.feed(b"HTTP/1.1 101 Switching Protocols\r\n\r\n");
        assert!(t.is_opaque());
        for bad in [&b"garbage\r\n\r\n"[..], b"HTTP/1.1 200 OK\r\n\r\n"] {
            let mut t = ResponseTracker::new();
            t.on_request(false);
            t.feed(bad);
            // 无长度的 200 以关闭为界：永不「完成」，也不再误数。
            assert_eq!(t.pending(), 1);
        }
        let mut t = ResponseTracker::new();
        t.on_request(false);
        t.feed(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz");
        assert!(t.is_opaque());
        let mut t = ResponseTracker::new();
        for _ in 0..=MAX_OUTSTANDING {
            t.on_request(false);
        }
        assert!(t.is_opaque());
        let mut t = ResponseTracker::new();
        t.on_request(false);
        t.feed(&vec![b'a'; crate::vhost::MAX_HEAD + 1]);
        assert!(t.is_opaque());
        // 没有登记请求时收到响应：不会 panic。
        let mut t = ResponseTracker::default();
        t.feed(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx");
        assert_eq!(t.pending(), 0);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn parsers_never_panic(data in proptest::collection::vec(any::<u8>(), 0..600)) {
                let _ = parse_request(&data);
                let _ = rewrite_request(&data, "h", &[("A".into(), "b".into())]);
                let mut s = ChunkedScanner::new();
                for c in data.chunks(7) { let _ = s.feed(c); }
                let _ = parse_basic(&String::from_utf8_lossy(&data));
            }

            #[test]
            fn chunked_split_points_do_not_change_the_result(
                sizes in proptest::collection::vec(1usize..40, 1..6),
                cut in 1usize..8,
            ) {
                let mut body = Vec::new();
                for n in &sizes {
                    body.extend_from_slice(format!("{n:x}\r\n").as_bytes());
                    body.extend(std::iter::repeat_n(b'z', *n));
                    body.extend_from_slice(b"\r\n");
                }
                body.extend_from_slice(b"0\r\n\r\n");
                let whole = scan(&[&body]).unwrap();
                let parts: Vec<&[u8]> = body.chunks(cut).collect();
                prop_assert_eq!(scan(&parts).unwrap(), whole);
                prop_assert_eq!(whole, (body.len(), true));
            }
        }
    }
}
