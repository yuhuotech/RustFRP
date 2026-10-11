CREATE TABLE client_config_sources (
 client_id TEXT PRIMARY KEY REFERENCES clients(id) ON DELETE CASCADE,
 source_json TEXT NOT NULL,
 updated_at INTEGER NOT NULL
);
