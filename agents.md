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
