# GoblinMetrics — Agent Reference

## Working Protocol

Every task follows this sequence — no exceptions:

1. **Code** — make the change
2. **Compile** — `cargo check` (fast) or `cargo test` if logic changed
3. **Deploy** — `bash scripts/deploy.sh` (builds release, uploads, restarts services)
4. **Verify** — `curl` the affected endpoints; check service status
5. **Update agents.md** — edit the relevant section in-place (schema, endpoints, features); do not just append another iteration block

```bash
# Typical verify after deploy
ssh azu@144.31.17.0 "systemctl is-active goblin-log-ingestor goblin-sys-metrics goblin-web-ui"
curl -s http://144.31.17.0:4444/health
curl -s "http://144.31.17.0:4444/api/requests/top_ips?hours=1&limit=5"
curl -s "http://144.31.17.0:4444/api/requests/latency?hours=1&bucket=minute"
```

---

## System Overview

Three Rust binaries in a Cargo workspace, deployed to `144.31.17.0` (Debian 13, 1 vCPU, ~966 MB RAM).

**Shared paths:**
- Binaries: `/opt/goblin-metrics/`
- Database: `/var/lib/goblin-metrics/metrics.db` (SQLite, WAL mode, `busy_timeout=5000`)
- Service user: `goblin-metrics` (system user, no login shell, `adm` group for nginx log access)
- Dashboard: `http://144.31.17.0:4444` and proxied at `goblin.geno.su/goblin-metrics/`

---

## Services

### `log-ingestor` — `goblin-log-ingestor.service`

Tails the nginx JSON access log and inserts one row per request into the `requests` table.

**Loop:** every 500 ms, opens `/var/log/nginx/goblin_metrics.log`, seeks to saved byte offset, reads all new complete lines, inserts a batch inside a single SQLite transaction, saves the new offset to `ingestor_state`. If the file is smaller than the saved offset (nginx reload/rotation), offset resets to 0.

**Parsing** (`parser.rs`):
- Deserialises each line as `RawEntry` JSON (nginx `goblin_json` format)
- Parses `time_local` (`%d/%b/%Y:%H:%M:%S %z`) → unix milliseconds
- Extracts URL from `request` field (`"GET /path HTTP/2.0"` → `"/path"`)
- Extracts method — only accepts `GET POST PUT PATCH DELETE HEAD OPTIONS CONNECT TRACE`; anything else (bot TLS bytes) becomes `""`
- `request_time` (seconds float) → `response_time_ms` (×1000)
- Stores `referer`, `user_agent`, `accept`, `accept_language`, `x_forwarded_for`, `content_type` as a compact JSON `headers` blob

**DB writes** (`db.rs`): uses `INSERT OR IGNORE` (primary key dedup). Byte offset persisted via `INSERT … ON CONFLICT DO UPDATE` in `ingestor_state`.

**Input:** reads with `read_until(b'\n')` + `from_utf8_lossy` — necessary because nginx `escape=json` does not escape raw bytes above 0x7F (TLS ClientHello data hitting port 80 produces non-UTF-8 lines).

**Env vars:** `DB_PATH`, `LOG_FILE`, `RUST_LOG=log_ingestor=info`  
**Restart:** `always`, `RestartSec=5`

---

### `sys-metrics` — `goblin-sys-metrics.service`

Collects CPU, memory, and load average every second and inserts 5 rows per tick into the `metrics` table.

**Loop:** Tokio `interval(1s)`. Each tick calls three collectors then commits a single transaction writing all samples at the same `timestamp` (unix ms at tick start).

**Collectors** (`collectors.rs`):

| Metric name | Source | Method |
|---|---|---|
| `cpu_usage_pct` | `/proc/stat` | Reads `cpu` line twice 100 ms apart; `(total_diff - idle_diff) / total_diff × 100` |
| `memory_used_mb` | `/proc/meminfo` | `MemTotal - MemAvailable` in MB |
| `memory_free_mb` | `/proc/meminfo` | `MemAvailable` in MB |
| `memory_used_pct` | `/proc/meminfo` | `(MemTotal - MemAvailable) / MemTotal × 100` |
| `load_avg_1m` | `/proc/loadavg` | First whitespace token, parsed as f64 |

Note: the 100 ms CPU sleep happens inside each 1-second tick, so each collection consumes ~100 ms of the interval.

**Env vars:** `DB_PATH`, `RUST_LOG=sys_metrics=info`  
**Restart:** `always`, `RestartSec=5`

---

### `web-ui` — `goblin-web-ui.service`

Axum HTTP server exposing the REST API and serving the single-page dashboard (`index.html` embedded at compile time via `include_str!`).

**Startup:** connects to SQLite, sets WAL + busy_timeout, runs `run_migrations()` (idempotent `ALTER TABLE` + `CREATE INDEX IF NOT EXISTS` calls, errors suppressed), then binds on `BIND_ADDR`.

**Middleware:** `tower_http::CompressionLayer` — compresses all responses.

**Route handlers** (`api.rs`): all share `AppState = SqlitePool`. Common query type `RangeQuery { from?, to?, hours?, bucket?, host? }` — `resolve()` returns `(from_ms, to_ms)`; missing `to` defaults to now, missing `from` defaults to `now - hours×3600000`. Host filter applied as `AND (?N IS NULL OR host = ?N)`.

