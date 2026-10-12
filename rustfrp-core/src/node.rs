//! 节点（外置 Edge）与 Controller 之间的控制协议。
//!
//! 一条 TLS 连接，帧格式同 [`crate::wire`]（4 字节大端长度 + JSON，单帧 ≤ 64 KiB）。
//! 先由 Controller 发 [`CtlMsg::Challenge`]，节点用身份密钥签名后在 [`NodeHello`] 里回应，防止重放。
//! 设备清单按设备逐条下发（`SnapshotBegin` → `Device`* → `SnapshotEnd`），任何一帧都不依赖设备总数。

use serde::{Deserialize, Serialize};

use crate::wire::{ConfigSet, EdgeRedirect, Enroll};

/// 节点协议版本。
pub const NODE_PROTOCOL_VERSION: u32 = 1;

/// 节点自述的运行信息（Controller 据此生成接入命令、校验隧道）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeInfo {
    /// 对外主机名（接入命令里的 `--controller` 地址）。
    #[serde(default)]
    pub public_host: String,
    /// 客户端接入端口。
    #[serde(default)]
    pub edge_port: u16,
    /// 节点叶证书 SHA-256 指纹。
    #[serde(default)]
    pub tls_fingerprint: String,
    /// HTTP 虚拟主机端口。
    #[serde(default)]
    pub vhost_http: Option<u16>,
    /// HTTPS 虚拟主机端口。
    #[serde(default)]
    pub vhost_https: Option<u16>,
    /// TCPMUX HTTP CONNECT 共享端口。
    #[serde(default)]
    pub tcpmux_http_connect_port: Option<u16>,
    /// 子域名后缀。
    #[serde(default)]
    pub subdomain_host: String,
    /// 允许分配的端口区间。
    #[serde(default)]
    pub allow_ports: Vec<(u16, u16)>,
    /// 节点版本。
    #[serde(default)]
    pub version: String,
    /// 节点所在系统。
    #[serde(default)]
    pub os: String,
}

/// 节点首次接入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEnroll {
    /// 一次性接入令牌（面板「接入新节点」生成）。
    pub token: String,
    /// 节点名。
    pub name: String,
    /// 节点公钥（十六进制 Ed25519）。
    pub pubkey: String,
}

/// 节点登录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHello {
    /// 协议版本。
    pub version: u32,
    /// 节点 ID（首次接入为空）。
    #[serde(default)]
    pub node_id: String,
    /// 对 `Challenge` 的签名（首次接入为空）。
    #[serde(default)]
    pub signature: String,
    /// 首次接入请求。
    #[serde(default)]
    pub enroll: Option<NodeEnroll>,
    /// 运行信息。
    pub info: NodeInfo,
}

/// 下发给节点的一台设备：公钥用于节点本地验证登录，配置即该设备应运行的全部隧道。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDevice {
    /// 设备 ID。
    pub id: String,
    /// 设备公钥。
    pub pubkey: String,
    /// 是否被停用（停用设备不得登录，且应立即断开）。
    #[serde(default)]
    pub disabled: bool,
    /// 受管配置。
    pub config: ConfigSet,
}

/// 一条代理的运行状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyReport {
    /// 设备 ID。
    pub client_id: String,
    /// 代理名（= 隧道 ID）。
    pub name: String,
    /// `running` / `error` / `stopped`。
    pub state: String,
    /// 实际监听端口。
    #[serde(default)]
    pub port: u16,
    /// 失败原因。
    #[serde(default)]
    pub error: String,
}

/// 一个代理实例的累计计数（同 [`crate`] 内 `pipe::TrafficSnapshot`，附实例编号以便去重）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficSample {
    /// 代理名（= 隧道 ID）。
    pub name: String,
    /// 实例编号：同名代理重建后会变，Controller 据此区分累计游标。
    pub generation: u64,
    /// 访问者 → 内网服务的累计字节。
    pub bytes_in: u64,
    /// 内网服务 → 访问者的累计字节。
    pub bytes_out: u64,
    /// 当前连接数。
    pub connections: u64,
    /// 累计连接数。
    pub connections_total: u64,
}

