-- How often the account's alert conversation gets a market briefing: off, edges (session open and
-- close only), or 1h/2h/4h during sessions as well.
ALTER TABLE accounts ADD COLUMN briefing TEXT NOT NULL DEFAULT '2h';
