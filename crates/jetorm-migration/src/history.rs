use chrono::{DateTime, Utc};
use jetorm_entity::ColumnType;
use jetorm_schema::{ColumnDef, TableDef, TableName};

/// Name of the table recording which migrations are applied.
pub const HISTORY_TABLE: &str = "jetorm_migrations";

/// One row of the migration history table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMigration {
    version: String,
    applied_at: DateTime<Utc>,
}

impl AppliedMigration {
    pub(crate) const fn new(version: String, applied_at: DateTime<Utc>) -> Self {
        Self {
            version,
            applied_at,
        }
    }

    /// Returns the applied migration's version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns when the migration was applied.
    #[must_use]
    pub const fn applied_at(&self) -> DateTime<Utc> {
        self.applied_at
    }
}

/// Describes the history table using JetORM's own schema model.
///
/// The migration system bootstraps itself through the same metadata → DDL
/// path it uses for user tables, so the history table cannot be spelled by a
/// second, divergent code path.
pub(crate) fn history_table() -> TableDef {
    TableDef::new(TableName::new(HISTORY_TABLE))
        .with_column(ColumnDef::new("version", ColumnType::Text))
        .with_column(ColumnDef::new("applied_at", ColumnType::TimestampUtc))
        .with_primary_key(vec!["version".to_owned()])
}
