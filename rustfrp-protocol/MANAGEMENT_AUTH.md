# 单管理 token 登录

Controller 配置文件（默认 `rustfrp-server.toml`，`run -c <path>` 或 `RUSTFRP_CONFIG` 指定）使用独立 `[admin] token = "..."`。示例值 `change-me` 和空值不能启动管理服务；修改后重启生效。可用 `RUSTFRP_ADMIN_TOKEN` 覆盖文件值。此凭据与 frp `[auth].token`、设备一次性接入令牌完全独立，不能互换。

Web 与桌面都向 `POST /api/v1/auth/login` 提交 `{"token":"..."}`，不再接受用户名密码。保持同源、自定义登录头、限流、HttpOnly/SameSite Cookie、CSRF、审计和响应容量限制。token 仅用于登录，不放在 URL、localStorage、桌面偏好或日志中。数据库只保存其 SHA-256 校验值，管理会话统一为 Owner；配置更换后启动时撤销旧管理会话，不改变隧道、设备身份或已有配置。

Web 默认当前服务器，可输入其他管理地址并跳转到目标站点的控制台后认证，不跨站转发 token。桌面默认 `http://127.0.0.1:9674`，通过原生传输选择管理地址。两端直接进入相同管理页面；后台设备运行与管理登录无关。旧管理员、自动化 API Token 的数据库及兼容接口保留供已有集成迁移，不再提供密码登录或多账号/管理 token 的页面。

增量常驻为一个有界配置 token（最多 1024 字节），认证比较固定长度摘要；不增加线程、无界会话缓存或依赖服务。
