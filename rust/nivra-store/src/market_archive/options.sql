CREATE TABLE option_evidence (
    id bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    session date NOT NULL,
    at_ms bigint NOT NULL CHECK (at_ms > 0),
    kind text NOT NULL CHECK (kind IN ('master','poll','held','gate','gap','capture_gap')),
    quotes integer NOT NULL CHECK (quotes >= 0),
    missing_quotes integer NOT NULL CHECK (missing_quotes >= 0),
    has_spot boolean NOT NULL,
    body jsonb NOT NULL CHECK (jsonb_typeof(body) = 'object'),
    PRIMARY KEY (session, kind, at_ms)
);

CREATE TRIGGER append_only BEFORE UPDATE OR DELETE ON option_evidence
    FOR EACH ROW EXECUTE FUNCTION market_archive_reject_mutation();
