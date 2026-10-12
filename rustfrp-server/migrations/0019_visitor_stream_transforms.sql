ALTER TABLE visitors ADD COLUMN use_encryption INTEGER NOT NULL DEFAULT 0
  CHECK (use_encryption IN (0, 1));
ALTER TABLE visitors ADD COLUMN use_compression INTEGER NOT NULL DEFAULT 0
  CHECK (use_compression IN (0, 1));
