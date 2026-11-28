CREATE TABLE IF NOT EXISTS campaign(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  segment TEXT NOT NULL,
  protocol_version INTEGER NOT NULL,
  manifest_digest TEXT NOT NULL,
  development_from TEXT NOT NULL,
  development_to TEXT NOT NULL,
  selection_from TEXT NOT NULL,
  validation_from TEXT, validation_to TEXT,
  holdout_from TEXT, holdout_to TEXT,
  trial_cap INTEGER NOT NULL CHECK(trial_cap > 0),
  finalist_experiment_id INTEGER REFERENCES experiment(id),
  holdout_state TEXT NOT NULL DEFAULT 'unopened' CHECK(holdout_state IN ('unopened','reserved','opened','retired')),
  status TEXT NOT NULL CHECK(status IN ('active','exhausted','closed')),
  created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS campaign_holdout ON campaign(segment, holdout_from, holdout_to)
  WHERE holdout_from IS NOT NULL;
CREATE TABLE IF NOT EXISTS idea(
  id INTEGER PRIMARY KEY,
  campaign_id INTEGER NOT NULL REFERENCES campaign(id),
  origin_request_key TEXT UNIQUE,
  family TEXT NOT NULL,
  regime TEXT NOT NULL CHECK(regime IN ('trend','range','unknown')),
  mechanism_key TEXT NOT NULL,
  hypothesis TEXT NOT NULL,
  falsifier TEXT NOT NULL,
  comparator TEXT NOT NULL,
  parent_experiment_id INTEGER REFERENCES experiment(id),
  status TEXT NOT NULL CHECK(status IN ('open','testing','kept','rejected','invalid','exhausted')),
  repairs INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idea_family ON idea(campaign_id, family, regime);
CREATE TABLE IF NOT EXISTS experiment(
  id INTEGER PRIMARY KEY,
  campaign_id INTEGER NOT NULL REFERENCES campaign(id),
  idea_id INTEGER NOT NULL REFERENCES idea(id),
  parent_id INTEGER REFERENCES experiment(id),
  stage TEXT NOT NULL CHECK(stage IN ('bars','quote_replay')),
  source_path TEXT NOT NULL,
  source_blob TEXT NOT NULL,
  params_blob TEXT NOT NULL,
  manifest_blob TEXT NOT NULL,
  evaluator_digest TEXT NOT NULL,
  experiment_key TEXT NOT NULL UNIQUE,
  ordinal INTEGER NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('reserved','scored','invalid','failed')),
  score TEXT,
  verdict TEXT CHECK(verdict IN ('keep','reject')),
  reason TEXT,
  report_blob TEXT,
  interpreted INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(campaign_id, ordinal)
);
CREATE TABLE IF NOT EXISTS attempt(
  id INTEGER PRIMARY KEY,
  experiment_id INTEGER NOT NULL REFERENCES experiment(id),
  attempt_no INTEGER NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  job_id TEXT,
  job_revision INTEGER,
  lease_generation INTEGER NOT NULL,
  deadline_ms INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('reserved','running','succeeded','invalid','failed','preempted')),
  failure TEXT,
  usage TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(experiment_id, attempt_no)
);
CREATE TABLE IF NOT EXISTS finding(
  id INTEGER PRIMARY KEY,
  idea_id INTEGER REFERENCES idea(id),
  experiment_id INTEGER REFERENCES experiment(id),
  kind TEXT NOT NULL CHECK(kind IN ('positive','negative','inconclusive','procedure')),
  claim TEXT NOT NULL,
  scope TEXT NOT NULL,
  counterevidence TEXT NOT NULL,
  next_test TEXT NOT NULL,
  evidence_digest TEXT,
  created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_revision(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  digest TEXT NOT NULL,
  parent_digest TEXT,
  finding_ids TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('proposed','approved','rejected','retired')),
  decision_reason TEXT,
  reviewer TEXT,
  active_from_ms INTEGER,
  retired_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  UNIQUE(name, digest)
);
CREATE UNIQUE INDEX IF NOT EXISTS skill_one_active ON skill_revision(name) WHERE status = 'approved';
CREATE TABLE IF NOT EXISTS cell(
  campaign_id INTEGER NOT NULL REFERENCES campaign(id),
  segment TEXT NOT NULL,
  family TEXT NOT NULL,
  regime TEXT NOT NULL,
  incumbent_experiment_id INTEGER NOT NULL REFERENCES experiment(id),
  PRIMARY KEY(campaign_id, segment, family, regime)
);
CREATE TABLE IF NOT EXISTS promotion(
  id INTEGER PRIMARY KEY,
  candidate_experiment_id INTEGER NOT NULL REFERENCES experiment(id),
  replay_report_blob TEXT NOT NULL,
  forward_report_blob TEXT,
  policy_version TEXT NOT NULL,
  library_id TEXT,
  state TEXT NOT NULL CHECK(state IN ('pending','completed','refused')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(candidate_experiment_id, policy_version, replay_report_blob, forward_report_blob)
);
CREATE TABLE IF NOT EXISTS shadow(
  id INTEGER PRIMARY KEY,
  promotion_id INTEGER NOT NULL REFERENCES promotion(id),
  session TEXT NOT NULL,
  ledger_blob TEXT,
  coverage_bps INTEGER,
  state TEXT NOT NULL,
  UNIQUE(promotion_id, session)
);
CREATE TABLE IF NOT EXISTS lease(
  name TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  pid INTEGER NOT NULL,
  generation INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reservation(
  key TEXT PRIMARY KEY,
  day TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('variant','turn')),
  calls INTEGER NOT NULL,
  usd_micros INTEGER NOT NULL,
  settled INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS reservation_day ON reservation(day, kind);
CREATE TABLE IF NOT EXISTS imported(
  sha256 TEXT NOT NULL,
  destination TEXT NOT NULL,
  origin TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  PRIMARY KEY(sha256, destination)
);
CREATE TABLE IF NOT EXISTS loop_state(
  id INTEGER PRIMARY KEY CHECK(id = 1),
  body TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS option_research_stage(
  session TEXT PRIMARY KEY,
  checked_ms INTEGER NOT NULL,
  proposed_ms INTEGER,
  cohort_digest TEXT,
  body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS option_research_comparison(
  session TEXT NOT NULL REFERENCES option_research_stage(session),
  ranking TEXT NOT NULL,
  hypothesis TEXT NOT NULL,
  body TEXT,
  PRIMARY KEY(session, ranking)
);

CREATE TABLE IF NOT EXISTS holdout_report(
  campaign_id INTEGER PRIMARY KEY REFERENCES campaign(id),
  verdict TEXT NOT NULL CHECK(verdict IN ('keep','reject','inconclusive')),
  report_blob TEXT
);

CREATE TABLE IF NOT EXISTS replay_exposure(
  from_day TEXT NOT NULL,
  to_day TEXT NOT NULL,
  PRIMARY KEY(from_day,to_day)
);

CREATE TABLE IF NOT EXISTS quote_draft(
  idea_id INTEGER PRIMARY KEY REFERENCES idea(id),
  source_blob TEXT NOT NULL,
  params_blob TEXT NOT NULL,
  frozen_at_ms INTEGER NOT NULL
);
