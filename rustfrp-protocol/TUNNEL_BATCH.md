# 隧道批量启停

`POST /api/v1/tunnels/batch`，管理会话 + CSRF + 同源校验；operator 及以上。请求 `{ids:[...],enabled:true,reason:"..."}`，1..100 个不同隧道 ID，ID 最多 64 字节。任意缺失目标或审计失败使整批回滚；每个目标的修改和 `tunnel.batch` 审计共事务。成功返回 `{updated:数量}`，随后按去重设备 ID 下发配置。数据已提交但下发失败时明确返回失败，配置不会回滚；重试同样的启停操作幂等。
