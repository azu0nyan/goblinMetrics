# GoblinMetrics — Agent Log

## Iteration 1 — Initial Build & Deploy  
**Date:** 2026-05-10  
**Status:** ✅ Complete

### What was built

Three Rust binaries in a Cargo workspace, deployed to `144.31.17.0` (Debian 13, 1 vCPU, ~966 MB RAM):

| Binary | Service | Purpose |
|---|---|---|
| `log-ingestor` | `goblin-log-ingestor.service` | Tails nginx JSON log → `requests` table |
| `sys-metrics`  | `goblin-sys-metrics.service`  | Collects CPU/memory/load every 1 s → `metrics` table |
| `web-ui`       | `goblin-web-ui.service`       | REST API + dashboard on port 4444 |

### Infrastructure

- **Database:** SQLite at `/var/lib/goblin-metrics/metrics.db`, WAL mode, `busy_timeout=5000`
- **Service user:** `goblin-metrics` (system user, no login shell), member of `adm` group for nginx log read access
- **Binaries:** `/opt/goblin-metrics/`
- **Nginx log:** `/var/log/nginx/goblin_metrics.log` (JSON format via `goblin_json` log_format)

### nginx config changes

Added `/etc/nginx/conf.d/goblin-metrics-logging.conf` with the `goblin_json` log format (captures `remote_addr`, `time_local`, `request`, `status`, `user_agent`, `referer`, `accept`, `accept_language`, `x_forwarded_for`, `content_type`).

Added `access_log /var/log/nginx/goblin_metrics.log goblin_json;` to both server blocks in `/etc/nginx/sites-enabled/goblin.geno.su`.

### Database schema

```sql
requests(id, timestamp INTEGER ms, url, ip, user_agent, status_code, headers JSON)
metrics(id, metric_name, metric_value REAL, timestamp INTEGER ms)
ingestor_state(key, value)   -- tracks log-ingestor byte offset
```

Indexes: `idx_req_ts`, `idx_req_ip`, `idx_req_status`, `idx_met_name_ts`

### API endpoints

```
GET /health
GET /api/metrics/names
GET /api/metrics?name=<n>&hours=<h>
GET /api/requests/timeseries?hours=<h>
GET /api/requests/status_codes?hours=<h>
GET /api/requests/top_urls?hours=<h>&limit=<n>
```

### Tests

12 tests across all three crates — all pass (`cargo test`).

### Deployment

```bash
DEPLOY_USER=azu DEPLOY_HOST=144.31.17.0 bash scripts/deploy.sh
```

Builds locally (x86_64 Linux), uploads with `scp`, sets up user/permissions, runs migrations, configures nginx, installs systemd units.

### Current system status

| Service | State |
|---|---|
| `goblin-log-ingestor` | active (running) |
| `goblin-sys-metrics`  | active (running) |
| `goblin-web-ui`       | active (running) |

Memory per service: ~900 KB each.

Dashboard: `http://144.31.17.0:4444`

---

## Iteration 2 — UI & API Improvements
**Date:** 2026-05-10  
**Status:** ✅ Complete

### What changed

**API (`crates/web-ui/src/api.rs`)**
- Replaced `hours` scalar with unified `RangeQuery { from?, to?, hours?, bucket? }` on all request endpoints
- `from`/`to` are unix milliseconds; `hours` is a backward-compat shortcut
- New `Bucket` enum: `second` (1 s), `minute` (60 s, default), `hour` (3600 s)
- New endpoint: `GET /api/requests/status_timeseries?from=&to=&bucket=` — returns `[{bucket_ts, status_code, count}]` for stacked bar chart
- `top_urls?limit=0` now returns up to 5000 URLs (SQLite `LIMIT -1`)

**Frontend (`crates/web-ui/src/static/index.html`)**
- Range controls: preset buttons (1h / 6h / 24h / 7d) + "Custom" expanding two `datetime-local` inputs with Apply button
- Shared `Bucket` dropdown (per second / per minute / per hour) affecting both request-rate and status-code charts
- Request rate chart: labels and title update dynamically with selected bucket
- Status codes: doughnut replaced by stacked bar chart (2xx green, 3xx blue, 4xx yellow, 5xx red)
- Top URLs: "Show all" button fetches unlimited results; "Show less" restores cached 10-row view

**Tests**: 15/15 passing (added `status_timeseries_endpoint_returns_array`, `requests_timeseries_with_range_params`, `requests_timeseries_with_hours_param`, `top_urls_limit_zero_returns_all`)

