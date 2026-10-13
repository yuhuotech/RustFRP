# RustFRP 双主机部署与联调验收（2026-10-13）

管理台：**https://rustfrp.dev99.cn**。部署基于 `259e71c` 加本轮提交中的 CLI、兼容报告、内存和登录说明修复。

## 主机、隔离和登录

| 用途 | 主机 | 持久服务 |
|---|---|---|
| 公网 Controller / 内置 Edge | `ubuntu@182.254.220.82` | `rustfrp-test-server` |
| 公网独立 Edge | 同上 | `rustfrp-test-node` |
| 受管客户端 | `ubuntu@172.17.32.251` | `rustfrp-test-client` |
| 客户端机器上的独立 Edge | 同上 | `rustfrp-test-lan-node`（`100.75.133.24:27500`，现有 Tailscale 网络） |
| 独立模式 frps / frpc | 两端各一个 | `rustfrp-test-standalone-edge` / `rustfrp-test-standalone-client` |
| 回环测试后端 | 两端 | `rustfrp-test-backends` |

全部程序在 `/opt/rustfrp-test`，配置在 `/etc/rustfrp-test`，数据库、身份和测试证书在 `/var/lib/rustfrp-test`；独立账号 `rustfrp-test`。没有替换原 frp 文件、配置、服务或端口。nginx 只新增 `/etc/nginx/conf.d/rustfrp-test.conf` 并在 `nginx -t` 成功后 reload。

管理员用户名 `root`；随机密码保存在本机 **`target/wan-test/credentials.json`**（目录 0700，文件 0600，Git 忽略）。该文件还包含测试 metrics 和独立模式凭据，不能提交或公开。浏览器验收用 `wan-ui` Owner 和 RBAC 用 `wan-viewer` 均禁用，未使用的本轮接入令牌已撤销。临时访问者和安装器设备已删除。

## 端口与网络

| 端口 | 主机 / 绑定 | 用途 |
|---|---|---|
| 80 / 443 | 公网主机，复用现有 nginx | 仅新增 `rustfrp.dev99.cn` vhost，HTTP 跳 HTTPS |
| 28080 TCP | 服务端 `127.0.0.1` | 管理 API，不直接公开 |
| 27000 / 27001 TCP | 服务端 | 受管 Client / 独立 Node 接入 |
| 27080 / 27443 / 27002 TCP | 服务端 | HTTP / HTTPS SNI / TCPMUX |
| 27100–27198 TCP/UDP | 服务端 | 受管隧道白名单；27199 保留并验证拒绝 |
| 27200 / 27280 / 27243 / 27202 TCP | 服务端 | 独立 Edge 的接入 / HTTP / HTTPS / TCPMUX |
| 27500 TCP | 客户端主机 | 第二台机器的 Edge，通过现有 Tailscale 私网访问 |
| 27600 / 27680 / 27643 / 27602 TCP | 服务端 | 独立 frps 配置测试 |
| 27300–27349 | 服务端 | 独立模式隧道白名单 |
| 28090 / 28091 TCP | 各端回环 | 独立 frpc / frps Dashboard |
| 28081 / 28082 / 28083 / 28084 / 28085 | 两端回环 | TCP / UDP / HTTP / HTTPS / PROXY 后端 |

公网 UDP 27102 最初未收到入站包；服务端 eth0 抓包为 0，回环入口经真实跨主机链路正常。用户放通云防火墙 UDP 27100–27198 后，外网 1 / 128 / 1200 字节回显全部通过。独立模式 UDP 27301 使用服务端回环入口验收，跨主机传输仍走真实独立 TLS 链路；未把它记录成额外公网 UDP 入站验收。

公网主机不能回连自身公网 IP。服务器上 RustFRP Agent 单元通过 `BindReadOnlyPaths` 使用 `/etc/rustfrp-test/agent-hosts`，将本服务的 `rustfrp.dev99.cn` 解析为回环；全局 `/etc/hosts`、DNS 与原 frp 不变。不同主机 Edge 的迁移和 AT-05 使用现有 Tailscale WAN 私网，不能称为 NAT 侧节点直接公开公网端口。

## 实际通过的联调

