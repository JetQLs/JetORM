use std::collections::BTreeSet;
use std::fmt;

use jetorm_entity::ColumnType;
use serde::{Deserialize, Serialize};

use crate::model::{ColumnDef, SchemaSet, TableDef, TableName};

/// One schema change turning a current state toward a target state.
///
/// Changes are database-independent; dialect crates render them as DDL.
/// Rename variants are never produced by [`diff`] directly — a differ cannot
/// distinguish a rename from a drop-plus-add, so renames enter a diff only
/// through explicit confirmation of a [`RenameCandidate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaChange {
    /// Creates a table that does not exist yet.
    CreateTable(TableDef),
    /// Drops an existing table and all of its data.
    DropTable(TableName),
    /// Renames an existing table, preserving its definition and data.
    RenameTable {
        /// Current table identity.
        from: TableName,
        /// New table identity.
        to: TableName,
    },
    /// Adds one column to an existing table.
    AddColumn {
        /// Table receiving the column.
        table: TableName,
        /// Complete definition of the new column.
        column: ColumnDef,
    },
    /// Drops one column and its data from an existing table.
    DropColumn {
        /// Table losing the column.
        table: TableName,
        /// Name of the dropped column.
        column: String,
    },
    /// Renames one column, preserving its definition and data.
    RenameColumn {
        /// Table owning the column.
        table: TableName,
        /// Current column name.
        from: String,
        /// New column name.
        to: String,
    },
    /// Changes one column's stored type.
    AlterColumnType {
        /// Table owning the column.
        table: TableName,
        /// Column being altered.
        column: String,
        /// Type recorded in the current state; guards replay drift.
        from: ColumnType,
        /// Type required by the target state.
        to: ColumnType,
    },
    /// Changes whether one column accepts SQL `NULL`.
    SetNullable {
        /// Table owning the column.
        table: TableName,
        /// Column being altered.
        column: String,
        /// Whether the column accepts SQL `NULL` afterwards.
        nullable: bool,
    },
    /// Adds or removes one column's uniqueness constraint.
    SetUnique {
        /// Table owning the column.
        table: TableName,
        /// Column being altered.
        column: String,
        /// Whether the column is unique afterwards.
        unique: bool,
    },
    /// Adds or removes database-generated values for one column.
    SetAutoIncrement {
        /// Table owning the column.
        table: TableName,
        /// Column being altered.
        column: String,
        /// Whether the database generates values afterwards.
        auto_increment: bool,
    },
    /// Replaces a table's primary key.
    SetPrimaryKey {
        /// Table being altered.
        table: TableName,
        /// Key recorded in the current state; guards replay drift.
        from: Vec<String>,
        /// Key required by the target state.
        to: Vec<String>,
    },
}

impl SchemaChange {
    /// Reports whether applying this change can lose data or fail against
    /// existing rows.
    ///
    /// The flag is deliberately broad: it covers outright data loss
    /// (dropping tables or columns), lossy conversions (type changes), and
    /// constraint tightening that a populated table can violate (`NOT NULL`,
    /// `UNIQUE`, identity, primary-key rebuilds, adding a non-nullable
    /// column without a default). Tooling must require explicit confirmation
    /// before applying a flagged change.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        match self {
            Self::DropTable(_)
            | Self::DropColumn { .. }
            | Self::AlterColumnType { .. }
            | Self::SetPrimaryKey { .. } => true,
            Self::AddColumn { column, .. } => !column.is_nullable(),
            Self::SetNullable { nullable, .. } => !nullable,
            Self::SetUnique { unique, .. } => *unique,
            Self::SetAutoIncrement { auto_increment, .. } => *auto_increment,
            Self::CreateTable(_) | Self::RenameTable { .. } | Self::RenameColumn { .. } => false,
        }
    }

    /// Returns the table this change applies to.
    #[must_use]
    pub fn table(&self) -> &TableName {
        match self {
            Self::CreateTable(table) => table.name(),
            Self::DropTable(table) | Self::RenameTable { from: table, .. } => table,
            Self::AddColumn { table, .. }
            | Self::DropColumn { table, .. }
            | Self::RenameColumn { table, .. }
            | Self::AlterColumnType { table, .. }
            | Self::SetNullable { table, .. }
            | Self::SetUnique { table, .. }
            | Self::SetAutoIncrement { table, .. }
            | Self::SetPrimaryKey { table, .. } => table,
        }
    }
}

