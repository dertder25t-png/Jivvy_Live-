-- Church accounts, sign-in codes, sessions and license keys (live.jivvy.org/account).
CREATE TABLE IF NOT EXISTS churches (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);

-- One live code per address; stored as a hash, never the code.
CREATE TABLE IF NOT EXISTS sign_in_codes (
  email TEXT PRIMARY KEY,
  code_hash TEXT NOT NULL,
  sent_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0
);

-- Session cookies, stored as a hash of the token.
CREATE TABLE IF NOT EXISTS sessions (
  token_hash TEXT PRIMARY KEY,
  church_id TEXT NOT NULL REFERENCES churches(id),
  expires_at INTEGER NOT NULL
);

-- Issued by the Stripe webhook; one per checkout, so a retried delivery issues nothing new.
CREATE TABLE IF NOT EXISTS licenses (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  church_id TEXT NOT NULL REFERENCES churches(id),
  stripe_session TEXT NOT NULL UNIQUE,
  license_key TEXT NOT NULL,
  plan TEXT NOT NULL,
  updates_until INTEGER NOT NULL,
  plus_until INTEGER NOT NULL,
  issued INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS licenses_church ON licenses(church_id);
