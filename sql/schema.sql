CREATE SCHEMA IF NOT EXISTS ops;
CREATE SCHEMA IF NOT EXISTS staging;
CREATE SCHEMA IF NOT EXISTS core;
CREATE SCHEMA IF NOT EXISTS research;
CREATE TABLE IF NOT EXISTS ops.schema_version(version INTEGER PRIMARY KEY, applied_at TIMESTAMPTZ DEFAULT current_timestamp);
INSERT INTO ops.schema_version(version) VALUES (1) ON CONFLICT DO NOTHING;
INSERT INTO ops.schema_version(version) VALUES (2) ON CONFLICT DO NOTHING;
INSERT INTO ops.schema_version(version) VALUES (3) ON CONFLICT DO NOTHING;
INSERT INTO ops.schema_version(version) VALUES (4) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS ops.ingest_run(
 run_id VARCHAR PRIMARY KEY, source VARCHAR NOT NULL, request_url VARCHAR NOT NULL,
 started_at TIMESTAMPTZ NOT NULL, finished_at TIMESTAMPTZ,
 status VARCHAR NOT NULL CHECK(status IN ('RUNNING','SUCCESS','FAILED')),
 raw_path VARCHAR, raw_sha256 VARCHAR, row_count INTEGER, inserted_count INTEGER, error VARCHAR
);
-- 样本只进入 staging；未经证券主表、日历与覆盖率核验，不发布为全市场标准库。
CREATE TABLE IF NOT EXISTS staging.daily_bar_revision(
 revision_id VARCHAR PRIMARY KEY, symbol VARCHAR NOT NULL, trade_date DATE NOT NULL,
 source VARCHAR NOT NULL, adjustment VARCHAR NOT NULL CHECK(adjustment='none'),
 open DECIMAL(20,6) NOT NULL, high DECIMAL(20,6) NOT NULL, low DECIMAL(20,6) NOT NULL,
 close DECIMAL(20,6) NOT NULL, volume_shares DECIMAL(24,4) NOT NULL,
 amount_cny DECIMAL(24,4), observed_at TIMESTAMPTZ NOT NULL,
 -- DuckDB does not support cross-schema foreign keys. `run_id` is validated by
 -- the writer transaction and release validation before this data is published.
 run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL,
 CHECK(low > 0 AND low <= open AND low <= close AND high >= open AND high >= close),
 CHECK(volume_shares >= 0), CHECK(amount_cny IS NULL OR amount_cny >= 0)
);
CREATE VIEW IF NOT EXISTS staging.daily_bar_latest AS
 SELECT * FROM staging.daily_bar_revision
 QUALIFY row_number() OVER(PARTITION BY symbol, trade_date, source, adjustment
 ORDER BY observed_at DESC, revision_id DESC)=1;

-- A market-data package lists securities observed that day. Its classification is
-- deliberately UNKNOWN until a primary listing source validates it.
CREATE TABLE IF NOT EXISTS core.instrument(
 instrument_id VARCHAR PRIMARY KEY, market VARCHAR NOT NULL CHECK(market IN ('SH','SZ','BJ')),
 code VARCHAR NOT NULL CHECK(regexp_full_match(code, '[0-9]{6}')),
 asset_class VARCHAR NOT NULL CHECK(asset_class IN ('UNKNOWN','EQUITY_CANDIDATE','EQUITY','ETF','INDEX','BOND','FUND')),
 first_observed_date DATE NOT NULL, last_observed_date DATE NOT NULL,
 created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL,
 UNIQUE(market, code)
);
CREATE TABLE IF NOT EXISTS core.instrument_symbol_revision(
 revision_id VARCHAR PRIMARY KEY, instrument_id VARCHAR NOT NULL, symbol VARCHAR NOT NULL,
 name VARCHAR NOT NULL, valid_from DATE NOT NULL, valid_to DATE,
 source VARCHAR NOT NULL, observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL,
 CHECK(valid_to IS NULL OR valid_to >= valid_from)
);
CREATE VIEW IF NOT EXISTS core.instrument_symbol_latest AS
 SELECT * FROM core.instrument_symbol_revision
 QUALIFY row_number() OVER(PARTITION BY instrument_id, symbol, source, valid_from
 ORDER BY observed_at DESC, revision_id DESC)=1;

-- Asset classifications are evidence-bearing revisions. `core.instrument.asset_class`
-- remains the convenient current value, while this table records why it changed.
CREATE TABLE IF NOT EXISTS core.instrument_classification_revision(
 revision_id VARCHAR PRIMARY KEY, instrument_id VARCHAR NOT NULL,
 asset_class VARCHAR NOT NULL CHECK(asset_class IN ('UNKNOWN','EQUITY_CANDIDATE','EQUITY','ETF','INDEX','BOND','FUND')),
 effective_date DATE NOT NULL, source VARCHAR NOT NULL, method VARCHAR NOT NULL,
 observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL
);
CREATE VIEW IF NOT EXISTS core.instrument_classification_latest AS
 SELECT * FROM core.instrument_classification_revision
 QUALIFY row_number() OVER(PARTITION BY instrument_id, source
 ORDER BY effective_date DESC, observed_at DESC, revision_id DESC)=1;

CREATE TABLE IF NOT EXISTS core.trading_calendar_revision(
 revision_id VARCHAR PRIMARY KEY, market VARCHAR NOT NULL CHECK(market IN ('CN')),
 trade_date DATE NOT NULL, is_open BOOLEAN NOT NULL, source VARCHAR NOT NULL,
 observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL
);
CREATE VIEW IF NOT EXISTS core.trading_calendar_latest AS
 SELECT * FROM core.trading_calendar_revision
 QUALIFY row_number() OVER(PARTITION BY market, trade_date, source
 ORDER BY observed_at DESC, revision_id DESC)=1;

