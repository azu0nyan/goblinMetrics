use crate::parser::LogEntry;
use anyhow::Result;
use sqlx::SqlitePool;

pub async fn insert_entries(pool: &SqlitePool, entries: &[LogEntry]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for e in entries {
        sqlx::query(
            "INSERT OR IGNORE INTO requests
             (timestamp, url, ip, host, user_agent, status_code, headers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(e.timestamp_ms)
        .bind(&e.url)
        .bind(&e.ip)
        .bind(&e.host)
        .bind(&e.user_agent)
        .bind(e.status_code)
        .bind(&e.headers)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn load_offset(pool: &SqlitePool) -> Result<u64> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM ingestor_state WHERE key = 'log_offset'")
            .fetch_optional(pool)
            .await?;
    Ok(row
        .and_then(|(v,)| v.parse().ok())
        .unwrap_or(0))
}

pub async fn save_offset(pool: &SqlitePool, offset: u64) -> Result<()> {
    sqlx::query(
        "INSERT INTO ingestor_state (key, value) VALUES ('log_offset', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(offset.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::parser::LogEntry;
    use sqlx::SqlitePool;

    pub async fn in_memory_pool() -> SqlitePool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(include_str!("../../../migrations/001_init.sql"))
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn db_insert_and_query() {
        let pool = in_memory_pool().await;
        let entries = vec![LogEntry {
            timestamp_ms: 1_000_000,
            url:          "/test".into(),
            ip:           "10.0.0.1".into(),
            host:         "goblin.geno.su".into(),
            user_agent:   Some("TestAgent".into()),
            status_code:  200,
            headers:      "{}".into(),
        }];
        insert_entries(&pool, &entries).await.unwrap();

        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM requests")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count.0, 1);
    }

    #[tokio::test]
    async fn offset_persist_and_load() {
        let pool = in_memory_pool().await;
        save_offset(&pool, 42_000).await.unwrap();
        let loaded = load_offset(&pool).await.unwrap();
        assert_eq!(loaded, 42_000);
    }
}
