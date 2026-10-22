-- When each alert kind was last emailed to a church (alert emails, /api/alert): one row per
-- church and kind. Holds only ids and times, never a message or an address.
CREATE TABLE IF NOT EXISTS alert_emails (
  church_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  sent_at INTEGER NOT NULL,
  PRIMARY KEY (church_id, kind)
);
