-- http / https 隧道的域名相关配置；域名以逗号分隔存放（小写）。
ALTER TABLE tunnels ADD COLUMN custom_domains TEXT NOT NULL DEFAULT '';
ALTER TABLE tunnels ADD COLUMN subdomain TEXT NOT NULL DEFAULT '';
ALTER TABLE tunnels ADD COLUMN host_header_rewrite TEXT NOT NULL DEFAULT '';
