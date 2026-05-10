PRAGMA journal_mode=WAL;
PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS requests (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   INTEGER NOT NULL,
    url         TEXT    NOT NULL,
    ip          TEXT    NOT NULL,
    user_agent  TEXT,
    status_code INTEGER NOT NULL,
    headers     TEXT
);
CREATE INDEX IF NOT EXISTS idx_req_ts     ON requests(timestamp);
CREATE INDEX IF NOT EXISTS idx_req_ip     ON requests(ip);
CREATE INDEX IF NOT EXISTS idx_req_status ON requests(status_code);

CREATE TABLE IF NOT EXISTS metrics (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    metric_name  TEXT    NOT NULL,
    metric_value REAL    NOT NULL,
    timestamp    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_met_name_ts ON metrics(metric_name, timestamp);

CREATE TABLE IF NOT EXISTS ingestor_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
