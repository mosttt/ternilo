use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use ternilo_protocol::HarnessError;

pub(crate) async fn sqlite_snapshot(database_url: &str, output: &Path) -> Result<(), HarnessError> {
    if !database_url.starts_with("sqlite:") || database_url.contains(":memory:") {
        return Err(HarnessError::invalid(
            "online SQLite backup requires an existing file-backed SQLite database",
        ));
    }
    if output.try_exists().map_err(io_error)? {
        return Err(HarnessError::conflict(
            "backup output already exists; choose a new filename",
        ));
    }
    let output = std::path::absolute(output).map_err(io_error)?;
    crate::config::create_private_parent(&output)?;
    let parent = output
        .parent()
        .ok_or_else(|| HarnessError::invalid("backup output requires a parent directory"))?;
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    let filename = temporary
        .path()
        .to_str()
        .ok_or_else(|| HarnessError::invalid("backup path is not UTF-8"))?;
    let options = SqliteConnectOptions::from_str(database_url)
        .map_err(database_error)?
        .read_only(true)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(15));
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(database_error)?;
    // VACUUM INTO reads a consistent snapshot, including committed WAL pages,
    // without copying a live main database file or changing its journal mode.
    let copied = sqlx::query("VACUUM INTO $1")
        .bind(filename)
        .execute(&mut connection)
        .await;
    let closed = connection.close().await;
    copied.map_err(database_error)?;
    closed.map_err(database_error)?;
    temporary.as_file().sync_all().map_err(io_error)?;
    temporary
        .persist_noclobber(&output)
        .map_err(|error| io_error(error.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)?;
    Ok(())
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Use the error converter directly with Result::map_err."
)]
fn database_error(error: sqlx::Error) -> HarnessError {
    HarnessError::execution(format!("SQLite snapshot failed: {error}"))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Use the error converter directly with Result::map_err."
)]
fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::execution(format!("SQLite snapshot file operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_wal_snapshot_keeps_committed_rows_without_overwriting_files() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source.sqlite3");
        let output = temporary.path().join("backup.sqlite3");
        let url = format!("sqlite:{}", source.display());
        let mut writer = SqliteConnection::connect_with(
            &SqliteConnectOptions::from_str(&url)
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql("PRAGMA journal_mode=WAL; CREATE TABLE facts (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO facts VALUES (1, 'committed');")
            .execute(&mut writer).await.unwrap();
        let mut transaction = writer.begin().await.unwrap();
        sqlx::query("INSERT INTO facts VALUES (2, 'uncommitted')")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlite_snapshot(&url, &output).await.unwrap();
        transaction.commit().await.unwrap();
        let mut backup = SqliteConnection::connect(&format!("sqlite:{}?mode=ro", output.display()))
            .await
            .unwrap();
        let rows: Vec<String> = sqlx::query_scalar("SELECT value FROM facts ORDER BY id")
            .fetch_all(&mut backup)
            .await
            .unwrap();
        assert_eq!(rows, ["committed"]);
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA quick_check")
                .fetch_one(&mut backup)
                .await
                .unwrap(),
            "ok"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM facts")
                .fetch_one(&mut writer)
                .await
                .unwrap(),
            2
        );
        let bytes = std::fs::read(&output).unwrap();
        assert!(sqlite_snapshot(&url, &output).await.is_err());
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        assert!(sqlite_snapshot(&url, &source).await.is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
