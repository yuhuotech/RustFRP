//! 存储层：全部同步函数，接收 `&Connection`（事务也可以传入）。业务规则（唯一性、状态迁移、软删除）都在这里，
//! HTTP 层不拼 SQL。

use rusqlite::{params, Connection, OptionalExtension, Row};
use rustfrp_core::auth::random_id;
use rustfrp_core::wire::{ConfigSet, ManagedProxy, SecretOpts, VisitorSpec};

use crate::db::StoreError;
use crate::models::{Admin, AuditEntry, Client, EnrollToken, Node, Role, Tunnel, Visitor};
use rustfrp_core::node::{NodeDevice, NodeInfo};
use rustfrp_core::wire::{HealthSpec, HttpOpts, LbOpts};

type Res<T> = Result<T, StoreError>;

/// 当前 UTC 毫秒。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 新的不透明 ID（64 位随机，16 个十六进制字符）。
pub fn new_id() -> String {
    random_id().chars().take(16).collect()
}

fn check_name(s: &str, what: &str) -> Res<()> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > 64 || t.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!(
            "{what} must be 1-64 printable characters"
        )));
    }
    Ok(())
}

// ───────────── 管理员 ─────────────

fn admin_from(r: &Row<'_>) -> rusqlite::Result<Admin> {
    Ok(Admin {
        id: r.get(0)?,
        name: r.get(1)?,
        role: Role::parse(&r.get::<_, String>(2)?),
        disabled_at: r.get(3)?,
        created_at: r.get(4)?,
    })
}
const ADMIN_COLS: &str = "id, name, role, disabled_at, created_at";

/// 创建管理员（`password_hash` 为已计算好的哈希）。
pub fn create_admin(
    c: &Connection,
    name: &str,
    password_hash: &str,
    role: Role,
    now: i64,
) -> Res<Admin> {
    check_name(name, "name")?;
    let id = new_id();
    c.execute(
        "INSERT INTO admins (id, name, password_hash, role, created_at) VALUES (?1,?2,?3,?4,?5)",
        params![id, name.trim(), password_hash, role.as_str(), now],
    )
    .map_err(|e| match StoreError::from(e) {
        StoreError::Conflict(_) => {
            StoreError::Conflict(format!("admin \"{}\" already exists", name.trim()))
        }
        other => other,
    })?;
    get_admin(c, &id)
}

/// 取管理员。
pub fn get_admin(c: &Connection, id: &str) -> Res<Admin> {
    Ok(c.query_row(
        &format!("SELECT {ADMIN_COLS} FROM admins WHERE id = ?1"),
        [id],
        admin_from,
    )?)
}

/// 按用户名取管理员及其密码哈希（登录用）。
pub fn find_admin_with_hash(c: &Connection, name: &str) -> Res<(Admin, String)> {
    Ok(c.query_row(
        &format!("SELECT {ADMIN_COLS}, password_hash FROM admins WHERE name = ?1"),
        [name],
        |r| Ok((admin_from(r)?, r.get(5)?)),
    )?)
}

/// 全部管理员。
pub fn list_admins(c: &Connection) -> Res<Vec<Admin>> {
    let mut st = c.prepare(&format!(
        "SELECT {ADMIN_COLS} FROM admins ORDER BY created_at, name"
    ))?;
    let rows = st
        .query_map([], admin_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 启用 / 禁用管理员；禁用时立即吊销其全部会话。
pub fn set_admin_disabled(c: &Connection, name: &str, disabled: bool, now: i64) -> Res<()> {
    let n = c.execute(
        "UPDATE admins SET disabled_at = ?2 WHERE name = ?1",
        params![name, if disabled { Some(now) } else { None }],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound);
    }
    if disabled {
        c.execute(
            "DELETE FROM admin_sessions WHERE admin_id = (SELECT id FROM admins WHERE name = ?1)",
            [name],
        )?;
    }
    Ok(())
}

/// 修改密码哈希，并吊销该管理员的全部会话。
pub fn set_admin_password(c: &Connection, name: &str, password_hash: &str) -> Res<()> {
    let n = c.execute(
        "UPDATE admins SET password_hash = ?2 WHERE name = ?1",
        params![name, password_hash],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound);
    }
    c.execute(
        "DELETE FROM admin_sessions WHERE admin_id = (SELECT id FROM admins WHERE name = ?1)",
        [name],
    )?;
    Ok(())
}

/// 修改管理员的角色 / 启停 / 密码（均可选）；至少保留一个启用的 owner。
/// 禁用、改密码、改角色都会吊销该管理员的全部会话。
pub fn update_admin(
    c: &Connection,
    id: &str,
    role: Option<Role>,
    disabled: Option<bool>,
    password_hash: Option<&str>,
    now: i64,
) -> Res<Admin> {
    let cur = get_admin(c, id)?;
    let was_active_owner = cur.role == Role::Owner && cur.disabled_at.is_none();
    let will_be_active_owner = role.unwrap_or(cur.role) == Role::Owner
        && disabled.map_or(cur.disabled_at.is_none(), |d| !d);
    if was_active_owner && !will_be_active_owner {
        let others: i64 = c.query_row(
            "SELECT COUNT(*) FROM admins WHERE role = 'owner' AND disabled_at IS NULL AND id <> ?1",
            [id],
            |r| r.get(0),
        )?;
        if others == 0 {
            return Err(StoreError::Conflict(
                "at least one active owner is required".into(),
            ));
        }
    }
    if let Some(r) = role {
        c.execute(
            "UPDATE admins SET role = ?2 WHERE id = ?1",
            params![id, r.as_str()],
        )?;
    }
    if let Some(d) = disabled {
        c.execute(
            "UPDATE admins SET disabled_at = ?2 WHERE id = ?1",
            params![id, if d { Some(now) } else { None }],
        )?;
    }
    if let Some(h) = password_hash {
        c.execute(
            "UPDATE admins SET password_hash = ?2 WHERE id = ?1",
            params![id, h],
        )?;
    }
    if role.is_some() || disabled == Some(true) || password_hash.is_some() {
        c.execute("DELETE FROM admin_sessions WHERE admin_id = ?1", [id])?;
    }
    get_admin(c, id)
}

// ───────────── 会话 ─────────────

/// 创建会话。
pub fn create_session(
    c: &Connection,
    token_hash: &str,
    admin_id: &str,
    csrf: &str,
    now: i64,
    ttl_ms: i64,
) -> Res<()> {
    c.execute(
        "INSERT INTO admin_sessions (token_hash, admin_id, csrf, created_at, expires_at) VALUES (?1,?2,?3,?4,?5)",
        params![token_hash, admin_id, csrf, now, now + ttl_ms],
    )?;
    Ok(())
}

/// 取有效会话（未过期且管理员未禁用），返回 `(管理员, csrf)`。
pub fn find_session(c: &Connection, token_hash: &str, now: i64) -> Res<(Admin, String)> {
    Ok(c.query_row(
        "SELECT a.id, a.name, a.role, a.disabled_at, a.created_at, s.csrf
           FROM admin_sessions s JOIN admins a ON a.id = s.admin_id
          WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND a.disabled_at IS NULL",
        params![token_hash, now],
        |r| Ok((admin_from(r)?, r.get(5)?)),
    )?)
}

/// 删除单个会话（登出）。
pub fn delete_session(c: &Connection, token_hash: &str) -> Res<()> {
    c.execute(
        "DELETE FROM admin_sessions WHERE token_hash = ?1",
        [token_hash],
    )?;
    Ok(())
}

/// 清理过期会话，返回删除条数。
pub fn purge_expired_sessions(c: &Connection, now: i64) -> Res<usize> {
    Ok(c.execute("DELETE FROM admin_sessions WHERE expires_at <= ?1", [now])?)
}

// ───────────── 接入令牌 ─────────────

fn token_from(r: &Row<'_>) -> rusqlite::Result<EnrollToken> {
    Ok(EnrollToken {
        id: r.get(0)?,
        name: r.get(1)?,
        created_by: r.get(2)?,
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        used_at: r.get(5)?,
        client_id: r.get(6)?,
        revoked_at: r.get(7)?,
        kind: r.get(8)?,
        node_id: r.get(9)?,
    })
}
const TOKEN_COLS: &str =
    "id, name, created_by, created_at, expires_at, used_at, client_id, revoked_at, kind, node_id";

/// 创建接入令牌（只存哈希）。
pub fn create_enroll_token(
    c: &Connection,
    token_hash: &str,
    name: &str,
    created_by: &str,
    now: i64,
    ttl_ms: i64,
) -> Res<EnrollToken> {
    create_token_for(c, token_hash, name, created_by, now, ttl_ms, "client", "")
}

/// 创建接入令牌：`kind` 为 `client`（可指定服务它的节点）或 `node`（接入新节点）。
#[allow(clippy::too_many_arguments)]
pub fn create_token_for(
    c: &Connection,
    token_hash: &str,
    name: &str,
    created_by: &str,
    now: i64,
    ttl_ms: i64,
    kind: &str,
    node_id: &str,
) -> Res<EnrollToken> {
    check_name(name, "name")?;
    if ttl_ms <= 0 {
        return Err(StoreError::Invalid("ttl must be positive".into()));
    }
    if !["client", "node"].contains(&kind) || (kind == "node" && !node_id.is_empty()) {
        return Err(StoreError::Invalid("invalid token kind".into()));
    }
    if !node_id.is_empty() {
        get_node(c, node_id)?;
    }
    let id = new_id();
    c.execute(
        "INSERT INTO enroll_tokens (id, token_hash, name, created_by, created_at, expires_at, kind, node_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![id, token_hash, name.trim(), created_by, now, now + ttl_ms, kind, node_id],
    )?;
    Ok(c.query_row(
        &format!("SELECT {TOKEN_COLS} FROM enroll_tokens WHERE id = ?1"),
        [id],
        token_from,
    )?)
}

/// 全部接入令牌（新的在前）。
pub fn list_enroll_tokens(c: &Connection) -> Res<Vec<EnrollToken>> {
    let mut st = c.prepare(&format!(
        "SELECT {TOKEN_COLS} FROM enroll_tokens ORDER BY created_at DESC"
    ))?;
    let rows = st
        .query_map([], token_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 撤销尚未使用的令牌。
pub fn revoke_enroll_token(c: &Connection, id: &str, now: i64) -> Res<()> {
    let n = c.execute(
        "UPDATE enroll_tokens SET revoked_at = ?2 WHERE id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
        params![id, now],
    )?;
    if n == 1 {
        return Ok(());
    }
    let exists: Option<i64> = c
        .query_row("SELECT 1 FROM enroll_tokens WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    Err(if exists.is_some() {
        StoreError::Conflict("token already used or revoked".into())
    } else {
        StoreError::NotFound
    })
}

/// 消耗一次性令牌：必须存在、未用、未撤销、未过期；成功后标记已用并关联设备。
pub fn consume_enroll_token(
    c: &Connection,
    token_hash: &str,
    client_id: &str,
    now: i64,
) -> Res<EnrollToken> {
    consume_token_of(c, token_hash, client_id, now, "client")
}

/// 消耗指定类型的一次性令牌（节点令牌把节点 ID 记在 `client_id` 列）。
pub fn consume_token_of(
    c: &Connection,
    token_hash: &str,
    subject_id: &str,
    now: i64,
    kind: &str,
) -> Res<EnrollToken> {
    let n = c.execute(
        "UPDATE enroll_tokens SET used_at = ?2, client_id = ?3
          WHERE token_hash = ?1 AND kind = ?4 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
        params![token_hash, now, subject_id, kind],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound);
    }
    Ok(c.query_row(
        &format!("SELECT {TOKEN_COLS} FROM enroll_tokens WHERE token_hash = ?1"),
        [token_hash],
        token_from,
    )?)
}

// ───────────── 节点 ─────────────

fn node_from(r: &Row<'_>) -> rusqlite::Result<Node> {
    let ports: String = r.get(8)?;
    Ok(Node {
        id: r.get(0)?,
        name: r.get(1)?,
        builtin: false,
        public_host: r.get(2)?,
        edge_port: u16::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
        tls_fingerprint: r.get(4)?,
        vhost_http: r
            .get::<_, Option<i64>>(5)?
            .and_then(|p| u16::try_from(p).ok()),
        vhost_https: r
            .get::<_, Option<i64>>(6)?
            .and_then(|p| u16::try_from(p).ok()),
        subdomain_host: r.get(7)?,
        allow_ports: serde_json::from_str(&ports).unwrap_or_default(),
        version: r.get(9)?,
        os: r.get(10)?,
        created_at: r.get(11)?,
        last_seen_at: r.get(12)?,
        disabled_at: r.get(13)?,
        online: false,
        clients: r.get(14)?,
    })
}
const NODE_COLS: &str = "id, name, public_host, edge_port, tls_fingerprint, vhost_http, vhost_https, subdomain_host, allow_ports, version, os, created_at, last_seen_at, disabled_at, (SELECT COUNT(*) FROM clients c WHERE c.node_id = nodes.id AND c.deleted_at IS NULL)";

/// 创建外置节点。
pub fn create_node(
    c: &Connection,
    id: &str,
    name: &str,
    pubkey: &str,
    info: &NodeInfo,
    now: i64,
) -> Res<Node> {
    check_name(name, "name")?;
    if !rustfrp_core::identity::is_valid_public_hex(pubkey) {
        return Err(StoreError::Invalid("invalid public key".into()));
    }
    c.execute(
        "INSERT INTO nodes (id, name, pubkey, created_at, last_seen_at) VALUES (?1,?2,?3,?4,?4)",
        params![id, name.trim(), pubkey, now],
    )?;
    update_node_info(c, id, info, now)?;
    get_node(c, id)
}

