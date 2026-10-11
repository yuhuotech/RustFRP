-- 批量清理按到期时间取固定数量，避免扫描整个会话/令牌表。
CREATE INDEX admin_sessions_expiry ON admin_sessions(expires_at, token_hash);
CREATE INDEX enroll_tokens_expiry ON enroll_tokens(expires_at, id);
