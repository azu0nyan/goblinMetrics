mod api;

use anyhow::Result;
use axum::{
    routing::get,
    Router,
    response::Html,
};
use sqlx::SqlitePool;
use tower_http::compression::CompressionLayer;
use tracing::info;

const INDEX_HTML: &str = include_str!("static/index.html");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("RUST_LOG")
                .add_directive("web_ui=info".parse()?),
        )
        .init();

    let db_path = std::env::var("DB_PATH")
        .unwrap_or_else(|_| "/var/lib/goblin-metrics/metrics.db".into());
    let bind = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:4444".into());

    let pool = SqlitePool::connect(&format!("sqlite:{db_path}?mode=rwc")).await?;
    sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await?;
    sqlx::query("PRAGMA busy_timeout=5000").execute(&pool).await?;
    run_migrations(&pool).await?;

    let app = build_app(pool);

    info!("web-ui listening on {bind}");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn build_app(pool: SqlitePool) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/health", get(api::health))
        .route("/api/metrics/names", get(api::metric_names))
        .route("/api/metrics", get(api::get_metric_named))
        .route("/api/requests/hosts", get(api::requests_hosts))
        .route("/api/requests/timeseries", get(api::requests_timeseries))
        .route("/api/requests/status_timeseries", get(api::requests_status_timeseries))
        .route("/api/requests/status_codes", get(api::requests_status_codes))
        .route("/api/requests/top_urls", get(api::requests_top_urls))
        .route("/api/requests/top_ips", get(api::requests_top_ips))
        .route("/api/requests/latency", get(api::requests_latency))
        .layer(CompressionLayer::new())
        .with_state(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::query(include_str!("../../../migrations/001_init.sql"))
        .execute(pool)
        .await?;
    let _ = sqlx::query("ALTER TABLE requests ADD COLUMN host TEXT DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_req_host ON requests(host)")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE requests ADD COLUMN method TEXT DEFAULT ''")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE requests ADD COLUMN response_time_ms REAL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("CREATE INDEX IF NOT EXISTS idx_req_method ON requests(method)")
        .execute(pool)
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::util::ServiceExt;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(include_str!("../../../migrations/001_init.sql"))
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn get(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let (status, body) = get(build_app(test_pool().await), "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn metrics_names_endpoint_returns_array() {
        let (status, body) = get(build_app(test_pool().await), "/api/metrics/names").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn requests_timeseries_with_range_params() {
        let (status, body) = get(
            build_app(test_pool().await),
            "/api/requests/timeseries?from=0&to=9999999999999&bucket=minute",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn requests_timeseries_with_hours_param() {
        let (status, body) = get(
            build_app(test_pool().await),
            "/api/requests/timeseries?hours=1&bucket=second",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn status_timeseries_endpoint_returns_array() {
        let (status, body) = get(
            build_app(test_pool().await),
            "/api/requests/status_timeseries?hours=1&bucket=minute",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn requests_hosts_endpoint_returns_array() {
        let (status, body) = get(build_app(test_pool().await), "/api/requests/hosts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn requests_timeseries_with_host_filter() {
        let pool = test_pool().await;
        let base_ts: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Insert rows for two different hosts
        for (i, host) in [(0, "site-a.example.com"), (1, "site-b.example.com")] {
            sqlx::query(
                "INSERT INTO requests (timestamp, url, ip, host, status_code, headers)
                 VALUES (?1, '/test', '127.0.0.1', ?2, 200, '{}')",
            )
            .bind(base_ts - i * 1000_i64)
            .bind(host)
            .execute(&pool)
            .await
            .unwrap();
        }
        // Filter for site-a only
        let (status, body) = get(
            build_app(pool),
            &format!("/api/requests/timeseries?hours=999&host=site-a.example.com"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let total: i64 = body.as_array().unwrap().iter().map(|p| p["count"].as_i64().unwrap_or(0)).sum();
        assert_eq!(total, 1, "host filter should return only site-a rows");
    }

    #[tokio::test]
    async fn latency_endpoint_returns_array() {
        let (status, body) = get(
            build_app(test_pool().await),
            "/api/requests/latency?hours=1&bucket=minute",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array());
    }

    #[tokio::test]
    async fn latency_endpoint_averages_across_methods() {
        let pool = test_pool().await;
        let base_ts: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        for (method, rt_ms) in [("GET", 50.0_f64), ("POST", 150.0_f64)] {
            sqlx::query(
                "INSERT INTO requests (timestamp, url, ip, status_code, headers, method, response_time_ms)
                 VALUES (?1, '/test', '127.0.0.1', 200, '{}', ?2, ?3)",
            )
            .bind(base_ts)
            .bind(method)
            .bind(rt_ms)
            .execute(&pool)
            .await
            .unwrap();
        }
        let (status, body) = get(
            build_app(pool),
            "/api/requests/latency?hours=999&bucket=minute",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!((arr[0]["avg_ms"].as_f64().unwrap() - 100.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn top_urls_limit_zero_returns_all() {
        let pool = test_pool().await;
        // Insert 15 distinct URLs with current-ish timestamps
        let base_ts: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        for i in 0..15i64 {
            sqlx::query(
                "INSERT INTO requests (timestamp, url, ip, status_code, headers)
                 VALUES (?1, ?2, '127.0.0.1', 200, '{}')",
            )
            .bind(base_ts - i * 1000)
            .bind(format!("/url-{i}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let (status, body) = get(
            build_app(pool),
            "/api/requests/top_urls?hours=999&limit=0",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().unwrap().len(), 15);
    }
}
