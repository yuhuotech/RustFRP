# 独立节点（外置 Edge）

内置节点与 Controller 在同一进程里：Controller 退出，内置节点上的隧道也随之中断。
需要「**控制器离线时隧道继续工作**」（验收用例 AT-05）时，部署独立节点：一个单独运行的 `rustfrp-agent --mode edge` 进程，
把设备名册与配置缓存在本地，Controller 恢复后自动对账。

## 部署

1. Controller 开启节点端口（默认不开）：

   ```sh
   RUSTFRP_NODE_ADDR=0.0.0.0:7001 rustfrp-server run
   ```

   节点端口复用内置 Edge 的 TLS 证书（`RUSTFRP_EDGE_TLS_CERT/KEY` 或自动生成的自签名证书）。
2. 面板「节点 → 接入新节点」生成一次性令牌和命令，在节点服务器上执行：

   ```sh
   rustfrp-agent --mode edge \
     --controller controller.example.com:7001 --enroll-token <token> \
     --public-host edge.example.com --tls-pin <controller 证书指纹> \
     -c /etc/rustfrp/frps.toml
   ```

   * `--public-host`：客户端访问这个节点所用的域名 / IP（首次必填，面板据此生成设备接入命令）。
   * `-c`：可选的 `frps.toml`，只用于**监听参数**（`bindAddr/bindPort`、`vhostHTTPPort/vhostHTTPSPort`、`subDomainHost`、
     `allowPorts`、`transport.tls.certFile/keyFile`、日志）。其中的 `auth.token` 在受管模式下被忽略并给出提示——设备用各自的密钥认证。
   * 状态目录（`--state-dir`，默认 `~/.rustfrp`）：`identity.json`（节点密钥与 ID）、`edge-cert.pem/edge-key.pem`（给设备的 TLS 证书，自动生成）、
     `devices.json`（设备名册缓存，权限 0600）。
3. 注册为系统服务：`rustfrp-agent service install -- --mode edge --controller … -c …`（见 `installer/README.md`）。
4. 让设备接入这个节点：面板「接入新客户端」里选择「接入到节点」，命令里的地址与证书指纹就是该节点的。

## 行为

* **离线可用**：设备登录只查节点本地名册（Ed25519 签名）；Controller 不可达时，已连接的隧道继续转发，设备断线重连也能登录。
  首次接入新设备需要 Controller 在线（要消耗令牌、落库）。
* **配置热下发**：Controller 上改隧道 / 停用 / 删除 / 强制下线，经节点链路增量推送，节点就地对账，不中断未改动的隧道。
* **回报**：设备上下线、每条代理的运行状态与端口、5 秒一次的累计流量样本（落到同一张分钟桶，面板图表照常工作）。
  断线重连后节点用一次全量对账补齐状态，Controller 不会因节点失联就把隧道标成停止。
* **安全**：节点登录用「Controller 现场下发的随机挑战 + Ed25519 签名」，无法重放；节点只能写属于自己所服务设备的数据
  （其他节点的设备 / 隧道 / 流量样本一律丢弃）；节点令牌与设备令牌互不通用，为某节点签发的设备令牌在其他节点（含内置节点）上不会被消耗。
  停用 / 删除节点会立即切断它的控制链路；节点已缓存的设备在节点重启前仍可使用。
* **内存**：节点名册最多缓存 4096 台设备；设备配置全量快照按设备逐条传输，单帧远小于 64 KiB；流量批次最多同时等待 8 个确认，
  与 Controller 失联超过 5 分钟会回收已停止代理的计数条目。

## 限制

* 一台设备同一时刻只属于一个节点。在线设备可由管理员执行迁移：Controller 审计理由、清理旧节点代理并通知旧 Edge，Agent 固定目标 Edge TLS 指纹后重连；目标节点需已接入且支持设备现有隧道。若现有 Visitor 与其目标隧道会被分到不同节点，Controller 会拒绝迁移并要求先移除/重建该 Visitor。迁移后 30 天内旧节点保留受限重定向记录，供旧控制链路中的 Agent 完成重连。
* 外置节点上的隧道 `allowPorts`、虚拟主机端口、子域名后缀由该节点自己的配置决定，Controller 的 `RUSTFRP_ALLOW_PORTS` 等只约束内置节点。
* 概览里「当前连接」「本次运行累计」是内置节点的实时计数；独立节点的流量只进入 24 小时 / 7 天历史（分钟桶），不进实时计数。
* 节点与设备数量上限、跨节点的负载均衡 / 健康检查尚未实现。
* 2026-10-13 已通过两台物理主机的迁移、控制链路重连、流量上报和 AT-05 离线恢复。NAT 侧 Edge 通过现有 Tailscale WAN 私网访问；公网服务端还验过内置/外置 Edge 往返迁移。具体网络边界见 [双主机验收记录](WAN_ACCEPTANCE_20261013.md)。
