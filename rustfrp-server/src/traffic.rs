//! 有界分钟采样；SQLite 游标与差量桶共事务，失败不确认最终样本。
use crate::{
    db::{Db, StoreError},
    store,
};
use rusqlite::{params, Connection, OptionalExtension};
use rustfrp_agent::{
    log::Logger,
    metrics::{Metrics, Sample, MAX_PROXIES},
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

fn number(n: u64) -> Result<i64, StoreError> {
    i64::try_from(n)
        .map_err(|_| StoreError::Invalid("traffic counter exceeds storage range".into()))
}

/// 写入一次本机 Edge 采样；新实例自零累计，过旧或重复时间戳不写桶。
pub fn persist(
    c: &mut Connection,
    epoch: &str,
    now: i64,
    samples: &[Sample],
) -> Result<(), StoreError> {
    if epoch.is_empty() || epoch.len() > 64 || now < 0 || samples.len() > MAX_PROXIES {
        return Err(StoreError::Invalid(
            "invalid traffic sample envelope".into(),
        ));
    }
    let tx = c.transaction()?;
    for sample in samples {
        let g = number(sample.generation)?;
        let incoming = number(sample.traffic.bytes_in)?;
        let outgoing = number(sample.traffic.bytes_out)?;
        let total = number(sample.traffic.connections_total)?;
        let connections = number(sample.traffic.connections)?;
        let old: Option<(i64,i64,i64,i64)> = tx.query_row("SELECT sampled_at,bytes_in,bytes_out,connections_total FROM traffic_cursors WHERE epoch=?1 AND tunnel_id=?2 AND generation=?3",params![epoch,sample.name,g],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let (previous, bi, bo, ct) = old.unwrap_or((now, 0, 0, 0));
        if old.is_some() && now <= previous {
            if incoming != bi || outgoing != bo || total != ct {
                return Err(StoreError::Invalid("sample clock did not advance".into()));
            }
            continue;
        }
        if incoming < bi || outgoing < bo || total < ct {
            return Err(StoreError::Invalid(
                "traffic counters decreased within an instance".into(),
            ));
        }
        let gap = i64::from(old.is_none() || now - previous > 10_000);
        tx.execute("INSERT INTO traffic_minutes VALUES (?1,?2,?3,?4,?5,?6,1,?7) ON CONFLICT(tunnel_id,minute) DO UPDATE SET bytes_in=bytes_in+excluded.bytes_in,bytes_out=bytes_out+excluded.bytes_out,connections_total=connections_total+excluded.connections_total,connections_peak=max(connections_peak,excluded.connections_peak),samples=samples+1,gap=max(gap,excluded.gap)", params![sample.name,now/MINUTE*MINUTE,incoming-bi,outgoing-bo,total-ct,connections,gap])?;
        tx.execute("INSERT INTO traffic_cursors VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(epoch,tunnel_id,generation) DO UPDATE SET sampled_at=excluded.sampled_at,bytes_in=excluded.bytes_in,bytes_out=excluded.bytes_out,connections_total=excluded.connections_total",params![epoch,sample.name,g,now,incoming,outgoing,total])?;
    }
    let cutoff = now.saturating_sub(7 * crate::maintenance::DAY_MS);
    let minutes = tx.execute("DELETE FROM traffic_minutes WHERE (tunnel_id,minute) IN (SELECT tunnel_id,minute FROM traffic_minutes WHERE minute < ?1 ORDER BY minute,tunnel_id LIMIT 2048)",[cutoff])?;
    let cursors = tx.execute("DELETE FROM traffic_cursors WHERE (epoch,tunnel_id,generation) IN (SELECT epoch,tunnel_id,generation FROM traffic_cursors WHERE sampled_at < ?1 ORDER BY sampled_at LIMIT 2048)",[cutoff])?;
    if minutes + cursors > 0 {
        store::audit(
            &tx,
            "system",
            "maintenance.traffic",
            "database",
            "",
            &format!("traffic retention: 7 days; minutes={minutes}, cursors={cursors}"),
            "ok",
            "",
            now,
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// 把节点经线协议上报的样本转成内置 Edge 同款的 [`Sample`]。
pub fn sample_from_wire(s: &rustfrp_core::node::TrafficSample) -> Sample {
    Sample {
        name: s.name.clone(),
        generation: s.generation,
        traffic: rustfrp_core::pipe::TrafficSnapshot {
            bytes_in: s.bytes_in,
            bytes_out: s.bytes_out,
            connections: s.connections,
            connections_total: s.connections_total,
        },
    }
}

/// 一个实际观测过的小时桶；不存在的桶不伪装为零。
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bucket {
    pub timestamp: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub connections_total: i64,
    /// 单隧道采样峰值；全局查询是各隧道峰值之和，不是瞬时并发。
    pub connections_peak: i64,
    pub samples: i64,
    pub gap: bool,
}

/// 最近 24 小时（含当前分钟）全部隧道的累计上下行字节，单条 SQL 聚合。
pub fn last_day_totals(c: &Connection, now: i64) -> Result<(i64, i64), StoreError> {
    Ok(c.query_row(
        "SELECT COALESCE(sum(bytes_in),0), COALESCE(sum(bytes_out),0) FROM traffic_minutes WHERE minute>?1 AND minute<=?2",
        params![now.saturating_sub(24 * HOUR), now],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// 限定 24/168 小时，先 SQL 聚合，最多返回 168 个桶。
pub fn history(
    c: &Connection,
    now: i64,
    hours: i64,
    tunnel: Option<&str>,
) -> Result<Vec<Bucket>, StoreError> {
    history_filtered(c, now, hours, tunnel, None)
}

/// 按客户端与隧道交集过滤历史。
pub fn history_filtered(
    c: &Connection,
    now: i64,
    hours: i64,
    tunnel: Option<&str>,
    client: Option<&str>,
) -> Result<Vec<Bucket>, StoreError> {
    if ![24, 168].contains(&hours) || now < 0 {
        return Err(StoreError::Invalid(
            "traffic range must be 24 or 168 hours".into(),
        ));
    }
    let end = now / HOUR * HOUR;
    let start = end.saturating_sub((hours - 1) * HOUR);
    let mut query = c.prepare("SELECT minute/?1*?1,sum(bytes_in),sum(bytes_out),sum(connections_total),sum(peak),sum(samples),max(gap) FROM (SELECT tunnel_id,minute/?1*?1 AS minute,sum(bytes_in) AS bytes_in,sum(bytes_out) AS bytes_out,sum(connections_total) AS connections_total,max(connections_peak) AS peak,sum(samples) AS samples,max(max(gap),count(*)<60) AS gap FROM traffic_minutes WHERE minute>=?2 AND minute<=?3 AND (?4 IS NULL OR tunnel_id=?4) AND (?5 IS NULL OR tunnel_id IN (SELECT id FROM tunnels WHERE client_id=?5)) GROUP BY tunnel_id,minute/?1) GROUP BY minute/?1 ORDER BY minute/?1 LIMIT 168")?;
    let rows = query.query_map(params![HOUR, start, now, tunnel, client], |r| {
        Ok(Bucket {
            timestamp: r.get(0)?,
            bytes_in: r.get(1)?,
            bytes_out: r.get(2)?,
            connections_total: r.get(3)?,
            connections_peak: r.get(4)?,
            samples: r.get(5)?,
            gap: r.get(6)?,
        })
    })?;
    rows.map(|r| r.map_err(StoreError::from)).collect()
}

/// 每次最多 1024 个样本；仅提交成功后回收停止条目，任务可取消。
pub async fn run(
    db: Db,
    metrics: Arc<Metrics>,
    epoch: String,
    period: Duration,
    cancel: CancellationToken,
    log: Logger,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(period) => {
                let samples = metrics.samples();
                let current_epoch = epoch.clone();
                let saved = db.call(move |c| { persist(c,&current_epoch,store::now_ms(),&samples)?; Ok(samples) }).await;
                match saved {
                    Ok(samples) => {
                        for s in samples { metrics.acknowledge_sample(&s); }
                    }
                    Err(e) => { log.error(&format!("traffic sampling failed: {e}")); }
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use rustfrp_core::pipe::TrafficSnapshot;
    fn sample(name: &str, generation: u64, bytes: u64) -> Sample {
        Sample {
            name: name.into(),
            generation,
            traffic: TrafficSnapshot {
                bytes_in: bytes,
                bytes_out: bytes * 2,
                connections: 2,
                connections_total: 3,
            },
        }
    }
    fn database() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        c
    }
    #[test]
    fn samples_are_deduplicated_and_instances_are_isolated() {
        let mut c = database();
        persist(
            &mut c,
            "epoch",
            HOUR,
            &[sample("a", 0, 10), sample("b", 1, 4)],
        )
        .unwrap();
        assert!(persist(&mut c, "epoch", HOUR, &[sample("a", 0, 99)]).is_err());
        persist(&mut c, "epoch", HOUR, &[sample("a", 0, 10)]).unwrap();
        assert!(persist(&mut c, "epoch", HOUR - 1, &[sample("a", 0, 99)]).is_err());
        persist(&mut c, "epoch", HOUR + 5000, &[sample("a", 0, 15)]).unwrap();
        persist(&mut c, "epoch", HOUR + 10_000, &[sample("a", 2, 7)]).unwrap();
        persist(&mut c, "new-epoch", HOUR + 15_000, &[sample("a", 0, 2)]).unwrap();
        let a = history(&c, HOUR + 15_000, 24, Some("a")).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(
            (
                a[0].bytes_in,
                a[0].bytes_out,
                a[0].connections_total,
                a[0].connections_peak,
                a[0].samples,
                a[0].gap
            ),
            (24, 48, 9, 2, 4, true)
        );
        let all = history(&c, HOUR + 15_000, 168, None).unwrap();
        assert_eq!((all[0].bytes_in, all[0].connections_peak), (28, 4));
        assert!(history(&c, HOUR, 24, Some("missing")).unwrap().is_empty());
        persist(&mut c, "epoch", HOUR + 120_000, &[sample("b", 1, 5)]).unwrap();
        assert_eq!(
            history(&c, HOUR + 120_000, 24, Some("b")).unwrap()[0].bytes_in,
            5
        );
    }
    #[test]
    fn invalid_values_and_database_errors_rollback_every_sample() {
        let mut c = database();
        for (epoch, now) in [("", 0), ("ok", -1)] {
            assert!(persist(&mut c, epoch, now, &[]).is_err());
        }
        assert!(persist(&mut c, &"x".repeat(65), 0, &[]).is_err());
        let too_many = (0..=MAX_PROXIES)
            .map(|_| sample("a", 0, 0))
            .collect::<Vec<_>>();
        assert!(persist(&mut c, "e", 0, &too_many).is_err());
        for hours in [0, 23, 169] {
            assert!(history(&c, 0, hours, None).is_err());
        }
        assert!(history(&c, -1, 24, None).is_err());
        let mut invalid = sample("overflow", 0, 0);
        invalid.traffic.bytes_in = u64::MAX;
        assert!(persist(&mut c, "e", 0, &[sample("a", 0, 4), invalid]).is_err());
        assert!(history(&c, 0, 24, None).unwrap().is_empty());
        persist(&mut c, "e", 1000, &[sample("a", 0, 4)]).unwrap();
        assert!(persist(&mut c, "e", 2000, &[sample("b", 0, 9), sample("a", 0, 3)]).is_err());
        assert!(history(&c, 2000, 24, Some("b")).unwrap().is_empty());
        c.execute_batch("CREATE TRIGGER fail_cursor BEFORE UPDATE ON traffic_cursors BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(persist(&mut c, "e", 3000, &[sample("a", 0, 9)]).is_err());
        assert_eq!(history(&c, 3000, 24, Some("a")).unwrap()[0].bytes_in, 4);
        c.execute_batch("DROP TABLE traffic_minutes").unwrap();
        assert!(persist(&mut c, "e", 4000, &[sample("a", 0, 9)]).is_err());
        assert!(history(&c, 4000, 24, None).is_err());
    }
    #[test]
    fn historical_query_is_bounded_and_keeps_gaps() {
        let mut c = database();
        for hour in 0..200 {
            persist(&mut c, "e", hour * HOUR, &[sample("a", 0, hour as u64)]).unwrap();
        }
        let result = history(&c, 199 * HOUR, 168, None).unwrap();
        assert_eq!(result.len(), 168);
        assert_eq!(result.first().unwrap().timestamp, 32 * HOUR);
        assert!(result.iter().all(|b| b.gap));
        assert_eq!(history(&c, 199 * HOUR, 24, None).unwrap().len(), 24);
    }
    #[tokio::test]
    async fn sampler_can_cancel_and_logs_database_errors() {
        use rustfrp_agent::log::Level;
        use std::sync::Mutex;
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let db = Db::open_memory().unwrap();
        let metrics = Arc::new(Metrics::default());
        let cancel = CancellationToken::new();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(run(
            db,
            metrics,
            "".into(),
            Duration::from_millis(2),
            cancel.clone(),
            Logger::new(Level::Error, Box::new(Capture(logs.clone()))),
        ));
        for _ in 0..100 {
            if !logs.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        cancel.cancel();
        task.await.unwrap();
        assert!(String::from_utf8(logs.lock().unwrap().clone())
            .unwrap()
            .contains("traffic sampling failed"));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            Db::open_memory().unwrap(),
            Arc::new(Metrics::default()),
            "valid".into(),
            Duration::from_millis(2),
            cancel.clone(),
            Logger::silent(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
        task.await.unwrap();
    }
    #[test]
    fn fixture_and_retention_boundaries() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../rustfrp-protocol/fixtures/traffic-history.json"
        ))
        .unwrap();
        let buckets: Vec<Bucket> = serde_json::from_value(fixture["items"].clone()).unwrap();
        assert_eq!(serde_json::to_value(&buckets).unwrap(), fixture["items"]);
        let mut c = database();
        persist(&mut c, "e", 0, &[sample("a", 0, 1)]).unwrap();
        let boundary = 7 * crate::maintenance::DAY_MS;
        persist(&mut c, "e", boundary, &[]).unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM traffic_minutes", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        c.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(persist(&mut c, "e", boundary + 1, &[]).is_err());
        assert_eq!(
            c.query_row("SELECT count(*) FROM traffic_cursors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        c.execute_batch("DROP TRIGGER fail_audit").unwrap();
        persist(&mut c, "e", boundary + 1, &[]).unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM traffic_minutes", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            crate::store::list_audit(&c, 10, None).unwrap()[0].action,
            "maintenance.traffic"
        );
        let mut big = sample("max", 0, 0);
        big.traffic.bytes_in = i64::MAX as u64;
        persist(&mut c, "max", boundary + 2, &[big]).unwrap();
        let mut one = sample("max", 1, 0);
        one.traffic.bytes_in = 1;
        assert!(persist(&mut c, "max", boundary + 3, &[one]).is_err());
    }
}
