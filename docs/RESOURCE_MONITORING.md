# RustFRP 两端资源与稳定性监控

适用范围：用户授权的 `182.254.220.82` / `172.17.32.251` 双主机隔离环境。监控于 **2026-10-13 21:45（北京时间）** 启用，采用主机上的 systemd timer，每分钟执行一次 Python 标准库采样器，执行后退出；没有向 Rust 服务内加入常驻模块，也没有新建公开监控端口。

## 当前检查结论

六个主要 Rust 进程自 10:14:30 起连续运行约 11.5 小时；systemd 自动重启次数均为 0，最近 6 小时的系统 warning 级日志和应用 `[W]/[E]` 日志计数均为 0。15 秒低负载 pidstat 采样 CPU 均为 0.00%；监控前后 PID 不变。

21:48 左右的采样数据（RSS 来源 `smaps_rollup`；MiB = 1024² 字节）：

| 主机 / 服务 | RSS MiB | 线程 | 文件句柄 | 当前进程累计 CPU 秒 |
|---|---:|---:|---:|---:|
| 公网 Controller + 内置 Edge | 11.80 | 4 | 25 | 约 9.6 |
| 公网独立 Edge | 7.25 | 3 | 14 | 约 1.0 |
| 公网独立模式 Edge | 6.99 | 3 | 18 | 约 1.2 |
| 受管 Client | 8.05 | 3 | 11 | 约 0.4 |
| 客户端主机独立 Edge | 8.21 | 3 | 11 | 约 0.7 |
| 独立模式 Client | 8.16 | 3 | 11 | 约 0.4 |

当前低负载资源占用符合 Controller ≤12 MiB、Agent/Edge ≤10 MiB 的预算，Controller 已接近预算上限，需继续关注页面访问和管理员登录后的持续占用。Controller 历史 RSS 峰值约 27.6 MiB，systemd 组峰值约 28.8 MiB；这是进程启动以来的高水位，不能当作当前常驻内存。登录哈希等短期分配与峰值升高是否直接相关，需要带时间点的进一步采样验证，不能仅凭峰值确认原因。

