use crate::Error;
use sqlx::SqlitePool;

pub struct SettingsRepo<'a> {
    pool: &'a SqlitePool,
}

#[derive(sqlx::FromRow)]
struct SettingRow {
    key: String,
    value: String,
}

impl<'a> SettingsRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, Error> {
        let row = sqlx::query_as::<_, SettingRow>("SELECT key, value FROM settings WHERE key = ?1")
            .bind(key)
            .fetch_optional(self.pool)
            .await?;
        Ok(row.map(|r| r.value))
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
        )
        .bind(key)
        .bind(value)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<(String, String)>, Error> {
        let rows = sqlx::query_as::<_, SettingRow>("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
    }
}
