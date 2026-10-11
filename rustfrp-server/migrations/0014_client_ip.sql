-- 设备最近一次登录时 Edge 看到的来源地址（由 Edge 填写，不取自客户端自报）。
ALTER TABLE clients ADD COLUMN last_ip TEXT NOT NULL DEFAULT '';
