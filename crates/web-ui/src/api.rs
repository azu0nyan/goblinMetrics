use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::time::{SystemTime, UNIX_EPOCH};

pub type AppState = SqlitePool;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn hours_to_ms(hours: f64) -> i64 {
    (hours * 3_600_000.0) as i64
}

// ── Shared query types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RangeQuery {
    pub from:        Option<i64>,
    pub to:          Option<i64>,
    pub hours:       Option<f64>,
    pub bucket:      Option<Bucket>,
    pub host:        Option<String>,
    pub path_prefix: Option<String>,
}

impl RangeQuery {
    pub fn resolve(&self) -> (i64, i64) {
        let to   = self.to.unwrap_or_else(now_ms);
        let from = self.from.unwrap_or_else(|| {
            to - hours_to_ms(self.hours.unwrap_or(1.0))
        });
        (from, to)
    }

    pub fn bucket_ms(&self) -> i64 {
        self.bucket.as_ref().map(Bucket::ms).unwrap_or(60_000)
    }

    pub fn host_filter(&self) -> Option<&str> {
        self.host.as_deref().filter(|h| !h.is_empty())
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Bucket {
    Second,
    #[default]
    Minute,
    Hour,
}

impl Bucket {
    pub fn ms(&self) -> i64 {
        match self {
            Bucket::Second => 1_000,
            Bucket::Minute => 60_000,
            Bucket::Hour   => 3_600_000,
        }
    }
}

// ── Path-filter helpers ───────────────────────────────────────────────────────

// SQL fragment added to all request queries.
// ?4 = path prefix (NULL = no filter), ?5 = 1 to negate (NOT LIKE), 0 to include (LIKE).
const PATH_FILTER_SQL: &str =
    "AND (?4 IS NULL OR (CASE WHEN ?5 = 1 \
     THEN (url NOT LIKE ?4 || '/%' AND url != ?4) \
     ELSE (url LIKE ?4 || '/%' OR url = ?4) END))";

fn path_parts(prefix: Option<&str>) -> (Option<&str>, i64) {
    match prefix.filter(|s| !s.is_empty()) {
        None                                    => (None, 0),
        Some(p) if p.starts_with('!') => (Some(&p[1..]), 1),
        Some(p)                                 => (Some(p), 0),
    }
}

// ── /health ──────────────────────────────────────────────────────────────────

pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

// ── /api/metrics/names ───────────────────────────────────────────────────────

pub async fn metric_names(State(pool): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT metric_name FROM metrics ORDER BY metric_name")
            .fetch_all(&pool)
            .await?;
    let names: Vec<String> = rows.into_iter().map(|(n,)| n).collect();
    Ok(Json(names))
}

// ── /api/metrics ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct MetricPoint {
    pub value:     f64,
    pub timestamp: i64,
}

#[derive(Deserialize)]
pub struct MetricQuery {
    pub name:  String,
    pub from:  Option<i64>,
    pub to:    Option<i64>,
    pub hours: Option<f64>,
}

pub async fn get_metric_named(
    State(pool): State<AppState>,
    Query(q): Query<MetricQuery>,
) -> Result<impl IntoResponse, AppError> {
    let to   = q.to.unwrap_or_else(now_ms);
    let from = q.from.unwrap_or_else(|| to - hours_to_ms(q.hours.unwrap_or(1.0)));

    let rows: Vec<(f64, i64)> = sqlx::query_as(
        "SELECT metric_value, timestamp FROM metrics
         WHERE metric_name = ?1 AND timestamp BETWEEN ?2 AND ?3
         ORDER BY timestamp ASC",
    )
    .bind(&q.name)
    .bind(from)
    .bind(to)
    .fetch_all(&pool)
    .await?;

    let points: Vec<MetricPoint> = rows
        .into_iter()
        .map(|(value, timestamp)| MetricPoint { value, timestamp })
        .collect();
    Ok(Json(points))
}

// ── /api/requests/hosts ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct HostFilter {
    pub label:       String,
    pub host:        String,
    pub path_prefix: Option<String>,
}

