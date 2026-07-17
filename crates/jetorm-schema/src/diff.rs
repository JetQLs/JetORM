use std::collections::BTreeSet;
use std::fmt;

use jetorm_entity::ColumnType;
use serde::{Deserialize, Serialize};

use crate::model::{ColumnDef, ForeignKeyDef, SchemaSet, TableDef, TableName};

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
        /// Named database type in the current state — a native enum's
        /// name — when the column has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_type_name: Option<String>,
        /// Named database type required by the target state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_type_name: Option<String>,
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
    /// Adds one foreign-key constraint to an existing table.
    AddForeignKey {
        /// Table owning the constraint.
        table: TableName,
        /// Complete definition of the constraint.
        foreign_key: ForeignKeyDef,
    },
    /// Drops one foreign-key constraint by name.
    DropForeignKey {
        /// Table owning the constraint.
        table: TableName,
        /// Constraint name.
        name: String,
    },
    /// Creates one named enum type.
    CreateEnum {
        /// Type name.
        name: String,
        /// Variants in declaration order.
        variants: Vec<String>,
    },
    /// Appends one variant to an existing enum type.
    ///
    /// Appending is the only in-place evolution the database offers;
    /// removal or reordering drops and recreates the type.
    AddEnumVariant {
        /// Type name.
        name: String,
        /// The appended variant.
        variant: String,
    },
    /// Drops one named enum type.
    DropEnum {
        /// Type name.
        name: String,
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
            // Existing rows can violate a new constraint; dropping one
            // cannot fail and loses no data.
            Self::AddForeignKey { .. } => true,
            // Dropping a type fails while any column uses it; recreation
            // paths rewrite tables.
            Self::DropEnum { .. } => true,
            Self::CreateTable(_)
            | Self::RenameTable { .. }
            | Self::RenameColumn { .. }
            | Self::DropForeignKey { .. }
            | Self::CreateEnum { .. }
            | Self::AddEnumVariant { .. } => false,
        }
    }

    /// Returns the table this change applies to; enum-type changes apply
    /// to none.
    #[must_use]
    pub fn table(&self) -> Option<&TableName> {
        match self {
            Self::CreateTable(table) => Some(table.name()),
            Self::DropTable(table) | Self::RenameTable { from: table, .. } => Some(table),
            Self::AddColumn { table, .. }
            | Self::DropColumn { table, .. }
            | Self::RenameColumn { table, .. }
            | Self::AlterColumnType { table, .. }
            | Self::SetNullable { table, .. }
            | Self::SetUnique { table, .. }
            | Self::SetAutoIncrement { table, .. }
            | Self::SetPrimaryKey { table, .. }
            | Self::AddForeignKey { table, .. }
            | Self::DropForeignKey { table, .. } => Some(table),
            Self::CreateEnum { .. } | Self::AddEnumVariant { .. } | Self::DropEnum { .. } => None,
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
                from_type_name,
                to_type_name,
            } => {
                let spell = |column_type: &ColumnType, name: &Option<String>| match name {
                    Some(name) => name.clone(),
                    None => format!("{column_type:?}"),
                };
                write!(
                    formatter,
                    "alter column {table}.{column} type {} -> {}",
                    spell(from, from_type_name),
                    spell(to, to_type_name)
                )
            }
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
            Self::AddForeignKey { table, foreign_key } => write!(
                formatter,
                "add foreign key {} on {table}.{} -> {}.{}",
                foreign_key.name(),
                foreign_key.column(),
                foreign_key.target_table(),
                foreign_key.target_column()
            ),
            Self::DropForeignKey { table, name } => {
                write!(formatter, "drop foreign key {name} on {table}")
            }
            Self::CreateEnum { name, variants } => {
                write!(formatter, "create enum {name} ({})", variants.join(", "))
            }
            Self::AddEnumVariant { name, variant } => {
                write!(formatter, "add enum variant {name}.{variant}")
            }
            Self::DropEnum { name } => write!(formatter, "drop enum {name}"),
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
        // Applying the rename already rewrites the table's primary-key list,
        // so a primary-key change diffed from the drop-plus-add pair must
        // record the renamed state — and vanish entirely once the rename
        // makes it a no-op — or its drift guard would reject the very state
        // the rename produced.
        self.changes.retain_mut(|change| {
            let SchemaChange::SetPrimaryKey {
                table: t,
                from: prior,
                to: desired,
            } = change
            else {
                return true;
            };
            if t != table {
                return true;
            }
            for column in prior.iter_mut() {
                if column == from {
                    *column = to.to_owned();
                }
            }
            prior != desired
        });
        true
    }
}

