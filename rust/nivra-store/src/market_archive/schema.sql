CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE instruments (
    id serial PRIMARY KEY,
    key text NOT NULL UNIQUE CHECK (key <> '')
);

CREATE TABLE receipts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind smallint NOT NULL CHECK (kind IN (1, 2, 3)),
    requested_ms bigint NOT NULL CHECK (requested_ms >= 0),
    received_ms bigint NOT NULL CHECK (received_ms >= requested_ms),
    sha256 bytea NOT NULL UNIQUE CHECK (octet_length(sha256) = 32),
    body bytea NOT NULL
);

CREATE TABLE quote_obs (
    instrument_id integer NOT NULL REFERENCES instruments(id),
    receipt_ms bigint NOT NULL,
    source_ms bigint NOT NULL CHECK (source_ms >= 0),
    purpose smallint NOT NULL CHECK (purpose IN (1, 2, 3, 4)),
    ltp bigint NOT NULL CHECK (ltp >= 0),
    bid bigint CHECK (bid > 0),
    ask bigint CHECK (ask > 0),
    bid_qty bigint CHECK (bid_qty > 0),
    ask_qty bigint CHECK (ask_qty > 0),
    volume bigint NOT NULL CHECK (volume >= 0),
    lower_circuit bigint NOT NULL CHECK (lower_circuit >= 0),
    upper_circuit bigint NOT NULL CHECK (upper_circuit >= lower_circuit),
    receipt_id bigint NOT NULL REFERENCES receipts(id),
    PRIMARY KEY (instrument_id, receipt_ms, receipt_id),
    CHECK ((bid IS NULL) = (bid_qty IS NULL)),
    CHECK ((ask IS NULL) = (ask_qty IS NULL)),
    CHECK (bid <= ask)
);

CREATE TABLE daily_bars (
    instrument_id integer NOT NULL REFERENCES instruments(id),
    day date NOT NULL,
    o bigint NOT NULL CHECK (o > 0),
    h bigint NOT NULL CHECK (h >= greatest(o, c)),
    l bigint NOT NULL CHECK (l > 0 AND l <= least(o, c)),
    c bigint NOT NULL CHECK (c > 0),
    volume bigint NOT NULL CHECK (volume >= 0),
    oi bigint NOT NULL CHECK (oi >= 0),
    receipt_id bigint NOT NULL REFERENCES receipts(id),
    PRIMARY KEY (instrument_id, day)
);

CREATE TABLE minute_bars (
    LIKE daily_bars INCLUDING CONSTRAINTS,
    bar_ms bigint NOT NULL CHECK (bar_ms % 60000 = 0),
    PRIMARY KEY (instrument_id, bar_ms),
    FOREIGN KEY (instrument_id) REFERENCES instruments(id),
    FOREIGN KEY (receipt_id) REFERENCES receipts(id)
);

-- Time stays in original Unix milliseconds, including inside TimescaleDB.
SELECT create_hypertable('quote_obs', by_range('receipt_ms', 86400000::bigint));
SELECT create_hypertable('minute_bars', by_range('bar_ms', 86400000::bigint));

CREATE FUNCTION market_archive_now_ms() RETURNS bigint
LANGUAGE sql STABLE AS $$
    SELECT (extract(epoch FROM now()) * 1000)::bigint
$$;
SELECT set_integer_now_func('quote_obs', 'market_archive_now_ms');
SELECT set_integer_now_func('minute_bars', 'market_archive_now_ms');
ALTER TABLE quote_obs SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'instrument_id',
    timescaledb.compress_orderby = 'receipt_ms DESC, receipt_id'
);
ALTER TABLE minute_bars SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'instrument_id',
    timescaledb.compress_orderby = 'bar_ms DESC'
);
SELECT add_compression_policy('quote_obs', 86400000::bigint);
SELECT add_compression_policy('minute_bars', 86400000::bigint);

CREATE TABLE coverage (
    instrument_id integer NOT NULL REFERENCES instruments(id),
    unit smallint NOT NULL CHECK (unit IN (2, 3)),
    day date NOT NULL,
    status text NOT NULL CHECK (status IN ('observed', 'empty', 'unavailable', 'failed_retryable')),
    attempted_ms bigint NOT NULL CHECK (attempted_ms >= 0),
    receipt_id bigint REFERENCES receipts(id),
    -- A later HTTP 400 exclusion, kept apart from earlier empty coverage.
    unavailable_ms bigint CHECK (unavailable_ms >= 0),
    PRIMARY KEY (instrument_id, unit, day),
    CHECK ((receipt_id IS NOT NULL) = (status IN ('observed', 'empty')))
);

CREATE FUNCTION market_archive_reject_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'market archive evidence is append-only' USING ERRCODE = '55000';
END;
$$;

CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON receipts
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();
CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON quote_obs
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();
CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON daily_bars
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();
CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON minute_bars
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();

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

CREATE TABLE book_quote_imports (
    book_binding text NOT NULL,
    tick_id bigint NOT NULL,
    event_ms bigint NOT NULL,
    receipt_id bigint NOT NULL REFERENCES receipts(id),
    PRIMARY KEY (book_binding, tick_id)
);
CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON book_quote_imports
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();
