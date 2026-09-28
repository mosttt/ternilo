use std::str::FromStr;

use sqlx::{ConnectOptions, postgres::PgConnectOptions};

pub fn database_url_for_role(admin_url: &str, username: &str, password: &str) -> String {
    PgConnectOptions::from_str(admin_url)
        .expect("test database URL must be a valid PostgreSQL connection URL")
        .username(username)
        .password(password)
        .to_url_lossy()
        .to_string()
}