impl fmt::Display for SchemaChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateTable(table) => write!(formatter, "create table {}", table.name()),
            Self::DropTable(table) => write!(formatter, "drop table {table}"),
            Self::RenameTable { from, to } => {
                write!(formatter, "rename table {from} to {}", to.name())
            }
            Self::AddColumn { table, column } => {
                write!(formatter, "add column {}.{}", table, column.name())
            }
            Self::DropColumn { table, column } => {
                write!(formatter, "drop column {table}.{column}")
            }
            Self::RenameColumn { table, from, to } => {
                write!(formatter, "rename column {table}.{from} to {to}")
            }
            Self::AlterColumnType {
                table,
                column,
                from,
                to,
            } => write!(
                formatter,
                "alter column {table}.{column} type {from:?} -> {to:?}"
            ),
            Self::SetNullable {
                table,
                column,
                nullable,
            } => write!(
                formatter,
                "set column {table}.{column} {}",
                if *nullable { "nullable" } else { "not null" }
            ),
            Self::SetUnique {
                table,
                column,
                unique,
            } => write!(
                formatter,
                "set column {table}.{column} {}",
                if *unique { "unique" } else { "not unique" }
            ),
            Self::SetAutoIncrement {
                table,
                column,
                auto_increment,
            } => write!(
                formatter,
                "set column {table}.{column} {}",
                if *auto_increment {
                    "generated"
                } else {
                    "not generated"
                }
            ),
            Self::SetPrimaryKey { table, to, .. } => {
                write!(
                    formatter,
                    "set primary key of {table} to ({})",
                    to.join(", ")
                )
            }
        }
    }
}

/// A drop-plus-add pair that may in fact be a rename.
///
/// Candidates require identical definitions modulo the name, so confirming
/// one preserves the diff's round-trip guarantee exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenameCandidate {
    /// A dropped and a created table with identical definitions.
    Table {
        /// Dropped table identity.
        from: TableName,
        /// Created table identity.
        to: TableName,
    },
    /// A dropped and an added column with identical definitions.
    Column {
        /// Table owning both columns.
        table: TableName,
        /// Dropped column name.
        from: String,
        /// Added column name.
        to: String,
    },
}

/// Ordered change set produced by [`diff`], plus unconfirmed rename
/// candidates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDiff {
    changes: Vec<SchemaChange>,
    rename_candidates: Vec<RenameCandidate>,
}

impl SchemaDiff {
    /// Returns changes in deterministic application order.
    #[must_use]
    pub fn changes(&self) -> &[SchemaChange] {
        &self.changes
    }

    /// Returns detected but unconfirmed rename candidates.
    #[must_use]
    pub fn rename_candidates(&self) -> &[RenameCandidate] {
        &self.rename_candidates
    }

    /// Reports whether the two states were identical.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Reports whether any change requires explicit confirmation.
    #[must_use]
    pub fn has_destructive_changes(&self) -> bool {
        self.changes.iter().any(SchemaChange::is_destructive)
    }

    /// Rewrites one confirmed candidate's drop-plus-add pair into a rename.
    ///
    /// Returns `false` when the candidate's underlying changes are no longer
    /// present, for example because an overlapping candidate was confirmed
    /// first. A confirmed candidate is removed from the candidate list.
    pub fn confirm_rename(&mut self, candidate: &RenameCandidate) -> bool {
        let rewritten = match candidate {
            RenameCandidate::Table { from, to } => {
                self.rewrite_table_rename(from.clone(), to.clone())
            }
            RenameCandidate::Column { table, from, to } => {
                self.rewrite_column_rename(table, from, to)
            }
        };
        if rewritten {
            self.rename_candidates
                .retain(|existing| existing != candidate);
        }
        rewritten
    }

    fn rewrite_table_rename(&mut self, from: TableName, to: TableName) -> bool {
        let drop_position = self
            .changes
            .iter()
            .position(|change| matches!(change, SchemaChange::DropTable(name) if *name == from));
        let create_position = self.changes.iter().position(
            |change| matches!(change, SchemaChange::CreateTable(table) if *table.name() == to),
        );
        let (Some(drop_position), Some(create_position)) = (drop_position, create_position) else {
            return false;
        };
        let insert_at = drop_position.min(create_position);
        // Remove the later index first so the earlier index stays valid.
        self.changes.remove(drop_position.max(create_position));
        self.changes.remove(insert_at);
        self.changes
            .insert(insert_at, SchemaChange::RenameTable { from, to });
        true
    }

