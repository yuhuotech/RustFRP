# 双主机隔离联调环境

本目录记录用户授权的 `182.254.220.82` / `172.17.32.251` 测试部署。端口、证据、登录凭据位置、已通过项、明确未验收项和回退步骤见 [双主机验收记录](../../docs/WAN_ACCEPTANCE_20261013.md)。

两端每分钟资源与业务采样已通过 `rustfrp-test-monitor.timer` 启用。配置为 `monitor-server.json` / `monitor-client.json`，任务不常驻、不重启业务服务；查看数据、预警阈值、保留与停用说明见 [资源监控说明](../../docs/RESOURCE_MONITORING.md)。

`server.env` 不含秘密；metrics 令牌在部署时生成，实际服务环境文件为 0600。Agent 单元由 `tools/e2e/wan.py` 按接入身份生成：只在首次接入临时携带一次性令牌，身份保存后移除令牌。使用独立账号、目录、单元名与端口，原 frp 不改动。

请先确认主机身份、端口空闲、sudo/SSH、现有 DNS/证书和 cloud firewall，再运行脚本；脚本会创建/修改本测试数据库中的设备、隧道、管理员并测试停止 Controller，不适用于已有业务数据的环境。

执行顺序：`bootstrap` → `managed` → `nodes` → `wan_advanced.py` → `standalone` → `installer` → `crosshost` → `extras` → `finish`。二进制和静态资源须先安装。`bootstrap` 只用于新建隔离数据库；其他阶段允许部分复测，但不是生产部署的幂等工具。