/// 取节点（未删除）。
pub fn get_node(c: &Connection, id: &str) -> Res<Node> {
    Ok(c.query_row(
        &format!("SELECT {NODE_COLS} FROM nodes WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        node_from,
    )?)
}

/// 取节点及其公钥（登录验签用）。
pub fn get_node_with_key(c: &Connection, id: &str) -> Res<(Node, String)> {
    let key: String = c.query_row(
        "SELECT pubkey FROM nodes WHERE id = ?1 AND deleted_at IS NULL",
        [id],
        |r| r.get(0),
    )?;
    Ok((get_node(c, id)?, key))
}

/// 全部外置节点。
pub fn list_nodes(c: &Connection) -> Res<Vec<Node>> {
    let mut st = c.prepare(&format!(
        "SELECT {NODE_COLS} FROM nodes WHERE deleted_at IS NULL ORDER BY created_at, name"
    ))?;
    let rows = st
        .query_map([], node_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 节点上线时更新其自述信息（对外主机名为空时保留已有值，允许管理员事先指定）。
pub fn update_node_info(c: &Connection, id: &str, info: &NodeInfo, now: i64) -> Res<()> {
    let ports = serde_json::to_string(&info.allow_ports).unwrap_or_else(|_| "[]".into());
    c.execute(
        "UPDATE nodes SET public_host = CASE WHEN ?2 = '' THEN public_host ELSE ?2 END,
                edge_port = ?3, tls_fingerprint = ?4, vhost_http = ?5, vhost_https = ?6,
                subdomain_host = ?7, allow_ports = ?8, version = ?9, os = ?10, last_seen_at = ?11
          WHERE id = ?1",
        params![
            id,
            info.public_host,
            i64::from(info.edge_port),
            info.tls_fingerprint,
            info.vhost_http.map(i64::from),
            info.vhost_https.map(i64::from),
            info.subdomain_host,
            ports,
            info.version,
            info.os,
            now
        ],
    )?;
    Ok(())
}

/// 记录节点最近通信时间。
pub fn touch_node(c: &Connection, id: &str, now: i64) -> Res<()> {
    c.execute(
        "UPDATE nodes SET last_seen_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

/// 改名 / 停用 / 启用节点。
pub fn update_node(
    c: &Connection,
    id: &str,
    name: Option<&str>,
    disabled: Option<bool>,
    now: i64,
) -> Res<Node> {
    get_node(c, id)?;
    if let Some(n) = name {
        check_name(n, "name")?;
        c.execute(
            "UPDATE nodes SET name = ?2 WHERE id = ?1",
            params![id, n.trim()],
        )?;
    }
    if let Some(d) = disabled {
        c.execute(
            "UPDATE nodes SET disabled_at = ?2 WHERE id = ?1",
            params![id, if d { Some(now) } else { None }],
        )?;
    }
    get_node(c, id)
}

/// 删除节点：仍有设备在用时拒绝。
pub fn delete_node(c: &Connection, id: &str, now: i64) -> Res<()> {
    let node = get_node(c, id)?;
    if node.clients > 0 {
        return Err(StoreError::Conflict(format!(
            "node still serves {} client(s); delete or move them first",
            node.clients
        )));
    }
    c.execute(
        "UPDATE nodes SET deleted_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

/// 把设备指派给节点（空串 = 内置节点）；只在接入时使用。
pub fn set_client_node(c: &Connection, client_id: &str, node_id: &str) -> Res<()> {
    if !node_id.is_empty() {
        get_node(c, node_id)?;
    }
    c.execute(
        "UPDATE clients SET node_id = ?2 WHERE id = ?1",
        params![client_id, node_id],
    )?;
    Ok(())
}

/// 设备所属节点（包含已删除的设备，用于删除后通知节点）。
pub fn client_node_id_any(c: &Connection, client_id: &str) -> Res<String> {
    Ok(c.query_row(
        "SELECT node_id FROM clients WHERE id = ?1",
        [client_id],
        |r| r.get(0),
    )?)
}

/// 下发给节点的设备条目；已删除的设备返回 `NotFound`。
pub fn node_device(c: &Connection, client_id: &str) -> Res<NodeDevice> {
    let (client, pubkey) = get_client_with_key(c, client_id)?;
    Ok(NodeDevice {
        id: client.id.clone(),
        pubkey,
        disabled: client.disabled_at.is_some(),
        config: managed_config(c, client_id)?,
    })
}

/// 某节点应持有的全部设备（不含被停用的）。
pub fn list_node_devices(c: &Connection, node_id: &str) -> Res<Vec<NodeDevice>> {
    let mut st = c.prepare(
        "SELECT id FROM clients WHERE node_id = ?1 AND deleted_at IS NULL AND disabled_at IS NULL ORDER BY created_at",
    )?;
    let ids = st
        .query_map([node_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.iter().map(|id| node_device(c, id)).collect()
}

// ───────────── 客户端 ─────────────

fn client_from(r: &Row<'_>) -> rusqlite::Result<Client> {
    Ok(Client {
        id: r.get(0)?,
        name: r.get(1)?,
        os: r.get(2)?,
        arch: r.get(3)?,
        version: r.get(4)?,
        revision: r.get(5)?,
        created_at: r.get(6)?,
        last_seen_at: r.get(7)?,
        disabled_at: r.get(8)?,
        online: false,
        node_id: r.get(9)?,
        tags: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default(),
        last_ip: r.get(11)?,
    })
}
const CLIENT_COLS: &str =
    "id, name, os, arch, version, revision, created_at, last_seen_at, disabled_at, node_id, tags, last_ip";

/// 创建设备。
#[allow(clippy::too_many_arguments)]
pub fn create_client(
    c: &Connection,
    id: &str,
    name: &str,
    pubkey: &str,
    os: &str,
    arch: &str,
    version: &str,
    now: i64,
) -> Res<Client> {
    check_name(name, "name")?;
    if !rustfrp_core::identity::is_valid_public_hex(pubkey) {
        return Err(StoreError::Invalid("invalid public key".into()));
    }
    c.execute(
        "INSERT INTO clients (id, name, pubkey, os, arch, version, created_at, last_seen_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
        params![id, name.trim(), pubkey, os, arch, version, now],
    )?;
    get_client(c, id)
}

/// 取设备（未删除）。
pub fn get_client(c: &Connection, id: &str) -> Res<Client> {
    Ok(c.query_row(
        &format!("SELECT {CLIENT_COLS} FROM clients WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        client_from,
    )?)
}

/// 取设备及其公钥（登录验签用；已删除的视为不存在）。
pub fn get_client_with_key(c: &Connection, id: &str) -> Res<(Client, String)> {
    Ok(c.query_row(
        &format!("SELECT {CLIENT_COLS}, pubkey FROM clients WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        |r| Ok((client_from(r)?, r.get(12)?)),
    )?)
}

/// 全部设备（未删除）。
pub fn list_clients(c: &Connection) -> Res<Vec<Client>> {
    let mut st = c.prepare(&format!(
        "SELECT {CLIENT_COLS} FROM clients WHERE deleted_at IS NULL ORDER BY created_at, name"
    ))?;
    let rows = st
        .query_map([], client_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 更新设备名 / 禁用状态。
pub fn update_client(
    c: &Connection,
    id: &str,
    name: Option<&str>,
    disabled: Option<bool>,
    now: i64,
) -> Res<Client> {
    get_client(c, id)?;
    if let Some(n) = name {
        check_name(n, "name")?;
        c.execute(
            "UPDATE clients SET name = ?2 WHERE id = ?1",
            params![id, n.trim()],
        )?;
    }
    if let Some(d) = disabled {
        c.execute(
            "UPDATE clients SET disabled_at = ?2 WHERE id = ?1",
            params![id, if d { Some(now) } else { None }],
        )?;
    }
    get_client(c, id)
}

/// 软删除设备及其全部隧道。
pub fn delete_client(c: &Connection, id: &str, now: i64) -> Res<()> {
    let n = c.execute(
        "UPDATE clients SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, now],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound);
    }
    c.execute("UPDATE tunnels SET deleted_at = ?2, enabled = 0 WHERE client_id = ?1 AND deleted_at IS NULL", params![id, now])?;
    c.execute("UPDATE visitors SET deleted_at = ?2, enabled = 0 WHERE client_id = ?1 AND deleted_at IS NULL", params![id, now])?;
    Ok(())
}

const MAX_TAGS: usize = 8;
/// 带宽上限的合理上界（1 TiB/s）。
const MAX_BANDWIDTH: u64 = 1 << 40;

/// 设置设备标签：去掉首尾空白和重复项，最多 8 个，每个 1–32 个字符且不含控制字符。
pub fn set_client_tags(c: &Connection, id: &str, tags: &[String]) -> Res<Client> {
    get_client(c, id)?;
    let mut clean: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim();
        if t.is_empty() || t.chars().count() > 32 || t.chars().any(char::is_control) {
            return Err(StoreError::Invalid(
                "each tag must be 1-32 printable characters".into(),
            ));
        }
        if !clean.iter().any(|x| x == t) {
            clean.push(t.to_string());
        }
    }
    if clean.len() > MAX_TAGS {
        return Err(StoreError::Invalid(format!("at most {MAX_TAGS} tags")));
    }
    let text = if clean.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&clean).unwrap_or_default()
    };
    c.execute(
        "UPDATE clients SET tags = ?2 WHERE id = ?1",
        params![id, text],
    )?;
    get_client(c, id)
}

/// 请求设备重启 agent：递增计数并随配置下发（设备在线才会生效）。
pub fn request_client_restart(c: &Connection, id: &str) -> Res<i64> {
    get_client(c, id)?;
    c.execute(
        "UPDATE clients SET restart_seq = restart_seq + 1 WHERE id = ?1",
        [id],
    )?;
    bump_revision(c, id)?;
    Ok(
        c.query_row("SELECT restart_seq FROM clients WHERE id = ?1", [id], |r| {
            r.get(0)
        })?,
    )
}

/// 记录在线时间（可同时更新版本）。
pub fn touch_client(c: &Connection, id: &str, now: i64) -> Res<()> {
    c.execute(
        "UPDATE clients SET last_seen_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

/// 记录设备最近一次登录的来源地址（空串不覆盖）。
pub fn set_client_ip(c: &Connection, id: &str, ip: &str) -> Res<()> {
    if !ip.is_empty() && ip.len() <= 64 {
        c.execute(
            "UPDATE clients SET last_ip = ?2 WHERE id = ?1",
            params![id, ip],
        )?;
    }
    Ok(())
}

/// 配置版本号 +1，返回新值。
pub fn bump_revision(c: &Connection, id: &str) -> Res<i64> {
    c.execute(
        "UPDATE clients SET revision = revision + 1 WHERE id = ?1",
        [id],
    )?;
    Ok(
        c.query_row("SELECT revision FROM clients WHERE id = ?1", [id], |r| {
            r.get(0)
        })?,
    )
}

// ───────────── 隧道 ─────────────

/// 新建隧道的参数。
#[derive(Debug, Clone, Default)]
pub struct NewTunnel {
    /// 所属设备。
    pub client_id: String,
    /// 显示名。
    pub name: String,
    /// 类型。
    pub kind: String,
    /// 本地目标主机。
    pub local_ip: String,
    /// 本地目标端口。
    pub local_port: i64,
    /// 远程端口。
    pub remote_port: i64,
    /// 是否启用。
    pub enabled: bool,
    /// http / https：自定义域名。
    pub custom_domains: Vec<String>,
    /// http / https：子域名。
    pub subdomain: String,
    /// http：改写 Host 头。
    pub host_header_rewrite: String,
    /// http：路径 / 认证 / 请求头选项。
    pub http: HttpOpts,
    /// PROXY protocol 版本（0 / 1 / 2）。
    pub proxy_protocol: u8,
    /// 带宽上限（字节 / 秒），0 = 不限。
    pub bandwidth_bps: u64,
    /// 限速由服务端执行。
    pub bandwidth_server: bool,
    /// 负载均衡组与健康检查。
    pub lb: LbOpts,
}

/// 修改隧道的参数（`None` 表示不改）。
#[derive(Debug, Clone, Default)]
pub struct TunnelPatch {
    /// 显示名。
    pub name: Option<String>,
    /// 本地目标主机。
    pub local_ip: Option<String>,
    /// 本地目标端口。
    pub local_port: Option<i64>,
    /// 远程端口。
    pub remote_port: Option<i64>,
    /// 是否启用。
    pub enabled: Option<bool>,
    /// http / https：自定义域名。
    pub custom_domains: Option<Vec<String>>,
    /// http / https：子域名。
    pub subdomain: Option<String>,
    /// http：改写 Host 头。
    pub host_header_rewrite: Option<String>,
    /// http：路径前缀。
    pub locations: Option<Vec<String>>,
    /// http：Basic 认证用户名。
    pub http_user: Option<String>,
    /// http：Basic 认证密码（`Some("")` 清除，`None` 保持）。
    pub http_password: Option<String>,
    /// http：按 Basic 用户名选路。
    pub route_by_http_user: Option<String>,
    /// http：转发前设置的请求头。
    pub request_headers: Option<std::collections::BTreeMap<String, String>>,
    /// http：转发给访问者前设置的响应头。
    pub response_headers: Option<std::collections::BTreeMap<String, String>>,
    /// PROXY protocol 版本。
    pub proxy_protocol: Option<u8>,
    /// 带宽上限（字节 / 秒）。
    pub bandwidth_bps: Option<u64>,
    /// 限速位置。
    pub bandwidth_server: Option<bool>,
    /// 负载均衡组名（`Some("")` 退出组）。
    pub lb_group: Option<String>,
    /// 组密钥（`Some("")` 清除，`None` 保持）。
    pub lb_group_key: Option<String>,
    /// 健康检查（`kind` 为空表示取消）。
    pub health_check: Option<HealthSpec>,
}

fn parse_opts(raw: &str) -> HttpOpts {
    serde_json::from_str(raw).unwrap_or_default()
}

fn store_opts(o: &HttpOpts) -> String {
    if o.is_default() {
        String::new()
    } else {
        serde_json::to_string(o).unwrap_or_default()
    }
}

fn parse_lb(raw: &str) -> LbOpts {
    serde_json::from_str(raw).unwrap_or_default()
}

fn store_lb(o: &LbOpts) -> String {
    if o.is_default() {
        String::new()
    } else {
        serde_json::to_string(o).unwrap_or_default()
    }
}

fn tunnel_from(r: &Row<'_>) -> rusqlite::Result<Tunnel> {
    let opts = parse_opts(&r.get::<_, String>(16)?);
    let lb = parse_lb(&r.get::<_, String>(20)?);
    Ok(Tunnel {
        id: r.get(0)?,
        client_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        local_ip: r.get(4)?,
        local_port: r.get::<_, i64>(5)? as u16,
        remote_port: r.get::<_, i64>(6)? as u16,
        enabled: r.get::<_, i64>(7)? != 0,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        state: r.get(10)?,
        status_port: r.get::<_, i64>(11)? as u16,
        status_error: r.get(12)?,
        custom_domains: split_domains(&r.get::<_, String>(13)?),
        subdomain: r.get(14)?,
        host_header_rewrite: r.get(15)?,
        locations: opts.locations,
        http_user: opts.http_user,
        http_password_set: !opts.http_password.is_empty(),
        route_by_http_user: opts.route_by_http_user,
        request_headers: opts.request_headers,
        response_headers: opts.response_headers,
        proxy_protocol: u8::try_from(r.get::<_, i64>(17)?).unwrap_or(0),
        bandwidth_bps: u64::try_from(r.get::<_, i64>(18)?).unwrap_or(0),
        bandwidth_server: r.get::<_, i64>(19)? != 0,
        lb_group: lb.group,
        lb_group_key_set: !lb.group_key.is_empty(),
        health_check: lb.health,
    })
}
const TUNNEL_COLS: &str = "id, client_id, name, kind, local_ip, local_port, remote_port, enabled, created_at, updated_at, status_state, status_port, status_error, custom_domains, subdomain, host_header_rewrite, http_opts, proxy_protocol, bandwidth_bps, bandwidth_server, lb_opts";

fn check_port(v: i64, what: &str, allow_zero: bool) -> Res<()> {
    let lo = i64::from(!allow_zero);
    if (lo..=65535).contains(&v) {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!(
            "{what} must be in {lo}..65535"
        )))
    }
}

fn check_host(h: &str) -> Res<()> {
    if h.is_empty() || h.len() > 253 || h.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(StoreError::Invalid(
            "local_ip must be a host name or IP address".into(),
        ));
    }
    Ok(())
}

const KINDS: &[&str] = &["tcp", "udp", "http", "https", "stcp", "sudp"];

fn is_secret(kind: &str) -> bool {
    kind == "stcp" || kind == "sudp"
}

fn is_web(kind: &str) -> bool {
    kind == "http" || kind == "https"
}

fn split_domains(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect()
}

/// 校验并规范化域名（小写；允许 `*.example.com` 通配）。
fn normalize_domains(list: &[String]) -> Res<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let d = raw.trim().to_ascii_lowercase();
        let body = d.strip_prefix("*.").unwrap_or(&d);
        let ok = !body.is_empty()
            && d.len() <= 253
            && body.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !l.starts_with('-')
                    && !l.ends_with('-')
            });
        if !ok {
            return Err(StoreError::Invalid(format!(
                "\"{}\" is not a valid domain name",
                raw.trim()
            )));
        }
        if !out.contains(&d) {
            out.push(d);
        }
    }
    Ok(out)
}

fn check_subdomain(s: &str) -> Res<()> {
    let ok = s.is_empty()
        || (s.len() <= 63
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !s.starts_with('-')
            && !s.ends_with('-'));
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "subdomain must be a single DNS label (letters, digits, '-')".into(),
        ))
    }
}

fn check_rewrite(s: &str) -> Res<()> {
    if s.len() > 253 || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(StoreError::Invalid(
            "host_header_rewrite must be a host name (optionally with port)".into(),
        ));
    }
    Ok(())
}

/// 同一协议类别内远程端口不能重复（tcp 与 udp 是不同的端口空间）。
fn ensure_remote_port_free(
    c: &Connection,
    kind: &str,
    port: i64,
    lb: &LbOpts,
    except_id: Option<&str>,
) -> Res<()> {
    if is_web(kind) {
        return Ok(());
    }
    let mut st = c.prepare(
        "SELECT remote_port, lb_opts FROM tunnels WHERE kind = ?1 AND deleted_at IS NULL AND (?2 IS NULL OR id <> ?2)",
    )?;
    let rows = st
        .query_map(params![kind, except_id], |r| {
            Ok((r.get::<_, i64>(0)?, parse_lb(&r.get::<_, String>(1)?)))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (other_port, other) in rows {
        let same_group = !lb.group.is_empty() && other.group == lb.group;
        if same_group && other.group_key != lb.group_key {
            return Err(StoreError::Conflict(format!(
                "group \"{}\" key doesn't match the existing members",
                lb.group
            )));
        }
        if same_group && port != 0 && other_port != 0 && port != other_port {
            return Err(StoreError::Invalid(format!(
                "members of group \"{}\" must share the same remote port",
                lb.group
            )));
        }
        if port != 0 && other_port == port && !same_group {
            return Err(StoreError::Conflict(format!(
                "remote port {port} is already used by another {kind} tunnel"
            )));
        }
    }
    Ok(())
}

/// 同类别（http / https）内：同一域名只有在路径前缀不重叠、或 `routeByHTTPUser` 不同时才可共存；
/// 同组（组名与密钥一致）且路径集合完全一致的隧道共用域名，由 Edge 在健康成员间轮转。
/// 子域名按 `sub.<根域名>` 在运行时解析，这里按「同名子域名」同样处理。
fn ensure_domains_free(
    c: &Connection,
    kind: &str,
    domains: &[String],
    subdomain: &str,
    http: &HttpOpts,
    lb: &LbOpts,
    except_id: Option<&str>,
) -> Res<()> {
    if !is_web(kind) {
        return Ok(());
    }
    let mut st = c.prepare(
        "SELECT custom_domains, subdomain, http_opts, lb_opts FROM tunnels WHERE kind = ?1 AND deleted_at IS NULL AND (?2 IS NULL OR id <> ?2)",
    )?;
    let rows = st
        .query_map(params![kind, except_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mine = effective_locations(http);
    for (other_domains, other_sub, other_opts, other_lb) in rows {
        let other = parse_opts(&other_opts);
        let other_lb = parse_lb(&other_lb);
        let theirs_locations = effective_locations(&other);
        let overlap = other.route_by_http_user == http.route_by_http_user
            && theirs_locations.iter().any(|l| mine.contains(l));
        if !overlap {
            continue;
        }
        let theirs = split_domains(&other_domains);
        let clash = domains
            .iter()
            .find(|d| theirs.contains(d))
            .map(|d| format!("domain \"{d}\" is already used by another {kind} tunnel"));
        let clash = clash.or_else(|| {
            (!subdomain.is_empty() && subdomain == other_sub).then(|| {
                format!("subdomain \"{subdomain}\" is already used by another {kind} tunnel")
            })
        });
        let Some(message) = clash else { continue };
        if !lb.group.is_empty() && other_lb.group == lb.group {
            if other_lb.group_key != lb.group_key {
                return Err(StoreError::Conflict(format!(
                    "group \"{}\" key doesn't match the existing members",
                    lb.group
                )));
            }
            let (mut a, mut b) = (mine.clone(), theirs_locations);
            a.sort();
            b.sort();
            if a == b {
                continue;
            }
        }
        return Err(StoreError::Conflict(message));
    }
    Ok(())
}

/// 校验负载均衡 / 健康检查选项；udp 隧道不支持。
fn check_lb(kind: &str, lb: LbOpts) -> Res<LbOpts> {
    let bad = |m: &str| Err(StoreError::Invalid(m.into()));
    if lb.is_default() {
        return Ok(lb);
    }
    if kind == "udp" || is_secret(kind) {
        return bad(
            "load balancer groups and health checks do not apply to udp, stcp and sudp tunnels",
        );
    }
    let printable = |v: &str| v.len() <= 64 && !v.chars().any(char::is_control);
    if !printable(&lb.group) || !printable(&lb.group_key) {
        return bad("group and group key must be at most 64 printable characters");
    }
    if lb.group.is_empty() && !lb.group_key.is_empty() {
        return bad("group key needs a group name");
    }
    if let Some(h) = &lb.health {
        if !["tcp", "http"].contains(&h.kind.as_str()) {
            return bad("health check type must be tcp or http");
        }
        if !(1..=60).contains(&h.timeout_secs)
            || !(1..=20).contains(&h.max_failed)
            || !(1..=3600).contains(&h.interval_secs)
        {
            return bad("health check timeout 1-60s, max failed 1-20, interval 1-3600s");
        }
        if h.kind == "http" {
            if !h.path.starts_with('/')
                || h.path.len() > 256
                || h.path.chars().any(|c| c.is_control() || c == ' ')
            {
                return bad("http health check path must start with / and contain no spaces");
            }
        } else if !h.path.is_empty() || !h.headers.is_empty() {
            return bad("path and headers only apply to http health checks");
        }
        if h.headers.len() > 8
            || h.headers.iter().any(|(k, v)| {
                !rustfrp_core::http1::valid_header(k, v) || k.len() > 64 || v.len() > 256
            })
        {
            return bad("invalid health check headers");
        }
    }
    Ok(lb)
}

fn effective_locations(o: &HttpOpts) -> Vec<String> {
    if o.locations.is_empty() {
        vec!["/".into()]
    } else {
        o.locations.clone()
    }
}

const MAX_LOCATIONS: usize = 16;
const MAX_REQUEST_HEADERS: usize = 16;

/// 校验 http 选项；非 http 隧道不得带有任何选项。
fn check_http_opts(kind: &str, o: HttpOpts) -> Res<HttpOpts> {
    if o.is_default() {
        return Ok(o);
    }
    if kind != "http" {
        return Err(StoreError::Invalid(
            "locations, Basic auth and request headers only apply to http tunnels".into(),
        ));
    }
    let bad = |m: &str| Err(StoreError::Invalid(m.into()));
    if o.locations.len() > MAX_LOCATIONS
        || o.request_headers.len() > MAX_REQUEST_HEADERS
        || o.response_headers.len() > MAX_REQUEST_HEADERS
    {
        return bad("too many locations or request headers");
    }
    let mut locations: Vec<String> = Vec::new();
    for l in &o.locations {
        if !l.starts_with('/') || l.len() > 256 || l.chars().any(|c| c.is_control() || c == ' ') {
            return bad("each location must start with / and contain no spaces");
        }
        if !locations.contains(l) {
            locations.push(l.clone());
        }
    }
    for (k, v) in &o.request_headers {
        if !rustfrp_core::http1::valid_header(k, v) || k.len() > 64 || v.len() > 1024 {
            return bad("invalid request header");
        }
    }
    for (k, v) in &o.response_headers {
        // 改写分帧 / 连接管理头会让 Edge 无法再判断响应边界。
        let framing = [
            "content-length",
            "transfer-encoding",
            "connection",
            "upgrade",
        ]
        .iter()
        .any(|f| k.eq_ignore_ascii_case(f));
        if framing || !rustfrp_core::http1::valid_header(k, v) || k.len() > 64 || v.len() > 1024 {
            return bad("invalid response header");
        }
    }
    for v in [&o.http_user, &o.http_password, &o.route_by_http_user] {
        if v.len() > 128 || v.chars().any(char::is_control) {
            return bad("Basic auth user, password and routeByHTTPUser must be at most 128 printable characters");
        }
    }
    if o.http_user.contains(':') {
        return bad("Basic auth user must not contain ':'");
    }
    Ok(HttpOpts { locations, ..o })
}

struct Checked {
    name: String,
    kind: String,
    local_ip: String,
    local_port: i64,
    remote_port: i64,
    domains: Vec<String>,
    subdomain: String,
    rewrite: String,
    http: HttpOpts,
    proxy_protocol: u8,
    bandwidth_bps: u64,
    bandwidth_server: bool,
    lb: LbOpts,
}

fn check_tunnel(
    c: &Connection,
    t: Checked,
    except_id: Option<&str>,
    client_id: &str,
) -> Res<Checked> {
    check_name(&t.name, "name")?;
    if !KINDS.contains(&t.kind.as_str()) {
        return Err(StoreError::Invalid(format!(
            "tunnel type \"{}\" is not supported yet, supported: tcp, udp, http, https, stcp, sudp",
            t.kind
        )));
    }
    check_host(&t.local_ip)?;
    check_port(t.local_port, "local_port", false)?;
    check_port(t.remote_port, "remote_port", true)?;
    let domains = normalize_domains(&t.domains)?;
    check_subdomain(&t.subdomain)?;
    check_rewrite(&t.rewrite)?;
    let http = check_http_opts(&t.kind, t.http.clone())?;
    if t.bandwidth_bps > MAX_BANDWIDTH
        || (t.bandwidth_bps != 0 && (t.kind == "udp" || t.kind == "sudp"))
    {
        return Err(StoreError::Invalid(
            "bandwidth limit must be 0..=1 TiB/s and does not apply to udp tunnels".into(),
        ));
    }
    if t.proxy_protocol > 2 || (t.proxy_protocol != 0 && (t.kind == "udp" || is_secret(&t.kind))) {
        return Err(StoreError::Invalid(
            "proxy_protocol must be 0, 1 or 2 and does not apply to udp, stcp and sudp tunnels"
                .into(),
        ));
    }
    if is_secret(&t.kind) && t.remote_port != 0 {
        return Err(StoreError::Invalid(
            "stcp / sudp tunnels have no public port; remote_port must be 0".into(),
        ));
    }
    if is_web(&t.kind) {
        if domains.is_empty() && t.subdomain.is_empty() {
            return Err(StoreError::Invalid(
                "http / https tunnels need a custom domain or a subdomain".into(),
            ));
        }
        if t.remote_port != 0 {
            return Err(StoreError::Invalid(
                "http / https tunnels use the vhost port; remote_port must be 0".into(),
            ));
        }
        if t.kind == "https" && !t.rewrite.is_empty() {
            return Err(StoreError::Invalid(
                "host_header_rewrite only applies to http tunnels".into(),
            ));
        }
    } else if !domains.is_empty() || !t.subdomain.is_empty() || !t.rewrite.is_empty() {
        return Err(StoreError::Invalid(
            "domains and host_header_rewrite only apply to http / https tunnels".into(),
        ));
    }
    let lb = check_lb(&t.kind, t.lb.clone())?;
    ensure_remote_port_free(c, &t.kind, t.remote_port, &lb, except_id)?;
    ensure_domains_free(c, &t.kind, &domains, &t.subdomain, &http, &lb, except_id)?;
    let dup: Option<String> = c
        .query_row(
            "SELECT id FROM tunnels WHERE client_id = ?1 AND name = ?2 AND deleted_at IS NULL AND (?3 IS NULL OR id <> ?3)",
            params![client_id, t.name.trim(), except_id],
            |r| r.get(0),
        )
        .optional()?;
    if dup.is_some() {
        return Err(StoreError::Conflict(format!(
            "tunnel \"{}\" already exists on this client",
            t.name.trim()
        )));
    }
    Ok(Checked {
        domains,
        http,
        lb,
        ..t
    })
}

/// 创建隧道。
pub fn create_tunnel(c: &Connection, t: &NewTunnel, now: i64) -> Res<Tunnel> {
    get_client(c, &t.client_id)?;
    let k = check_tunnel(
        c,
        Checked {
            name: t.name.clone(),
            kind: t.kind.clone(),
            local_ip: t.local_ip.clone(),
            local_port: t.local_port,
            remote_port: t.remote_port,
            domains: t.custom_domains.clone(),
            subdomain: t.subdomain.to_ascii_lowercase(),
            rewrite: t.host_header_rewrite.clone(),
            http: t.http.clone(),
            proxy_protocol: t.proxy_protocol,
            bandwidth_bps: t.bandwidth_bps,
            bandwidth_server: t.bandwidth_server,
            lb: t.lb.clone(),
        },
        None,
        &t.client_id,
    )?;
    let id = new_id();
    c.execute(
        "INSERT INTO tunnels (id, client_id, name, kind, local_ip, local_port, remote_port, enabled, created_at, updated_at, custom_domains, subdomain, host_header_rewrite, http_opts, proxy_protocol, bandwidth_bps, bandwidth_server, lb_opts, secret_key)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![id, t.client_id, k.name.trim(), k.kind, k.local_ip, k.local_port, k.remote_port, i64::from(t.enabled), now, k.domains.join(","), k.subdomain, k.rewrite, store_opts(&k.http), i64::from(k.proxy_protocol), i64::try_from(k.bandwidth_bps).unwrap_or(0), i64::from(k.bandwidth_server), store_lb(&k.lb), if is_secret(&k.kind) { random_id() } else { String::new() }],
    )?;
    bump_revision(c, &t.client_id)?;
    get_tunnel(c, &id)
}

/// 客户端的隧道数不得超过 `max`（0 = 不限）。在写入同一事务内调用，超限即回滚。
pub fn enforce_tunnel_quota(c: &Connection, client_id: &str, max: i64) -> Res<()> {
    if max <= 0 {
        return Ok(());
    }
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM tunnels WHERE client_id = ?1 AND deleted_at IS NULL",
        [client_id],
        |r| r.get(0),
    )?;
    if n > max {
        return Err(StoreError::Conflict(format!(
            "client tunnel limit reached ({max})"
        )));
    }
    Ok(())
}

/// 取隧道（未删除）。
pub fn get_tunnel(c: &Connection, id: &str) -> Res<Tunnel> {
    Ok(c.query_row(
        &format!("SELECT {TUNNEL_COLS} FROM tunnels WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        tunnel_from,
    )?)
}

/// 隧道列表，可按设备过滤。
pub fn list_tunnels(c: &Connection, client_id: Option<&str>) -> Res<Vec<Tunnel>> {
    let mut st = c.prepare(&format!(
        "SELECT {TUNNEL_COLS} FROM tunnels WHERE deleted_at IS NULL AND (?1 IS NULL OR client_id = ?1) ORDER BY created_at, name"
    ))?;
    let rows = st
        .query_map([client_id], tunnel_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 修改隧道（类型不可改；需要换类型请删除重建）。
pub fn update_tunnel(c: &Connection, id: &str, p: &TunnelPatch, now: i64) -> Res<Tunnel> {
    let cur = get_tunnel(c, id)?;
    let cur_opts = tunnel_http_opts(c, id)?;
    let new_opts = HttpOpts {
        locations: p
            .locations
            .clone()
            .unwrap_or_else(|| cur_opts.locations.clone()),
        http_user: p
            .http_user
            .clone()
            .unwrap_or_else(|| cur_opts.http_user.clone()),
        http_password: p
            .http_password
            .clone()
            .unwrap_or_else(|| cur_opts.http_password.clone()),
        route_by_http_user: p
            .route_by_http_user
            .clone()
            .unwrap_or_else(|| cur_opts.route_by_http_user.clone()),
        request_headers: p
            .request_headers
            .clone()
            .unwrap_or_else(|| cur_opts.request_headers.clone()),
        response_headers: p
            .response_headers
            .clone()
            .unwrap_or_else(|| cur_opts.response_headers.clone()),
    };
    let cur_lb = tunnel_lb(c, id)?;
    let new_lb = LbOpts {
        group: p.lb_group.clone().unwrap_or_else(|| cur_lb.group.clone()),
        group_key: p
            .lb_group_key
            .clone()
            .unwrap_or_else(|| cur_lb.group_key.clone()),
        health: match &p.health_check {
            Some(h) if h.kind.is_empty() => None,
            Some(h) => Some(h.clone()),
            None => cur_lb.health.clone(),
        },
    };
    let enabled = p.enabled.unwrap_or(cur.enabled);
    let k = check_tunnel(
        c,
        Checked {
            name: p.name.clone().unwrap_or_else(|| cur.name.clone()),
            kind: cur.kind.clone(),
            local_ip: p.local_ip.clone().unwrap_or_else(|| cur.local_ip.clone()),
            local_port: p.local_port.unwrap_or(i64::from(cur.local_port)),
            remote_port: p.remote_port.unwrap_or(i64::from(cur.remote_port)),
            domains: p
                .custom_domains
                .clone()
                .unwrap_or_else(|| cur.custom_domains.clone()),
            subdomain: p
                .subdomain
                .clone()
                .map_or_else(|| cur.subdomain.clone(), |s| s.to_ascii_lowercase()),
            rewrite: p
                .host_header_rewrite
                .clone()
                .unwrap_or_else(|| cur.host_header_rewrite.clone()),
            http: new_opts,
            proxy_protocol: p.proxy_protocol.unwrap_or(cur.proxy_protocol),
            bandwidth_bps: p.bandwidth_bps.unwrap_or(cur.bandwidth_bps),
            bandwidth_server: p.bandwidth_server.unwrap_or(cur.bandwidth_server),
            lb: new_lb,
        },
        Some(id),
        &cur.client_id,
    )?;
    let runtime_changed = enabled != cur.enabled
        || k.http != cur_opts
        || k.proxy_protocol != cur.proxy_protocol
        || k.bandwidth_bps != cur.bandwidth_bps
        || k.bandwidth_server != cur.bandwidth_server
        || k.lb != cur_lb
        || k.local_ip != cur.local_ip
        || k.local_port != i64::from(cur.local_port)
        || k.remote_port != i64::from(cur.remote_port)
        || k.domains != cur.custom_domains
        || k.subdomain != cur.subdomain
        || k.rewrite != cur.host_header_rewrite;
    // 仅数据面配置变化时等待 Edge 回报；重复启停和改显示名保留实际状态。
    c.execute(
        "UPDATE tunnels SET name=?2, local_ip=?3, local_port=?4, remote_port=?5, enabled=?6, updated_at=?7, status_state=CASE WHEN ?11 THEN 'pending' ELSE status_state END, status_error=CASE WHEN ?11 THEN '' ELSE status_error END,
                custom_domains=?8, subdomain=?9, host_header_rewrite=?10, http_opts=?12, proxy_protocol=?13, bandwidth_bps=?14, bandwidth_server=?15, lb_opts=?16 WHERE id=?1",
        params![id, k.name.trim(), k.local_ip, k.local_port, k.remote_port, i64::from(enabled), now, k.domains.join(","), k.subdomain, k.rewrite, runtime_changed, store_opts(&k.http), i64::from(k.proxy_protocol), i64::try_from(k.bandwidth_bps).unwrap_or(0), i64::from(k.bandwidth_server), store_lb(&k.lb)],
    )?;
    bump_revision(c, &cur.client_id)?;
    // 访问者只在目标隧道启用时才下发：启停会改变它们所在设备的配置。
    if enabled != cur.enabled {
        for client in visitor_clients(c, "tunnel_id", id)? {
            bump_revision(c, &client)?;
        }
    }
    get_tunnel(c, id)
}

/// 隧道的 http 选项（含 Basic 认证密码，仅供服务端内部下发 / 导出使用，不得经 REST 返回）。
pub fn tunnel_http_opts(c: &Connection, id: &str) -> Res<HttpOpts> {
    let raw: String = c.query_row(
        "SELECT http_opts FROM tunnels WHERE id = ?1 AND deleted_at IS NULL",
        [id],
        |r| r.get(0),
    )?;
    Ok(parse_opts(&raw))
}

/// 隧道的负载均衡 / 健康检查选项（含组密钥，仅供服务端内部下发 / 导出使用，不得经 REST 返回）。
pub fn tunnel_lb(c: &Connection, id: &str) -> Res<LbOpts> {
    let raw: String = c.query_row(
        "SELECT lb_opts FROM tunnels WHERE id = ?1 AND deleted_at IS NULL",
        [id],
        |r| r.get(0),
    )?;
    Ok(parse_lb(&raw))
}

/// 软删除隧道。
pub fn delete_tunnel(c: &Connection, id: &str, now: i64) -> Res<()> {
    let t = get_tunnel(c, id)?;
    c.execute(
        "UPDATE tunnels SET deleted_at = ?2, enabled = 0 WHERE id = ?1",
        params![id, now],
    )?;
    bump_revision(c, &t.client_id)?;
    // 指向这条隧道的访问者也一并移除，并让它们所在的设备更新配置。
    for client in visitor_clients(c, "tunnel_id", id)? {
        bump_revision(c, &client)?;
    }
    c.execute(
        "UPDATE visitors SET deleted_at = ?2, enabled = 0 WHERE tunnel_id = ?1 AND deleted_at IS NULL",
        params![id, now],
    )?;
    Ok(())
}

/// 指向某条隧道的访问者所在的设备（隧道启停 / 删除后需要重新下发它们的配置）。
pub fn tunnel_visitor_clients(c: &Connection, tunnel_id: &str) -> Res<Vec<String>> {
    visitor_clients(c, "tunnel_id", tunnel_id)
}

/// 受影响设备：按 `column`（`tunnel_id` / `client_id`）取未删除访问者所在的设备。
fn visitor_clients(c: &Connection, column: &str, value: &str) -> Res<Vec<String>> {
    let mut st = c.prepare(&format!(
        "SELECT DISTINCT client_id FROM visitors WHERE {column} = ?1 AND deleted_at IS NULL"
    ))?;
    let rows = st
        .query_map([value], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 记录 Edge 回报的运行状态（`state`：running / error / stopped）。
pub fn set_tunnel_status(c: &Connection, id: &str, state: &str, port: u16, error: &str) -> Res<()> {
    c.execute(
        "UPDATE tunnels SET status_state = ?2, status_port = ?3, status_error = ?4 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, state, i64::from(port), error],
    )?;
    Ok(())
}

/// 节点上报的代理状态：只接受属于该节点所服务设备的隧道。
pub fn set_tunnel_status_of_node(
    c: &Connection,
    node_id: &str,
    client_id: &str,
    tunnel_id: &str,
    state: &str,
    port: u16,
    error: &str,
) -> Res<()> {
    if !["running", "error", "stopped"].contains(&state) {
        return Err(StoreError::Invalid("invalid tunnel state".into()));
    }
    c.execute(
        "UPDATE tunnels SET status_state = ?5, status_port = ?6, status_error = ?7
          WHERE id = ?1 AND client_id = ?2 AND deleted_at IS NULL
            AND client_id IN (SELECT id FROM clients WHERE node_id = ?3 AND id = ?4)",
        params![
            tunnel_id,
            client_id,
            node_id,
            client_id,
            state,
            i64::from(port),
            error
        ],
    )?;
    Ok(())
}

/// 写入节点上报的流量样本：只接受属于该节点所服务设备的隧道（其余静默丢弃，不影响其他样本）。
pub fn persist_node_traffic(
    c: &mut Connection,
    node_id: &str,
    epoch_key: &str,
    samples: &[rustfrp_core::node::TrafficSample],
) -> Res<()> {
    let mut owned: Vec<rustfrp_agent::metrics::Sample> = Vec::new();
    for s in samples {
        let ok: Option<i64> = c
            .query_row(
                "SELECT 1 FROM tunnels t JOIN clients c ON c.id = t.client_id
                  WHERE t.id = ?1 AND c.node_id = ?2",
                params![s.name, node_id],
                |r| r.get(0),
            )
            .optional()?;
        if ok.is_some() {
            owned.push(crate::traffic::sample_from_wire(s));
        }
    }
    crate::traffic::persist(c, epoch_key, now_ms(), &owned)
}

/// 节点上报设备上下线：只更新属于该节点的设备；离线时同时把其隧道置为 stopped。
pub fn touch_client_of_node(
    c: &Connection,
    node_id: &str,
    client_id: &str,
    now: i64,
    offline: bool,
    ip: &str,
) -> Res<()> {
    let owned: Option<i64> = c
        .query_row(
            "SELECT 1 FROM clients WHERE id = ?1 AND node_id = ?2 AND deleted_at IS NULL",
            params![client_id, node_id],
            |r| r.get(0),
        )
        .optional()?;
    if owned.is_none() {
        return Ok(());
    }
    touch_client(c, client_id, now)?;
    if !offline {
        set_client_ip(c, client_id, ip)?;
    }
    if offline {
        mark_client_tunnels_stopped(c, client_id)?;
    }
    Ok(())
}

/// 设备下线后，把其隧道的运行状态置为 stopped。
pub fn mark_client_tunnels_stopped(c: &Connection, client_id: &str) -> Res<()> {
    c.execute(
        "UPDATE tunnels SET status_state = 'stopped', status_port = 0 WHERE client_id = ?1 AND deleted_at IS NULL AND status_state = 'running'",
        [client_id],
    )?;
    Ok(())
}

/// 生成下发给设备的受管配置：只含启用的隧道，代理名即隧道 ID。
pub fn managed_config(c: &Connection, client_id: &str) -> Res<ConfigSet> {
    let client = get_client(c, client_id)?;
    let mut opts = c.prepare(
        "SELECT id, http_opts, lb_opts FROM tunnels WHERE client_id = ?1 AND deleted_at IS NULL",
    )?;
    let mut opts: std::collections::HashMap<String, (HttpOpts, LbOpts)> = opts
        .query_map([client_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    parse_opts(&r.get::<_, String>(1)?),
                    parse_lb(&r.get::<_, String>(2)?),
                ),
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut secrets: std::collections::HashMap<String, String> = {
        let mut st = c.prepare(
            "SELECT id, secret_key FROM tunnels WHERE deleted_at IS NULL AND secret_key <> ''",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<_, _>>()?;
        rows
    };
    let visitors = list_visitors(c, Some(client_id))?
        .into_iter()
        .filter(|v| v.enabled)
        .filter_map(|v| {
            let t = get_tunnel(c, &v.tunnel_id).ok().filter(|t| t.enabled)?;
            Some(VisitorSpec {
                name: v.name,
                kind: t.kind,
                server_user: String::new(),
                server_name: t.id.clone(),
                sk: secrets.get(&t.id).cloned().unwrap_or_default(),
                bind_addr: v.bind_addr,
                bind_port: v.bind_port,
            })
        })
        .collect();
    let proxies = list_tunnels(c, Some(client_id))?
        .into_iter()
        .filter(|t| t.enabled)
        .map(|t| {
            let (http, lb) = opts.remove(&t.id).unwrap_or_default();
            ManagedProxy {
                http,
                lb,
                secret: SecretOpts {
                    sk: secrets.remove(&t.id).unwrap_or_default(),
                    allow_users: vec![],
                },
                proxy_protocol: t.proxy_protocol,
                bandwidth_bps: t.bandwidth_bps,
                bandwidth_server: t.bandwidth_server,
                name: t.id,
                kind: t.kind,
                remote_port: t.remote_port,
                local_ip: t.local_ip,
                local_port: t.local_port,
                custom_domains: t.custom_domains,
                subdomain: t.subdomain,
                host_header_rewrite: t.host_header_rewrite,
            }
        })
        .collect();
    let restart_seq: i64 = c.query_row(
        "SELECT restart_seq FROM clients WHERE id = ?1",
        [client_id],
        |r| r.get(0),
    )?;
    Ok(ConfigSet {
        restart_seq: u64::try_from(restart_seq).unwrap_or(0),
        revision: client.revision as u64,
        proxies,
        visitors,
    })
}

// ───────────── 访问者 ─────────────

const VISITOR_COLS: &str =
    "id, client_id, name, tunnel_id, bind_addr, bind_port, enabled, created_at, updated_at";

fn visitor_from(r: &Row<'_>) -> rusqlite::Result<Visitor> {
    Ok(Visitor {
        id: r.get(0)?,
        client_id: r.get(1)?,
        name: r.get(2)?,
        tunnel_id: r.get(3)?,
        bind_addr: r.get(4)?,
        bind_port: u16::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
        enabled: r.get::<_, i64>(6)? != 0,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// 新建访问者的参数。
#[derive(Debug, Clone, Default)]
pub struct NewVisitor {
    /// 所属设备。
    pub client_id: String,
    /// 名称。
    pub name: String,
    /// 目标隧道。
    pub tunnel_id: String,
    /// 本机监听地址（空 = 127.0.0.1）。
    pub bind_addr: String,
    /// 本机监听端口。
    pub bind_port: i64,
    /// 是否启用。
    pub enabled: bool,
}

/// 修改访问者的参数（`None` 表示不改）。
#[derive(Debug, Clone, Default)]
pub struct VisitorPatch {
    /// 名称。
    pub name: Option<String>,
    /// 目标隧道。
    pub tunnel_id: Option<String>,
    /// 本机监听地址。
    pub bind_addr: Option<String>,
    /// 本机监听端口。
    pub bind_port: Option<i64>,
    /// 是否启用。
    pub enabled: Option<bool>,
}

/// 取访问者。
pub fn get_visitor(c: &Connection, id: &str) -> Res<Visitor> {
    Ok(c.query_row(
        &format!("SELECT {VISITOR_COLS} FROM visitors WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        visitor_from,
    )?)
}

/// 访问者列表（可按设备过滤）。
pub fn list_visitors(c: &Connection, client_id: Option<&str>) -> Res<Vec<Visitor>> {
    let mut st = c.prepare(&format!(
        "SELECT {VISITOR_COLS} FROM visitors WHERE deleted_at IS NULL AND (?1 IS NULL OR client_id = ?1) ORDER BY created_at, name"
    ))?;
    let rows = st
        .query_map([client_id], visitor_from)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 校验访问者：目标必须是同一节点上的启用中的 stcp / sudp 隧道，名称与监听端口在设备内不冲突。
fn check_visitor(
    c: &Connection,
    client_id: &str,
    v: &NewVisitor,
    except_id: Option<&str>,
) -> Res<String> {
    check_name(&v.name, "name")?;
    let bind_addr = if v.bind_addr.trim().is_empty() {
        "127.0.0.1".to_string()
    } else {
        v.bind_addr.trim().to_string()
    };
    check_host(&bind_addr)?;
    check_port(v.bind_port, "bind_port", true)?;
    let tunnel = get_tunnel(c, &v.tunnel_id).map_err(|e| match e {
        StoreError::NotFound => StoreError::Invalid("target tunnel does not exist".into()),
        other => other,
    })?;
    if !is_secret(&tunnel.kind) {
        return Err(StoreError::Invalid(
            "visitors can only target stcp or sudp tunnels".into(),
        ));
    }
    let here = get_client(c, client_id)?;
    let there = get_client(c, &tunnel.client_id)?;
    if here.node_id != there.node_id {
        return Err(StoreError::Invalid(
            "the visitor and the target tunnel must be served by the same node".into(),
        ));
    }
    let clash: Option<(String, i64)> = c
        .query_row(
            "SELECT name, bind_port FROM visitors WHERE client_id = ?1 AND deleted_at IS NULL AND (?4 IS NULL OR id <> ?4)
               AND (name = ?2 OR (bind_addr = ?5 AND ?3 <> 0 AND bind_port = ?3))",
            params![client_id, v.name.trim(), v.bind_port, except_id, bind_addr],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((name, _)) = clash {
        return Err(StoreError::Conflict(format!(
            "visitor \"{name}\" already uses this name or listening port on the client"
        )));
    }
    Ok(bind_addr)
}

/// 创建访问者。
pub fn create_visitor(c: &Connection, v: &NewVisitor, now: i64) -> Res<Visitor> {
    get_client(c, &v.client_id)?;
    let bind_addr = check_visitor(c, &v.client_id, v, None)?;
    let id = new_id();
    c.execute(
        "INSERT INTO visitors (id, client_id, name, tunnel_id, bind_addr, bind_port, enabled, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
        params![id, v.client_id, v.name.trim(), v.tunnel_id, bind_addr, v.bind_port, i64::from(v.enabled), now],
    )?;
    bump_revision(c, &v.client_id)?;
    get_visitor(c, &id)
}

/// 修改访问者。
pub fn update_visitor(c: &Connection, id: &str, p: &VisitorPatch, now: i64) -> Res<Visitor> {
    let cur = get_visitor(c, id)?;
    let next = NewVisitor {
        client_id: cur.client_id.clone(),
        name: p.name.clone().unwrap_or_else(|| cur.name.clone()),
        tunnel_id: p.tunnel_id.clone().unwrap_or_else(|| cur.tunnel_id.clone()),
        bind_addr: p.bind_addr.clone().unwrap_or_else(|| cur.bind_addr.clone()),
        bind_port: p.bind_port.unwrap_or(i64::from(cur.bind_port)),
        enabled: p.enabled.unwrap_or(cur.enabled),
    };
    let bind_addr = check_visitor(c, &cur.client_id, &next, Some(id))?;
    c.execute(
        "UPDATE visitors SET name=?2, tunnel_id=?3, bind_addr=?4, bind_port=?5, enabled=?6, updated_at=?7 WHERE id=?1",
        params![id, next.name.trim(), next.tunnel_id, bind_addr, next.bind_port, i64::from(next.enabled), now],
    )?;
    bump_revision(c, &cur.client_id)?;
    get_visitor(c, id)
}

/// 软删除访问者。
pub fn delete_visitor(c: &Connection, id: &str, now: i64) -> Res<()> {
    let v = get_visitor(c, id)?;
    c.execute(
        "UPDATE visitors SET deleted_at = ?2, enabled = 0 WHERE id = ?1",
        params![id, now],
    )?;
    bump_revision(c, &v.client_id)?;
    Ok(())
}

// ───────────── 审计与统计 ─────────────

/// 写一条审计。
#[allow(clippy::too_many_arguments)]
pub fn audit(
    c: &Connection,
    actor: &str,
    action: &str,
    target_kind: &str,
    target_id: &str,
    reason: &str,
    result: &str,
    ip: &str,
    now: i64,
) -> Res<()> {
    c.execute(
        "INSERT INTO audit_log (ts, actor, action, target_kind, target_id, reason, result, ip) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![now, actor, action, target_kind, target_id, reason, result, ip],
    )?;
    Ok(())
}

/// 审计列表（新的在前），`before` 为上一页最后一条的 id。
pub fn list_audit(c: &Connection, limit: i64, before: Option<i64>) -> Res<Vec<AuditEntry>> {
    let limit = limit.clamp(1, 500);
    let mut st = c.prepare(
        "SELECT id, ts, actor, action, target_kind, target_id, reason, result, ip FROM audit_log
          WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = st
        .query_map(params![before, limit], |r| {
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                action: r.get(3)?,
                target_kind: r.get(4)?,
                target_id: r.get(5)?,
                reason: r.get(6)?,
                result: r.get(7)?,
                ip: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 概览计数。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    /// 设备总数。
    pub clients: i64,
    /// 隧道总数。
    pub tunnels: i64,
    /// 运行中的隧道。
    pub tunnels_running: i64,
    /// 待使用的接入令牌。
    pub tokens_pending: i64,
}

/// 统计概览所需的计数。
pub fn counts(c: &Connection, now: i64) -> Res<Counts> {
    let one = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> Res<i64> {
        Ok(c.query_row(sql, p, |r| r.get(0))?)
    };
    Ok(Counts {
        clients: one("SELECT count(*) FROM clients WHERE deleted_at IS NULL", &[])?,
        tunnels: one("SELECT count(*) FROM tunnels WHERE deleted_at IS NULL", &[])?,
        tunnels_running: one("SELECT count(*) FROM tunnels WHERE deleted_at IS NULL AND status_state = 'running'", &[])?,
        tokens_pending: one(
            "SELECT count(*) FROM enroll_tokens WHERE used_at IS NULL AND revoked_at IS NULL AND expires_at > ?1",
            &[&now],
        )?,
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::db::migrate;

    const PK: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&mut c).unwrap();
        c
    }

    fn client(c: &Connection, name: &str) -> Client {
        create_client(c, &new_id(), name, PK, "linux", "x86_64", "0.1", 1000).unwrap()
    }

    fn nt(client_id: &str, name: &str, remote: i64) -> NewTunnel {
        NewTunnel {
            client_id: client_id.into(),
            name: name.into(),
            kind: "tcp".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 22,
            remote_port: remote,
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn ids_and_time() {
        assert_eq!(new_id().len(), 16);
        assert_ne!(new_id(), new_id());
        assert!(now_ms() > 1_600_000_000_000);
    }

    #[test]
    fn admins() {
        let c = conn();
        let a = create_admin(&c, " root ", "hash", Role::Owner, 5).unwrap();
        assert_eq!(
            (a.name.as_str(), a.role, a.created_at),
            ("root", Role::Owner, 5)
        );
        assert!(
            matches!(create_admin(&c, "root", "h", Role::Viewer, 6), Err(StoreError::Conflict(m)) if m.contains("already exists"))
        );
        for bad in ["", "   ", &"x".repeat(65), "a\nb"] {
            assert!(
                matches!(
                    create_admin(&c, bad, "h", Role::Viewer, 1),
                    Err(StoreError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        let (found, hash) = find_admin_with_hash(&c, "root").unwrap();
        assert_eq!((found.id.as_str(), hash.as_str()), (a.id.as_str(), "hash"));
        assert_eq!(
            find_admin_with_hash(&c, "nobody"),
            Err(StoreError::NotFound)
        );
        assert_eq!(get_admin(&c, &a.id).unwrap(), a);
        create_admin(&c, "viewer1", "h", Role::Viewer, 7).unwrap();
        assert_eq!(list_admins(&c).unwrap().len(), 2);

        set_admin_password(&c, "root", "newhash").unwrap();
        assert_eq!(find_admin_with_hash(&c, "root").unwrap().1, "newhash");
        assert_eq!(
            set_admin_password(&c, "nobody", "x"),
            Err(StoreError::NotFound)
        );
        assert_eq!(
            set_admin_disabled(&c, "nobody", true, 1),
            Err(StoreError::NotFound)
        );
        set_admin_disabled(&c, "root", true, 99).unwrap();
        assert_eq!(get_admin(&c, &a.id).unwrap().disabled_at, Some(99));
        set_admin_disabled(&c, "root", false, 100).unwrap();
        assert_eq!(get_admin(&c, &a.id).unwrap().disabled_at, None);
    }

    #[test]
    fn http_options_validate_coexist_by_location_and_never_leak_the_password() {
        let c = conn();
        let a = client(&c, "a");
        let web = |name: &str, http: HttpOpts| NewTunnel {
            client_id: a.id.clone(),
            name: name.into(),
            kind: "http".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 80,
            custom_domains: vec!["app.example.com".into()],
            enabled: true,
            http,
            ..Default::default()
        };
        let loc = |l: &[&str], user: &str| HttpOpts {
            locations: l.iter().map(ToString::to_string).collect(),
            route_by_http_user: user.into(),
            ..Default::default()
        };
        // 同域名：路径不重叠或 routeByHTTPUser 不同才能共存。
        let root = create_tunnel(&c, &web("root", HttpOpts::default()), 1).unwrap();
        create_tunnel(&c, &web("api", loc(&["/api", "/rpc"], "")), 2).unwrap();
        create_tunnel(&c, &web("bob", loc(&[], "bob")), 3).unwrap();
        for (name, http) in [
            ("dup-root", HttpOpts::default()),
            ("dup-api", loc(&["/x", "/api"], "")),
            ("dup-bob", loc(&["/"], "bob")),
        ] {
            assert!(
                matches!(create_tunnel(&c, &web(name, http), 4), Err(StoreError::Conflict(m)) if m.contains("already used")),
                "{name}"
            );
        }
        // 非法选项。
        let many: Vec<String> = (0..17).map(|i| format!("/p{i}")).collect();
        let headers = |n: usize| -> std::collections::BTreeMap<String, String> {
            (0..n)
                .map(|i| (format!("X-{i}"), "v".to_string()))
                .collect()
        };
        let bad = [
            HttpOpts {
                locations: many,
                ..Default::default()
            },
            HttpOpts {
                request_headers: headers(17),
                ..Default::default()
            },
            HttpOpts {
                locations: vec!["nope".into()],
                ..Default::default()
            },
            HttpOpts {
                locations: vec!["/a b".into()],
                ..Default::default()
            },
            HttpOpts {
                request_headers: [("Bad Name".to_string(), "v".to_string())].into(),
                ..Default::default()
            },
            HttpOpts {
                response_headers: headers(17),
                ..Default::default()
            },
            HttpOpts {
                response_headers: [("Bad Name".to_string(), "v".to_string())].into(),
                ..Default::default()
            },
            HttpOpts {
                response_headers: [("Content-Length".to_string(), "1".to_string())].into(),
                ..Default::default()
            },
            HttpOpts {
                http_password: "p".repeat(129),
                ..Default::default()
            },
            HttpOpts {
                http_user: "a:b".into(),
                ..Default::default()
            },
            HttpOpts {
                route_by_http_user: "x\n".into(),
                ..Default::default()
            },
        ];
        for (i, h) in bad.into_iter().enumerate() {
            assert!(
                matches!(
                    create_tunnel(&c, &web(&format!("bad{i}"), h), 5),
                    Err(StoreError::Invalid(_))
                ),
                "{i}"
            );
        }
        // 非 http 隧道不能带这些选项。
        let mut tcp = nt(&a.id, "plain", 6200);
        tcp.http = HttpOpts {
            http_user: "u".into(),
            ..Default::default()
        };
        assert!(
            matches!(create_tunnel(&c, &tcp, 6), Err(StoreError::Invalid(m)) if m.contains("only apply to http"))
        );

        // 带认证的隧道：Tunnel 视图只有「是否设置密码」；下发配置里有完整选项。
        let secured = create_tunnel(
            &c,
            &NewTunnel {
                custom_domains: vec!["sec.example.com".into()],
                http: HttpOpts {
                    http_user: "alice".into(),
                    http_password: "s3cret".into(),
                    request_headers: [("X-Env".to_string(), "prod".to_string())].into(),
                    ..Default::default()
                },
                ..web("sec", HttpOpts::default())
            },
            7,
        )
        .unwrap();
        assert_eq!(
            (secured.http_user.as_str(), secured.http_password_set),
            ("alice", true)
        );
        assert!(!serde_json::to_string(&secured).unwrap().contains("s3cret"));
        let managed = managed_config(&c, &a.id).unwrap();
        let p = managed
            .proxies
            .iter()
            .find(|p| p.name == secured.id)
            .unwrap();
        assert_eq!(p.http.http_password, "s3cret");
        assert_eq!(
            p.http.request_headers.get("X-Env").map(String::as_str),
            Some("prod")
        );
        assert!(managed
            .proxies
            .iter()
            .find(|p| p.name == root.id)
            .unwrap()
            .http
            .is_default());

        // 修改：不带密码字段保持原密码；空串清除；运行态因此回到 pending。
        set_tunnel_status(&c, &secured.id, "running", 80, "").unwrap();
        let kept = update_tunnel(
            &c,
            &secured.id,
            &TunnelPatch {
                http_user: Some("bob".into()),
                ..Default::default()
            },
            8,
        )
        .unwrap();
        assert_eq!(
            (
                kept.http_user.as_str(),
                kept.http_password_set,
                kept.state.as_str()
            ),
            ("bob", true, "pending")
        );
        assert_eq!(
            tunnel_http_opts(&c, &secured.id).unwrap().http_password,
            "s3cret"
        );
        let cleared = update_tunnel(
            &c,
            &secured.id,
            &TunnelPatch {
                http_password: Some(String::new()),
                ..Default::default()
            },
            9,
        )
        .unwrap();
        assert!(!cleared.http_password_set);
        set_tunnel_status(&c, &secured.id, "running", 80, "").unwrap();
        let same = update_tunnel(
            &c,
            &secured.id,
            &TunnelPatch {
                name: Some("renamed".into()),
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(
            same.state, "running",
            "renaming does not restart the data plane"
        );
        for patch in [
            TunnelPatch {
                locations: Some(vec!["/z".into()]),
                ..Default::default()
            },
            TunnelPatch {
                route_by_http_user: Some("carol".into()),
                ..Default::default()
            },
            TunnelPatch {
                request_headers: Some(Default::default()),
                ..Default::default()
            },
        ] {
            assert!(update_tunnel(&c, &secured.id, &patch, 11).is_ok());
        }
    }

    #[test]
    fn nodes_tokens_and_device_assignment() {
        let c = conn();
        let key = "ab".repeat(32);
        let info = NodeInfo {
            public_host: "edge.example.com".into(),
            edge_port: 7000,
            tls_fingerprint: "f".repeat(64),
            vhost_http: Some(80),
            vhost_https: None,
            subdomain_host: "t.example.com".into(),
            allow_ports: vec![(6000, 6100)],
            version: "0.1".into(),
            os: "linux".into(),
        };
        assert!(matches!(
            create_node(&c, "n1", "x", "zz", &info, 1),
            Err(StoreError::Invalid(_))
        ));
        let n = create_node(&c, "n1", " edge ", &key, &info, 5).unwrap();
        assert_eq!(
            (
                n.name.as_str(),
                n.public_host.as_str(),
                n.edge_port,
                n.vhost_http,
                n.vhost_https
            ),
            ("edge", "edge.example.com", 7000, Some(80), None)
        );
        assert_eq!(
            (n.allow_ports.clone(), n.clients, n.builtin, n.online),
            (vec![(6000, 6100)], 0, false, false)
        );
        assert!(matches!(
            create_node(&c, "n1", "dup", &key, &info, 6),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(get_node_with_key(&c, "n1").unwrap().1, key);
        // 重新上线时空的 public_host 不覆盖管理员/先前设定的值。
        let mut again = info.clone();
        again.public_host = String::new();
        again.edge_port = 7100;
        update_node_info(&c, "n1", &again, 9).unwrap();
        let n = get_node(&c, "n1").unwrap();
        assert_eq!(
            (n.public_host.as_str(), n.edge_port, n.last_seen_at),
            ("edge.example.com", 7100, Some(9))
        );
        touch_node(&c, "n1", 12).unwrap();
        assert_eq!(get_node(&c, "n1").unwrap().last_seen_at, Some(12));
        assert_eq!(list_nodes(&c).unwrap().len(), 1);

        // 节点令牌与设备令牌互不通用；设备令牌可指定节点。
        let node_tok = create_token_for(&c, "h-node", "node", "root", 1, 1000, "node", "").unwrap();
        assert_eq!(
            (node_tok.kind.as_str(), node_tok.node_id.as_str()),
            ("node", "")
        );
        assert!(create_token_for(&c, "h2", "x", "root", 1, 1000, "node", "n1").is_err());
        assert!(create_token_for(&c, "h3", "x", "root", 1, 1000, "bogus", "").is_err());
        assert!(matches!(
            create_token_for(&c, "h4", "x", "root", 1, 1000, "client", "missing"),
            Err(StoreError::NotFound)
        ));
        let ct = create_token_for(&c, "h-client", "c", "root", 1, 1000, "client", "n1").unwrap();
        assert_eq!(ct.node_id, "n1");
        assert!(
            matches!(
                consume_enroll_token(&c, "h-node", "x", 2),
                Err(StoreError::NotFound)
            ),
            "node token cannot enroll a device"
        );
        assert!(matches!(
            consume_token_of(&c, "h-client", "x", 2, "node"),
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            consume_token_of(&c, "h-node", "n1", 2, "node")
                .unwrap()
                .client_id
                .as_deref(),
            Some("n1")
        );
        assert!(
            matches!(
                consume_token_of(&c, "h-node", "n1", 3, "node"),
                Err(StoreError::NotFound)
            ),
            "single use"
        );

        // 设备指派、节点设备清单、删除保护。
        let a = client(&c, "a");
        let b = client(&c, "b");
        assert_eq!(a.node_id, "");
        assert!(matches!(
            set_client_node(&c, &a.id, "missing"),
            Err(StoreError::NotFound)
        ));
        set_client_node(&c, &a.id, "n1").unwrap();
        set_client_node(&c, &b.id, "n1").unwrap();
        create_tunnel(&c, &nt(&a.id, "ssh", 6000), 1).unwrap();
        let devices = list_node_devices(&c, "n1").unwrap();
        assert_eq!(devices.len(), 2);
        let da = devices.iter().find(|d| d.id == a.id).unwrap();
        assert_eq!(
            (da.config.proxies.len(), da.disabled, da.pubkey.len()),
            (1, false, 64)
        );
        assert_eq!(get_node(&c, "n1").unwrap().clients, 2);
        assert!(
            matches!(delete_node(&c, "n1", 20), Err(StoreError::Conflict(m)) if m.contains("2 client"))
        );
        update_client(&c, &b.id, None, Some(true), 21).unwrap();
        assert_eq!(
            list_node_devices(&c, "n1").unwrap().len(),
            1,
            "disabled devices are not sent"
        );
        assert!(node_device(&c, &b.id).unwrap().disabled);
        delete_client(&c, &b.id, 22).unwrap();
        assert!(matches!(node_device(&c, &b.id), Err(StoreError::NotFound)));
        assert_eq!(
            client_node_id_any(&c, &b.id).unwrap(),
            "n1",
            "deleted devices still tell which node to notify"
        );
        delete_client(&c, &a.id, 23).unwrap();
        // 改名 / 停用 / 删除。
        let n = update_node(&c, "n1", Some("renamed"), Some(true), 30).unwrap();
        assert_eq!((n.name.as_str(), n.disabled_at), ("renamed", Some(30)));
        assert_eq!(
            update_node(&c, "n1", None, Some(false), 31)
                .unwrap()
                .disabled_at,
            None
        );
        assert!(update_node(&c, "n1", Some(""), None, 32).is_err());
        assert_eq!(
            update_node(&c, "n1", Some("only-name"), None, 33)
                .unwrap()
                .name,
            "only-name"
        );
        set_client_node(&c, &a.id, "").unwrap();
        assert_eq!(client_node_id_any(&c, &a.id).unwrap(), "");
        delete_node(&c, "n1", 40).unwrap();
        assert!(matches!(get_node(&c, "n1"), Err(StoreError::NotFound)));
        assert!(list_nodes(&c).unwrap().is_empty());
        assert!(matches!(
            delete_node(&c, "n1", 41),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn proxy_protocol_is_validated_persisted_and_shipped_to_devices() {
        let c = conn();
        let a = client(&c, "a");
        let mut t = nt(&a.id, "ssh", 6000);
        t.proxy_protocol = 2;
        let created = create_tunnel(&c, &t, 1).unwrap();
        assert_eq!(created.proxy_protocol, 2);
        let managed = managed_config(&c, &a.id).unwrap();
        assert_eq!(managed.proxies[0].proxy_protocol, 2);
        // 修改：缺省保持；显式改成 0 / 1。
        let kept = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                name: Some("ssh2".into()),
                ..Default::default()
            },
            2,
        )
        .unwrap();
        assert_eq!(kept.proxy_protocol, 2);
        set_tunnel_status(&c, &created.id, "running", 6000, "").unwrap();
        let same = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                proxy_protocol: Some(2),
                ..Default::default()
            },
            3,
        )
        .unwrap();
        assert_eq!(
            same.state, "running",
            "unchanged value does not restart the data plane"
        );
        let changed = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                proxy_protocol: Some(1),
                ..Default::default()
            },
            4,
        )
        .unwrap();
        assert_eq!(
            (changed.proxy_protocol, changed.state.as_str()),
            (1, "pending")
        );
        // 非法：版本号超范围；udp 不支持。
        for (kind, v) in [("tcp", 3u8), ("udp", 1u8)] {
            let mut bad = nt(&a.id, "bad", 0);
            bad.kind = kind.into();
            bad.proxy_protocol = v;
            assert!(
                matches!(create_tunnel(&c, &bad, 5), Err(StoreError::Invalid(m)) if m.contains("proxy_protocol")),
                "{kind}"
            );
        }
    }

    #[test]
    fn bandwidth_limit_is_validated_persisted_and_shipped_to_devices() {
        let c = conn();
        let a = client(&c, "a");
        let mut t = nt(&a.id, "dl", 6000);
        t.bandwidth_bps = 512 * 1024;
        t.bandwidth_server = true;
        let created = create_tunnel(&c, &t, 1).unwrap();
        assert_eq!(
            (created.bandwidth_bps, created.bandwidth_server),
            (524_288, true)
        );
        let m = managed_config(&c, &a.id).unwrap();
        assert_eq!(
            (m.proxies[0].bandwidth_bps, m.proxies[0].bandwidth_server),
            (524_288, true)
        );
        // 缺省保持；显式 0 取消限速。
        let kept = update_tunnel(&c, &created.id, &TunnelPatch::default(), 2).unwrap();
        assert_eq!(kept.bandwidth_bps, 524_288);
        let off = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                bandwidth_bps: Some(0),
                bandwidth_server: Some(false),
                ..Default::default()
            },
            3,
        )
        .unwrap();
        assert_eq!((off.bandwidth_bps, off.bandwidth_server), (0, false));
        for (kind, v) in [("tcp", MAX_BANDWIDTH + 1), ("udp", 1)] {
            let mut bad = nt(&a.id, "bad", 0);
            bad.kind = kind.into();
            bad.bandwidth_bps = v;
            assert!(
                matches!(create_tunnel(&c, &bad, 4), Err(StoreError::Invalid(m)) if m.contains("bandwidth")),
                "{kind}"
            );
        }
    }

    fn lb(group: &str, key: &str) -> LbOpts {
        LbOpts {
            group: group.into(),
            group_key: key.into(),
            health: None,
        }
    }

    #[test]
    fn lb_groups_share_ports_and_domains_only_with_matching_key_and_shape() {
        let c = conn();
        let a = client(&c, "a");
        let b = client(&c, "b");
        let mut first = nt(&a.id, "one", 6000);
        first.lb = lb("g", "k");
        let one = create_tunnel(&c, &first, 1).unwrap();
        assert_eq!((one.lb_group.as_str(), one.lb_group_key_set), ("g", true));
        // 同组同密钥同端口：允许（跨设备）。
        let mut second = nt(&b.id, "two", 6000);
        second.lb = lb("g", "k");
        create_tunnel(&c, &second, 2).unwrap();
        // 端口被非组成员 / 组外占用：冲突；同组端口不一致、密钥不同：拒绝。
        let plain = nt(&b.id, "plain", 6000);
        assert!(
            matches!(create_tunnel(&c, &plain, 3), Err(StoreError::Conflict(m)) if m.contains("already used"))
        );
        let mut other_port = nt(&b.id, "port", 6001);
        other_port.lb = lb("g", "k");
        assert!(
            matches!(create_tunnel(&c, &other_port, 3), Err(StoreError::Invalid(m)) if m.contains("same remote port"))
        );
        let mut wrong_key = nt(&b.id, "key", 6000);
        wrong_key.lb = lb("g", "zzz");
        assert!(
            matches!(create_tunnel(&c, &wrong_key, 3), Err(StoreError::Conflict(m)) if m.contains("key doesn't match"))
        );
        // 组内已有自动分配端口（0）的成员时，显式端口的成员也可加入。
        let mut auto0 = nt(&a.id, "auto0", 0);
        auto0.lb = lb("g2", "k");
        create_tunnel(&c, &auto0, 4).unwrap();
        let mut explicit = nt(&b.id, "explicit", 6100);
        explicit.lb = lb("g2", "k");
        create_tunnel(&c, &explicit, 4).unwrap();
        // 自动分配端口的成员可加入。
        let mut auto = nt(&b.id, "auto", 0);
        auto.lb = lb("g", "k");
        create_tunnel(&c, &auto, 4).unwrap();
        // 下发的配置带着组密钥。
        let managed = managed_config(&c, &a.id).unwrap();
        assert_eq!(managed.proxies[0].lb, lb("g", "k"));

        // 域名：同组且路径集合一致可共用；路径不同或密钥不同冲突。
        let web = |client: &str, name: &str, group: &str, key: &str, locs: &[&str]| NewTunnel {
            client_id: client.into(),
            name: name.into(),
            kind: "http".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 80,
            custom_domains: vec!["lb.example.com".into()],
            enabled: true,
            http: HttpOpts {
                locations: locs.iter().map(|l| l.to_string()).collect(),
                ..Default::default()
            },
            lb: lb(group, key),
            ..Default::default()
        };
        create_tunnel(&c, &web(&a.id, "w1", "web", "k", &["/a", "/b"]), 5).unwrap();
        create_tunnel(&c, &web(&b.id, "w2", "web", "k", &["/b", "/a"]), 6).unwrap();
        for (name, g, k, locs) in [
            ("w3", "web", "k", vec!["/a"]),
            ("w4", "web", "bad", vec!["/a", "/b"]),
            ("w5", "", "", vec!["/a", "/b"]),
        ] {
            assert!(
                matches!(
                    create_tunnel(&c, &web(&b.id, name, g, k, &locs), 7),
                    Err(StoreError::Conflict(_))
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn lb_options_are_validated_updated_and_clearable() {
        let c = conn();
        let a = client(&c, "a");
        let health = |kind: &str, path: &str| HealthSpec {
            kind: kind.into(),
            timeout_secs: 3,
            max_failed: 1,
            interval_secs: 10,
            path: path.into(),
            headers: Default::default(),
        };
        let mut t = nt(&a.id, "hc", 6000);
        t.lb = LbOpts {
            health: Some(health("tcp", "")),
            ..Default::default()
        };
        let created = create_tunnel(&c, &t, 1).unwrap();
        assert_eq!(
            created.health_check.as_ref().map(|h| h.kind.as_str()),
            Some("tcp")
        );
        // 非法：类型、数值范围、路径、udp、组密钥缺组名、过长。
        let bad = |kind: &str, lb: LbOpts| {
            let mut x = nt(&a.id, "bad", 0);
            x.kind = kind.into();
            x.lb = lb;
            matches!(create_tunnel(&c, &x, 2), Err(StoreError::Invalid(_)))
        };
        assert!(bad(
            "tcp",
            LbOpts {
                health: Some(health("icmp", "")),
                ..Default::default()
            }
        ));
        assert!(bad(
            "tcp",
            LbOpts {
                health: Some(health("http", "x")),
                ..Default::default()
            }
        ));
        assert!(bad(
            "tcp",
            LbOpts {
                health: Some(health("tcp", "/p")),
                ..Default::default()
            }
        ));
        assert!(bad(
            "tcp",
            LbOpts {
                health: Some(HealthSpec {
                    timeout_secs: 0,
                    ..health("tcp", "")
                }),
                ..Default::default()
            }
        ));
        assert!(bad(
            "tcp",
            LbOpts {
                health: Some(HealthSpec {
                    headers: [("Bad Name".to_string(), "v".to_string())].into(),
                    ..health("http", "/")
                }),
                ..Default::default()
            }
        ));
        assert!(bad("udp", lb("g", "k")));
        assert!(bad("tcp", lb("", "k")));
        assert!(bad("tcp", lb(&"g".repeat(65), "")));
        assert!(bad("tcp", lb("g\n", "")));
        assert!(bad("tcp", lb("g", "k\n")));
        let with = |h: HealthSpec| LbOpts {
            health: Some(h),
            ..Default::default()
        };
        for broken in [
            HealthSpec {
                max_failed: 21,
                ..health("tcp", "")
            },
            HealthSpec {
                max_failed: 0,
                ..health("tcp", "")
            },
            HealthSpec {
                interval_secs: 0,
                ..health("tcp", "")
            },
            HealthSpec {
                interval_secs: 3601,
                ..health("tcp", "")
            },
            HealthSpec {
                timeout_secs: 61,
                ..health("tcp", "")
            },
            health("http", "/a b"),
            health("http", &format!("/{}", "a".repeat(300))),
            HealthSpec {
                headers: [("X".to_string(), "1".to_string())].into(),
                ..health("tcp", "")
            },
            HealthSpec {
                headers: (0..9)
                    .map(|i| (format!("X-{i}"), "v".to_string()))
                    .collect(),
                ..health("http", "/")
            },
            HealthSpec {
                headers: [("X".to_string(), "v".repeat(300))].into(),
                ..health("http", "/")
            },
        ] {
            assert!(bad("tcp", with(broken.clone())), "{broken:?}");
        }
        let ok_http = HealthSpec {
            headers: [("X-A".to_string(), "1".to_string())].into(),
            ..health("http", "/h")
        };
        let mut good = nt(&a.id, "good-http", 0);
        good.lb = with(ok_http);
        create_tunnel(&c, &good, 2).unwrap();
        // 修改：缺省保持；改组；空 kind 取消检查；数据面变化让状态回到 pending。
        set_tunnel_status(&c, &created.id, "running", 6000, "").unwrap();
        let kept = update_tunnel(&c, &created.id, &TunnelPatch::default(), 3).unwrap();
        assert_eq!(
            (kept.health_check.is_some(), kept.state.as_str()),
            (true, "running")
        );
        let grouped = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                lb_group: Some("g".into()),
                lb_group_key: Some("k".into()),
                ..Default::default()
            },
            4,
        )
        .unwrap();
        assert_eq!(
            (grouped.lb_group.as_str(), grouped.state.as_str()),
            ("g", "pending")
        );
        assert_eq!(tunnel_lb(&c, &created.id).unwrap().group_key, "k");
        let cleared = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                lb_group: Some(String::new()),
                lb_group_key: Some(String::new()),
                health_check: Some(HealthSpec::default()),
                ..Default::default()
            },
            5,
        )
        .unwrap();
        assert!(
            cleared.lb_group.is_empty()
                && cleared.health_check.is_none()
                && !cleared.lb_group_key_set
        );
    }

    #[test]
    fn visitors_must_share_a_node_with_their_target_and_health_can_be_replaced() {
        let c = conn();
        let a = client(&c, "a");
        let b = client(&c, "b");
        let mut t = nt(&a.id, "ssh", 0);
        t.kind = "stcp".into();
        let tunnel = create_tunnel(&c, &t, 1).unwrap();
        let key = rustfrp_core::identity::DeviceKey::generate()
            .unwrap()
            .public_hex();
        let info = NodeInfo {
            public_host: "edge.example.com".into(),
            edge_port: 7000,
            tls_fingerprint: "f".repeat(64),
            vhost_http: None,
            vhost_https: None,
            subdomain_host: String::new(),
            allow_ports: vec![],
            version: "0.1".into(),
            os: "linux".into(),
        };
        create_node(&c, "n1", "edge", &key, &info, 5).unwrap();
        set_client_node(&c, &b.id, "n1").unwrap();
        let v = NewVisitor {
            client_id: b.id.clone(),
            name: "v".into(),
            tunnel_id: tunnel.id.clone(),
            enabled: true,
            ..Default::default()
        };
        assert!(
            matches!(create_visitor(&c, &v, 6), Err(StoreError::Invalid(m)) if m.contains("same node"))
        );
        // 同一节点后可以创建。
        set_client_node(&c, &a.id, "n1").unwrap();
        create_visitor(&c, &v, 7).unwrap();

        // 健康检查：先配、再整体替换成另一种。
        let mut hc = nt(&a.id, "hc", 6000);
        hc.lb = LbOpts {
            health: Some(HealthSpec {
                kind: "tcp".into(),
                timeout_secs: 3,
                max_failed: 1,
                interval_secs: 10,
                ..Default::default()
            }),
            ..Default::default()
        };
        let created = create_tunnel(&c, &hc, 8).unwrap();
        let replaced = update_tunnel(
            &c,
            &created.id,
            &TunnelPatch {
                health_check: Some(HealthSpec {
                    kind: "http".into(),
                    path: "/h".into(),
                    timeout_secs: 2,
                    max_failed: 2,
                    interval_secs: 5,
                    ..Default::default()
                }),
                ..Default::default()
            },
            9,
        )
        .unwrap();
        assert_eq!(
            replaced.health_check.map(|h| (h.kind, h.path)),
            Some(("http".to_string(), "/h".to_string()))
        );
    }

    #[test]
    fn client_tags_and_restart_requests() {
        let c = conn();
        let a = client(&c, "a");
        assert!(a.tags.is_empty());
        let t =
            set_client_tags(&c, &a.id, &[" prod ".into(), "web".into(), "prod".into()]).unwrap();
        assert_eq!(t.tags, vec!["prod".to_string(), "web".to_string()]);
        assert!(
            get_client(&c, &a.id).unwrap().tags.len() == 2
                && list_clients(&c).unwrap()[0].tags.len() == 2
        );
        for bad in [
            vec!["".to_string()],
            vec!["x".repeat(33)],
            vec!["a\nb".to_string()],
            (0..9).map(|i| format!("t{i}")).collect(),
        ] {
            assert!(
                matches!(
                    set_client_tags(&c, &a.id, &bad),
                    Err(StoreError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        assert!(set_client_tags(&c, &a.id, &[]).unwrap().tags.is_empty());
        assert!(matches!(
            set_client_tags(&c, "nope", &[]),
            Err(StoreError::NotFound)
        ));
        // 重启请求递增计数，并随配置下发、同时提升配置版本。
        let before = managed_config(&c, &a.id).unwrap();
        assert_eq!(before.restart_seq, 0);
        assert_eq!(request_client_restart(&c, &a.id).unwrap(), 1);
        assert_eq!(request_client_restart(&c, &a.id).unwrap(), 2);
        let after = managed_config(&c, &a.id).unwrap();
        assert_eq!(after.restart_seq, 2);
        assert!(after.revision > before.revision);
        assert!(matches!(
            request_client_restart(&c, "nope"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn domain_subdomain_and_rewrite_validation_boundaries() {
        let long_label = "a".repeat(64);
        let long_name = format!("{}.com", "a.".repeat(127));
        for bad in [
            "",
            "*.",
            "a..b",
            "a_b.com",
            "-a.com",
            "a-.com",
            long_label.as_str(),
            long_name.as_str(),
        ] {
            assert!(normalize_domains(&[bad.to_string()]).is_err(), "{bad}");
        }
        assert_eq!(
            normalize_domains(&[
                "*.Example.COM".into(),
                " a.b-c.io ".into(),
                "*.example.com".into()
            ])
            .unwrap(),
            vec!["*.example.com", "a.b-c.io"]
        );
        for bad in [long_label.as_str(), "a_b", "-a", "a-"] {
            assert!(check_subdomain(bad).is_err(), "{bad}");
        }
        assert!(check_subdomain("").is_ok() && check_subdomain("web-1").is_ok());
        for bad in ["a b", "a\tb", "a\u{7}b", "x".repeat(254).as_str()] {
            assert!(check_rewrite(bad).is_err(), "{bad:?}");
        }
        assert!(check_rewrite("inner.lan:8080").is_ok());
    }

    #[test]
    fn web_only_fields_are_rejected_on_other_tunnel_kinds() {
        let c = conn();
        let a = client(&c, "a");
        for field in 0..3 {
            let mut t = nt(&a.id, &format!("t{field}"), 6100 + field);
            match field {
                0 => t.custom_domains = vec!["a.example.com".into()],
                1 => t.subdomain = "web".into(),
                _ => t.host_header_rewrite = "inner".into(),
            }
            assert!(
                matches!(create_tunnel(&c, &t, 1), Err(StoreError::Invalid(m)) if m.contains("only apply to http")),
                "{field}"
            );
        }
    }

    #[test]
    fn tunnel_quota_counts_live_tunnels_only() {
        let c = conn();
        let cl = create_client(
            &c,
            &new_id(),
            "c",
            &"a".repeat(64),
            "linux",
            "x86_64",
            "0",
            1,
        )
        .unwrap();
        let mk = |name: &str, port: i64| NewTunnel {
            client_id: cl.id.clone(),
            name: name.into(),
            kind: "tcp".into(),
            local_ip: "127.0.0.1".into(),
            local_port: port,
            remote_port: 0,
            enabled: true,
            custom_domains: vec![],
            subdomain: String::new(),
            host_header_rewrite: String::new(),
            http: Default::default(),
            proxy_protocol: 0,
            bandwidth_bps: 0,
            bandwidth_server: false,
            lb: Default::default(),
        };
        let a = create_tunnel(&c, &mk("a", 1), 1).unwrap();
        assert!(enforce_tunnel_quota(&c, &cl.id, 1).is_ok());
        assert!(enforce_tunnel_quota(&c, &cl.id, 0).is_ok(), "0 = unlimited");
        create_tunnel(&c, &mk("b", 2), 2).unwrap();
        assert!(matches!(
            enforce_tunnel_quota(&c, &cl.id, 1),
            Err(StoreError::Conflict(m)) if m.contains("limit")
        ));
        delete_tunnel(&c, &a.id, 3).unwrap();
        assert!(enforce_tunnel_quota(&c, &cl.id, 1).is_ok());
    }

    #[test]
    fn update_admin_keeps_one_active_owner_and_revokes_sessions() {
        let c = conn();
        let o1 = create_admin(&c, "o1", "h", Role::Owner, 0).unwrap();
        let o2 = create_admin(&c, "o2", "h", Role::Owner, 0).unwrap();
        create_session(&c, "t1", &o1.id, "c", 0, 10_000).unwrap();
        // 还有另一个 owner 时可以降级，且会话被吊销。
        let d = update_admin(&c, &o1.id, Some(Role::Viewer), None, None, 5).unwrap();
        assert_eq!(d.role, Role::Viewer);
        assert!(matches!(
            find_session(&c, "t1", 1),
            Err(StoreError::NotFound)
        ));
        // 最后一个启用的 owner 不能降级或禁用。
        for (r, dis) in [(Some(Role::Admin), None), (None, Some(true))] {
            assert!(matches!(
                update_admin(&c, &o2.id, r, dis, None, 6),
                Err(StoreError::Conflict(m)) if m.contains("owner")
            ));
        }
        assert_eq!(get_admin(&c, &o2.id).unwrap().role, Role::Owner);
        // 非 owner 的启停与改密码；启用不吊销会话。
        update_admin(&c, &o1.id, None, Some(true), None, 7).unwrap();
        assert_eq!(get_admin(&c, &o1.id).unwrap().disabled_at, Some(7));
        create_session(&c, "t2", &o2.id, "c", 0, 10_000).unwrap();
        update_admin(&c, &o1.id, None, Some(false), None, 8).unwrap();
        assert!(find_session(&c, "t2", 1).is_ok());
        update_admin(&c, &o1.id, None, None, Some("new"), 9).unwrap();
        assert_eq!(find_admin_with_hash(&c, "o1").unwrap().1, "new");
        assert!(matches!(
            update_admin(&c, "missing", None, None, None, 1),
            Err(StoreError::NotFound)
        ));
        // 只改密码：即使是唯一启用的 owner 也允许。
        update_admin(&c, &o2.id, None, None, Some("pw"), 9).unwrap();
        assert_eq!(find_admin_with_hash(&c, "o2").unwrap().1, "pw");
        // 禁用的 owner 再启用 / 提升不受限制。
        update_admin(&c, &o1.id, Some(Role::Owner), None, None, 10).unwrap();
        assert!(update_admin(&c, &o2.id, Some(Role::Admin), None, None, 11).is_ok());
    }

    #[test]
    fn sessions_expire_and_die_with_the_admin() {
        let c = conn();
        let a = create_admin(&c, "root", "h", Role::Admin, 0).unwrap();
        create_session(&c, "th", &a.id, "csrf1", 1000, 500).unwrap();
        let (who, csrf) = find_session(&c, "th", 1499).unwrap();
        assert_eq!((who.name.as_str(), csrf.as_str()), ("root", "csrf1"));
        assert_eq!(find_session(&c, "th", 1500), Err(StoreError::NotFound));
        assert_eq!(find_session(&c, "nope", 1000), Err(StoreError::NotFound));
        assert_eq!(purge_expired_sessions(&c, 1500).unwrap(), 1);
        assert_eq!(purge_expired_sessions(&c, 1500).unwrap(), 0);

        create_session(&c, "t2", &a.id, "c", 1000, 10_000).unwrap();
        delete_session(&c, "t2").unwrap();
        assert_eq!(find_session(&c, "t2", 1001), Err(StoreError::NotFound));

        // 禁用管理员 / 改密码 → 会话立即失效。
        create_session(&c, "t3", &a.id, "c", 1000, 10_000).unwrap();
        set_admin_disabled(&c, "root", true, 1001).unwrap();
        assert_eq!(find_session(&c, "t3", 1002), Err(StoreError::NotFound));
        set_admin_disabled(&c, "root", false, 1003).unwrap();
        create_session(&c, "t4", &a.id, "c", 1000, 10_000).unwrap();
        set_admin_password(&c, "root", "h2").unwrap();
        assert_eq!(find_session(&c, "t4", 1004), Err(StoreError::NotFound));
    }

    #[test]
    fn disabled_admin_cannot_use_an_existing_session() {
        let c = conn();
        let a = create_admin(&c, "root", "h", Role::Admin, 0).unwrap();
        create_session(&c, "t", &a.id, "c", 1000, 10_000).unwrap();
        c.execute("UPDATE admins SET disabled_at = 5 WHERE id = ?1", [&a.id])
            .unwrap();
        assert_eq!(find_session(&c, "t", 1001), Err(StoreError::NotFound));
    }

    #[test]
    fn enroll_tokens_are_single_use_and_expire() {
        let c = conn();
        let t = create_enroll_token(&c, "h1", "laptop", "root", 1000, 500).unwrap();
        assert_eq!(
            (t.name.as_str(), t.expires_at, t.used_at),
            ("laptop", 1500, None)
        );
        assert_eq!(
            create_enroll_token(&c, "h2", "x", "r", 1, 0),
            Err(StoreError::Invalid("ttl must be positive".into()))
        );
        assert!(matches!(
            create_enroll_token(&c, "h2", "", "r", 1, 5),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            create_enroll_token(&c, "h1", "dup", "r", 1, 5),
            Err(StoreError::Conflict(_))
        ));

        let cl = client(&c, "laptop");
        assert_eq!(
            consume_enroll_token(&c, "h1", &cl.id, 1600),
            Err(StoreError::NotFound),
            "expired"
        );
        assert_eq!(
            consume_enroll_token(&c, "zzz", &cl.id, 1100),
            Err(StoreError::NotFound),
            "unknown"
        );
        let used = consume_enroll_token(&c, "h1", &cl.id, 1100).unwrap();
        assert_eq!(
            (used.used_at, used.client_id.as_deref()),
            (Some(1100), Some(cl.id.as_str()))
        );
        assert_eq!(
            consume_enroll_token(&c, "h1", &cl.id, 1101),
            Err(StoreError::NotFound),
            "single use"
        );

        let t2 = create_enroll_token(&c, "h3", "box", "root", 1000, 5000).unwrap();
        revoke_enroll_token(&c, &t2.id, 1200).unwrap();
        assert!(matches!(
            revoke_enroll_token(&c, &t2.id, 1201),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            revoke_enroll_token(&c, "nope", 1),
            Err(StoreError::NotFound)
        );
        assert_eq!(
            consume_enroll_token(&c, "h3", &cl.id, 1300),
            Err(StoreError::NotFound),
            "revoked"
        );
        assert!(
            matches!(
                revoke_enroll_token(&c, &t.id, 1300),
                Err(StoreError::Conflict(_))
            ),
            "used tokens cannot be revoked"
        );

        assert_eq!(list_enroll_tokens(&c).unwrap().len(), 2);
        assert_eq!(counts(&c, 1250).unwrap().tokens_pending, 0);
        create_enroll_token(&c, "h4", "fresh", "root", 1250, 5000).unwrap();
        assert_eq!(counts(&c, 1250).unwrap().tokens_pending, 1);
    }

    #[test]
    fn clients_lifecycle() {
        let c = conn();
        let a = client(&c, "alpha");
        assert_eq!(
            (a.revision, a.online, a.last_seen_at),
            (1, false, Some(1000))
        );
        assert!(
            matches!(create_client(&c, "x", "n", "short", "", "", "", 1), Err(StoreError::Invalid(m)) if m.contains("public key"))
        );
        assert!(matches!(
            create_client(&c, "x", "", PK, "", "", "", 1),
            Err(StoreError::Invalid(_))
        ));
        let (got, key) = get_client_with_key(&c, &a.id).unwrap();
        assert_eq!((got.id.as_str(), key.as_str()), (a.id.as_str(), PK));
        assert_eq!(get_client(&c, "nope"), Err(StoreError::NotFound));

        let renamed = update_client(&c, &a.id, Some(" beta "), Some(true), 2000).unwrap();
        assert_eq!(
            (renamed.name.as_str(), renamed.disabled_at),
            ("beta", Some(2000))
        );
        assert_eq!(
            update_client(&c, &a.id, None, Some(false), 3000)
                .unwrap()
                .disabled_at,
            None
        );
        assert_eq!(
            update_client(&c, &a.id, None, None, 3000).unwrap().name,
            "beta"
        );
        assert!(matches!(
            update_client(&c, &a.id, Some(""), None, 1),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(
            update_client(&c, "nope", None, None, 1),
            Err(StoreError::NotFound)
        );

        touch_client(&c, &a.id, 5555).unwrap();
        assert_eq!(get_client(&c, &a.id).unwrap().last_seen_at, Some(5555));
        assert_eq!(bump_revision(&c, &a.id).unwrap(), 2);
        assert_eq!(list_clients(&c).unwrap().len(), 1);

        delete_client(&c, &a.id, 9000).unwrap();
        assert_eq!(delete_client(&c, &a.id, 9001), Err(StoreError::NotFound));
        assert_eq!(get_client(&c, &a.id), Err(StoreError::NotFound));
        assert_eq!(
            get_client_with_key(&c, &a.id),
            Err(StoreError::NotFound),
            "deleted devices cannot log in"
        );
        assert!(list_clients(&c).unwrap().is_empty());
    }

    #[test]
    fn tunnels_validation_and_uniqueness() {
        let c = conn();
        let a = client(&c, "a");
        let b = client(&c, "b");
        let t = create_tunnel(&c, &nt(&a.id, "ssh", 6000), 10).unwrap();
        assert_eq!(
            (t.state.as_str(), t.remote_port, t.enabled),
            ("pending", 6000, true)
        );
        assert_eq!(
            get_client(&c, &a.id).unwrap().revision,
            2,
            "creating a tunnel bumps the revision"
        );

        assert!(
            matches!(create_tunnel(&c, &nt(&b.id, "other", 6000), 11), Err(StoreError::Conflict(m)) if m.contains("remote port 6000"))
        );
        assert!(
            matches!(create_tunnel(&c, &nt(&a.id, "ssh", 6001), 11), Err(StoreError::Conflict(m)) if m.contains("already exists"))
        );
        assert!(matches!(
            create_tunnel(&c, &nt("nope", "x", 0), 11),
            Err(StoreError::NotFound)
        ));
        // remote_port = 0（自动分配）不参与端口冲突。
        create_tunnel(&c, &nt(&a.id, "auto1", 0), 12).unwrap();
        create_tunnel(&c, &nt(&b.id, "auto2", 0), 12).unwrap();

        let mut bad = nt(&a.id, "x", 1);
        bad.kind = "xtcp".into();
        assert!(
            matches!(create_tunnel(&c, &bad, 1), Err(StoreError::Invalid(m)) if m.contains("not supported yet"))
        );
        for (lp, rp) in [(0, 1), (70000, 1), (1, -1), (1, 70000)] {
            let mut t = nt(&a.id, "bad", rp);
            t.local_port = lp;
            assert!(
                matches!(create_tunnel(&c, &t, 1), Err(StoreError::Invalid(_))),
                "{lp}/{rp}"
            );
        }
        for host in ["", "a b", "x\ny"] {
            let mut t = nt(&a.id, "bad", 1);
            t.local_ip = host.into();
            assert!(
                matches!(create_tunnel(&c, &t, 1), Err(StoreError::Invalid(_))),
                "{host:?}"
            );
        }
        let mut long = nt(&a.id, "bad", 1);
        long.local_ip = "h".repeat(254);
        assert!(matches!(
            create_tunnel(&c, &long, 1),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            create_tunnel(&c, &nt(&a.id, "", 1), 1),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn udp_and_web_tunnels() {
        let c = conn();
        let a = client(&c, "a");
        // tcp 与 udp 是不同的端口空间。
        create_tunnel(&c, &nt(&a.id, "tcp53", 5353), 1).unwrap();
        let mut udp = nt(&a.id, "udp53", 5353);
        udp.kind = "udp".into();
        let u = create_tunnel(&c, &udp, 1).unwrap();
        assert_eq!((u.kind.as_str(), u.remote_port), ("udp", 5353));
        let mut udp2 = nt(&a.id, "udp53b", 5353);
        udp2.kind = "udp".into();
        assert!(
            matches!(create_tunnel(&c, &udp2, 1), Err(StoreError::Conflict(m)) if m.contains("udp tunnel"))
        );

        let web = |name: &str, domains: &[&str], sub: &str| NewTunnel {
            client_id: a.id.clone(),
            name: name.into(),
            kind: "http".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 80,
            remote_port: 0,
            enabled: true,
            custom_domains: domains.iter().map(|d| d.to_string()).collect(),
            subdomain: sub.into(),
            host_header_rewrite: String::new(),
            http: Default::default(),
            proxy_protocol: 0,
            bandwidth_bps: 0,
            bandwidth_server: false,
            lb: Default::default(),
        };
        let w = create_tunnel(
            &c,
            &web(
                "site",
                &["WWW.Example.com", "www.example.com", "*.wild.example.com"],
                "Blog",
            ),
            2,
        )
        .unwrap();
        assert_eq!(
            w.custom_domains,
            vec!["www.example.com", "*.wild.example.com"],
            "lower-cased and de-duplicated"
        );
        assert_eq!(w.subdomain, "blog");
        // 域名 / 子域名冲突只在同一类别（http / https）内检查。
        assert!(
            matches!(create_tunnel(&c, &web("dup", &["www.example.com"], ""), 3), Err(StoreError::Conflict(m)) if m.contains("domain"))
        );
        assert!(
            matches!(create_tunnel(&c, &web("dup2", &[], "blog"), 3), Err(StoreError::Conflict(m)) if m.contains("subdomain"))
        );
        let mut https = web("tls", &["www.example.com"], "");
        https.kind = "https".into();
        assert!(create_tunnel(&c, &https, 3).is_ok());

        for (bad, why) in [
            (web("n1", &[], ""), "need a custom domain"),
            (web("n2", &["bad domain"], ""), "not a valid domain"),
            (web("n3", &["-x.com"], ""), "not a valid domain"),
            (web("n4", &["*."], ""), "not a valid domain"),
            (web("n5", &["a.com"], "a.b"), "single DNS label"),
            (web("n6", &["a.com"], "-x"), "single DNS label"),
        ] {
            assert!(
                matches!(create_tunnel(&c, &bad, 4), Err(StoreError::Invalid(m)) if m.contains(why)),
                "{why}"
            );
        }
        let mut with_port = web("p", &["p.com"], "");
        with_port.remote_port = 8080;
        assert!(
            matches!(create_tunnel(&c, &with_port, 4), Err(StoreError::Invalid(m)) if m.contains("vhost port"))
        );
        let mut tls_rewrite = web("r", &["r.com"], "");
        tls_rewrite.kind = "https".into();
        tls_rewrite.host_header_rewrite = "x".into();
        assert!(
            matches!(create_tunnel(&c, &tls_rewrite, 4), Err(StoreError::Invalid(m)) if m.contains("only applies to http"))
        );
        let mut bad_rewrite = web("r2", &["r2.com"], "");
        bad_rewrite.host_header_rewrite = "a b".into();
        assert!(
            matches!(create_tunnel(&c, &bad_rewrite, 4), Err(StoreError::Invalid(m)) if m.contains("host name"))
        );
        let mut tcp_domains = nt(&a.id, "t", 0);
        tcp_domains.custom_domains = vec!["a.com".into()];
        assert!(
            matches!(create_tunnel(&c, &tcp_domains, 4), Err(StoreError::Invalid(m)) if m.contains("only apply"))
        );

        // 更新：改域名、改写 Host；类型不可改；更新自己不算冲突。
        let up = update_tunnel(
            &c,
            &w.id,
            &TunnelPatch {
                custom_domains: Some(vec!["new.example.com".into()]),
                host_header_rewrite: Some("internal.lan".into()),
                subdomain: Some("BLOG".into()),
                ..Default::default()
            },
            9,
        )
        .unwrap();
        assert_eq!(
            (
                up.custom_domains.clone(),
                up.host_header_rewrite.as_str(),
                up.subdomain.as_str()
            ),
            (vec!["new.example.com".to_string()], "internal.lan", "blog")
        );
        assert!(update_tunnel(
            &c,
            &w.id,
            &TunnelPatch {
                custom_domains: Some(vec![]),
                subdomain: Some(String::new()),
                ..Default::default()
            },
            10
        )
        .is_err());
        // 下发配置带上域名与 Host 改写。
        let cfg = managed_config(&c, &a.id).unwrap();
        let p = cfg.proxies.iter().find(|p| p.name == w.id).unwrap();
        assert_eq!(
            (
                p.kind.as_str(),
                p.custom_domains.clone(),
                p.host_header_rewrite.as_str()
            ),
            ("http", vec!["new.example.com".to_string()], "internal.lan")
        );
    }

    #[test]
    fn tunnels_update_delete_status_and_config() {
        let c = conn();
        let a = client(&c, "a");
        let t = create_tunnel(&c, &nt(&a.id, "ssh", 6000), 10).unwrap();
        let t2 = create_tunnel(&c, &nt(&a.id, "web", 6001), 10).unwrap();

        set_tunnel_status(&c, &t.id, "running", 6000, "").unwrap();
        assert_eq!(get_tunnel(&c, &t.id).unwrap().state, "running");
        assert_eq!(counts(&c, 0).unwrap().tunnels_running, 1);

        let patch = TunnelPatch {
            name: Some("ssh2".into()),
            local_port: Some(2222),
            remote_port: Some(6100),
            enabled: Some(false),
            local_ip: Some("10.0.0.5".into()),
            ..Default::default()
        };
        let u = update_tunnel(&c, &t.id, &patch, 20).unwrap();
        assert_eq!(
            (
                u.name.as_str(),
                u.local_port,
                u.remote_port,
                u.enabled,
                u.state.as_str(),
                u.updated_at
            ),
            ("ssh2", 2222, 6100, false, "pending", 20)
        );
        assert_eq!(
            update_tunnel(&c, &t.id, &TunnelPatch::default(), 21)
                .unwrap()
                .name,
            "ssh2"
        );
        assert!(matches!(
            update_tunnel(
                &c,
                &t.id,
                &TunnelPatch {
                    remote_port: Some(6001),
                    ..Default::default()
                },
                1
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            update_tunnel(
                &c,
                &t.id,
                &TunnelPatch {
                    name: Some("web".into()),
                    ..Default::default()
                },
                1
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            update_tunnel(
                &c,
                &t.id,
                &TunnelPatch {
                    local_port: Some(0),
                    ..Default::default()
                },
                1
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            update_tunnel(
                &c,
                &t.id,
                &TunnelPatch {
                    name: Some(String::new()),
                    ..Default::default()
                },
                1
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            update_tunnel(
                &c,
                &t.id,
                &TunnelPatch {
                    local_ip: Some(String::new()),
                    ..Default::default()
                },
                1
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            update_tunnel(
                &c,
                &t.id,
                &TunnelPatch {
                    remote_port: Some(-3),
                    ..Default::default()
                },
                1
            ),
            Err(StoreError::Invalid(_))
        ));
        // 保持自身端口不算冲突。
        assert!(update_tunnel(
            &c,
            &t2.id,
            &TunnelPatch {
                remote_port: Some(6001),
                ..Default::default()
            },
            1
        )
        .is_ok());
        assert_eq!(
            update_tunnel(&c, "nope", &TunnelPatch::default(), 1),
            Err(StoreError::NotFound)
        );

        // 只下发启用的隧道，代理名即隧道 ID。
        let cfg = managed_config(&c, &a.id).unwrap();
        assert_eq!(cfg.proxies.len(), 1);
        assert_eq!(
            (cfg.proxies[0].name.as_str(), cfg.proxies[0].local_port),
            (t2.id.as_str(), 22)
        );
        assert_eq!(cfg.revision as i64, get_client(&c, &a.id).unwrap().revision);
        assert_eq!(managed_config(&c, "nope"), Err(StoreError::NotFound));

        assert_eq!(list_tunnels(&c, Some(&a.id)).unwrap().len(), 2);
        assert_eq!(list_tunnels(&c, None).unwrap().len(), 2);
        assert!(list_tunnels(&c, Some("other")).unwrap().is_empty());
        let before = get_client(&c, &a.id).unwrap().revision;
        delete_tunnel(&c, &t.id, 30).unwrap();
        assert_eq!(get_client(&c, &a.id).unwrap().revision, before + 1);
        assert_eq!(delete_tunnel(&c, &t.id, 31), Err(StoreError::NotFound));
        assert_eq!(list_tunnels(&c, None).unwrap().len(), 1);
        // 端口随软删除释放。
        create_tunnel(&c, &nt(&a.id, "again", 6100), 40).unwrap();

        set_tunnel_status(&c, &t2.id, "running", 6001, "").unwrap();
        mark_client_tunnels_stopped(&c, &a.id).unwrap();
        let s = get_tunnel(&c, &t2.id).unwrap();
        assert_eq!((s.state.as_str(), s.status_port), ("stopped", 0));

        delete_client(&c, &a.id, 50).unwrap();
        assert!(
            list_tunnels(&c, None).unwrap().is_empty(),
            "deleting a client removes its tunnels"
        );
    }

    #[test]
    fn audit_log_paginates_newest_first() {
        let c = conn();
        for i in 0..5 {
            audit(
                &c,
                "root",
                &format!("act{i}"),
                "client",
                "c1",
                "because",
                "ok",
                "127.0.0.1",
                100 + i,
            )
            .unwrap();
        }
        let page1 = list_audit(&c, 2, None).unwrap();
        assert_eq!(
            page1.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
            vec!["act4", "act3"]
        );
        let page2 = list_audit(&c, 2, Some(page1[1].id)).unwrap();
        assert_eq!(
            page2.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
            vec!["act2", "act1"]
        );
        assert_eq!(
            list_audit(&c, 0, None).unwrap().len(),
            1,
            "limit is clamped to at least 1"
        );
        assert_eq!(list_audit(&c, 10_000, None).unwrap().len(), 5);
        assert_eq!(page1[0].reason, "because");
    }

    #[test]
    fn counts_overview() {
        let c = conn();
        assert_eq!(
            counts(&c, 0).unwrap(),
            Counts {
                clients: 0,
                tunnels: 0,
                tunnels_running: 0,
                tokens_pending: 0
            }
        );
        let a = client(&c, "a");
        create_tunnel(&c, &nt(&a.id, "t", 0), 1).unwrap();
        assert_eq!(
            counts(&c, 0).unwrap(),
            Counts {
                clients: 1,
                tunnels: 1,
                tunnels_running: 0,
                tokens_pending: 0
            }
        );
    }
    #[test]
    fn no_op_and_display_name_changes_preserve_live_status() {
        let c = conn();
        let a = client(&c, "idempotent");
        let t = create_tunnel(&c, &nt(&a.id, "ssh", 6000), 0).unwrap();
        set_tunnel_status(&c, &t.id, "running", 6000, "").unwrap();
        for patch in [
            TunnelPatch::default(),
            TunnelPatch {
                enabled: Some(true),
                ..Default::default()
            },
            TunnelPatch {
                name: Some("renamed".into()),
                ..Default::default()
            },
        ] {
            let updated = update_tunnel(&c, &t.id, &patch, 1).unwrap();
            assert_eq!(
                (updated.state.as_str(), updated.status_port),
                ("running", 6000)
            );
        }
        let updated = update_tunnel(
            &c,
            &t.id,
            &TunnelPatch {
                local_port: Some(23),
                ..Default::default()
            },
            2,
        )
        .unwrap();
        assert_eq!(updated.state, "pending");
    }
}
