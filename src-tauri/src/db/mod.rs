mod migrations;
mod pool;
pub mod queries;

pub use pool::Database;

#[cfg(test)]
pub fn migrations_run_on(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    migrations::run(conn)
}
