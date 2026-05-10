mod db;
mod parser;

use anyhow::Result;
use sqlx::SqlitePool;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("RUST_LOG")
                .add_directive("log_ingestor=info".parse()?),
        )
        .init();

    let db_path = std::env::var("DB_PATH")
        .unwrap_or_else(|_| "/var/lib/goblin-metrics/metrics.db".into());
    let log_file = std::env::var("LOG_FILE")
        .unwrap_or_else(|_| "/var/log/nginx/goblin_metrics.log".into());

    let pool = SqlitePool::connect(&format!("sqlite:{db_path}?mode=rwc")).await?;
    sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await?;
    sqlx::query("PRAGMA busy_timeout=5000").execute(&pool).await?;
    run_migrations(&pool).await?;

    info!("log-ingestor watching {log_file}");

    tail_loop(&pool, &log_file).await
}

async fn tail_loop(pool: &SqlitePool, path: &str) -> Result<()> {
    let mut offset = db::load_offset(pool).await?;
    info!("resuming from byte offset {offset}");

    loop {
        match process_new_lines(pool, path, &mut offset).await {
            Ok(n) if n > 0 => info!("ingested {n} log entries"),
            Ok(_) => {}
            Err(e) => error!("ingest error: {e:#}"),
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn process_new_lines(pool: &SqlitePool, path: &str, offset: &mut u64) -> Result<usize> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(0);
        }
        Err(e) => return Err(e.into()),
    };

    let file_len = file.metadata()?.len();

    // Log rotation: file shrank → start from beginning.
    if file_len < *offset {
        warn!("log file shrank ({file_len} < {offset}), resetting offset");
        *offset = 0;
        db::save_offset(pool, 0).await?;
    }

    if file_len == *offset {
        return Ok(0);
    }

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(*offset))?;

    let mut entries = Vec::new();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        match parser::parse_line(trimmed) {
            Ok(e) => entries.push(e),
            Err(e) => warn!("skip bad line: {e:#}"),
        }
    }

    let new_offset = reader.stream_position()?;
    let count = entries.len();

    db::insert_entries(pool, &entries).await?;
    *offset = new_offset;
    db::save_offset(pool, new_offset).await?;

    Ok(count)
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