**Deploy fix**: `scripts/deploy.sh` now stops services before installing binaries (avoids "text file busy" error on re-deploy), using `sudo install` for atomic placement.

### Known issues / notes

- The `goblin_metrics.log` nginx log is recreated (empty) on each nginx reload/restart. The log-ingestor detects file shrinkage and resets the offset automatically.
- The log-ingestor sleeps 500 ms between polls; ingestion latency is < 1 second under normal load.
- `sys-metrics` reads `/proc/stat` twice 100 ms apart to compute CPU usage delta — this means one tick consumes 100 ms of the 1-second interval.

---

## Iteration 3 — Gitignore, Bar/Line Toggle, Host Filtering
**Date:** 2026-05-10  
**Status:** ✅ Complete

### What changed

**`.gitignore`** (new)  
Standard Rust ignores: `/target/`, `**/*.rs.bk`, `.env`, `*.db`, `*.db-wal`, `*.db-shm`. `Cargo.lock` is tracked (binary workspace convention).

**Database schema**  
Added `host TEXT DEFAULT ''` column to `requests` table.  
Migration strategy:
- `migrations/001_init.sql` — `host` column included for fresh installs; `idx_req_host` deliberately absent (would fail on existing DBs via `sqlite3` CLI before the column is added)
- `migrations/002_add_host.sql` — `ALTER TABLE requests ADD COLUMN host TEXT DEFAULT ''` for existing installs (deploy script runs with `2>/dev/null || true`)
- `run_migrations()` in both binaries runs the `ALTER TABLE` and `CREATE INDEX IF NOT EXISTS idx_req_host` inline via sqlx, ignoring errors for idempotency

**nginx logging**  
Added `"host":"$host"` as last field in `goblin_json` log_format in `deploy/nginx-logging.conf`.

**log-ingestor (`crates/log-ingestor/`)**  
- `parser.rs`: `LogEntry` and `RawEntry` gain `host: String` (`#[serde(default)]` handles old log lines)
- `db.rs`: `INSERT` statement includes `host` as `?4`
- `main.rs`: `read_line` replaced with `read_until(b'\n')` + `String::from_utf8_lossy` — nginx's `escape=json` does not escape raw bytes above 0x7F (e.g. TLS ClientHello data hitting the HTTP port), which caused `read_line` to error on every poll

**web-ui (`crates/web-ui/src/`)**  
- `api.rs`: `RangeQuery` and `TopUrlsQuery` gain `host: Option<String>`; all `requests` SQL queries add `AND (?N IS NULL OR host = ?N)`; new `requests_hosts` handler returns distinct non-empty hosts
- `main.rs`: new route `GET /api/requests/hosts`; 2 new tests (`requests_hosts_endpoint_returns_array`, `requests_timeseries_with_host_filter`)

**Frontend (`crates/web-ui/src/static/index.html`)**  
- **Host dropdown**: populated from `/api/requests/hosts` on load; selected host appended to all API calls via `rangeParams()`; selection persisted in `localStorage`
- **Bar/line toggle**: `▬` button on the RPS card swaps the chart type between bar and line in-place (preserves current data); state persisted in `localStorage`; icon switches to `∿` in line mode

### API additions

```
GET /api/requests/hosts                                    → ["goblin.geno.su", ...]
GET /api/requests/timeseries?hours=1&host=goblin.geno.su   → filtered timeseries
GET /api/requests/status_codes?hours=1&host=goblin.geno.su → filtered status codes
GET /api/requests/top_urls?hours=1&host=goblin.geno.su     → filtered top URLs
```

### Tests

18 tests across all three crates — all pass.

### Bug fixed during deploy

`read_line` (requires valid UTF-8) crashed on log lines containing raw TLS handshake bytes logged by nginx when bots hit port 80 with HTTPS clients. Fixed by switching to `read_until(b'\n', &mut Vec<u8>)` and decoding with `from_utf8_lossy`.

---

## Iteration 4 — Latency, Tooltips, Status Toggle, Method Chart
**Date:** 2026-05-10  
**Status:** ✅ Complete

### What changed

**nginx logging (`deploy/nginx-logging.conf`)**  
Added `"request_time":$request_time` (bare float, seconds) as the last field in `goblin_json`.

**Database schema**  
Two new columns on `requests`:
- `method TEXT DEFAULT ''` — HTTP verb extracted from the `request` field
- `response_time_ms REAL DEFAULT 0` — `nginx $request_time * 1000`