| 范围 | 真实断言与证据 |
|---|---|
| 管理面 HTTPS | 公网证书验证、HTTP 重定向、同源静态资源和 CSP；Secure / HttpOnly / SameSite=Strict Cookie；未登录、错误 Origin、错误 CSRF 被拒绝 |
| 接入 / 本机控制 | 固定 Edge TLS 指纹、一次性令牌换 Ed25519 身份、身份文件 0600、本机回环状态与有界日志；原 frp 两份原始配置只读 verify 成功 |
| TCP / UDP | 300000 字节随机数据摘要和逐字节回显；TCP AES-256-GCM/LZ4；公网 UDP 三种报文大小；40 路并发 TCP 各自精确回显 |
| HTTP | 域名、子域名、路径前缀、Basic Auth、Host 改写、请求/响应头；未知域名和不匹配路径 404；按 Basic 用户选路；负载均衡两成员轮转 |
| HTTPS / TCPMUX | 后端证书校验的 SNI 透传；HTTP CONNECT 建隧道后真实数据回显 |
| STCP / SUDP | 服务端访问者访问远端客户端后端；Visitor 加密和压缩；目标 SUDP 代理不错误配置 TCP 流变换 |
| 8 类插件 | socks5 RFC1929、http_proxy CONNECT、static_file 内容/鉴权/穿越拒绝、Unix socket、http2https、https2http、https2https、tls2raw，全部跨主机实际收发 |
| 通用选项 | PROXY v1/v2 后端先收到正确头；Client/Edge 双向共享 64 KiB/s 限速约 5 秒；连接上限拒绝第二条连接并在关闭后释放；HTTP 健康失败撤回、恢复重新转发 |
| 管理操作 | 隧道 CRUD、批量启停、热下发保留未修改连接；设备标签、停用/恢复、kick 重连；远程重启退出码 75 由真实 systemd 恢复 |
| 节点 | 独立节点接入、能力上报、改名、启停；内置与外置往返迁移后 TCP/UDP/HTTPS 恢复；跨物理主机节点往返迁移、固定指纹、目标持久化 |
| AT-05 | Controller 停止后外置 Edge 继续转发；远端 Client 重启仍可凭缓存名册签名登录；Controller 恢复后全量对账和历史流量上报。两台物理主机路径也通过 |
| 配置 | frpc 预览、摘要/警告确认导入、导出且保留原隧道；独立 TOML 启动、服务器证书和 trustedCaFile 域名校验、CLI status/reload/stop；真实 service install/status/stop/start/uninstall |
| 权限 / 审计 | Viewer 只读、Operator 管理边界、管理员改角色/密码/禁用、禁用后会话失效、read API Token 范围与撤销立即失效、操作 reason 与审计存在 |
| 可观测性 / 备份 | 24h/7d/隧道筛选历史、Linux RSS/CPU/运行时间、128 行日志和游标、Prometheus Bearer 鉴权；运行中 WAL 快照、integrity_check、schema=19、权限 0600 |
| 公网安装器 | HTTPS 下载 Linux x86_64 Agent 和 SHA256、固定 TLS pin、设备接入、实际 WAN 传输；使用的是本轮构建测试二进制，不是正式签名 Release |
| 浏览器 | 真实 Edge 浏览器验证错误/正确登录、客户端和隧道列表、创建隧道后网络回显、批量启停、设置/日志/管理员、24h/7d 图表、深浅主题、390px 窄屏与恢复会话；Controller 离线时旧图表提示刷新失败 |

每个网络断言的执行时间、摘要与必要数值保存于 `target/wan-test/results.json`；截图位于 `target/wan-test/screenshots`。脚本为 `tools/e2e/wan.py`、`wan_advanced.py`、`wan_backends.py`。这些脚本只用于本次已授权的隔离环境，不可直接对任意生产主机运行。

## 本轮修复与质量

1. `rustfrp-agent status/reload/stop -c ...` 按帮助说明默认 Client，显式 Edge 仍拒绝；新增测试先复现失败再修复。
2. 已支持的 `tcpmuxHTTPConnectPort` 不再被兼容报告误报为「后续阶段」；新增回归测试。
3. 双向普通转发每方向缓冲 8 → 4 KiB，100 连接减少约 800 KiB，不新增线程/队列；插件读缓冲与限速突发语义保持 8 KiB。
4. Controller 配置 glibc 固定 mmap/trim 阈值与 arena 上限，减少 Argon2 和静态资源请求后内存保留；真实面板请求后 RSS 约 11 MiB，目标 ≤12 MiB。
5. systemd Agent 单元包含 `RUSTFRP_SUPERVISED=1`；登录说明区分首位管理员 CLI 与之后的 Owner 管理。