pub async fn requests_hosts(State(pool): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT host FROM requests WHERE host != '' ORDER BY host",
    )
    .fetch_all(&pool)
    .await?;

    let mut filters: Vec<HostFilter> = Vec::new();

    for (host,) in rows {
        if host == "goblin.geno.su" {
            filters.push(HostFilter {
                label:       "goblin.geno.su".into(),
                host:        host.clone(),
                path_prefix: Some("!/goblin-metrics".into()),
            });
            filters.push(HostFilter {
                label:       "metrics".into(),
                host:        host.clone(),
                path_prefix: Some("/goblin-metrics".into()),
            });
        } else {
            filters.push(HostFilter {
                label:       host.clone(),
                host:        host.clone(),
                path_prefix: None,
            });
        }
    }

    Ok(Json(filters))
}

// ── /api/requests/timeseries ─────────────────────────────────────────────────

#[derive(Serialize)]
pub struct TimeseriesPoint {
    pub count:     i64,
    pub bucket_ts: i64,
}

pub async fn requests_timeseries(
    State(pool): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = q.resolve();
    let bucket_ms  = q.bucket_ms();
    let host       = q.host_filter();
    let (path_val, negate) = path_parts(q.path_prefix.as_deref());

    let sql = format!(
        "SELECT COUNT(*) as count, (timestamp / {bucket_ms}) * {bucket_ms} AS bucket_ts
         FROM requests
         WHERE timestamp BETWEEN ?1 AND ?2
           AND (?3 IS NULL OR host = ?3)
           {PATH_FILTER_SQL}
         GROUP BY bucket_ts
         ORDER BY bucket_ts ASC"
    );

    let rows: Vec<(i64, i64)> = sqlx::query_as(&sql)
        .bind(from)
        .bind(to)
        .bind(host)
        .bind(path_val)
        .bind(negate)
        .fetch_all(&pool)
        .await?;

    let points: Vec<TimeseriesPoint> = rows
        .into_iter()
        .map(|(count, bucket_ts)| TimeseriesPoint { count, bucket_ts })
        .collect();
    Ok(Json(points))
}

// ── /api/requests/status_timeseries ──────────────────────────────────────────

#[derive(Serialize)]
pub struct StatusTimeseriesPoint {
    pub bucket_ts:   i64,
    pub status_code: i64,
    pub count:       i64,
}

pub async fn requests_status_timeseries(
    State(pool): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = q.resolve();
    let bucket_ms  = q.bucket_ms();
    let host       = q.host_filter();
    let (path_val, negate) = path_parts(q.path_prefix.as_deref());

    let sql = format!(
        "SELECT (timestamp / {bucket_ms}) * {bucket_ms} AS bucket_ts,
                status_code, COUNT(*) as count
         FROM requests
         WHERE timestamp BETWEEN ?1 AND ?2
           AND (?3 IS NULL OR host = ?3)
           {PATH_FILTER_SQL}
         GROUP BY bucket_ts, status_code
         ORDER BY bucket_ts ASC, status_code ASC"
    );

    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(&sql)
        .bind(from)
        .bind(to)
        .bind(host)
        .bind(path_val)
        .bind(negate)
        .fetch_all(&pool)
        .await?;

    let points: Vec<StatusTimeseriesPoint> = rows
        .into_iter()
        .map(|(bucket_ts, status_code, count)| StatusTimeseriesPoint { bucket_ts, status_code, count })
        .collect();
    Ok(Json(points))
}

// ── /api/requests/status_codes ───────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatusCodeCount {
    pub status_code: i64,
    pub count:       i64,
}

pub async fn requests_status_codes(
    State(pool): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = q.resolve();
    let host       = q.host_filter();
    let (path_val, negate) = path_parts(q.path_prefix.as_deref());

    let sql = format!(
        "SELECT status_code, COUNT(*) FROM requests
         WHERE timestamp BETWEEN ?1 AND ?2
           AND (?3 IS NULL OR host = ?3)
           {PATH_FILTER_SQL}
         GROUP BY status_code
         ORDER BY status_code ASC"
    );

    let rows: Vec<(i64, i64)> = sqlx::query_as(&sql)
        .bind(from)
        .bind(to)
        .bind(host)
        .bind(path_val)
        .bind(negate)
        .fetch_all(&pool)
        .await?;

    let data: Vec<StatusCodeCount> = rows
        .into_iter()
        .map(|(status_code, count)| StatusCodeCount { status_code, count })
        .collect();
    Ok(Json(data))
}

