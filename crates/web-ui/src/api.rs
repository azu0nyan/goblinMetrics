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

#[derive(Deserialize)]
pub struct MetricQuery {
    pub name:  String,
    /// Hours of history to return (default 1).
    #[serde(default = "default_hours")]
    pub hours: f64,
}

fn default_hours() -> f64 { 1.0 }

#[derive(Serialize)]
pub struct MetricPoint {
    pub value:     f64,
    pub timestamp: i64,
}

pub async fn get_metric(
    State(pool): State<AppState>,
    Query(q): Query<MetricQuery>,
) -> Result<impl IntoResponse, AppError> {
    let since = now_ms() - hours_to_ms(q.hours);
    let rows: Vec<(f64, i64)> = sqlx::query_as(
        "SELECT metric_value, timestamp FROM metrics
         WHERE metric_name = ?1 AND timestamp >= ?2
         ORDER BY timestamp ASC",
    )
    .bind(&q.name)
    .bind(since)
    .fetch_all(&pool)
    .await?;

    let points: Vec<MetricPoint> = rows
        .into_iter()
        .map(|(value, timestamp)| MetricPoint { value, timestamp })
        .collect();
    Ok(Json(points))
}

// ── /api/requests/timeseries ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TimeQuery {
    #[serde(default = "default_hours")]
    pub hours: f64,
}

#[derive(Serialize)]
pub struct TimeseriesPoint {
    pub count:     i64,
    pub bucket_ts: i64,
}

pub async fn requests_timeseries(
    State(pool): State<AppState>,
    Query(q): Query<TimeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let since = now_ms() - hours_to_ms(q.hours);
    // bucket by minute (60 000 ms)
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT COUNT(*) as count, (timestamp / 60000) * 60000 AS bucket_ts
         FROM requests
         WHERE timestamp >= ?1
         GROUP BY bucket_ts
         ORDER BY bucket_ts ASC",
    )
    .bind(since)
    .fetch_all(&pool)
    .await?;

    let points: Vec<TimeseriesPoint> = rows
        .into_iter()
        .map(|(count, bucket_ts)| TimeseriesPoint { count, bucket_ts })
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
    Query(q): Query<TimeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let since = now_ms() - hours_to_ms(q.hours);
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT status_code, COUNT(*) FROM requests
         WHERE timestamp >= ?1
         GROUP BY status_code
         ORDER BY status_code ASC",
    )
    .bind(since)
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
    #[serde(default = "default_hours")]
    pub hours: f64,
    #[serde(default = "default_limit")]
    pub limit: i64,
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
    let since = now_ms() - hours_to_ms(q.hours);
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT url, COUNT(*) AS count FROM requests
         WHERE timestamp >= ?1
         GROUP BY url
         ORDER BY count DESC
         LIMIT ?2",
    )
    .bind(since)
    .bind(q.limit)
    .fetch_all(&pool)
    .await?;

    let data: Vec<UrlCount> = rows
        .into_iter()
        .map(|(url, count)| UrlCount { url, count })
        .collect();
    Ok(Json(data))
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
