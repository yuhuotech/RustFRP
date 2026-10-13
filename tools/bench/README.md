# 内存基准

`python3 tools/bench/mem.py` 启动真实的 edge + client，建立隧道与并发连接，读取常驻内存（RSS）。
`make bench-mem` 会先构建 release 并以 10 MB 为上限（原则一）；超出即失败。

## 实测（macOS arm64，release，系统分配器；2026-10-11，提交时）

| 场景 | edge | client |
|---|---|---|
| 空载，10 条隧道 | 4.00 MB | 4.11 MB |
| 100 条空闲连接 | 5.92 MB | 5.94 MB |
| 20 条连接各回显 512 KiB 后 | 6.38 MB | 6.44 MB |
| 100 条隧道、1000 条空闲连接 | 22.4 MB | 23.0 MB |

该历史版本典型场景（≤100 连接）约 6 MB，每条空闲连接约 18 KB（两个 8 KiB 转发缓冲 + 任务与流状态）。2026-10-13 双向转发缓冲调整为每方向 4 KiB，插件读缓冲与限速突发额度不变。
二进制 2.3 MiB。

## Linux x86_64 双主机联调版（2026-10-13）

Linux 默认系统分配器、默认两个 Tokio worker，未指定 CPU 亲和或分配器环境变量。10 条隧道的空载 Edge/Client 为 7.54/7.84 MiB，100 个空闲连接 8.56/9.27 MiB，20 次 512 KiB 回显后及关闭后为 8.94/9.57 MiB。未改动前 Client 峰值 10.28–10.52 MiB，超过 10 MiB 门槛；4 KiB 双向缓冲减少约 800 KiB 后通过。真实 WAN 40 路并发和插件/限速/PROXY 转发另有逐字节断言，见 [验收记录](../../docs/WAN_ACCEPTANCE_20261013.md)。

Controller 的 nginx 静态资源/Argon2 请求后 RSS 曾约 57 MiB。测试 systemd 配置固定 glibc mmap/trim 阈值并限制 arena 后约 11 MiB；这是部署配置，不代表已完成高负载 Controller 性能验收。

## 已知的优化空间（不影响当前达标，按需推进）
- 转发缓冲改为「读到数据才租用」的按需缓冲，空闲连接可降到约 2–4 KB。
- 连接关闭后 RSS 不回落（分配器保留）：Linux 上评估 `mimalloc` / `MALLOC_TRIM`。
- yamux 每流窗口固定 256 KiB（上限而非预分配）；若恶意对端占满窗口，内存会增长，必要时在 `mux.rs` 内替换为固定小窗口的自研实现。

> 加入 UDP / HTTP / HTTPS 路由后复测（同一方法）：见 `make bench-mem` 输出；门槛仍为 10 MB（100 连接场景）。