CREATE TABLE IF NOT EXISTS core.adjustment_factor_revision(
 revision_id VARCHAR PRIMARY KEY, symbol VARCHAR NOT NULL, effective_date DATE NOT NULL,
 adjustment_kind VARCHAR NOT NULL CHECK(adjustment_kind IN ('qfq','hfq')),
 factor DECIMAL(28,10) NOT NULL CHECK(factor > 0), source VARCHAR NOT NULL,
 observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL
);
CREATE VIEW IF NOT EXISTS core.adjustment_factor_latest AS
 SELECT * FROM core.adjustment_factor_revision
 QUALIFY row_number() OVER(PARTITION BY symbol, effective_date, adjustment_kind, source
 ORDER BY observed_at DESC, revision_id DESC)=1;

-- `status` is source-declared: UNKNOWN is never converted to tradable or halted.
CREATE TABLE IF NOT EXISTS core.security_status_revision(
 revision_id VARCHAR PRIMARY KEY, symbol VARCHAR NOT NULL, effective_date DATE NOT NULL,
 trade_status VARCHAR NOT NULL CHECK(trade_status IN ('TRADABLE','HALTED','UNKNOWN')),
 is_st BOOLEAN, limit_rule_id VARCHAR, source VARCHAR NOT NULL,
 observed_at TIMESTAMPTZ NOT NULL, run_id VARCHAR NOT NULL, row_hash VARCHAR NOT NULL
);
CREATE VIEW IF NOT EXISTS core.security_status_latest AS
 SELECT * FROM core.security_status_revision
 QUALIFY row_number() OVER(PARTITION BY symbol, effective_date, source
 ORDER BY observed_at DESC, revision_id DESC)=1;

CREATE VIEW IF NOT EXISTS research.daily_bar_qfq AS
SELECT b.*, f.factor AS adjustment_factor,
       CASE WHEN f.factor IS NULL THEN NULL ELSE b.open / f.factor END AS adjusted_open,
       CASE WHEN f.factor IS NULL THEN NULL ELSE b.high / f.factor END AS adjusted_high,
       CASE WHEN f.factor IS NULL THEN NULL ELSE b.low / f.factor END AS adjusted_low,
       CASE WHEN f.factor IS NULL THEN NULL ELSE b.close / f.factor END AS adjusted_close
FROM staging.daily_bar_latest b
LEFT JOIN LATERAL (
 SELECT factor FROM core.adjustment_factor_latest f
 WHERE f.symbol=b.symbol AND f.adjustment_kind='qfq' AND f.effective_date<=b.trade_date
 ORDER BY f.effective_date DESC LIMIT 1
) f ON true;
CREATE VIEW IF NOT EXISTS research.daily_bar_execution AS
SELECT b.*, s.trade_status, s.is_st, s.limit_rule_id,
       coalesce(s.trade_status='TRADABLE', false) AS is_tradable
FROM staging.daily_bar_latest b
LEFT JOIN core.security_status_latest s
 ON s.symbol=b.symbol AND s.effective_date=b.trade_date;

-- ETF prices are raw, unadjusted exchange-traded prices. Identification requires
-- both an ETF name marker and an exchange fund code family in the writer.
CREATE VIEW IF NOT EXISTS research.etf_daily_bar AS
SELECT b.*, i.instrument_id, n.name
FROM staging.daily_bar_latest b
JOIN core.instrument i
  ON b.symbol=(CASE i.market WHEN 'SH' THEN 'sh' WHEN 'SZ' THEN 'sz' ELSE 'bj' END || i.code)
LEFT JOIN LATERAL (
 SELECT name FROM core.instrument_symbol_latest n
 WHERE n.instrument_id=i.instrument_id AND n.valid_from<=b.trade_date
 ORDER BY n.valid_from DESC, n.observed_at DESC LIMIT 1
) n ON true
WHERE i.asset_class='ETF';

CREATE TABLE IF NOT EXISTS ops.quality_issue(
 issue_id VARCHAR PRIMARY KEY, dataset VARCHAR NOT NULL, trade_date DATE, severity VARCHAR NOT NULL,
 code VARCHAR NOT NULL, detail VARCHAR NOT NULL, run_id VARCHAR, created_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE IF NOT EXISTS ops.dataset_release(
 release_id VARCHAR PRIMARY KEY, dataset VARCHAR NOT NULL, as_of_date DATE NOT NULL,
 status VARCHAR NOT NULL CHECK(status IN ('PUBLISHED','BLOCKED')), manifest_path VARCHAR,
 detail VARCHAR NOT NULL, created_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE IF NOT EXISTS ops.market_daily_coverage(
 run_id VARCHAR NOT NULL, trade_date DATE NOT NULL, market VARCHAR NOT NULL CHECK(market IN ('SH','SZ','BJ')),
 observed_rows INTEGER NOT NULL, minimum_rows INTEGER NOT NULL, is_complete BOOLEAN NOT NULL,
 PRIMARY KEY(run_id, market)
);
CREATE TABLE IF NOT EXISTS ops.history_backfill_window(
 dataset VARCHAR NOT NULL, symbol VARCHAR NOT NULL, window_start DATE NOT NULL,
 window_end DATE NOT NULL, source VARCHAR NOT NULL,
 status VARCHAR NOT NULL CHECK(status IN ('SUCCESS','EMPTY','FAILED')),
 row_count INTEGER NOT NULL, error VARCHAR, updated_at TIMESTAMPTZ NOT NULL,
 PRIMARY KEY(dataset, symbol, window_start, window_end, source)
);
