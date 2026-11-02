//! Initial transactional migration, not a supervisor persistence implementation.
use rusqlite::{Connection, Result};
pub const SCHEMA_VERSION: u32 = 4;

pub fn migrate(connection: &mut Connection) -> Result<()> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let tx = connection.transaction()?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if version == 0 {
        tx.execute_batch(include_str!("schema.sql"))?;
    }
    if version < 2 {
        tx.execute_batch(include_str!("supervisor-schema.sql"))?;
    }
    if version < 3 {
        tx.execute_batch("ALTER TABLE fake_runs ADD COLUMN script_bytes BLOB;")?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    crate::execution::migrate(&tx)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()
}
