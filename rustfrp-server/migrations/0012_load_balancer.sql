-- 负载均衡组与健康检查（LbOpts 的 JSON；空 = 未配置）。
ALTER TABLE tunnels ADD COLUMN lb_opts TEXT NOT NULL DEFAULT '';
