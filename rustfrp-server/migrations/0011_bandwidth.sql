-- 带宽上限（字节 / 秒，0 = 不限）与限速位置（0 = 客户端，1 = 服务端）。
ALTER TABLE tunnels ADD COLUMN bandwidth_bps INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tunnels ADD COLUMN bandwidth_server INTEGER NOT NULL DEFAULT 0;