/// 节点 → Controller。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum NodeMsg {
    /// 登录。
    Hello(NodeHello),
    /// 设备上线。
    Online {
        /// 设备 ID。
        client_id: String,
        /// 节点看到的设备来源地址（不含端口）。
        #[serde(default)]
        ip: String,
    },
    /// 设备离线。
    Offline {
        /// 设备 ID。
        client_id: String,
    },
    /// 代理状态变化。
    Proxy(ProxyReport),
    /// 重连后的全量对账：此刻在线的设备与全部代理状态（分多帧时用 `more`）。
    Resync {
        /// 在线设备。
        online: Vec<String>,
        /// 代理状态。
        proxies: Vec<ProxyReport>,
        /// 后面是否还有更多对账帧。
        #[serde(default)]
        more: bool,
    },
    /// 有设备想经此节点首次接入。
    Enroll {
        /// 请求编号。
        req_id: u64,
        /// 接入信息。
        enroll: Enroll,
    },
    /// 流量采样（累计值）；Controller 落库后用同一个 `seq` 应答 [`CtlMsg::TrafficAck`]。
    Traffic {
        /// 批次号（节点内递增）。
        seq: u64,
        /// 进程纪元：节点重启后计数从零开始，必须换新纪元。
        epoch: String,
        /// 样本（每批 ≤ 200 条）。
        samples: Vec<TrafficSample>,
    },
    /// 心跳。
    Ping,
}

