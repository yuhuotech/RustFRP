//! SQLite 连接：单连接 + `spawn_blocking`（管理面写入量很小，一个连接足够，常驻内存最低）。

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;

/// 存储层错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    /// 数据库错误。
    #[error("database: {0}")]
    Sqlite(String),
    /// 目标不存在。
    #[error("not found")]
    NotFound,
    /// 与现有数据冲突。
    #[error("conflict: {0}")]
    Conflict(String),
    /// 参数不合法。
    #[error("invalid: {0}")]
    Invalid(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound,
            rusqlite::Error::SqliteFailure(f, msg)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StoreError::Conflict(msg.unwrap_or_else(|| "constraint violation".into()))
            }
            other => StoreError::Sqlite(other.to_string()),
        }
    }
}

const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_init.sql"))];

/// 执行未应用的迁移（以 `PRAGMA user_version` 记录进度）。
pub fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        tx.commit()?;
    }
    Ok(())
}

fn tune(conn: &Connection) -> Result<(), StoreError> {
    // 内存纪律：缓存页封顶 2 MiB，不使用 mmap。
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA cache_size = -2000;
         PRAGMA mmap_size = 0;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// 数据库句柄，可克隆共享。
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// 打开（或创建）数据库文件并迁移；启用 WAL。
    pub fn open(path: &Path) -> Result<Db, StoreError> {
        let mut conn = Connection::open(path)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        tune(&conn)?;
        migrate(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 内存数据库（测试）。
    pub fn open_memory() -> Result<Db, StoreError> {
        let mut conn = Connection::open_in_memory()?;
        tune(&conn)?;
        migrate(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 在阻塞线程里执行数据库操作。
    pub async fn call<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut guard)
        })
        .await
        .map_err(|e| StoreError::Sqlite(format!("worker failed: {e}")))?
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_are_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
        for t in [
            "admins",
            "admin_sessions",
            "enroll_tokens",
            "clients",
            "tunnels",
            "audit_log",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{t}");
        }
    }

    #[test]
    fn error_mapping() {
        assert_eq!(
            StoreError::from(rusqlite::Error::QueryReturnedNoRows),
            StoreError::NotFound
        );
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a TEXT UNIQUE); INSERT INTO t VALUES ('x');")
            .unwrap();
        let e = conn.execute("INSERT INTO t VALUES ('x')", []).unwrap_err();
        assert!(matches!(StoreError::from(e), StoreError::Conflict(_)));
        let e = conn.execute("SELECT * FROM nope", []).unwrap_err();
        assert!(matches!(StoreError::from(e), StoreError::Sqlite(_)));
    }

    #[tokio::test]
    async fn file_database_uses_wal_and_persists() {
        let path = std::env::temp_dir().join(format!("rustfrp-db-{}.sqlite", std::process::id()));
        {
            let db = Db::open(&path).unwrap();
            db.call(|c| Ok(c.execute("INSERT INTO audit_log (ts,actor,action,target_kind,target_id,reason,result) VALUES (1,'a','b','c','d','e','ok')", [])?))
                .await
                .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let n: i64 = db
            .call(|c| Ok(c.query_row("SELECT count(*) FROM audit_log", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 1);
        let mode: String = db
            .call(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(mode, "wal");
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
        assert!(Db::open(Path::new("/nonexistent-dir/x/y.db")).is_err());
    }

    #[tokio::test]
    async fn call_propagates_errors() {
        let db = Db::open_memory().unwrap();
        let r: Result<(), StoreError> = db.call(|_| Err(StoreError::Invalid("nope".into()))).await;
        assert_eq!(r, Err(StoreError::Invalid("nope".into())));
    }
}
