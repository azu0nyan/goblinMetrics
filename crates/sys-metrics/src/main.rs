mod collectors;

use anyhow::Result;
use sqlx::SqlitePool;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("RUST_LOG")
                .add_directive("sys_metrics=info".parse()?),
        )
        .init();

    let db_path = std::env::var("DB_PATH")
        .unwrap_or_else(|_| "/var/lib/goblin-metrics/metrics.db".into());

    let pool = SqlitePool::connect(&format!("sqlite:{db_path}?mode=rwc"))
        .await?;

    sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await?;
    sqlx::query("PRAGMA busy_timeout=5000").execute(&pool).await?;

    run_migrations(&pool).await?;

    info!("sys-metrics started, writing to {db_path}");

    let mut ticker = interval(Duration::from_secs(1));
    loop {
        ticker.tick().await;
        if let Err(e) = collect_and_store(&pool).await {
            error!("collection error: {e:#}");
        }
    }
}

async fn collect_and_store(pool: &SqlitePool) -> Result<()> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis() as i64;

    let cpu  = collectors::collect_cpu().await?;
    let mem  = collectors::collect_memory()?;
    let load = collectors::collect_load()?;

    let mut tx = pool.begin().await?;

    let samples: &[(&str, f64)] = &[
        ("cpu_usage_pct",   cpu.usage_pct),
        ("memory_used_mb",  mem.used_mb),
        ("memory_free_mb",  mem.free_mb),
        ("memory_used_pct", mem.used_pct),
        ("load_avg_1m",     load.avg_1m),
    ];

    for (name, value) in samples {
        sqlx::query(
            "INSERT INTO metrics (metric_name, metric_value, timestamp) VALUES (?1, ?2, ?3)",
        )
        .bind(name)
        .bind(value)
        .bind(now_ms)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::query(include_str!("../../../migrations/001_init.sql"))
        .execute(pool)
        .await?;
    Ok(())
}
