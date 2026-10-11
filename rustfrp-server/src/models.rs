//! 传输模型（与 REST 响应一一对应）。时间一律为 UTC 毫秒整数，ID 为不透明字符串。

use serde::{Deserialize, Serialize};

/// 管理员角色。权限由高到低：owner > admin > operator > viewer。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 只读。
    Viewer,
    /// 可管理隧道。
    Operator,
    /// 可管理客户端、令牌。
    Admin,
    /// 全部权限。
    Owner,
}

impl Role {
    /// 数据库中的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    /// 解析；未知值按最低权限 `viewer`，避免被篡改的数据意外提权。
    pub fn parse(s: &str) -> Role {
        match s {
            "owner" => Role::Owner,
            "admin" => Role::Admin,
            "operator" => Role::Operator,
            _ => Role::Viewer,
        }
    }
}

/// 管理员（不含密码哈希）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admin {
    /// ID。
    pub id: String,
    /// 用户名。
    pub name: String,
    /// 角色。
    pub role: Role,
    /// 禁用时间。
    pub disabled_at: Option<i64>,
    /// 创建时间。
    pub created_at: i64,
}

/// 客户端设备。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    /// 设备 ID。
    pub id: String,
    /// 名称。
    pub name: String,
    /// 操作系统。
    pub os: String,
    /// 架构。
    pub arch: String,
    /// Agent 版本。
    pub version: String,
    /// 配置版本号。
    pub revision: i64,
    /// 创建时间。
    pub created_at: i64,
    /// 最近在线时间。
    pub last_seen_at: Option<i64>,
    /// 禁用时间。
    pub disabled_at: Option<i64>,
    /// 是否在线（运行时状态，不入库）。
    #[serde(default)]
    pub online: bool,
    /// 服务该设备的节点 ID（空串 = 内置节点）。
    #[serde(default)]
    pub node_id: String,
    /// 标签（最多 8 个，每个 1–32 个字符）。
    #[serde(default)]
    pub tags: Vec<String>,
}

/// 隧道（spec + status）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tunnel {
    /// ID（同时用作线协议里的代理名）。
    pub id: String,
    /// 所属设备。
    pub client_id: String,
    /// 显示名。
    pub name: String,
    /// 类型。
    pub kind: String,
    /// 本地目标主机。
    pub local_ip: String,
    /// 本地目标端口。
    pub local_port: u16,
    /// 远程端口，0 = 自动分配。
    pub remote_port: u16,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: i64,
    /// 更新时间。
    pub updated_at: i64,
    /// 运行状态：pending / running / error / stopped。
    pub state: String,
    /// 实际监听端口。
    pub status_port: u16,
    /// 失败原因。
    pub status_error: String,
    /// http / https：自定义域名。
    #[serde(default)]
    pub custom_domains: Vec<String>,
    /// http / https：子域名。
    #[serde(default)]
    pub subdomain: String,
    /// http：改写 Host 头。
    #[serde(default)]
    pub host_header_rewrite: String,
    /// http：路径前缀（空 = `/`）。
    #[serde(default)]
    pub locations: Vec<String>,
    /// http：Basic 认证用户名。
    #[serde(default)]
    pub http_user: String,
    /// http：是否设置了 Basic 认证密码（密码本身永不通过 API 返回）。
    #[serde(default)]
    pub http_password_set: bool,
    /// http：按请求里的 Basic 用户名选路。
    #[serde(default)]
    pub route_by_http_user: String,
    /// http：转发前设置的请求头。
    #[serde(default)]
    pub request_headers: std::collections::BTreeMap<String, String>,
    /// PROXY protocol 版本：0 = 不写，1 / 2 = v1 / v2。
    #[serde(default)]
    pub proxy_protocol: u8,
    /// 带宽上限（字节 / 秒），0 = 不限。
    #[serde(default)]
    pub bandwidth_bps: u64,
    /// 限速由服务端执行（否则由客户端）。
    #[serde(default)]
    pub bandwidth_server: bool,
}

/// 接入令牌（不含明文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollToken {
    /// ID。
    pub id: String,
    /// 预设设备名。
    pub name: String,
    /// 创建者。
    pub created_by: String,
    /// 创建时间。
    pub created_at: i64,
    /// 过期时间。
    pub expires_at: i64,
    /// 使用时间。
    pub used_at: Option<i64>,
    /// 接入后的设备 ID。
    pub client_id: Option<String>,
    /// 撤销时间。
    pub revoked_at: Option<i64>,
    /// 令牌类型：`client` 或 `node`。
    #[serde(default)]
    pub kind: String,
    /// 设备令牌指定的节点（空串 = 内置节点）。
    #[serde(default)]
    pub node_id: String,
}

/// 节点（运行 Edge 的进程）。`id` 为空串表示内置节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// ID（内置节点为空串）。
    pub id: String,
    /// 名称。
    pub name: String,
    /// 是否为内置节点。
    pub builtin: bool,
    /// 对外主机名。
    pub public_host: String,
    /// 客户端接入端口。
    pub edge_port: u16,
    /// 叶证书 SHA-256 指纹。
    pub tls_fingerprint: String,
    /// HTTP 虚拟主机端口。
    pub vhost_http: Option<u16>,
    /// HTTPS 虚拟主机端口。
    pub vhost_https: Option<u16>,
    /// 子域名后缀。
    pub subdomain_host: String,
    /// 允许分配的端口区间。
    pub allow_ports: Vec<(u16, u16)>,
    /// 版本。
    pub version: String,
    /// 系统。
    pub os: String,
    /// 创建时间。
    pub created_at: i64,
    /// 最近一次与 Controller 通信的时间。
    pub last_seen_at: Option<i64>,
    /// 停用时间。
    pub disabled_at: Option<i64>,
    /// 控制连接是否在线（运行时状态，不入库）。
    #[serde(default)]
    pub online: bool,
    /// 由该节点服务的设备数。
    #[serde(default)]
    pub clients: i64,
}

/// 审计记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// 自增序号。
    pub id: i64,
    /// 时间。
    pub ts: i64,
    /// 操作者。
    pub actor: String,
    /// 动作。
    pub action: String,
    /// 目标类型。
    pub target_kind: String,
    /// 目标 ID。
    pub target_id: String,
    /// 理由。
    pub reason: String,
    /// 结果。
    pub result: String,
    /// 来源 IP。
    pub ip: String,
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn role_ordering_and_parsing() {
        assert!(
            Role::Owner > Role::Admin
                && Role::Admin > Role::Operator
                && Role::Operator > Role::Viewer
        );
        for r in [Role::Viewer, Role::Operator, Role::Admin, Role::Owner] {
            assert_eq!(Role::parse(r.as_str()), r);
        }
        assert_eq!(
            Role::parse("root"),
            Role::Viewer,
            "unknown roles get the lowest privilege"
        );
        assert_eq!(
            serde_json::to_string(&Role::Operator).unwrap(),
            "\"operator\""
        );
    }
}