Migration strategy identical to iteration 3:
- `migrations/001_init.sql` — columns included for fresh installs; index omitted
- `migrations/003_add_latency.sql` — `ALTER TABLE` for existing installs (deploy runs with `2>/dev/null || true`)
- `run_migrations()` in both binaries — inline `ALTER TABLE` + `CREATE INDEX IF NOT EXISTS idx_req_method`

**log-ingestor (`crates/log-ingestor/`)**  
- `parser.rs`: `RawEntry` gains `request_time: f64` (`#[serde(default)]`); `LogEntry` gains `method: String` and `response_time_ms: f64`; new `extract_method()` filters to known HTTP verbs only (non-verb tokens → `""`, handles garbage from TLS bots)
- `db.rs`: INSERT now writes 9 columns including `method` and `response_time_ms`

**web-ui (`crates/web-ui/src/`)**  
- `api.rs`: new `LatencyPoint { bucket_ts, method, avg_ms }` and `requests_latency_by_method` handler — groups by `(bucket_ts, method)`, filters `method != ''`
- `main.rs`: route `GET /api/requests/latency_by_method`; 2 new tests

**Frontend (`crates/web-ui/src/static/index.html`)**  
- **Hover tooltips**: `lineChart()` and `barChart()` accept a `suffix` parameter (`'%'`, `' req'`, `''`); tooltip label callbacks format as `"Label: value[suffix]"`. Stacked charts use `tooltip: { mode: 'index' }` showing all series at a cursor position
- **Status chart toggle**: `▬`/`∿` button on the status codes card, same pattern as RPS toggle. New `stackedLineChart()` factory for line mode. Toggle preserves current datasets and persists to localStorage
- **Latency chart**: New `Response Time by Method` card between status codes and top URLs. `multiLineChart()` factory with ms y-axis label. `buildLatencyDatasets()` builds one line per method (GET=blue, POST=green, PUT=yellow, DELETE=red, PATCH=cyan, HEAD=purple). Null-fills missing (bucket, method) pairs; `spanGaps: false`

### API additions

```
GET /api/requests/latency_by_method?hours=1&bucket=minute&host=goblin.geno.su
→ [{"bucket_ts":..., "method":"GET", "avg_ms":4.0}, ...]
```

### Tests

22 tests across all three crates — all pass.

### Bug fixed during deploy

The `response_time_ms > 0` filter in the latency query excluded nginx 301 redirects (processed in ~0ms, `request_time: 0.000`). Fixed by removing the filter — `method != ''` alone is sufficient to skip rows with no latency data.

---

## Iteration 5 — Status Toggle Fix, Split Latency Metrics
**Date:** 2026-05-10  
**Status:** ✅ Complete

### What changed

**Bug fix — status codes line mode**  
`buildStatusDatasets()` set `borderWidth: 0` (fine for bars but invisible lines). Changed to `borderWidth: 1.5` and added `pointRadius: 0`, `tension: 0.3`, `fill: true` so datasets render correctly in both bar and stacked-line modes.

**Latency split into two distinct metrics**  
Per user request, the single "Response Time by Method" chart was replaced with two specialized charts:

1. **Avg Backend Latency (nginx logs)** — single line, overall avg from nginx `$request_time`, respects the global host filter.
2. **Dashboard API Latency (UI-measured)** — multi-line chart, measured client-side via `performance.now()` around each `fetchJSON` call. One line per dashboard API endpoint (`metrics`, `requests/timeseries`, etc.). Rolling window of 60 timings per endpoint. Manual clear button (⟲).

**API changes (`crates/web-ui/src/api.rs` + `main.rs`)**  
- Removed `requests_latency_by_method` handler / route
- Added `requests_latency` handler (single avg per bucket) + `/api/requests/latency` route
- `LatencyPoint` struct shrunk: `{ bucket_ts, avg_ms }` (no `method`)
- Tests renamed: `latency_endpoint_returns_array`, `latency_endpoint_averages_across_methods`

**Frontend (`crates/web-ui/src/static/index.html`)**  
- `fetchJSON()` wrapped to record `(timestamp, ms)` per endpoint into in-memory `apiTimings` map
- Removed `methodColor`/`METHOD_COLORS`; added `endpointColor(i)` cycling through the palette
- New `updateLatency(rows)` for the single-line backend latency chart
- New `buildApiLatencyDatasets()` for the multi-line dashboard chart, aligning all endpoints' timings on a unified label axis (null gaps, `spanGaps: false`)

### Tests

22 tests across all three crates — all pass.
