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
    /// An added foreign key's constraint name already exists on the table.
    ForeignKeyExists {
        /// Table owning the constraint.
        table: TableName,
        /// Conflicting constraint name.
        name: String,
    },
    /// A dropped foreign key does not exist.
    ForeignKeyMissing {
        /// Table expected to own the constraint.
        table: TableName,
        /// Missing constraint name.
        name: String,
    },
    /// A created enum type already exists.
    EnumExists {
        /// Type name.
        name: String,
    },
    /// A referenced enum type does not exist.
    EnumMissing {
        /// Type name.
        name: String,
    },
    /// An appended enum variant already exists.
    EnumVariantExists {
        /// Type name.
        name: String,
        /// The repeated variant.
        variant: String,
    },
    /// A dropped enum type is still used by a column.
    EnumInUse {
        /// Type name.
        name: String,
        /// Table owning the using column.
        table: TableName,
        /// The using column.
        column: String,
    },
    /// A dropped table or column is still referenced by a foreign key.
    ///
    /// Mirrors the database, where such a drop fails outright; the
    /// referencing constraint must be dropped first.
    StillReferenced {
        /// Table whose drop was rejected.
        table: TableName,
        /// Table owning the referencing constraint.
        referencing_table: TableName,
        /// Name of the referencing constraint.
        constraint: String,
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
            Self::ForeignKeyExists { table, name } => {
                write!(formatter, "foreign key {name} on {table} already exists")
            }
            Self::ForeignKeyMissing { table, name } => {
                write!(formatter, "foreign key {name} on {table} does not exist")
            }
            Self::EnumExists { name } => write!(formatter, "enum type {name} already exists"),
            Self::EnumMissing { name } => write!(formatter, "enum type {name} does not exist"),
            Self::EnumVariantExists { name, variant } => {
                write!(
                    formatter,
                    "enum type {name} already has variant {variant:?}"
                )
            }
            Self::EnumInUse {
                name,
                table,
                column,
            } => write!(
                formatter,
                "enum type {name} is still used by {table}.{column}"
            ),
            Self::StillReferenced {
                table,
                referencing_table,
                constraint,
            } => write!(
                formatter,
                "{table} is still referenced by foreign key {constraint} on {referencing_table}"
            ),
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
                // A self-reference vanishes with its table; any other
                // inbound constraint must be dropped first, as it would be
                // in the database.
                self.require_unreferenced(name, None)?;
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
                // The database tracks references by identity, so inbound
                // constraints follow a rename; mirror that in the model.
                for table in self.tables_values_mut() {
                    for foreign_key in table.foreign_keys_mut().values_mut() {
                        if foreign_key.target_table() == from {
                            foreign_key.set_target_table(to.clone());
                        }
                    }
                }
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
                self.require_unreferenced(table, Some(column))?;
                let definition = self.require_table_mut(table)?;
                if definition.columns_mut().remove(column).is_none() {
                    return Err(ApplyError::ColumnMissing {
                        table: table.clone(),
                        column: column.clone(),
                    });
                }
                // The database drops the altered table's own constraints
                // involving the column — whether the column is the
                // referencing side or the referenced side of a
                // self-reference.
                definition.foreign_keys_mut().retain(|_, foreign_key| {
                    foreign_key.column() != column
                        && !(foreign_key.target_table() == table
                            && foreign_key.target_column() == column)
                });
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
                // Constraints track columns by identity in the database, so
                // both the owning and the referencing side follow a rename.
                for foreign_key in definition.foreign_keys_mut().values_mut() {
                    if foreign_key.column() == from {
                        foreign_key.set_column(to.clone());
                    }
                }
                let renamed_table = table;
                for table in self.tables_values_mut() {
                    for foreign_key in table.foreign_keys_mut().values_mut() {
                        if foreign_key.target_table() == renamed_table
                            && foreign_key.target_column() == from
                        {
                            foreign_key.set_target_column(to.clone());
                        }
                    }
                }
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
            SchemaChange::AddForeignKey { table, foreign_key } => {
                {
                    let definition = self
                        .table(table)
                        .ok_or_else(|| ApplyError::TableMissing(table.clone()))?;
                    if definition.foreign_key(foreign_key.name()).is_some() {
                        return Err(ApplyError::ForeignKeyExists {
                            table: table.clone(),
                            name: foreign_key.name().to_owned(),
                        });
                    }
                    if definition.column(foreign_key.column()).is_none() {
                        return Err(ApplyError::ColumnMissing {
                            table: table.clone(),
                            column: foreign_key.column().to_owned(),
                        });
                    }
                }
                let target = self
                    .table(foreign_key.target_table())
                    .ok_or_else(|| ApplyError::TableMissing(foreign_key.target_table().clone()))?;
                let Some(referenced) = target.column(foreign_key.target_column()) else {
                    return Err(ApplyError::ColumnMissing {
                        table: foreign_key.target_table().clone(),
                        column: foreign_key.target_column().to_owned(),
                    });
                };
                // The database requires the referenced column to be unique;
                // catching the violation here keeps a bad change set from
                // reaching DDL at all.
                let is_sole_key = target.primary_key() == [referenced.name().to_owned()];
                if !referenced.is_unique() && !is_sole_key {
                    return Err(ApplyError::StateMismatch {
                        table: foreign_key.target_table().clone(),
                        detail: format!(
                            "column {} referenced by foreign key {} is neither \
                             unique nor the table's primary key",
                            foreign_key.target_column(),
                            foreign_key.name()
                        ),
                    });
                }
                let definition = self.require_table_mut(table)?;
                definition
                    .foreign_keys_mut()
                    .insert(foreign_key.name().to_owned(), foreign_key.clone());
                Ok(())
            }
            SchemaChange::DropForeignKey { table, name } => {
                let definition = self.require_table_mut(table)?;
                if definition.foreign_keys_mut().remove(name).is_none() {
                    return Err(ApplyError::ForeignKeyMissing {
                        table: table.clone(),
                        name: name.clone(),
                    });
                }
                Ok(())
            }
            SchemaChange::CreateEnum { name, variants } => {
                if self.enum_variants(name).is_some() {
                    return Err(ApplyError::EnumExists { name: name.clone() });
                }
                self.enums_mut().insert(name.clone(), variants.clone());
                Ok(())
            }
            SchemaChange::AddEnumVariant { name, variant } => {
                let Some(variants) = self.enums_mut().get_mut(name) else {
                    return Err(ApplyError::EnumMissing { name: name.clone() });
                };
                if variants.contains(variant) {
                    return Err(ApplyError::EnumVariantExists {
                        name: name.clone(),
                        variant: variant.clone(),
                    });
                }
                variants.push(variant.clone());
                Ok(())
            }
            SchemaChange::DropEnum { name } => {
                // The database refuses to drop a type in use; the model
                // mirrors that so replay fails where the server would.
                for table in self.tables() {
                    for column in table.columns() {
                        if column.type_name() == Some(name.as_str()) {
                            return Err(ApplyError::EnumInUse {
                                name: name.clone(),
                                table: table.name().clone(),
                                column: column.name().to_owned(),
                            });
                        }
                    }
                }
                if self.enums_mut().remove(name).is_none() {
                    return Err(ApplyError::EnumMissing { name: name.clone() });
                }
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

    /// Rejects the change when another table's foreign key targets `table`
    /// (or one of its columns, when `column` is given). The altered table's
    /// own constraints never count: a dropped table takes them along, and a
    /// dropped column takes the table's constraints involving it along,
    /// exactly as the database does.
    fn require_unreferenced(
        &self,
        table: &TableName,
        column: Option<&str>,
    ) -> Result<(), ApplyError> {
        for owner in self.tables() {
            if owner.name() == table {
                continue;
            }
            for foreign_key in owner.foreign_keys() {
                if foreign_key.target_table() != table {
                    continue;
                }
                if column.is_some_and(|column| foreign_key.target_column() != column) {
                    continue;
                }
                return Err(ApplyError::StillReferenced {
                    table: table.clone(),
                    referencing_table: owner.name().clone(),
                    constraint: foreign_key.name().to_owned(),
                });
            }
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
