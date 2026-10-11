//! 线协议：控制流上的「4 字节大端长度 + JSON」帧，以及数据流的起始消息。

use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::TunnelError;

/// 单帧最大长度（控制消息很小，限制可防止恶意对端撑爆内存）。
pub const MAX_FRAME: usize = 64 * 1024;
/// 协议版本。
pub const PROTOCOL_VERSION: u32 = 1;

/// 登录请求（Client → Edge，控制流第一条）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// 协议版本。
    pub version: u32,
    /// frp 的 `user`：代理名前缀。
    pub user: String,
    /// 客户端标识。
    pub client_id: String,
    /// Unix 秒。
    pub timestamp: i64,
    /// `auth_key(token, timestamp)`。
    pub auth: String,
    /// 元数据。
    #[serde(default)]
    pub metas: BTreeMap<String, String>,
    /// 接入请求：首次由 Controller 签发身份时携带（此时 `client_id` / `auth` 为空）。
    #[serde(default)]
    pub enroll: Option<Enroll>,
}

/// 接入请求：用一次性令牌换取设备身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enroll {
    /// 一次性接入令牌。
    pub token: String,
    /// 设备名。
    pub name: String,
    /// 设备公钥（十六进制 Ed25519）。
    pub pubkey: String,
    /// 操作系统。
    #[serde(default)]
    pub os: String,
    /// CPU 架构。
    #[serde(default)]
    pub arch: String,
    /// Agent 版本。
    #[serde(default)]
    pub version: String,
}

/// http 代理的附加选项（frp 的 `locations` / `httpUser` / `httpPassword` / `routeByHTTPUser` / `requestHeaders.set`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HttpOpts {
    /// 路径前缀；空 = `/`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
    /// Basic 认证用户名（Edge 校验）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub http_user: String,
    /// Basic 认证密码。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub http_password: String,
    /// 同域名同路径下按请求里的 Basic 用户名选路。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub route_by_http_user: String,
    /// 转发前设置（覆盖）的请求头。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_headers: BTreeMap<String, String>,
    /// 转发给访问者前设置（覆盖）的响应头。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub response_headers: BTreeMap<String, String>,
}

/// 客户端对本地服务的健康检查（frp 的 `healthCheck`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HealthSpec {
    /// `tcp` / `http`。
    pub kind: String,
    /// 单次探测超时（秒）。
    pub timeout_secs: u32,
    /// 连续失败多少次判定不健康。
    pub max_failed: u32,
    /// 探测间隔（秒）。
    pub interval_secs: u32,
    /// http：探测路径。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// http：附加请求头。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// 负载均衡组与健康检查（frp 的 `loadBalancer` / `healthCheck`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LbOpts {
    /// 组名；同组成员共享同一个端口 / 域名，连接在健康成员间轮流分配。空 = 不入组。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// 组密钥：同组成员必须一致。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group_key: String,
    /// 健康检查；启用后代理在第一次探测成功前不接收访问者。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthSpec>,
}