/// Controller → 节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum CtlMsg {
    /// 连接建立后的第一条：登录挑战。
    Challenge {
        /// 随机挑战。
        nonce: String,
    },
    /// 登录结果。
    HelloResp {
        /// 成功与否。
        ok: bool,
        /// 失败原因。
        #[serde(default)]
        error: String,
        /// 首次接入时签发的节点 ID。
        #[serde(default)]
        node_id: String,
    },
    /// 开始一轮全量设备清单。
    SnapshotBegin,
    /// 一台设备（新增或更新）。
    Device(NodeDevice),
    /// 全量清单结束：未出现过的设备应被移除。
    SnapshotEnd,
    /// 移除设备（删除，或需要强制断开）。
    Remove {
        /// 设备 ID。
        client_id: String,
    },
    /// 强制设备重连（保留名册条目）。
    Kick {
        /// 设备 ID。
        client_id: String,
    },
    /// 设备迁移到另一个 Edge；旧节点只发送空代理快照和目标地址。
    RedirectClient {
        /// 设备 ID。
        client_id: String,
        /// 新节点。
        target: EdgeRedirect,
    },
    /// 设备接入结果。
    EnrollResp {
        /// 请求编号。
        req_id: u64,
        /// 成功与否。
        ok: bool,
        /// 失败原因。
        #[serde(default)]
        error: String,
        /// 成功时的设备。
        #[serde(default)]
        device: Option<NodeDevice>,
    },
    /// 流量批次已处理，节点可以回收对应的已停止条目。
    TrafficAck {
        /// 批次号。
        seq: u64,
    },
    /// 心跳应答。
    Pong,
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::wire::{read_frame, write_frame, ManagedProxy};
    use tokio::io::duplex;

    fn device() -> NodeDevice {
        NodeDevice {
            id: "c1".into(),
            pubkey: "ab".repeat(32),
            disabled: false,
            config: ConfigSet {
                revision: 3,
                proxies: vec![ManagedProxy {
                    name: "t1".into(),
                    kind: "tcp".into(),
                    remote_port: 6000,
                    local_ip: "127.0.0.1".into(),
                    local_port: 22,
                    ..Default::default()
                }],
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn every_message_roundtrips_as_a_frame() {
        let (mut a, mut b) = duplex(1 << 16);
        let node = vec![
            NodeMsg::Hello(NodeHello {
                version: NODE_PROTOCOL_VERSION,
                node_id: "n".into(),
                signature: "sig".into(),
                enroll: Some(NodeEnroll {
                    token: "t".into(),
                    name: "edge".into(),
                    pubkey: "pk".into(),
                }),
                info: NodeInfo {
                    public_host: "edge.example.com".into(),
                    edge_port: 7000,
                    allow_ports: vec![(6000, 6100)],
                    ..Default::default()
                },
            }),
            NodeMsg::Online {
                client_id: "c1".into(),
                ip: "203.0.113.9".into(),
            },
            NodeMsg::Offline {
                client_id: "c1".into(),
            },
            NodeMsg::Proxy(ProxyReport {
                client_id: "c1".into(),
                name: "t1".into(),
                state: "running".into(),
                port: 6000,
                error: String::new(),
            }),
            NodeMsg::Resync {
                online: vec!["c1".into()],
                proxies: vec![],
                more: true,
            },
            NodeMsg::Enroll {
                req_id: 9,
                enroll: Enroll {
                    token: "t".into(),
                    name: "box".into(),
                    pubkey: "pk".into(),
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    version: "0.1".into(),
                },
            },
            NodeMsg::Traffic {
                seq: 4,
                epoch: "e1".into(),
                samples: vec![TrafficSample {
                    name: "t1".into(),
                    generation: 2,
                    bytes_in: 10,
                    bytes_out: 20,
                    connections: 1,
                    connections_total: 3,
                }],
            },
            NodeMsg::Ping,
        ];
        for m in &node {
            write_frame(&mut a, m).await.unwrap();
            assert_eq!(&read_frame::<_, NodeMsg>(&mut b).await.unwrap(), m);
        }
        let ctl = vec![
            CtlMsg::Challenge { nonce: "n".into() },
            CtlMsg::HelloResp {
                ok: true,
                error: String::new(),
                node_id: "n1".into(),
            },
            CtlMsg::SnapshotBegin,
            CtlMsg::Device(device()),
            CtlMsg::SnapshotEnd,
            CtlMsg::Remove {
                client_id: "c1".into(),
            },
            CtlMsg::EnrollResp {
                req_id: 9,
                ok: true,
                error: String::new(),
                device: Some(device()),
            },
            CtlMsg::EnrollResp {
                req_id: 10,
                ok: false,
                error: "bad".into(),
                device: None,
            },
            CtlMsg::TrafficAck { seq: 4 },
            CtlMsg::Pong,
        ];
        for m in &ctl {
            write_frame(&mut a, m).await.unwrap();
            assert_eq!(&read_frame::<_, CtlMsg>(&mut b).await.unwrap(), m);
        }
    }

    #[test]
    fn optional_fields_default_for_forward_compatibility() {
        let hello: NodeHello = serde_json::from_str(r#"{"version":1,"info":{}}"#).unwrap();
        assert_eq!((hello.node_id.as_str(), hello.enroll), ("", None));
        let d: NodeDevice =
            serde_json::from_str(r#"{"id":"c","pubkey":"k","config":{"revision":1,"proxies":[]}}"#)
                .unwrap();
        assert!(!d.disabled);
        let r: CtlMsg = serde_json::from_str(r#"{"t":"hello_resp","ok":false}"#).unwrap();
        assert_eq!(
            r,
            CtlMsg::HelloResp {
                ok: false,
                error: String::new(),
                node_id: String::new()
            }
        );
        // 一个设备的帧远小于上限：100 条隧道也放得下。
        let mut big = device();
        big.config.proxies = (0..100)
            .map(|i| ManagedProxy {
                name: format!("tunnel-{i:040}"),
                ..Default::default()
            })
            .collect();
        assert!(serde_json::to_vec(&CtlMsg::Device(big)).unwrap().len() < crate::wire::MAX_FRAME);
    }
}