    fn rewrite_column_rename(&mut self, table: &TableName, from: &str, to: &str) -> bool {
        let drop_position = self.changes.iter().position(|change| {
            matches!(
                change,
                SchemaChange::DropColumn { table: t, column } if t == table && column == from
            )
        });
        let add_position = self.changes.iter().position(|change| {
            matches!(
                change,
                SchemaChange::AddColumn { table: t, column } if t == table && column.name() == to
            )
        });
        let (Some(drop_position), Some(add_position)) = (drop_position, add_position) else {
            return false;
        };
        let insert_at = drop_position.min(add_position);
        self.changes.remove(drop_position.max(add_position));
        self.changes.remove(insert_at);
        self.changes.insert(
            insert_at,
            SchemaChange::RenameColumn {
                table: table.clone(),
                from: from.to_owned(),
                to: to.to_owned(),
            },
        );
        true
    }
}

/// Computes the ordered change set turning `current` into `target`.
///
/// The result is deterministic for any pair of inputs, and applying it to
/// `current` with [`SchemaSet::apply`] reproduces `target` exactly. Renames
/// are surfaced as candidates, never guessed.
#[must_use]
pub fn diff(current: &SchemaSet, target: &SchemaSet) -> SchemaDiff {
    let mut changes = Vec::new();
    let mut rename_candidates = Vec::new();

    let names: BTreeSet<&TableName> = current
        .tables()
        .map(TableDef::name)
        .chain(target.tables().map(TableDef::name))
        .collect();

    for name in &names {
        match (current.table(name), target.table(name)) {
            (Some(_), None) => changes.push(SchemaChange::DropTable((*name).clone())),
            (None, Some(created)) => changes.push(SchemaChange::CreateTable(created.clone())),
            (Some(from), Some(to)) => {
                diff_table(from, to, &mut changes, &mut rename_candidates);
            }
            (None, None) => unreachable!("names came from one of the two sets"),
        }
    }

    for dropped in current.tables() {
        if target.table(dropped.name()).is_some() {
            continue;
        }
        for created in target.tables() {
            if current.table(created.name()).is_some() {
                continue;
            }
            if same_table_shape(dropped, created) {
                rename_candidates.push(RenameCandidate::Table {
                    from: dropped.name().clone(),
                    to: created.name().clone(),
                });
            }
        }
    }

    SchemaDiff {
        changes,
        rename_candidates,
    }
}

/// Reports whether two tables are identical modulo their name.
fn same_table_shape(left: &TableDef, right: &TableDef) -> bool {
    left.primary_key() == right.primary_key() && left.columns().eq(right.columns())
}

fn diff_table(
    current: &TableDef,
    target: &TableDef,
    changes: &mut Vec<SchemaChange>,
    rename_candidates: &mut Vec<RenameCandidate>,
) {
    let table = current.name().clone();

    for added in target.columns() {
        if current.column(added.name()).is_none() {
            changes.push(SchemaChange::AddColumn {
                table: table.clone(),
                column: added.clone(),
            });
        }
    }

    for column in current.columns() {
        let Some(desired) = target.column(column.name()) else {
            continue;
        };
        if column.column_type() != desired.column_type() {
            changes.push(SchemaChange::AlterColumnType {
                table: table.clone(),
                column: column.name().to_owned(),
                from: column.column_type(),
                to: desired.column_type(),
            });
        }
        if column.is_nullable() != desired.is_nullable() {
            changes.push(SchemaChange::SetNullable {
                table: table.clone(),
                column: column.name().to_owned(),
                nullable: desired.is_nullable(),
            });
        }
        if column.is_unique() != desired.is_unique() {
            changes.push(SchemaChange::SetUnique {
                table: table.clone(),
                column: column.name().to_owned(),
                unique: desired.is_unique(),
            });
        }
        if column.is_auto_increment() != desired.is_auto_increment() {
            changes.push(SchemaChange::SetAutoIncrement {
                table: table.clone(),
                column: column.name().to_owned(),
                auto_increment: desired.is_auto_increment(),
            });
        }
    }

    for dropped in current.columns() {
        if target.column(dropped.name()).is_none() {
            changes.push(SchemaChange::DropColumn {
                table: table.clone(),
                column: dropped.name().to_owned(),
            });
        }
    }

    if current.primary_key() != target.primary_key() {
        changes.push(SchemaChange::SetPrimaryKey {
            table: table.clone(),
            from: current.primary_key().to_vec(),
            to: target.primary_key().to_vec(),
        });
    }

    for dropped in current.columns() {
        if target.column(dropped.name()).is_some() {
            continue;
        }
        for added in target.columns() {
            if current.column(added.name()).is_some() {
                continue;
            }
            if dropped.same_shape(added) {
                rename_candidates.push(RenameCandidate::Column {
                    table: table.clone(),
                    from: dropped.name().to_owned(),
                    to: added.name().to_owned(),
                });
            }
        }
    }
}