Linux x86_64 Release，10 条隧道、100 连接，无 CPU 亲和/分配器参数限制的独立内存基准：

| 场景 | Edge MiB | Client MiB |
|---|---:|---:|
| 空载 | 7.55 | 7.69 |
| 100 空闲连接 | 8.57 | 9.12 |
| 20 次 512 KiB 回显后 | 8.91 | 9.46 |
| 关闭后 | 8.91 | 9.46 |

此前客户端峰值 10.28–10.52 MiB 超门槛，已实际修复，没有放宽门槛。本轮 67 组不同证据记录包含真实网络/管理/UI 断言；重复复测保留时间记录，早期公网 UDP 阻断已解决。

`make lint`、`make test`（601 项）、`make app-test`（158 项）、`make cov`、`make web-build`、`make e2e`、`make bench-mem` 均通过。macOS 最新峰值 Edge/Client 6.64/7.19 MiB。Flutter 默认跳过一项专用截图用例，本轮浏览器真实宽窄/深浅截图另行保存。完整最终日志在 `target/wan-test/final-*.log`。

| 范围 | 行 | 函数 | 分支 |
|---|---:|---:|---:|
| Core | 99.03% | 97.97% | 97.15% |
| Agent | 97.08% | 91.41% | 91.52% |
| Server | 97.16% | 95.79% | 92.88% |
| Flutter | 97.26% | — | — |

原 frps PID `213397`、frpc PID `5851` 保持不变，配置 SHA256 与部署前一致；原 SSH 穿透正常。最终保留 1 台受管客户端、7 条演示隧道及两个独立节点，临时测试资源已清理。

## 尚不能作为已完成的项目验收

- CA-07 / D-09 的**成功签名升级**：当前没有可信签名 Release、公钥/私钥发布配置。实际下发升级命令被明确拒绝，现有二进制与隧道仍正常；拒绝路径通过不等于成功升级。签名、摘要、回滚和安装计划由 Rust/Flutter 自动化用例验证。
- Windows/macOS/Linux Flutter 桌面安装包、托盘、自启动、通知、GUI 本机配置编辑的完整实机验收；本轮安装的是 Linux Agent 和 Web 面板，不能据此宣布三平台桌面全部通过。
- 独立 Dashboard TLS/assetsDir、TCPMUX passthrough、HTTP Upgrade/流水线/走私边界、配置 YAML/JSON/includes/INI、UDP/SUDP 所有额度边界：有现有 Linux/Rust 集成测试，未把每个组合都重复成跨主机公网用例。
- 官网 `rustfrp-web` 公网发布、正式安全审计、1000 连接和 Controller 50 设备、吞吐≥直连90%与新增延迟<5ms仍待专项验收。
- 测试后端/独立 TLS 证书有效期 30 天，只用于本隔离环境；管理台复用既有通配证书，其续期由原证书管理流程负责。

## 运维与回退

```sh
ssh ubuntu@182.254.220.82 'sudo systemctl status rustfrp-test-server rustfrp-test-node'
ssh ubuntu@172.17.32.251 'sudo systemctl status rustfrp-test-client rustfrp-test-lan-node'
ssh ubuntu@182.254.220.82 'sudo journalctl -u rustfrp-test-server -n 100 --no-pager'
ssh ubuntu@172.17.32.251 'sudo journalctl -u rustfrp-test-client -n 100 --no-pager'
```

数据库快照 `/var/lib/rustfrp-test/acceptance-backup.db`；备份 Edge 身份还需保留 `/var/lib/rustfrp-test/controller.edge-tls/` 和节点状态目录。回退仅停止/禁用 `rustfrp-test-*` 单元，并移除本轮 nginx vhost 后 `nginx -t` / reload；不要停止原 `frps` / `frpc` 或整台机器。复核原 frp PID、配置摘要和 SSH tunnel 与部署前一致。
