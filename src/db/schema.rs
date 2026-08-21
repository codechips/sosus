//! Versioned, forward-only database migrations.

use rusqlite::{Connection, TransactionBehavior};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{DatabaseError, queries};

pub const LATEST_SCHEMA_VERSION: i64 = 1;

pub(super) fn migrate(connection: &mut Connection) -> Result<(), DatabaseError> {
    queries::create_schema_version(connection)?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = queries::current_schema_version(&transaction)?.unwrap_or(0);
    if current > LATEST_SCHEMA_VERSION {
        return Err(DatabaseError::UnsupportedSchema {
            found: current,
            supported: LATEST_SCHEMA_VERSION,
        });
    }

    if current < 1 {
        let applied_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
        queries::apply_v1(&transaction, &applied_at)?;
    }

    transaction.commit()?;
    Ok(())
}
