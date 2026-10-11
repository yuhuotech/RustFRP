//! 存储层：全部同步函数，接收 `&Connection`（事务也可以传入）。业务规则（唯一性、状态迁移、软删除）都在这里，
//! HTTP 层不拼 SQL。

use rusqlite::{params, Connection, OptionalExtension, Row};
use rustfrp_core::auth::random_id;
use rustfrp_core::wire::{ConfigSet, ManagedProxy};

use crate::db::StoreError;
use crate::models::{Admin, AuditEntry, Client, EnrollToken, Role, Tunnel};

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
    })
}
const TOKEN_COLS: &str =
    "id, name, created_by, created_at, expires_at, used_at, client_id, revoked_at";

/// 创建接入令牌（只存哈希）。
pub fn create_enroll_token(
    c: &Connection,
    token_hash: &str,
    name: &str,
    created_by: &str,
    now: i64,
    ttl_ms: i64,
) -> Res<EnrollToken> {
    check_name(name, "name")?;
    if ttl_ms <= 0 {
        return Err(StoreError::Invalid("ttl must be positive".into()));
    }
    let id = new_id();
    c.execute(
        "INSERT INTO enroll_tokens (id, token_hash, name, created_by, created_at, expires_at) VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, token_hash, name.trim(), created_by, now, now + ttl_ms],
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
    let n = c.execute(
        "UPDATE enroll_tokens SET used_at = ?2, client_id = ?3
          WHERE token_hash = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
        params![token_hash, now, client_id],
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
    })
}
const CLIENT_COLS: &str =
    "id, name, os, arch, version, revision, created_at, last_seen_at, disabled_at";

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
        |r| Ok((client_from(r)?, r.get(9)?)),
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
    Ok(())
}

/// 记录在线时间（可同时更新版本）。
pub fn touch_client(c: &Connection, id: &str, now: i64) -> Res<()> {
    c.execute(
        "UPDATE clients SET last_seen_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
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
}

fn tunnel_from(r: &Row<'_>) -> rusqlite::Result<Tunnel> {
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
    })
}
const TUNNEL_COLS: &str = "id, client_id, name, kind, local_ip, local_port, remote_port, enabled, created_at, updated_at, status_state, status_port, status_error, custom_domains, subdomain, host_header_rewrite";

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

const KINDS: &[&str] = &["tcp", "udp", "http", "https"];

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
    except_id: Option<&str>,
) -> Res<()> {
    if port == 0 || is_web(kind) {
        return Ok(());
    }
    let taken: Option<String> = c
        .query_row(
            "SELECT id FROM tunnels WHERE remote_port = ?1 AND kind = ?3 AND deleted_at IS NULL AND (?2 IS NULL OR id <> ?2)",
            params![port, except_id, kind],
            |r| r.get(0),
        )
        .optional()?;
    if taken.is_some() {
        return Err(StoreError::Conflict(format!(
            "remote port {port} is already used by another {kind} tunnel"
        )));
    }
    Ok(())
}

/// 同类别（http / https）内域名不能重复；子域名按 `sub.<根域名>` 在运行时解析，这里按「同名子域名」去重。
fn ensure_domains_free(
    c: &Connection,
    kind: &str,
    domains: &[String],
    subdomain: &str,
    except_id: Option<&str>,
) -> Res<()> {
    if !is_web(kind) {
        return Ok(());
    }
    let mut st = c.prepare(
        "SELECT id, custom_domains, subdomain FROM tunnels WHERE kind = ?1 AND deleted_at IS NULL AND (?2 IS NULL OR id <> ?2)",
    )?;
    let rows = st
        .query_map(params![kind, except_id], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (other_domains, other_sub) in rows {
        let theirs = split_domains(&other_domains);
        if let Some(d) = domains.iter().find(|d| theirs.contains(d)) {
            return Err(StoreError::Conflict(format!(
                "domain \"{d}\" is already used by another {kind} tunnel"
            )));
        }
        if !subdomain.is_empty() && subdomain == other_sub {
            return Err(StoreError::Conflict(format!(
                "subdomain \"{subdomain}\" is already used by another {kind} tunnel"
            )));
        }
    }
    Ok(())
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
            "tunnel type \"{}\" is not supported yet, supported: tcp, udp, http, https",
            t.kind
        )));
    }
    check_host(&t.local_ip)?;
    check_port(t.local_port, "local_port", false)?;
    check_port(t.remote_port, "remote_port", true)?;
    let domains = normalize_domains(&t.domains)?;
    check_subdomain(&t.subdomain)?;
    check_rewrite(&t.rewrite)?;
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
    ensure_remote_port_free(c, &t.kind, t.remote_port, except_id)?;
    ensure_domains_free(c, &t.kind, &domains, &t.subdomain, except_id)?;
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
    Ok(Checked { domains, ..t })
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
        },
        None,
        &t.client_id,
    )?;
    let id = new_id();
    c.execute(
        "INSERT INTO tunnels (id, client_id, name, kind, local_ip, local_port, remote_port, enabled, created_at, updated_at, custom_domains, subdomain, host_header_rewrite)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12)",
        params![id, t.client_id, k.name.trim(), k.kind, k.local_ip, k.local_port, k.remote_port, i64::from(t.enabled), now, k.domains.join(","), k.subdomain, k.rewrite],
    )?;
    bump_revision(c, &t.client_id)?;
    get_tunnel(c, &id)
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
        },
        Some(id),
        &cur.client_id,
    )?;
    // 配置变化后状态回到 pending，等待 Edge 重新回报。
    c.execute(
        "UPDATE tunnels SET name=?2, local_ip=?3, local_port=?4, remote_port=?5, enabled=?6, updated_at=?7, status_state='pending', status_error='',
                custom_domains=?8, subdomain=?9, host_header_rewrite=?10 WHERE id=?1",
        params![id, k.name.trim(), k.local_ip, k.local_port, k.remote_port, i64::from(enabled), now, k.domains.join(","), k.subdomain, k.rewrite],
    )?;
    bump_revision(c, &cur.client_id)?;
    get_tunnel(c, id)
}

/// 软删除隧道。
pub fn delete_tunnel(c: &Connection, id: &str, now: i64) -> Res<()> {
    let t = get_tunnel(c, id)?;
    c.execute(
        "UPDATE tunnels SET deleted_at = ?2, enabled = 0 WHERE id = ?1",
        params![id, now],
    )?;
    bump_revision(c, &t.client_id)?;
    Ok(())
}

/// 记录 Edge 回报的运行状态（`state`：running / error / stopped）。
pub fn set_tunnel_status(c: &Connection, id: &str, state: &str, port: u16, error: &str) -> Res<()> {
    c.execute(
        "UPDATE tunnels SET status_state = ?2, status_port = ?3, status_error = ?4 WHERE id = ?1 AND deleted_at IS NULL",
        params![id, state, i64::from(port), error],
    )?;
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
    let proxies = list_tunnels(c, Some(client_id))?
        .into_iter()
        .filter(|t| t.enabled)
        .map(|t| ManagedProxy {
            name: t.id,
            kind: t.kind,
            remote_port: t.remote_port,
            local_ip: t.local_ip,
            local_port: t.local_port,
            custom_domains: t.custom_domains,
            subdomain: t.subdomain,
            host_header_rewrite: t.host_header_rewrite,
        })
        .collect();
    Ok(ConfigSet {
        revision: client.revision as u64,
        proxies,
    })
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
        bad.kind = "stcp".into();
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
}
