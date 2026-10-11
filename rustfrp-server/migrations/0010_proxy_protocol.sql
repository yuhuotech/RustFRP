-- PROXY protocol 版本：0 = 不写，1 / 2 = v1 / v2（tcp / http / https 隧道）。
ALTER TABLE tunnels ADD COLUMN proxy_protocol INTEGER NOT NULL DEFAULT 0;
