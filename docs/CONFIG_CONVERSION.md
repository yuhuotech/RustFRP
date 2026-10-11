# 旧版 INI 迁移

```sh
umask 077
rustfrp-agent convert --mode client -c frpc.ini > frpc.toml
rustfrp-agent verify --mode client -c frpc.toml
rustfrp-agent convert --mode edge -c frps.ini > frps.toml
```

转换只读源文件，标准 TOML 输出到 stdout；兼容报告和错误输出到 stderr。转换后的文件可能含认证令牌/共享密钥，请保持 0600 并避免入库。生产使用前先检查输出；两端都必须使用 RustFRP，线协议不与官方 frp 互通。

已覆盖：`[common]` 公共字段、TCP/UDP/HTTP/HTTPS/STCP/SUDP/XTCP/TCPMUX 代理字段、visitor、认证/日志/TLS/QUIC 配置映射、元数据、请求头、插件字段、服务端 HTTP 插件、allow_ports、range 端口展开；环境模板与 INI/TOML includes 会展开到单份输出，不依赖旧文件。每文件最多 1 MiB、最多 1024 段/展开代理，超限明确失败。未知键带 `[段名].字段` 报错，不生成缺字段的迁移结果。

转换保留配置不代表运行时支持：未实现的类型、visitor、传输/HTTP 选项和插件进入兼容报告，不能静默宣称已生效。字段映射依据仓库固定的上游 frp commit `2542d11` 的 `pkg/config/legacy/{client,server,proxy,visitor,conversion}.go`，参见 [官方转换源码](https://github.com/fatedier/frp/blob/2542d11/pkg/config/legacy/conversion.go)。后续升级须复核映射。

仍需补齐：多行/三引号等 legacy INI 语法、少数插件专属字段、dashboard TLS 关闭时的旧版字段忽略报告。当前遇到未支持字段/语法时明确拒绝转换，不写出残缺配置；这些限制仍在全项目验收清单内。
