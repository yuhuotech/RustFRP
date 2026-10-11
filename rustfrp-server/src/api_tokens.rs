//! 自动化凭据只存哈希，所有管理变更与审计共事务。
use crate::{
    db::StoreError,
    models::{Admin, Role},
    store,
};
use rusqlite::{params, Connection};
use serde::Serialize;

/// 不包含秘密的令牌信息。
#[derive(Debug, Serialize)]
pub struct ApiToken {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ApiToken> {
    Ok(ApiToken {
        id: r.get(0)?,
        name: r.get(1)?,
        scope: r.get(2)?,
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        revoked_at: r.get(5)?,
    })
}
/// 仅返回当前管理员最近 100 个令牌。
pub fn list(c: &Connection, admin: &str) -> Result<Vec<ApiToken>, StoreError> {
    let mut q=c.prepare("SELECT id,name,scope,created_at,expires_at,revoked_at FROM api_tokens WHERE admin_id=?1 ORDER BY created_at DESC,id DESC LIMIT 100")?;
    let result = q
        .query_map([admin], row)?
        .map(|r| r.map_err(StoreError::from))
        .collect();
    result
}
/// 创建参数。
pub struct NewToken<'a> {
    pub name: &'a str,
    pub scope: &'a str,
    pub days: i64,
}
/// 创建令牌元数据并写审计。
pub fn create(
    c: &mut Connection,
    admin: &Admin,
    spec: NewToken<'_>,
    hash: &str,
    reason: &str,
    ip: &str,
    now: i64,
) -> Result<ApiToken, StoreError> {
    let NewToken { name, scope, days } = spec;
    if name.trim().is_empty()
        || name.len() > 100
        || !["read", "tunnels", "admin"].contains(&scope)
        || !(1..=365).contains(&days)
    {
        return Err(StoreError::Invalid(
            "invalid token name, scope or lifetime".into(),
        ));
    }
    let tx = c.transaction()?;
    let count:i64=tx.query_row("SELECT count(*) FROM api_tokens WHERE admin_id=?1 AND revoked_at IS NULL AND expires_at>?2",params![admin.id,now],|r|r.get(0))?;
    if count >= 100 {
        return Err(StoreError::Invalid("active token capacity reached".into()));
    }
    let id = store::new_id();
    let expires = now.saturating_add(days * 86_400_000);
    tx.execute(
        "INSERT INTO api_tokens VALUES(?1,?2,?3,?4,?5,?6,?7,NULL)",
        params![id, hash, admin.id, name.trim(), scope, now, expires],
    )?;
    store::audit(
        &tx,
        &admin.name,
        "api_token.create",
        "api_token",
        &id,
        reason,
        "ok",
        ip,
        now,
    )?;
    let token = tx.query_row(
        "SELECT id,name,scope,created_at,expires_at,revoked_at FROM api_tokens WHERE id=?1",
        [&id],
        row,
    )?;
    tx.commit()?;
    Ok(token)
}
/// 撤销只允许自己的令牌，重复撤销仍幂等。
pub fn revoke(
    c: &mut Connection,
    admin: &Admin,
    id: &str,
    reason: &str,
    ip: &str,
    now: i64,
) -> Result<(), StoreError> {
    let tx = c.transaction()?;
    let changed = tx.execute(
        "UPDATE api_tokens SET revoked_at=coalesce(revoked_at,?1) WHERE id=?2 AND admin_id=?3",
        params![now, id, admin.id],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound);
    }
    store::audit(
        &tx,
        &admin.name,
        "api_token.revoke",
        "api_token",
        id,
        reason,
        "ok",
        ip,
        now,
    )?;
    tx.commit()?;
    Ok(())
}
/// 查询有效令牌及当前角色；禁用账号不会因已有 Token 绕过校验。
pub fn authenticate(
    c: &Connection,
    hash: &str,
    now: i64,
    path: &str,
    safe: bool,
) -> Result<Admin, StoreError> {
    let (id,admin_id,scope):(String,String,String)=c.query_row("SELECT id,admin_id,scope FROM api_tokens WHERE token_hash=?1 AND revoked_at IS NULL AND expires_at>?2",params![hash,now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if ["/api/v1/auth", "/api/v1/admins", "/api/v1/api-tokens"]
        .iter()
        .any(|prefix| {
            path == *prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
        || (!safe && scope == "read")
        || (!safe
            && scope == "tunnels"
            && path != "/api/v1/tunnels"
            && !path.starts_with("/api/v1/tunnels/"))
    {
        return Err(StoreError::Invalid(
            "token scope denies this endpoint".into(),
        ));
    }
    let mut admin = store::get_admin(c, &admin_id)?;
    if admin.disabled_at.is_some() {
        return Err(StoreError::NotFound);
    }
    admin.role = admin.role.min(match scope.as_str() {
        "read" => Role::Viewer,
        "tunnels" => Role::Operator,
        _ => Role::Admin,
    });
    admin.name = format!("{} [api_token:{id}]", admin.name);
    Ok(admin)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    fn setup() -> (Connection, Admin) {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        let a = store::create_admin(&c, "root", "hash", Role::Owner, 0).unwrap();
        (c, a)
    }
    #[test]
    fn limits_disabled_expiry_revocation_and_transactions() {
        let (mut c, admin) = setup();
        for (name, scope, days) in [
            ("", "read", 1),
            ("x", "bad", 1),
            ("x", "read", 0),
            ("x", "read", 366),
        ] {
            assert!(create(
                &mut c,
                &admin,
                NewToken { name, scope, days },
                "hash",
                "test",
                "",
                0
            )
            .is_err());
        }
        assert!(create(
            &mut c,
            &admin,
            NewToken {
                name: &"x".repeat(101),
                scope: "read",
                days: 1
            },
            "hash",
            "test",
            "",
            0
        )
        .is_err());
        c.execute_batch("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'test'); END").unwrap();
        assert!(create(
            &mut c,
            &admin,
            NewToken {
                name: "CI",
                scope: "read",
                days: 1
            },
            "hash",
            "test",
            "",
            0
        )
        .is_err());
        assert!(list(&c, &admin.id).unwrap().is_empty());
        c.execute_batch("DROP TRIGGER reject_audit").unwrap();
        let token = create(
            &mut c,
            &admin,
            NewToken {
                name: "CI",
                scope: "read",
                days: 1,
            },
            "hash",
            "test",
            "",
            0,
        )
        .unwrap();
        assert_eq!(
            authenticate(&c, "hash", 1, "/api/v1/overview", true)
                .unwrap()
                .role,
            Role::Viewer
        );
        assert!(authenticate(&c, "hash", 86_400_000, "/api/v1/overview", true).is_err());
        assert!(authenticate(&c, "hash", 1, "/api/v1/admins/abc", true).is_err());
        c.execute("UPDATE admins SET disabled_at=1 WHERE id=?1", [&admin.id])
            .unwrap();
        assert!(authenticate(&c, "hash", 1, "/api/v1/overview", true).is_err());
        c.execute(
            "UPDATE admins SET disabled_at=NULL WHERE id=?1",
            [&admin.id],
        )
        .unwrap();
        c.execute_batch("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'test'); END").unwrap();
        assert!(revoke(&mut c, &admin, &token.id, "test", "", 2).is_err());
        assert!(authenticate(&c, "hash", 3, "/api/v1/overview", true).is_ok());
        c.execute_batch("DROP TRIGGER reject_audit").unwrap();
        revoke(&mut c, &admin, &token.id, "test", "", 2).unwrap();
        assert!(authenticate(&c, "hash", 3, "/api/v1/overview", true).is_err());
        for n in 0..100 {
            create(
                &mut c,
                &admin,
                NewToken {
                    name: "CI",
                    scope: "admin",
                    days: 1,
                },
                &n.to_string(),
                "test",
                "",
                3,
            )
            .unwrap();
        }
        assert!(create(
            &mut c,
            &admin,
            NewToken {
                name: "CI",
                scope: "read",
                days: 1
            },
            "overflow",
            "test",
            "",
            3
        )
        .is_err());
        assert_eq!(list(&c, &admin.id).unwrap().len(), 100);
        let other = store::create_admin(&c, "other", "hash", Role::Operator, 0).unwrap();
        let low = create(
            &mut c,
            &other,
            NewToken {
                name: "CI",
                scope: "admin",
                days: 1,
            },
            "low",
            "test",
            "",
            0,
        )
        .unwrap();
        assert_eq!(
            authenticate(&c, "low", 1, "/api/v1/tunnels", false)
                .unwrap()
                .role,
            Role::Operator
        );
        assert!(revoke(&mut c, &admin, &low.id, "test", "", 2).is_err());
    }
}
