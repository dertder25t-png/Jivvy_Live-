-- Per-caller request limits for sign-in (key: kind + hashed IP); fixed windows, no raw IPs.
CREATE TABLE IF NOT EXISTS rate_limits (
  bucket TEXT PRIMARY KEY,
  window_start INTEGER NOT NULL,
  hits INTEGER NOT NULL
);
