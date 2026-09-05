CREATE TABLE audit (
  id             INTEGER PRIMARY KEY,
  actor          TEXT NOT NULL,
  action         TEXT NOT NULL,
  target_session TEXT,
  risk_level     TEXT NOT NULL,
  verdict        TEXT NOT NULL,
  exit_code      INTEGER,
  output_digest  TEXT,
  created_at     TEXT NOT NULL,
  prev_hash      TEXT,
  hash           TEXT NOT NULL
);

CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit BEGIN SELECT RAISE(ABORT,'audit append-only'); END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit BEGIN SELECT RAISE(ABORT,'audit append-only'); END;
