-- 外置节点（独立 Edge 进程）；内置节点不入表（id 约定为空串）。
CREATE TABLE nodes (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  pubkey          TEXT NOT NULL,
  public_host     TEXT NOT NULL DEFAULT '',
  edge_port       INTEGER NOT NULL DEFAULT 0,
  tls_fingerprint TEXT NOT NULL DEFAULT '',
  vhost_http      INTEGER,
  vhost_https     INTEGER,
  subdomain_host  TEXT NOT NULL DEFAULT '',
  allow_ports     TEXT NOT NULL DEFAULT '[]',
  version         TEXT NOT NULL DEFAULT '',
  os              TEXT NOT NULL DEFAULT '',
  created_at      INTEGER NOT NULL,
  last_seen_at    INTEGER,
  disabled_at     INTEGER,
  deleted_at      INTEGER
);
-- 设备由哪个节点服务（空串 = 内置节点）。
ALTER TABLE clients ADD COLUMN node_id TEXT NOT NULL DEFAULT '';
CREATE INDEX clients_node ON clients(node_id) WHERE deleted_at IS NULL;
-- 接入令牌：kind = client | node；node_id 指定设备令牌对应的节点（空串 = 内置）。
ALTER TABLE enroll_tokens ADD COLUMN kind TEXT NOT NULL DEFAULT 'client';
ALTER TABLE enroll_tokens ADD COLUMN node_id TEXT NOT NULL DEFAULT '';