impl LbOpts {
    /// 全部为默认值（序列化时整体省略）。
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl HttpOpts {
    /// 全部为默认值（序列化时整体省略，保持旧线格式不变）。
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// 受管代理：Controller 下发给 Edge 与 Client 的完整定义。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ManagedProxy {
    /// 代理名。
    pub name: String,
    /// `tcp` / `udp` / `http` / `https`。
    pub kind: String,
    /// 远程端口，0 = 自动分配（http / https 不使用）。
    pub remote_port: u16,
    /// 本地目标主机。
    pub local_ip: String,
    /// 本地目标端口。
    pub local_port: u16,
    /// http / https：自定义域名。
    #[serde(default)]
    pub custom_domains: Vec<String>,
    /// http / https：子域名。
    #[serde(default)]
    pub subdomain: String,
    /// http：改写 Host 头。
    #[serde(default)]
    pub host_header_rewrite: String,
    /// http：路径 / 认证 / 请求头选项。
    #[serde(default, skip_serializing_if = "HttpOpts::is_default")]
    pub http: HttpOpts,
    /// 连接本地服务时写入 PROXY protocol 头：0 = 不写，1 / 2 = v1 / v2（由客户端写）。
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub proxy_protocol: u8,
    /// 带宽上限（字节 / 秒，整个代理共享）；0 = 不限。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bandwidth_bps: u64,
    /// 限速在服务端（Edge）执行；缺省在客户端执行（frp 的 `bandwidthLimitMode`）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub bandwidth_server: bool,
    /// 负载均衡组与健康检查。
    #[serde(default, skip_serializing_if = "LbOpts::is_default")]
    pub lb: LbOpts,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

/// 一版受管配置（全量快照；幂等，可重复下发）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConfigSet {
    /// 单调递增的版本号。
    pub revision: u64,
    /// 全部代理。
    pub proxies: Vec<ManagedProxy>,
    /// 远程重启计数：值比会话开始时大，说明管理员要求重启这台设备上的 agent。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub restart_seq: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// 代理注册信息。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProxySpec {
    /// 代理名（已带 user 前缀）。
    pub name: String,
    /// `tcp` / `udp` / `http` / `https`。
    pub kind: String,
    /// 远程端口，0 = 由服务端从允许范围内分配（http / https 不使用）。
    pub remote_port: u16,
    /// http / https：自定义域名（可含 `*.example.com` 通配）。
    #[serde(default)]
    pub custom_domains: Vec<String>,
    /// http / https：子域名（与服务端的 `subDomainHost` 拼成完整域名）。
    #[serde(default)]
    pub subdomain: String,
    /// http：把转发给内网服务的 `Host` 头改写成这个值。
    #[serde(default)]
    pub host_header_rewrite: String,
    /// http：路径 / 认证 / 请求头选项。
    #[serde(default, skip_serializing_if = "HttpOpts::is_default")]
    pub http: HttpOpts,
    /// 带宽上限（字节 / 秒）；0 = 不限。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bandwidth_bps: u64,
    /// 限速在服务端（Edge）执行。
    #[serde(default, skip_serializing_if = "is_false")]
    pub bandwidth_server: bool,
    /// 负载均衡组与健康检查。
    #[serde(default, skip_serializing_if = "LbOpts::is_default")]
    pub lb: LbOpts,
}

/// Client → Edge。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 登录。
    Login(Login),
    /// 注册代理。
    NewProxy(ProxySpec),
    /// 心跳。
    Ping,
    /// 已应用某版受管配置。
    Applied {
        /// 版本号。
        revision: u64,
    },
    /// 健康检查结果变化：不健康的代理暂时不接收访问者，恢复后继续。
    Health {
        /// 代理名。
        name: String,
        /// 是否健康。
        healthy: bool,
    },
}

/// Edge → Client。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// 登录结果。
    LoginResp {
        /// 成功与否。
        ok: bool,
        /// 失败原因。
        error: String,
        /// 本次会话标识。
        run_id: String,
        /// 接入成功时由 Controller 签发的设备 ID（其余情况为空）。
        #[serde(default)]
        client_id: String,
        /// 为 true 表示由 Controller 管理：随后会下发 `Config`，客户端不需要自己注册代理。
        #[serde(default)]
        managed: bool,
    },
    /// 下发受管配置。
    Config(ConfigSet),
    /// 代理注册结果。
    NewProxyResp {
        /// 代理名。
        name: String,
        /// 实际监听端口（失败为 0）。
        remote_port: u16,
        /// 失败原因，空表示成功。
        error: String,
    },
    /// 心跳应答。
    Pong,
}

/// 数据流起始消息（Edge → Client，每条新流的第一帧）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartWork {
    /// 代理名。
    pub proxy: String,
    /// 访问者地址。
    pub src: String,
    /// 访问者连接到的公网地址（PROXY protocol 头的目的地址）。
    #[serde(default)]
    pub dst: String,
}

