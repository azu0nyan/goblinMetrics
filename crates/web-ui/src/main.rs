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

    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/health", get(api::health))
        .route("/api/metrics/names", get(api::metric_names))
        .route("/api/metrics", get(api::get_metric))
        .route("/api/requests/timeseries", get(api::requests_timeseries))
        .route("/api/requests/status_codes", get(api::requests_status_codes))
        .route("/api/requests/top_urls", get(api::requests_top_urls))
        .layer(CompressionLayer::new())
        .with_state(pool);

    info!("web-ui listening on {bind}");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::query(include_str!("../../../migrations/001_init.sql"))
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::util::ServiceExt;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(include_str!("../../../migrations/001_init.sql"))
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    fn build_app(pool: sqlx::SqlitePool) -> axum::Router {
        use axum::{routing::get, Router, response::Html};
        use super::api;
        use tower_http::compression::CompressionLayer;

        Router::new()
            .route("/", get(|| async { Html(super::INDEX_HTML) }))
            .route("/health", get(api::health))
            .route("/api/metrics/names", get(api::metric_names))
            .route("/api/metrics", get(api::get_metric))
            .route("/api/requests/timeseries", get(api::requests_timeseries))
            .route("/api/requests/status_codes", get(api::requests_status_codes))
            .route("/api/requests/top_urls", get(api::requests_top_urls))
            .layer(CompressionLayer::new())
            .with_state(pool)
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = build_app(test_pool().await);
        let resp = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_names_endpoint_returns_array() {
        let app = build_app(test_pool().await);
        let resp = app
            .oneshot(Request::builder().uri("/api/metrics/names").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array());
    }

    #[tokio::test]
    async fn requests_timeseries_endpoint_returns_array() {
        let app = build_app(test_pool().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/requests/timeseries?hours=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array());
    }
}
