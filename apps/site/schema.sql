CREATE TABLE IF NOT EXISTS waitlist (
  email TEXT PRIMARY KEY,
  church TEXT NOT NULL DEFAULT '',
  size TEXT NOT NULL DEFAULT '',
  software TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
