-- Church computers that sync (each by its P-256 device key) and the single-use nonces they sign
-- to get a short-lived Supabase token (functions/api/sync/*).
CREATE TABLE IF NOT EXISTS sync_devices (
  public_key TEXT PRIMARY KEY,
  church_id TEXT NOT NULL REFERENCES churches(id),
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sync_devices_church ON sync_devices(church_id);

CREATE TABLE IF NOT EXISTS sync_nonces (
  nonce TEXT PRIMARY KEY,
  expires_at INTEGER NOT NULL
);