/// 写一帧。
pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), TunnelError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(msg).map_err(|e| TunnelError::Wire(e.to_string()))?;
    if body.len() > MAX_FRAME {
        return Err(TunnelError::Wire(format!(
            "frame too large: {}",
            body.len()
        )));
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// 读一帧。
pub async fn read_frame<R, T>(r: &mut R) -> Result<T, TunnelError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(TunnelError::Wire(format!("frame too large: {len}")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|e| TunnelError::Wire(e.to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn login() -> Login {
        Login {
            version: PROTOCOL_VERSION,
            user: "u".into(),
            client_id: "c".into(),
            timestamp: 1,
            auth: "a".into(),
            metas: BTreeMap::new(),
            enroll: None,
        }
    }

    #[tokio::test]
    async fn roundtrip_all_messages() {
        let (mut a, mut b) = duplex(4096);
        let c = [
            ClientMsg::Login(login()),
            ClientMsg::NewProxy(ProxySpec {
                name: "n".into(),
                kind: "tcp".into(),
                remote_port: 6000,
                ..Default::default()
            }),
            ClientMsg::Ping,
            ClientMsg::Applied { revision: 7 },
            ClientMsg::Login(Login {
                enroll: Some(Enroll {
                    token: "t".into(),
                    name: "laptop".into(),
                    pubkey: "ab".into(),
                    os: "macos".into(),
                    arch: "arm64".into(),
                    version: "0.1".into(),
                }),
                ..login()
            }),
        ];
        for m in &c {
            write_frame(&mut a, m).await.unwrap();
            assert_eq!(&read_frame::<_, ClientMsg>(&mut b).await.unwrap(), m);
        }
        let s = [
            ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
                client_id: "c1".into(),
                managed: true,
            },
            ServerMsg::NewProxyResp {
                name: "n".into(),
                remote_port: 1,
                error: String::new(),
            },
            ServerMsg::Config(ConfigSet {
                revision: 7,
                proxies: vec![ManagedProxy {
                    name: "web".into(),
                    kind: "tcp".into(),
                    remote_port: 6000,
                    local_ip: "127.0.0.1".into(),
                    local_port: 80,
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ServerMsg::Pong,
        ];
        for m in &s {
            write_frame(&mut a, m).await.unwrap();
            assert_eq!(&read_frame::<_, ServerMsg>(&mut b).await.unwrap(), m);
        }
        let w = StartWork {
            proxy: "p".into(),
            src: "1.2.3.4:5".into(),
            dst: String::new(),
        };
        write_frame(&mut a, &w).await.unwrap();
        assert_eq!(read_frame::<_, StartWork>(&mut b).await.unwrap(), w);
    }

    #[test]
    fn old_peers_without_new_fields_still_parse() {
        let login: ClientMsg = serde_json::from_str(
            r#"{"t":"login","version":1,"user":"","client_id":"c","timestamp":1,"auth":"a"}"#,
        )
        .unwrap();
        assert!(matches!(
            login,
            ClientMsg::Login(Login { enroll: None, .. })
        ));
        let resp: ServerMsg =
            serde_json::from_str(r#"{"t":"login_resp","ok":true,"error":"","run_id":"r"}"#)
                .unwrap();
        assert!(matches!(resp, ServerMsg::LoginResp { managed: false, .. }));
        assert_eq!(ConfigSet::default().revision, 0);
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected_both_ways() {
        let (mut a, mut b) = duplex(1 << 20);
        let big = StartWork {
            proxy: "x".repeat(MAX_FRAME),
            src: String::new(),
            dst: String::new(),
        };
        assert!(matches!(
            write_frame(&mut a, &big).await,
            Err(TunnelError::Wire(_))
        ));
        a.write_all(&((MAX_FRAME as u32 + 1).to_be_bytes()))
            .await
            .unwrap();
        assert!(matches!(
            read_frame::<_, StartWork>(&mut b).await,
            Err(TunnelError::Wire(_))
        ));
    }

    #[tokio::test]
    async fn malformed_and_truncated_input() {
        let (mut a, mut b) = duplex(4096);
        a.write_all(&3u32.to_be_bytes()).await.unwrap();
        a.write_all(b"{{{").await.unwrap();
        assert!(matches!(
            read_frame::<_, StartWork>(&mut b).await,
            Err(TunnelError::Wire(_))
        ));
        a.write_all(&10u32.to_be_bytes()).await.unwrap();
        a.write_all(b"ab").await.unwrap();
        drop(a);
        assert!(matches!(
            read_frame::<_, StartWork>(&mut b).await,
            Err(TunnelError::Io(_))
        ));
    }

    #[tokio::test]
    async fn write_to_closed_peer_fails() {
        let (mut a, b) = duplex(8);
        drop(b);
        assert!(matches!(
            write_frame(&mut a, &ClientMsg::Ping).await,
            Err(TunnelError::Io(_))
        ));
    }
}
