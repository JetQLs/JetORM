use std::{error::Error, fmt};

use crate::diff::SchemaChange;
use crate::model::{SchemaSet, TableName};

/// Failure while applying one [`SchemaChange`] to a [`SchemaSet`].
///
/// Apply errors indicate that a change set was produced against a different
/// state than the one it is being applied to — the exact drift that
/// migration replay must surface instead of papering over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// A created or renamed-to table already exists.
    TableExists(TableName),
    /// A referenced table does not exist.
    TableMissing(TableName),
    /// An added or renamed-to column already exists.
    ColumnExists {
        /// Table owning the column.
        table: TableName,
        /// Conflicting column name.
        column: String,
    },
    /// A referenced column does not exist.
    ColumnMissing {
        /// Table expected to own the column.
        table: TableName,
        /// Missing column name.
        column: String,
    },
    /// A change's recorded prior state disagrees with the actual state.
    StateMismatch {
        /// Table owning the mismatch.
        table: TableName,
        /// Human-readable description of the disagreement.
        detail: String,
    },
}

impl fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TableExists(table) => write!(formatter, "table {table} already exists"),
            Self::TableMissing(table) => write!(formatter, "table {table} does not exist"),
            Self::ColumnExists { table, column } => {
                write!(formatter, "column {table}.{column} already exists")
            }
            Self::ColumnMissing { table, column } => {
                write!(formatter, "column {table}.{column} does not exist")
            }
            Self::StateMismatch { table, detail } => {
                write!(formatter, "state mismatch on {table}: {detail}")
            }
        }
    }
}

impl Error for ApplyError {}

impl SchemaSet {
    /// Applies one change, validating it against the current state.
    ///
    /// # Errors
    ///
    /// Returns an error when the change references entities that do not
    /// exist, would overwrite entities that do, or records a prior state
    /// that disagrees with this set.
    pub fn apply(&mut self, change: &SchemaChange) -> Result<(), ApplyError> {
        match change {
            SchemaChange::CreateTable(table) => {
                if self.table(table.name()).is_some() {
                    return Err(ApplyError::TableExists(table.name().clone()));
                }
                self.insert(table.clone());
                Ok(())
            }
            SchemaChange::DropTable(name) => {
                if self.remove(name).is_none() {
                    return Err(ApplyError::TableMissing(name.clone()));
                }
                Ok(())
            }
            SchemaChange::RenameTable { from, to } => {
                if self.table(to).is_some() {
                    return Err(ApplyError::TableExists(to.clone()));
                }
                let Some(mut table) = self.remove(from) else {
                    return Err(ApplyError::TableMissing(from.clone()));
                };
                table.set_name(to.clone());
                self.insert(table);
                Ok(())
            }
            SchemaChange::AddColumn { table, column } => {
                let definition = self.require_table_mut(table)?;
                if definition.column(column.name()).is_some() {
                    return Err(ApplyError::ColumnExists {
                        table: table.clone(),
                        column: column.name().to_owned(),
                    });
                }
                definition
                    .columns_mut()
                    .insert(column.name().to_owned(), column.clone());
                Ok(())
            }
            SchemaChange::DropColumn { table, column } => {
                let definition = self.require_table_mut(table)?;
                if definition.columns_mut().remove(column).is_none() {
                    return Err(ApplyError::ColumnMissing {
                        table: table.clone(),
                        column: column.clone(),
                    });
                }
                Ok(())
            }
            SchemaChange::RenameColumn { table, from, to } => {
                let definition = self.require_table_mut(table)?;
                if definition.column(to).is_some() {
                    return Err(ApplyError::ColumnExists {
                        table: table.clone(),
                        column: to.clone(),
                    });
                }
                let Some(mut column) = definition.columns_mut().remove(from) else {
                    return Err(ApplyError::ColumnMissing {
                        table: table.clone(),
                        column: from.clone(),
                    });
                };
                column.set_name(to.clone());
                definition.columns_mut().insert(to.clone(), column);
                let renamed_key: Vec<String> = definition
                    .primary_key()
                    .iter()
                    .map(|name| {
                        if name == from {
                            to.clone()
                        } else {
                            name.clone()
                        }
                    })
                    .collect();
                definition.set_primary_key(renamed_key);
                Ok(())
            }
            SchemaChange::AlterColumnType {
                table,
                column,
                from,
                to,
            } => {
                let definition = self.require_column_mut(table, column)?;
                if definition.column_type() != *from {
                    return Err(ApplyError::StateMismatch {
                        table: table.clone(),
                        detail: format!(
                            "column {column} has type {:?}, change expected {from:?}",
                            definition.column_type()
                        ),
                    });
                }
                definition.set_column_type(*to);
                Ok(())
            }
            SchemaChange::SetNullable {
                table,
                column,
                nullable,
            } => {
                self.require_column_mut(table, column)?
                    .set_nullable(*nullable);
                Ok(())
            }
            SchemaChange::SetUnique {
                table,
                column,
                unique,
            } => {
                self.require_column_mut(table, column)?.set_unique(*unique);
                Ok(())
            }
            SchemaChange::SetAutoIncrement {
                table,
                column,
                auto_increment,
            } => {
                self.require_column_mut(table, column)?
                    .set_auto_increment(*auto_increment);
                Ok(())
            }
            SchemaChange::SetPrimaryKey { table, from, to } => {
                let definition = self.require_table_mut(table)?;
                if definition.primary_key() != from.as_slice() {
                    return Err(ApplyError::StateMismatch {
                        table: table.clone(),
                        detail: format!(
                            "primary key is ({}), change expected ({})",
                            definition.primary_key().join(", "),
                            from.join(", ")
                        ),
                    });
                }
                for column in to {
                    if definition.column(column).is_none() {
                        return Err(ApplyError::ColumnMissing {
                            table: table.clone(),
                            column: column.clone(),
                        });
                    }
                }
                definition.set_primary_key(to.clone());
                Ok(())
            }
        }
    }

    /// Applies every change in order, stopping at the first failure.
    ///
    /// # Errors
    ///
    /// Returns the first [`ApplyError`]; earlier changes stay applied, which
    /// mirrors how a partially failed non-transactional migration behaves.
    pub fn apply_all<'change>(
        &mut self,
        changes: impl IntoIterator<Item = &'change SchemaChange>,
    ) -> Result<(), ApplyError> {
        for change in changes {
            self.apply(change)?;
        }
        Ok(())
    }

    fn require_table_mut(
        &mut self,
        table: &TableName,
    ) -> Result<&mut crate::model::TableDef, ApplyError> {
        self.table_mut(table)
            .ok_or_else(|| ApplyError::TableMissing(table.clone()))
    }

    fn require_column_mut(
        &mut self,
        table: &TableName,
        column: &str,
    ) -> Result<&mut crate::model::ColumnDef, ApplyError> {
        self.require_table_mut(table)?
            .columns_mut()
            .get_mut(column)
            .ok_or_else(|| ApplyError::ColumnMissing {
                table: table.clone(),
                column: column.to_owned(),
            })
    }
}
