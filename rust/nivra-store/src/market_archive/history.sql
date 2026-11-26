-- Keep a later HTTP 400 exclusion separately when a successful response
-- already established empty coverage for that day.
ALTER TABLE coverage ADD COLUMN unavailable_ms bigint CHECK (unavailable_ms >= 0);
UPDATE coverage SET unavailable_ms = attempted_ms WHERE status = 'unavailable';

CREATE INDEX coverage_unit_day ON coverage (unit, day, instrument_id);
CREATE INDEX daily_bars_day ON daily_bars (day, instrument_id);

CREATE TABLE history_imports (
    range_id text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('receipt', 'unavailable')),
    sha256 bytea NOT NULL CHECK (octet_length(sha256) = 32),
    PRIMARY KEY (range_id, kind)
);
CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON history_imports
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();
