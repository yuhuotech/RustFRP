-- 管理员（不是设备，只能通过服务器 CLI 创建）
CREATE TABLE admins (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role          TEXT NOT NULL CHECK (role IN ('owner','admin','operator','viewer')),
  disabled_at   INTEGER,
  created_at    INTEGER NOT NULL
);

-- 管理会话：只存令牌哈希
CREATE TABLE admin_sessions (
  token_hash TEXT PRIMARY KEY,
  admin_id   TEXT NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
  csrf       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);

-- 一次性接入令牌：只存哈希
CREATE TABLE enroll_tokens (
  id         TEXT PRIMARY KEY,
  token_hash TEXT NOT NULL UNIQUE,
  name       TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  client_id  TEXT,
  revoked_at INTEGER
);

-- 客户端设备
CREATE TABLE clients (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  pubkey       TEXT NOT NULL,
  os           TEXT NOT NULL DEFAULT '',
  arch         TEXT NOT NULL DEFAULT '',
  version      TEXT NOT NULL DEFAULT '',
  revision     INTEGER NOT NULL DEFAULT 1,
  created_at   INTEGER NOT NULL,
  last_seen_at INTEGER,
  disabled_at  INTEGER,
  deleted_at   INTEGER
);

-- 隧道：spec（用户意图）与 status（Edge 回报）分栏
CREATE TABLE tunnels (
  id           TEXT PRIMARY KEY,
  client_id    TEXT NOT NULL REFERENCES clients(id),
  name         TEXT NOT NULL,
  kind         TEXT NOT NULL,
  local_ip     TEXT NOT NULL,
  local_port   INTEGER NOT NULL,
  remote_port  INTEGER NOT NULL,
  enabled      INTEGER NOT NULL DEFAULT 1,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  deleted_at   INTEGER,
  status_state TEXT NOT NULL DEFAULT 'pending',
  status_port  INTEGER NOT NULL DEFAULT 0,
  status_error TEXT NOT NULL DEFAULT ''
);
CREATE INDEX tunnels_client ON tunnels(client_id) WHERE deleted_at IS NULL;

-- 审计：只追加；target_id 为字符串，目标物理删除后仍可追溯
CREATE TABLE audit_log (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  ts          INTEGER NOT NULL,
  actor       TEXT NOT NULL,
  action      TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id   TEXT NOT NULL,
  reason      TEXT NOT NULL,
  result      TEXT NOT NULL,
  ip          TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_ts ON audit_log(ts);