/// Computes the ordered change set turning `current` into `target`.
///
/// The result is deterministic for any pair of inputs, and applying it to
/// `current` with [`SchemaSet::apply`] reproduces `target` exactly. Renames
/// are surfaced as candidates, never guessed.
///
/// Foreign keys bracket everything else: every `DropForeignKey` comes
/// first — a reference must be gone before its column or table can go —
/// and every `AddForeignKey` comes last, after all referenced tables and
/// columns exist. `CREATE TABLE` therefore never carries constraints
/// inline, so mutually referencing tables and self-references order
/// correctly no matter their names.
#[must_use]
pub fn diff(current: &SchemaSet, target: &SchemaSet) -> SchemaDiff {
    let mut foreign_key_drops = Vec::new();
    let mut changes = Vec::new();
    let mut foreign_key_adds = Vec::new();
    let mut rename_candidates = Vec::new();

    let names: BTreeSet<&TableName> = current
        .tables()
        .map(TableDef::name)
        .chain(target.tables().map(TableDef::name))
        .collect();

    for name in &names {
        match (current.table(name), target.table(name)) {
            (Some(dropped), None) => {
                // Dropping the constraints first makes `DROP TABLE` order
                // irrelevant even among mutually referencing tables.
                for foreign_key in dropped.foreign_keys() {
                    foreign_key_drops.push(SchemaChange::DropForeignKey {
                        table: (*name).clone(),
                        name: foreign_key.name().to_owned(),
                    });
                }
                changes.push(SchemaChange::DropTable((*name).clone()));
            }
            (None, Some(created)) => {
                changes.push(SchemaChange::CreateTable(created.without_foreign_keys()));
                for foreign_key in created.foreign_keys() {
                    foreign_key_adds.push(SchemaChange::AddForeignKey {
                        table: (*name).clone(),
                        foreign_key: foreign_key.clone(),
                    });
                }
            }
            (Some(from), Some(to)) => {
                diff_foreign_keys(from, to, &mut foreign_key_drops, &mut foreign_key_adds);
                diff_table(from, to, &mut changes, &mut rename_candidates);
            }
            (None, None) => unreachable!("names came from one of the two sets"),
        }
    }

    // Enum types bracket the whole set: a column can only take a type
    // that exists, and a type can only drop once nothing uses it.
    let mut enum_creates = Vec::new();
    let mut enum_drops = Vec::new();
    for (name, target_variants) in target.enums() {
        match current.enum_variants(name) {
            None => enum_creates.push(SchemaChange::CreateEnum {
                name: name.to_owned(),
                variants: target_variants.to_vec(),
            }),
            Some(current_variants) if current_variants == target_variants => {}
            Some(current_variants) => {
                // Appended variants evolve in place; anything else — a
                // removal or reorder — recreates the type.
                if target_variants.len() > current_variants.len()
                    && target_variants[..current_variants.len()] == *current_variants
                {
                    for variant in &target_variants[current_variants.len()..] {
                        enum_creates.push(SchemaChange::AddEnumVariant {
                            name: name.to_owned(),
                            variant: variant.clone(),
                        });
                    }
                } else {
                    // Recreation must drop first, and dropping fails while
                    // any column uses the type — an honest apply-time
                    // failure directing the author to a hand-written
                    // migration, since a silent rewrite would guess.
                    enum_creates.push(SchemaChange::DropEnum {
                        name: name.to_owned(),
                    });
                    enum_creates.push(SchemaChange::CreateEnum {
                        name: name.to_owned(),
                        variants: target_variants.to_vec(),
                    });
                }
            }
        }
    }
    for (name, _) in current.enums() {
        if target.enum_variants(name).is_none() {
            enum_drops.push(SchemaChange::DropEnum {
                name: name.to_owned(),
            });
        }
    }

    let mut ordered = enum_creates;
    ordered.append(&mut foreign_key_drops);
    ordered.append(&mut changes);
    ordered.append(&mut foreign_key_adds);
    ordered.append(&mut enum_drops);
    let changes = ordered;

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
///
/// Foreign keys compare by shape rather than by constraint name: names
/// regenerated from entity metadata embed the new table name, which must
/// not disqualify an otherwise exact rename. A self-reference necessarily
/// embeds the table's own name too, so each side's self-references count
/// as targeting "myself" rather than a literal name — otherwise a renamed
/// self-referencing table could never surface as a rename candidate.
fn same_table_shape(left: &TableDef, right: &TableDef) -> bool {
    left.primary_key() == right.primary_key()
        && left.columns().eq(right.columns())
        && left.foreign_keys().count() == right.foreign_keys().count()
        && left.foreign_keys().zip(right.foreign_keys()).all(|(a, b)| {
            let same_target = a.target_table() == b.target_table()
                || (a.target_table() == left.name() && b.target_table() == right.name());
            same_target
                && a.column() == b.column()
                && a.target_column() == b.target_column()
                && a.delete_action() == b.delete_action()
                && a.update_action() == b.update_action()
        })
}

/// Emits the drop-then-add changes reconciling one table's foreign keys.
///
/// A constraint whose definition changed drops and re-adds under the same
/// name: `ALTER CONSTRAINT` cannot change columns or targets.
fn diff_foreign_keys(
    current: &TableDef,
    target: &TableDef,
    drops: &mut Vec<SchemaChange>,
    adds: &mut Vec<SchemaChange>,
) {
    let table = current.name().clone();
    for existing in current.foreign_keys() {
        if target.foreign_key(existing.name()) != Some(existing) {
            drops.push(SchemaChange::DropForeignKey {
                table: table.clone(),
                name: existing.name().to_owned(),
            });
        }
    }
    for desired in target.foreign_keys() {
        if current.foreign_key(desired.name()) != Some(desired) {
            adds.push(SchemaChange::AddForeignKey {
                table: table.clone(),
                foreign_key: desired.clone(),
            });
        }
    }
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
        // The named type is part of a column's identity: text vs enum, or
        // one enum vs another, differ even when the carrier type matches.
        if column.column_type() != desired.column_type()
            || column.type_name() != desired.type_name()
        {
            changes.push(SchemaChange::AlterColumnType {
                table: table.clone(),
                column: column.name().to_owned(),
                from: column.column_type(),
                to: desired.column_type(),
                from_type_name: column.type_name().map(str::to_owned),
                to_type_name: desired.type_name().map(str::to_owned),
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