**Nginx integration:**  
- `deploy/nginx-logging.conf` → `/etc/nginx/conf.d/goblin-metrics-logging.conf`: defines `goblin_json` log_format capturing `remote_addr`, `time_local`, `request`, `status`, `body_bytes_sent`, `referer`, `user_agent`, `accept`, `accept_language`, `x_forwarded_for`, `content_type`, `host`, `request_time`
- `deploy/nginx-metrics-location.conf` → `/etc/nginx/snippets/goblin-metrics.conf`: proxies `goblin.geno.su/goblin-metrics/` → `127.0.0.1:4444`

**Env vars:** `DB_PATH`, `BIND_ADDR` (default `0.0.0.0:4444`), `RUST_LOG=web_ui=info`  
**Restart:** `always`, `RestartSec=5`

---

## Database Schema

```sql
requests(
  id INTEGER PK,
  timestamp INTEGER NOT NULL,        -- unix ms
  url TEXT NOT NULL,
  ip TEXT NOT NULL,
  host TEXT DEFAULT '',
  method TEXT DEFAULT '',            -- HTTP verb; '' for non-HTTP/bot traffic
  response_time_ms REAL DEFAULT 0,   -- nginx $request_time * 1000
  user_agent TEXT,
  status_code INTEGER NOT NULL,
  headers TEXT                       -- JSON
)
-- Indexes: idx_req_ts, idx_req_ip, idx_req_status, idx_req_host, idx_req_method

metrics(
  id INTEGER PK,
  metric_name TEXT NOT NULL,
  metric_value REAL NOT NULL,
  timestamp INTEGER NOT NULL         -- unix ms
)
-- Index: idx_met_name_ts

ingestor_state(key TEXT PK, value TEXT)  -- log-ingestor byte offset
```

Migrations run idempotently at startup via `run_migrations()` in both binaries. New columns use `ALTER TABLE … IF NOT EXISTS` pattern (errors suppressed). Migration files in `migrations/`.

---

## API Endpoints

All request endpoints accept `from` + `to` (unix ms) **or** `hours` (float, default 1). Optional `host` filter. Bucket endpoints accept `bucket=second|minute|hour`.

```
GET /health
GET /api/metrics/names
GET /api/metrics?name=<n>&hours=<h>
GET /api/requests/hosts
GET /api/requests/timeseries?hours=<h>&bucket=<b>&host=<h>
GET /api/requests/status_timeseries?hours=<h>&bucket=<b>&host=<h>
GET /api/requests/status_codes?hours=<h>&host=<h>
GET /api/requests/latency?hours=<h>&bucket=<b>&host=<h>
GET /api/requests/top_urls?hours=<h>&limit=<n>&host=<h>   -- limit=0 → all (cap 5000)
GET /api/requests/top_ips?hours=<h>&limit=<n>&host=<h>    -- limit=0 → all (cap 5000)
```

---

## Dashboard Features

**Global controls:** range presets (1h/6h/24h/7d) + custom datetime range, bucket selector (per second/minute/hour), host filter (populated from `/api/requests/hosts`), auto-refresh (5s/10s/30s/60s/off). All prefs persisted in `localStorage`.

**System section:** CPU %, Memory Used %, Load Average 1m — line charts from `metrics` table.

**Nginx Requests section:**
- Requests/bucket — bar or line (toggle ▬/∿, persisted)
- Status Codes/bucket — stacked bar or line (toggle ▬/∿, persisted)
- Avg Backend Latency — single line, avg `response_time_ms` per bucket from nginx logs
- Dashboard API Latency — client-measured `performance.now()` per endpoint; timestamps rounded to nearest second so co-fetched endpoints share x-axis; `spanGaps: true`; ⟲ clear button
- Top URLs + Top IPs — side-by-side tables, Show all / Show less toggle (Show all fetches `limit=0`)

---

## Deployment

```bash
bash scripts/deploy.sh
# Builds locally (x86_64 Linux release), uploads binaries + migrations + systemd units,
# sets up goblin-metrics user, runs migrations, configures nginx snippet, restarts services.
```

Services restart order: `goblin-log-ingestor` → `goblin-sys-metrics` → `goblin-web-ui`.  
The script stops services before installing binaries (avoids "text file busy").  
Nginx reload emits a deprecation warning about `listen … http2` — harmless, config test passes.

**Current service status (last deploy: 2026-05-11):**

| Service | State |
|---|---|
| `goblin-log-ingestor` | active (running) |
| `goblin-sys-metrics`  | active (running) |
| `goblin-web-ui`       | active (running) |

Memory per service: ~1–1.4 MB each.

---

## Tests

```bash
cargo test
```

22 tests across all three crates. Tests use in-memory SQLite (`":memory:"`). Key coverage: host filtering, bucket aggregation, latency averaging, `limit=0` URL fetch, migration idempotency.

---

## Known Behaviours / Gotchas

- **Bot traffic / TLS on port 80**: nginx logs raw TLS handshake bytes; log-ingestor uses `read_until(b'\n')` + `from_utf8_lossy` to handle non-UTF-8 safely. `method` is filtered to known HTTP verbs only — garbage becomes `''`.
- **nginx log reset**: log is recreated empty on nginx reload. Log-ingestor detects file shrinkage and resets byte offset automatically.
- **Latency filter**: `WHERE method != ''` skips bot/TLS rows. `response_time_ms = 0` rows (e.g. 301 redirects) are intentionally included.
- **API latency chart**: all dashboard fetches happen in `Promise.all`, completing within ms of each other. Timestamps are rounded to the nearest second to align all endpoints on a shared x-axis point per refresh cycle.
