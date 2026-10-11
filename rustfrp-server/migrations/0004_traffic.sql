-- 累计游标与分钟桶在同一事务更新；进程/代理实例隔离，避免重启重复计数。
CREATE TABLE traffic_cursors (
  epoch TEXT NOT NULL,
  tunnel_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  sampled_at INTEGER NOT NULL,
  bytes_in INTEGER NOT NULL CHECK(typeof(bytes_in)='integer' AND bytes_in>=0),
  bytes_out INTEGER NOT NULL CHECK(typeof(bytes_out)='integer' AND bytes_out>=0),
  connections_total INTEGER NOT NULL CHECK(typeof(connections_total)='integer' AND connections_total>=0),
  PRIMARY KEY(epoch, tunnel_id, generation)
);
CREATE INDEX traffic_cursors_time ON traffic_cursors(sampled_at);
CREATE TABLE traffic_minutes (
  tunnel_id TEXT NOT NULL,
  minute INTEGER NOT NULL,
  bytes_in INTEGER NOT NULL CHECK(typeof(bytes_in)='integer' AND bytes_in>=0),
  bytes_out INTEGER NOT NULL CHECK(typeof(bytes_out)='integer' AND bytes_out>=0),
  connections_total INTEGER NOT NULL CHECK(typeof(connections_total)='integer' AND connections_total>=0),
  connections_peak INTEGER NOT NULL CHECK(typeof(connections_peak)='integer' AND connections_peak>=0),
  samples INTEGER NOT NULL CHECK(typeof(samples)='integer' AND samples>=0),
  gap INTEGER NOT NULL,
  PRIMARY KEY(tunnel_id, minute)
);
CREATE INDEX traffic_minutes_time ON traffic_minutes(minute, tunnel_id);
