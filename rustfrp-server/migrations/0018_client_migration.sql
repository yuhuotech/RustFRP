ALTER TABLE clients ADD COLUMN previous_node_id TEXT NOT NULL DEFAULT '';
ALTER TABLE clients ADD COLUMN redirect_address TEXT NOT NULL DEFAULT '';
ALTER TABLE clients ADD COLUMN redirect_port INTEGER NOT NULL DEFAULT 0;
ALTER TABLE clients ADD COLUMN redirect_tls_pin TEXT NOT NULL DEFAULT '';
ALTER TABLE clients ADD COLUMN redirect_expires_at INTEGER NOT NULL DEFAULT 0;
CREATE INDEX clients_previous_node ON clients(previous_node_id) WHERE previous_node_id <> '';
