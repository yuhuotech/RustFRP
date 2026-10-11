//! 运维：真实 WAL 数据库一致性备份、有界删除、保留边界与失败回滚。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use rustfrp_server::{db::Db, maintenance, models::Role, store};
use std::path::PathBuf;

fn folder() -> PathBuf {
    let p = std::env::temp_dir().join(format!("rustfrp-maintenance-{}", store::new_id()));
    std::fs::create_dir(&p).unwrap();
    p
}

#[tokio::test]
async fn wal_snapshot_is_consistent_private_and_never_overwrites() {
    let dir = folder();
    let source = dir.join("live.db");
    let dest = dir.join("copy 'with spaces'.db");
    let db = Db::open(&source).unwrap();
    db.call(|c| {
        store::create_admin(c, "root", "test-hash", Role::Owner, 1)?;
        store::audit(c, "root", "test", "db", "", "test", "ok", "", 2)?;
        Ok(())
    })
    .await
    .unwrap();
    db.backup(dest.clone()).await.unwrap();
    let restored = Db::open(&dest).unwrap();
    let (names, audits, integrity, version) = restored
        .call(|c| {
            Ok((
                store::list_admins(c)?
                    .into_iter()
                    .map(|a| a.name)
                    .collect::<Vec<_>>(),
                store::list_audit(c, 10, None)?.len(),
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(names, ["root"]);
    assert_eq!(audits, 1);
    assert_eq!(integrity, "ok");
    assert_eq!(version, 13);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let old = std::fs::read(&dest).unwrap();
    assert!(db.backup(dest.clone()).await.is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), old);
    let empty = dir.join("existing-empty.db");
    std::fs::write(&empty, "").unwrap();
    assert!(db.backup(empty.clone()).await.is_err());
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);
    assert!(db.backup(source.clone()).await.is_err());
    db.call(|c| {
        store::create_admin(c, "later", "test-hash", Role::Viewer, 3)?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        restored
            .call(|c| Ok(store::list_admins(c)?.len()))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        db.call(|c| Ok(store::list_admins(c)?.len())).await.unwrap(),
        2
    );
    drop(restored);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn backup_failures_leave_no_published_or_partial_file() {
    let dir = folder();
    let db = Db::open_memory().unwrap();
    assert!(db.backup(dir.join("missing/f.db")).await.is_err());
    db.call(|c| {
        c.execute_batch("BEGIN")?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(db.backup(dir.join("snapshot.db")).await.is_err());
    assert!(!dir.join("snapshot.db").exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    db.call(|c| {
        c.execute_batch("ROLLBACK")?;
        Ok(())
    })
    .await
    .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn retention_is_bounded_keeps_boundaries_and_audits_in_same_transaction() {
    let db = Db::open_memory().unwrap();
    let now = 10 * maintenance::DAY_MS;
    db.call(move |c| {
        let admin = store::create_admin(c, "root", "h", Role::Owner, 0)?;
        for i in 0..5 {
            store::create_session(c, &format!("old-{i}"), &admin.id, "csrf", 0, now)?;
            store::create_enroll_token(c, &format!("h-{i}"), "test", "root", 0, now)?;
            store::audit(
                c,
                "root",
                "old",
                "t",
                "",
                "test",
                "ok",
                "",
                now - maintenance::DAY_MS - 1,
            )?;
        }
        store::create_session(c, "fresh", &admin.id, "csrf", now, 1)?;
        store::audit(
            c,
            "root",
            "boundary",
            "t",
            "",
            "test",
            "ok",
            "",
            now - maintenance::DAY_MS,
        )?;
        store::audit(c, "root", "future", "t", "", "test", "ok", "", now + 1)?;
        Ok(())
    })
    .await
    .unwrap();
    let n = db
        .call(move |c| maintenance::purge(c, now, 1, 2))
        .await
        .unwrap();
    assert_eq!(
        n,
        maintenance::Purged {
            sessions: 2,
            tokens: 2,
            audits: 2,
            api_tokens: 0,
        }
    );
    assert_eq!(
        db.call(|c| Ok(store::list_audit(c, 50, None)?
            .iter()
            .filter(|a| a.action == "maintenance.purge")
            .count()))
            .await
            .unwrap(),
        1
    );
    for _ in 0..3 {
        db.call(move |c| maintenance::purge(c, now, 1, 2))
            .await
            .unwrap();
    }
    let list = db.call(|c| store::list_audit(c, 50, None)).await.unwrap();
    assert!(!list.iter().any(|a| a.action == "old"));
    assert!(list.iter().any(|a| a.action == "boundary"));
    assert!(list.iter().any(|a| a.action == "future"));
    assert!(db
        .call(move |c| store::find_session(c, "fresh", now))
        .await
        .is_ok());
    let remaining = db
        .call(move |c| maintenance::purge(c, now, 0, 2))
        .await
        .unwrap();
    assert_eq!(remaining, maintenance::Purged::default());
    for (days, batch) in [(-1, 2), (3651, 2), (1, 0), (1, 1001)] {
        assert!(db
            .call(move |c| maintenance::purge(c, now, days, batch))
            .await
            .is_err());
    }
}

#[tokio::test]
async fn failed_audit_rolls_back_session_deletion_and_disabled_retention_keeps_history() {
    let db = Db::open_memory().unwrap();
    db.call(|c| {
        let a = store::create_admin(c, "root", "h", Role::Owner, 0)?;
        store::create_session(c, "old", &a.id, "csrf", 0, 1)?;
        store::audit(c, "root", "old", "t", "", "why", "ok", "", 0)?;
        Ok(())
    })
    .await
    .unwrap();
    let n = db
        .call(|c| maintenance::purge(c, 10 * maintenance::DAY_MS, 0, 2))
        .await
        .unwrap();
    assert_eq!(n.sessions, 1);
    assert_eq!(n.audits, 0);
    db.call(|c| {
        let a = store::list_admins(c)?.remove(0);
        store::create_session(c, "another", &a.id, "csrf", 0, 1)?;
        c.execute_batch("DROP TABLE audit_log")?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(db
        .call(|c| maintenance::purge(c, 10 * maintenance::DAY_MS, 1, 2))
        .await
        .is_err());
    assert_eq!(
        db.call(
            |c| Ok(c.query_row("SELECT count(*) FROM admin_sessions", [], |r| r
                .get::<_, i64>(0))?)
        )
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn maintenance_timer_runs_and_cancellation_stops_future_batches() {
    use rustfrp_agent::log::{Level, Logger};
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let db = Db::open_memory().unwrap();
    db.call(|c| {
        let a = store::create_admin(c, "root", "h", Role::Owner, 0)?;
        store::create_session(c, "old", &a.id, "csrf", 0, 1)?;
        Ok(())
    })
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let run = tokio::spawn(maintenance::run(
        db.clone(),
        90,
        2,
        Duration::from_millis(5),
        cancel.clone(),
        Logger::silent(),
    ));
    for _ in 0..100 {
        if db
            .call(|c| {
                Ok(c.query_row("SELECT count(*) FROM admin_sessions", [], |r| {
                    r.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap()
            == 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        db.call(
            |c| Ok(c.query_row("SELECT count(*) FROM admin_sessions", [], |r| r
                .get::<_, i64>(0))?)
        )
        .await
        .unwrap(),
        0
    );
    cancel.cancel();
    run.await.unwrap();
    let data = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();
    let run = tokio::spawn(maintenance::run(
        db,
        -1,
        2,
        Duration::from_millis(5),
        cancel.clone(),
        Logger::new(Level::Error, Box::new(Capture(data.clone()))),
    ));
    for _ in 0..100 {
        if !data.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cancel.cancel();
    run.await.unwrap();
    assert!(String::from_utf8(data.lock().unwrap().clone())
        .unwrap()
        .contains("maintenance failed"));
}

#[tokio::test]
async fn invalid_backup_paths_are_rejected_and_staging_is_removed() {
    let db = Db::open_memory().unwrap();
    assert!(db.backup(PathBuf::new()).await.is_err());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let dir = folder();
        let bad = dir.join(std::ffi::OsString::from_vec(b"bad-\xff".to_vec()));
        assert!(matches!(
            db.backup(bad.join("copy.db")).await,
            Err(rustfrp_server::db::StoreError::Invalid(_))
        ));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn read_only_backup_preserves_older_schema_and_never_creates_source() {
    let dir = folder();
    let source = dir.join("v2.db");
    let dest = dir.join("copy.db");
    assert!(Db::open_read_only(&source).is_err());
    assert!(!source.exists());
    let c = rusqlite::Connection::open(&source).unwrap();
    c.execute_batch(include_str!("../migrations/0001_init.sql"))
        .unwrap();
    c.execute_batch(include_str!("../migrations/0002_tunnel_domains.sql"))
        .unwrap();
    c.execute_batch("PRAGMA user_version=2").unwrap();
    drop(c);
    let db = Db::open_read_only(&source).unwrap();
    db.backup(dest.clone()).await.unwrap();
    assert!(db
        .call(|c| {
            c.execute_batch("CREATE TABLE forbidden (id INTEGER)")?;
            Ok(())
        })
        .await
        .is_err());
    for file in [&source, &dest] {
        let c = rusqlite::Connection::open(file).unwrap();
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn audit_insert_failure_rolls_back_every_retention_delete() {
    let db = Db::open_memory().unwrap();
    db.call(|c| {
        let a=store::create_admin(c,"root","h",Role::Owner,0)?;
        store::create_session(c,"old",&a.id,"csrf",0,1)?;
        store::create_enroll_token(c,"token-hash","test","root",0,1)?;
        store::audit(c,"root","old","db","","why","ok","",0)?;
        c.execute_batch("CREATE TRIGGER deny_maintenance BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END")?;
        Ok(())
    }).await.unwrap();
    assert!(db
        .call(|c| maintenance::purge(c, 10 * maintenance::DAY_MS, 1, 500))
        .await
        .is_err());
    let counts = db
        .call(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM admin_sessions", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                store::list_enroll_tokens(c)?.len(),
                store::list_audit(c, 50, None)?.len(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(counts, (1, 1, 1));
}

#[tokio::test]
async fn automation_token_retention_is_bounded_and_audited() {
    let db = Db::open_memory().unwrap();
    db.call(|c| {
        let a = store::create_admin(c, "token-owner", "hash", Role::Owner, 0)?;
        for n in 0..4 {
            c.execute(
                "INSERT INTO api_tokens VALUES(?1,?2,?3,'test','read',0,?4,?5)",
                rusqlite::params![
                    n.to_string(),
                    format!("hash{n}"),
                    a.id,
                    if n == 3 { 200 } else { 100 },
                    if n == 3 { Some(50) } else { None }
                ],
            )?;
        }
        c.execute(
            "INSERT INTO api_tokens VALUES('active','active',?1,'test','read',0,200,NULL)",
            [a.id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let first = db.call(|c| maintenance::purge(c, 100, 0, 2)).await.unwrap();
    assert_eq!(first.api_tokens, 2);
    let second = db.call(|c| maintenance::purge(c, 100, 0, 2)).await.unwrap();
    assert_eq!(second.api_tokens, 2);
    assert_eq!(
        db.call(
            |c| Ok(c.query_row("SELECT count(*) FROM api_tokens", [], |r| r
                .get::<_, i64>(0))?)
        )
        .await
        .unwrap(),
        1
    );
    assert!(db
        .call(|c| Ok(store::list_audit(c, 10, None)?
            .iter()
            .all(|a| a.action == "maintenance.purge")))
        .await
        .unwrap());
}