公网主机可用内存约 4.7 GiB，客户端主机约 25.3 GiB；公网主机约 214 MiB swap 使用量属于整机，六个被监控 Rust 进程自身 `VmSwap` 均为 0。虚拟地址空间 VSZ 约 79–209 MiB，不代表对应大小的物理内存。RSS、PSS、私有内存与 cgroup 内存分别记录，不能混为同一口径。[Linux `/proc` 字段说明](https://docs.kernel.org/filesystems/proc.html)

目前证据只能支持“当前低负载正常、未发现近期异常重启”；分钟趋势才刚开始，尚不能证明 24 小时 / 7 天稳定性，也不能据此排除内存泄漏或高负载问题。

## 采集内容与开销

- 服务：状态、Result、PID、启动时间、systemd 自动重启次数、cgroup 内存/峰值/CPU/任务数、文件句柄上限。
- 主进程：RSS/PSS/私有内存、匿名/文件映射内存、RSS 高水位、VSZ、swap、CPU 累计时间和区间利用率、线程数、文件句柄数、socket 数、磁盘读写累计字节、运行时长。
- CPU 区间利用率 = 两次累计 CPU 时间差 / 单调时钟间隔 ×100%；100% 表示占用一个 CPU 核。使用真实 `SC_CLK_TCK`；首次采样、PID/启动标识变化或主机重启时返回空值，不冒充 0%。
- 日志：同时识别 journald warning/error 级日志以及 RustFRP stderr 的 `[W]/[E]` 和 panic/fatal 标记；只保存计数，不复制消息文本、令牌、命令行或环境变量。每个窗口每类最多 201 条，超限标记 `capped`，计数是下界。仅检查最近窗口，采样中断超过两分钟不能视为该时段无错误。
- 主机：负载、CPU 数、可用内存与 swap 使用量。
- 公网主机：通过已有、需要鉴权的回环 `/metrics` 采集客户端/节点在线数、隧道运行数与连接数；令牌从原环境文件读取，不复制进新配置或输出。
- 业务探测：两端每分钟向既有隔离测试 TCP 隧道 `27100` 发送 31 字节随机回显载荷，验证公网主机 → TLS 链路 → 客户端测试后端的路径；客户端侧使用公网入口验证返回。仅此探测成功不能代表所有 HTTP/UDP/私密协议都正常。

实际单次采集约 **0.08–0.21 秒**，采样 Python 进程瞬时峰值约 **23–26 MiB**，结束即退出；没有新增常驻 Agent 内存、每连接线程或持续网络连接。监控服务限制内存 64 MiB、任务数 16、执行时间 45 秒，并通过锁避免重复写入。[systemd timer 说明](https://github.com/systemd/systemd/blob/main/man/systemd.timer.xml)

## 预警策略

告警仅记入最新快照、历史 JSONL 和监控服务 journal；当前没有配置邮件、企业微信或其他外部通知，不会自动重启业务服务。

| 条件 | 触发方式 |
|---|---|
| 服务不活跃/采样失败、进程重启、自动重启次数增加、错误日志 | 本次采样提示 |
| Controller RSS >12 MiB / Agent RSS >10 MiB | 连续 5 次，约 5 分钟后提示 |
| 单进程 CPU >一个核的 10%、线程数 >16 | 连续 5 次后提示 |
| 文件句柄达到硬上限 80%、进程 swap >0 | 连续 3 次后提示 |
| metrics 读取失败、TCP 回显失败 | 本次采样提示 |
| 演示环境在线数低于预期 | Client ≥1、独立 Node ≥2、运行中隧道 ≥7；低于配置值时提示 |

这些是观察性预警，不是新的性能验收门槛。业务维护、刻意停用隧道或修改演示规模时，需要调整 `/etc/rustfrp-test/monitor.json` 的 `minimum_metrics`，避免将预期变更误报为故障。高负载分析要结合连接数和业务流量，不能仅凭越过空载阈值判断实现退化。

初始化前两条记录曾因 `journalctl --grep` 无匹配日志返回 1 而标记 `sampling_error`，于 21:46 修复并记录恢复；历史原样保留。这是采样器调试记录，不是业务服务故障。

## 文件与使用方式

仓库源码：`tools/monitor/sample.py`、`tools/monitor/report.py`；systemd 单元与两端配置样板在 `deploy/wan-test/`。两台服务器的脚本均安装在 `/opt/rustfrp-test/monitor/`，配置为 `/etc/rustfrp-test/monitor.json`。

数据目录 `/var/lib/rustfrp-test-monitor` 权限 0700，文件 0600。`latest.json` 保存最新状态，`samples-YYYY-MM-DD.jsonl` 按 UTC 日期记录；只保留当前与之前六天。每天单文件最多 4 MiB，保留一份 `.1` 轮转文件，历史数据最多约 56 MiB/主机；超过容量会舍弃更早样本，不保证保留每一分钟。

在任意一台服务器运行：

```sh
# 查看周期任务与采样结果
sudo systemctl list-timers rustfrp-test-monitor.timer
sudo python3 -m json.tool /var/lib/rustfrp-test-monitor/latest.json

# 流式汇总实际已采集的最近 24 小时；不足 24 小时会列出真实观察窗口
sudo python3 -B /opt/rustfrp-test/monitor/report.py --hours 24

# 查看告警与采样器执行错误
sudo journalctl -u rustfrp-test-monitor.service --since '24 hours ago'

# 手动采样 / 停用周期监控
sudo systemctl start rustfrp-test-monitor.service
sudo systemctl disable --now rustfrp-test-monitor.timer
```

远程检查示例：

```sh
ssh ubuntu@182.254.220.82 'sudo python3 -B /opt/rustfrp-test/monitor/report.py --hours 24'
ssh ubuntu@172.17.32.251 'sudo python3 -B /opt/rustfrp-test/monitor/report.py --hours 24'
```

安装只新增 `rustfrp-test-monitor.service/.timer` 与上述脚本/配置/数据目录，未重启或修改原 RustFRP、frps/frpc 服务。测试采样器与汇总逻辑：

```sh
python3 -B tools/monitor/test_sample.py -v
python3 -B tools/monitor/test_report.py -v
```
