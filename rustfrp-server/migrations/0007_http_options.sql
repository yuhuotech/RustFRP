-- http 隧道的路径前缀 / Basic 认证 / 按用户选路 / 请求头：整体存成 JSON（空串 = 全部默认）。
ALTER TABLE tunnels ADD COLUMN http_opts TEXT NOT NULL DEFAULT '';
