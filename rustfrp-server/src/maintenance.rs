//! 每轮固定数量的过期数据清理；审计与删除共事务，无额外常驻容器。
use crate::{
    db::{Db, StoreError},
    store,
};
use rusqlite::{params, Connection};
use rustfrp_agent::log::Logger;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// UTC 保留周期使用的毫秒数。
pub const DAY_MS: i64 = 86_400_000;
/// 一轮最多处理的数量（分别应用于每张表）。
pub const MAX_BATCH: i64 = 1000;

/// 此轮删除的行数。
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Purged {
    /// 已过期管理会话。
    pub sessions: usize,
    /// 已过期接入令牌。
    pub tokens: usize,
    /// 超出保留期限的审计。
    pub audits: usize,
    /// 超出保留期限的流量分钟桶。
    pub traffic_minutes: usize,
    /// 超出保留期限的流量采样游标。
    pub traffic_cursors: usize,
    /// 已过期或已撤销自动化令牌。
    pub api_tokens: usize,
}

/// 0 天关闭审计删除；保留边界当天不删除，每张表至多 `batch` 行。
pub fn purge(
    c: &mut Connection,
    now: i64,
    audit_days: i64,
    traffic_days: i64,
    batch: i64,
) -> Result<Purged, StoreError> {
    if !(0..=3650).contains(&audit_days)
        || !(0..=3650).contains(&traffic_days)
        || !(1..=MAX_BATCH).contains(&batch)
    {
        return Err(StoreError::Invalid(
            "audit and traffic retention must be 0..3650 days; batch must be 1..1000".into(),
        ));
    }
    let tx = c.transaction()?;
    let sessions = tx.execute("DELETE FROM admin_sessions WHERE token_hash IN (SELECT token_hash FROM admin_sessions WHERE expires_at <= ?1 ORDER BY expires_at, token_hash LIMIT ?2)", params![now,batch])?;
    let tokens = tx.execute("DELETE FROM enroll_tokens WHERE id IN (SELECT id FROM enroll_tokens WHERE expires_at <= ?1 ORDER BY expires_at, id LIMIT ?2)", params![now,batch])?;
    let api_tokens = tx.execute("DELETE FROM api_tokens WHERE id IN (SELECT id FROM api_tokens WHERE expires_at <= ?1 OR revoked_at IS NOT NULL ORDER BY expires_at,id LIMIT ?2)",params![now,batch])?;
    let audits = if audit_days == 0 {
        0
    } else {
        tx.execute("DELETE FROM audit_log WHERE id IN (SELECT id FROM audit_log WHERE ts < ?1 ORDER BY ts, id LIMIT ?2)",params![now.saturating_sub(audit_days * DAY_MS),batch])?
    };
    let traffic_minutes = if traffic_days == 0 {
        0
    } else {
        tx.execute("DELETE FROM traffic_minutes WHERE (tunnel_id,minute) IN (SELECT tunnel_id,minute FROM traffic_minutes WHERE minute < ?1 ORDER BY minute,tunnel_id LIMIT ?2)", params![now.saturating_sub(traffic_days * DAY_MS), batch])?
    };
    let traffic_cursors = if traffic_days == 0 {
        0
    } else {
        tx.execute("DELETE FROM traffic_cursors WHERE (epoch,tunnel_id,generation) IN (SELECT epoch,tunnel_id,generation FROM traffic_cursors WHERE sampled_at < ?1 ORDER BY sampled_at,epoch,tunnel_id,generation LIMIT ?2)", params![now.saturating_sub(traffic_days * DAY_MS), batch])?
    };
    if sessions + tokens + audits + api_tokens + traffic_minutes + traffic_cursors > 0 {
        let reason = format!("retention policy: sessions={sessions}, enroll_tokens={tokens}, audit={audits}, api_tokens={api_tokens}, traffic_minutes={traffic_minutes}, traffic_cursors={traffic_cursors}; audit_days={audit_days}, traffic_days={traffic_days}");
        store::audit(
            &tx,
            "system",
            "maintenance.purge",
            "database",
            "",
            &reason,
            "ok",
            "",
            now,
        )?;
    }
    tx.commit()?;
    Ok(Purged {
        sessions,
        tokens,
        audits,
        api_tokens,
        traffic_minutes,
        traffic_cursors,
    })
}

/// 一个周期只清理一批；取消后不再提交新的任务，数据库错误明确进入日志。
pub async fn run(
    db: Db,
    audit_days: i64,
    traffic_days: i64,
    batch: i64,
    period: Duration,
    cancel: CancellationToken,
    log: Logger,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(period) => {
                if let Err(e) = db.call(move |c| purge(c, store::now_ms(), audit_days, traffic_days, batch)).await {
                    log.error(&format!("retention maintenance failed: {e}"));
                }
            }
        }
    }
}