// ── /api/requests/top_urls ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TopUrlsQuery {
    pub from:        Option<i64>,
    pub to:          Option<i64>,
    pub hours:       Option<f64>,
    pub host:        Option<String>,
    pub path_prefix: Option<String>,
    #[serde(default = "default_limit")]
    pub limit:       i64,
}
fn default_limit() -> i64 { 10 }

#[derive(Serialize)]
pub struct UrlCount {
    pub url:   String,
    pub count: i64,
}

pub async fn requests_top_urls(
    State(pool): State<AppState>,
    Query(q): Query<TopUrlsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let to   = q.to.unwrap_or_else(now_ms);
    let from = q.from.unwrap_or_else(|| to - hours_to_ms(q.hours.unwrap_or(1.0)));
    let host = q.host.as_deref().filter(|h| !h.is_empty());
    let sql_limit: i64 = if q.limit == 0 { 5000 } else { q.limit };
    let (path_val, negate) = path_parts(q.path_prefix.as_deref());

    let sql = format!(
        "SELECT url, COUNT(*) AS count FROM requests
         WHERE timestamp BETWEEN ?1 AND ?2
           AND (?3 IS NULL OR host = ?3)
           {PATH_FILTER_SQL}
         GROUP BY url
         ORDER BY count DESC
         LIMIT ?6"
    );

    let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
        .bind(from)
        .bind(to)
        .bind(host)
        .bind(path_val)
        .bind(negate)
        .bind(sql_limit)
        .fetch_all(&pool)
        .await?;

    let data: Vec<UrlCount> = rows
        .into_iter()
        .map(|(url, count)| UrlCount { url, count })
        .collect();
    Ok(Json(data))
}

// ── /api/requests/top_ips ────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct IpCount {
    pub ip:    String,
    pub count: i64,
}

pub async fn requests_top_ips(
    State(pool): State<AppState>,
    Query(q): Query<TopUrlsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let to   = q.to.unwrap_or_else(now_ms);
    let from = q.from.unwrap_or_else(|| to - hours_to_ms(q.hours.unwrap_or(1.0)));
    let host = q.host.as_deref().filter(|h| !h.is_empty());
    let sql_limit: i64 = if q.limit == 0 { 5000 } else { q.limit };
    let (path_val, negate) = path_parts(q.path_prefix.as_deref());

    let sql = format!(
        "SELECT ip, COUNT(*) AS count FROM requests
         WHERE timestamp BETWEEN ?1 AND ?2
           AND (?3 IS NULL OR host = ?3)
           {PATH_FILTER_SQL}
           AND ip != ''
         GROUP BY ip
         ORDER BY count DESC
         LIMIT ?6"
    );

    let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
        .bind(from)
        .bind(to)
        .bind(host)
        .bind(path_val)
        .bind(negate)
        .bind(sql_limit)
        .fetch_all(&pool)
        .await?;

    let data: Vec<IpCount> = rows
        .into_iter()
        .map(|(ip, count)| IpCount { ip, count })
        .collect();
    Ok(Json(data))
}

// ── /api/requests/latency ───────────────────────────────────────────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct LatencyPoint {
    pub bucket_ts: i64,
    pub avg_ms:    f64,
}

pub async fn requests_latency(
    State(pool): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = q.resolve();
    let bucket_ms  = q.bucket_ms();
    let host       = q.host_filter();
    let (path_val, negate) = path_parts(q.path_prefix.as_deref());

    let sql = format!(
        "SELECT (timestamp / {bucket_ms}) * {bucket_ms} AS bucket_ts,
                AVG(response_time_ms) AS avg_ms
         FROM requests
         WHERE timestamp BETWEEN ?1 AND ?2
           AND (?3 IS NULL OR host = ?3)
           {PATH_FILTER_SQL}
           AND method != ''
         GROUP BY bucket_ts
         ORDER BY bucket_ts ASC"
    );

    let rows: Vec<LatencyPoint> = sqlx::query_as(&sql)
        .bind(from)
        .bind(to)
        .bind(host)
        .bind(path_val)
        .bind(negate)
        .fetch_all(&pool)
        .await?;

    Ok(Json(rows))
}

// ── Error type ───────────────────────────────────────────────────────────────

pub struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {}", self.0),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}
