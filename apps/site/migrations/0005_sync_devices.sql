-- Church computers that sync (each by its P-256 device key) and the single-use nonces they sign
-- to get a short-lived Supabase token (functions/api/sync/*). A removed computer keeps its row
-- (removed_at set) so its key can't register again; it doesn't count toward the limit.
CREATE TABLE IF NOT EXISTS sync_devices (
  public_key TEXT PRIMARY KEY,
  church_id TEXT NOT NULL REFERENCES churches(id),
  name TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL,
  removed_at INTEGER
);
CREATE INDEX IF NOT EXISTS sync_devices_church ON sync_devices(church_id);

CREATE TABLE IF NOT EXISTS sync_nonces (
  nonce TEXT PRIMARY KEY,
  expires_at INTEGER NOT NULL
);
