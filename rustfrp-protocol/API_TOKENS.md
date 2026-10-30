# API Token

> 当前管理控制台只使用配置文件 `[admin].token`，见 `MANAGEMENT_AUTH.md`。本文是既有自动化 API 的兼容协议，不是新增管理登录/多管理 token 功能；普通控制台不再展示签发页面，自动化 token 不能用于 `/auth/login`。

Owner 使用 Cookie 会话创建/列出/撤销自动化令牌；写操作带 reason 并同事务审计。创建返回原文一次，数据库仅存 SHA-256。有效期 1..365 天，每个管理员最多 100 个未撤销未过期令牌。scope `read` 只允许 GET/HEAD/OPTIONS，`tunnels` 额外允许 `/api/v1/tunnels` 写操作，`admin` 允许管理资源写操作；Token 不可访问 `/auth`、`/admins` 或 `/api-tokens`，不能创建令牌或管理账号。有效角色不高于创建者当前角色；管理员禁用使令牌失效。

自动化使用 `Authorization: Bearer rfrp_...`，不使用 Cookie 或 CSRF。存在 Origin 的请求必须同源；跨源浏览器请求拒绝。冲突凭据以 Bearer 为准，无效 Bearer 不回落 Cookie。所有现有资源写入仍要求操作理由与事务审计；actor 中包含 token ID，以便追溯。令牌不得写日志。
