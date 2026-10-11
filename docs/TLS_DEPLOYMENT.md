# Edge 证书与固定指纹

Controller 内置 Edge 可配置 `RUSTFRP_EDGE_TLS_CERT` / `RUSTFRP_EDGE_TLS_KEY`，必须同时设置，加载 PEM 链与匹配私钥，单文件上限 64 KiB。证书必须覆盖客户端使用的地址；可通过 Agent `--ca-file` 严格校验 CA 与域名。

未指定 PEM 时，Controller 首次启动在数据库同名的 `.edge-tls/` 目录生成持久自签名证书，目录 Unix 0700、文件 0600。下次启动复用；损坏或缺失材料报错，绝不自动换证书。备份/迁移需额外保存该目录（数据库快照不含文件），丢失后需明确重新接入/更换指纹。生成临时目录只有在所有材料同步落盘后才发布；失败可能留下 `.partial`，不得当作有效身份。

管理面生成的接入命令固定实际叶证书 SHA256（`--tls-pin`），Unix/PowerShell 安装脚本透传该值；Node REST 显示公开指纹。桌面可填写同一指纹并持久保存。固定值必须来自可信 HTTPS 管理面或本机管理员，不使用首次未验证连接自动信任。单独固定指纹时验证证书匹配和握手签名；同时配置 CA 时必须通过 CA/域名与指纹校验。证书轮换后需有计划更新客户端固定值，不静默回退。

独立模式保持 frp 配置语义：仍用 `transport.tls.trustedCaFile` 进行 CA 校验，未配置时沿用 frp 默认行为。`--tls-pin` 仅适用于受管模式，不能静默忽略。HTTPS 类型隧道继续按 SNI 透传，不进行 TLS 终止。

`rustfrp-protocol/fixtures/edge-test-{cert,key}.pem` 仅为自动化测试的公开、无业务用途的密钥材料，严禁用于部署。管理 HTTP 接口仍由反向代理提供 HTTPS，证书配置只保护数据面 Edge。非回环 `RUSTFRP_HTTP` 要求同时设置 `RUSTFRP_SECURE_COOKIE=true` 和 `RUSTFRP_PUBLIC_URL=https://...`，否则 Controller 拒绝启动。反向代理模板见 [`deploy/README.md`](../deploy/README.md)。
