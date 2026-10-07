-- People who ticked "I'd like to help test the alpha" on the waitlist form.
ALTER TABLE waitlist ADD COLUMN alpha_tester INTEGER NOT NULL DEFAULT 0;
