-- stcp / sudp 隧道的访问密钥（服务端生成，访问者由 Controller 代为出示，不经 REST 返回）。
ALTER TABLE tunnels ADD COLUMN secret_key TEXT NOT NULL DEFAULT '';

-- 访问者：在某台设备上监听本机端口，把连接经 Edge 送到 stcp / sudp 隧道。
CREATE TABLE visitors (
  id         TEXT PRIMARY KEY,
  client_id  TEXT NOT NULL REFERENCES clients(id),
  name       TEXT NOT NULL,
  tunnel_id  TEXT NOT NULL,
  bind_addr  TEXT NOT NULL DEFAULT '127.0.0.1',
  bind_port  INTEGER NOT NULL DEFAULT 0,
  enabled    INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX visitors_client ON visitors(client_id) WHERE deleted_at IS NULL;
CREATE INDEX visitors_tunnel ON visitors(tunnel_id) WHERE deleted_at IS NULL;
