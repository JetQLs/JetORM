use std::collections::BTreeMap;
use std::fmt;

use jetorm_entity::{ColumnType, Entity, ReferentialAction};
use serde::{Deserialize, Serialize};

/// Qualified table identity used as the schema-set key.
///
/// Ordering is lexicographic on `(schema, name)`, which keeps every walk of
/// a [`SchemaSet`] deterministic regardless of insertion order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TableName {
    schema: Option<String>,
    name: String,
}

impl TableName {
    /// Creates an unqualified table name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema: None,
            name: name.into(),
        }
    }

    /// Creates a schema-qualified table name.
    #[must_use]
    pub fn qualified(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: Some(schema.into()),
            name: name.into(),
        }
    }

    /// Returns the optional database schema qualifier.
    #[must_use]
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Returns the table name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for TableName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(schema) = &self.schema {
            write!(formatter, "{schema}.")?;
        }
        formatter.write_str(&self.name)
    }
}

/// Database-independent description of one column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    name: String,
    column_type: ColumnType,
    nullable: bool,
    unique: bool,
    auto_increment: bool,
}

impl ColumnDef {
    /// Creates a non-nullable column definition.
    #[must_use]
    pub fn new(name: impl Into<String>, column_type: ColumnType) -> Self {
        Self {
            name: name.into(),
            column_type,
            nullable: false,
            unique: false,
            auto_increment: false,
        }
    }

    /// Clones this definition allowing SQL `NULL` values.
    #[must_use]
    pub const fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// Clones this definition with a uniqueness constraint.
    #[must_use]
    pub const fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    /// Clones this definition as database-generated on insert.
    #[must_use]
    pub const fn auto_increment(mut self) -> Self {
        self.auto_increment = true;
        self
    }

    /// Returns the column name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the dialect-independent column type.
    #[must_use]
    pub const fn column_type(&self) -> ColumnType {
        self.column_type
    }

    /// Reports whether SQL `NULL` is a valid stored value.
    #[must_use]
    pub const fn is_nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether this column carries a uniqueness constraint.
    #[must_use]
    pub const fn is_unique(&self) -> bool {
        self.unique
    }

    /// Reports whether the database generates this column's value on insert.
    #[must_use]
    pub const fn is_auto_increment(&self) -> bool {
        self.auto_increment
    }

    /// Reports whether two definitions describe the same stored shape,
    /// ignoring the column name; used for rename-candidate detection.
    #[must_use]
    pub fn same_shape(&self, other: &Self) -> bool {
        self.column_type == other.column_type
            && self.nullable == other.nullable
            && self.unique == other.unique
            && self.auto_increment == other.auto_increment
    }

    pub(crate) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub(crate) fn set_column_type(&mut self, column_type: ColumnType) {
        self.column_type = column_type;
    }

    pub(crate) fn set_nullable(&mut self, nullable: bool) {
        self.nullable = nullable;
    }

    pub(crate) fn set_unique(&mut self, unique: bool) {
        self.unique = unique;
    }

    pub(crate) fn set_auto_increment(&mut self, auto_increment: bool) {
        self.auto_increment = auto_increment;
    }
}

/// Database-independent description of one foreign-key constraint.
///
/// A constraint is identified by its name within the owning table; the name
/// follows PostgreSQL's default spelling (`{table}_{column}_fkey`) when the
/// definition comes from entity metadata, matching how the dialect names
/// every other constraint it creates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyDef {
    name: String,
    column: String,
    target_table: TableName,
    target_column: String,
    on_delete: ReferentialAction,
    on_update: ReferentialAction,
}

impl ForeignKeyDef {
    /// Creates a foreign-key definition with `NO ACTION` semantics.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        column: impl Into<String>,
        target_table: TableName,
        target_column: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            column: column.into(),
            target_table,
            target_column: target_column.into(),
            on_delete: ReferentialAction::NoAction,
            on_update: ReferentialAction::NoAction,
        }
    }

    /// Clones this definition with an `ON DELETE` action.
    #[must_use]
    pub fn on_delete(mut self, action: ReferentialAction) -> Self {
        self.on_delete = action;
        self
    }

    /// Clones this definition with an `ON UPDATE` action.
    #[must_use]
    pub fn on_update(mut self, action: ReferentialAction) -> Self {
        self.on_update = action;
        self
    }

    /// Returns the constraint name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the referencing column's name.
    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    /// Returns the referenced table's identity.
    #[must_use]
    pub const fn target_table(&self) -> &TableName {
        &self.target_table
    }

    /// Returns the referenced column's name.
    #[must_use]
    pub fn target_column(&self) -> &str {
        &self.target_column
    }

    /// Returns the action taken when the referenced row is deleted.
    #[must_use]
    pub const fn delete_action(&self) -> ReferentialAction {
        self.on_delete
    }

    /// Returns the action taken when the referenced key is updated.
    #[must_use]
    pub const fn update_action(&self) -> ReferentialAction {
        self.on_update
    }

    /// Reports whether two definitions describe the same constraint,
    /// ignoring the constraint name; used for rename-candidate detection,
    /// where regenerated names embed the new table name.
    #[must_use]
    pub fn same_shape(&self, other: &Self) -> bool {
        self.column == other.column
            && self.target_table == other.target_table
            && self.target_column == other.target_column
            && self.on_delete == other.on_delete
            && self.on_update == other.on_update
    }

    pub(crate) fn set_column(&mut self, column: String) {
        self.column = column;
    }

    pub(crate) fn set_target_table(&mut self, table: TableName) {
        self.target_table = table;
    }

    pub(crate) fn set_target_column(&mut self, column: String) {
        self.target_column = column;
    }
}

/// Database-independent description of one table.
///
/// Column order is deliberately not semantic: relational DDL cannot reorder
/// columns in place, so the model stores them keyed by name and every walk
/// is alphabetical. Positional row layout for query decoding is an entity
/// concern, not a schema concern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    name: TableName,
    columns: BTreeMap<String, ColumnDef>,
    primary_key: Vec<String>,
    #[serde(default)]
    foreign_keys: BTreeMap<String, ForeignKeyDef>,
}

impl TableDef {
    /// Creates a table definition without columns.
    #[must_use]
    pub fn new(name: TableName) -> Self {
        Self {
            name,
            columns: BTreeMap::new(),
            primary_key: Vec::new(),
            foreign_keys: BTreeMap::new(),
        }
    }

    /// Inserts or replaces one column.
    #[must_use]
    pub fn with_column(mut self, column: ColumnDef) -> Self {
        self.columns.insert(column.name().to_owned(), column);
        self
    }

    /// Inserts or replaces one foreign-key constraint.
    #[must_use]
    pub fn with_foreign_key(mut self, foreign_key: ForeignKeyDef) -> Self {
        self.foreign_keys
            .insert(foreign_key.name().to_owned(), foreign_key);
        self
    }

    /// Replaces the primary-key column names.
    #[must_use]
    pub fn with_primary_key(mut self, columns: impl Into<Vec<String>>) -> Self {
        self.primary_key = columns.into();
        self
    }

    /// Builds the table definition described by an entity's metadata.
    #[must_use]
    pub fn from_entity<E>() -> Self
    where
        E: Entity,
    {
        let name = match E::TABLE.schema() {
            Some(schema) => TableName::qualified(schema, E::TABLE.name()),
            None => TableName::new(E::TABLE.name()),
        };
        let mut table = Self::new(name);
        for column in E::COLUMNS {
            let mut definition = ColumnDef::new(column.name(), column.column_type());
            if column.is_nullable() {
                definition = definition.nullable();
            }
            if column.is_unique() {
                definition = definition.unique();
            }
            if column.is_auto_increment() {
                definition = definition.auto_increment();
            }
            table = table.with_column(definition);
        }
        for foreign_key in E::FOREIGN_KEYS {
            let column = E::COLUMNS[foreign_key.column()].name();
            let target = match foreign_key.target_table().schema() {
                Some(schema) => TableName::qualified(schema, foreign_key.target_table().name()),
                None => TableName::new(foreign_key.target_table().name()),
            };
            table = table.with_foreign_key(
                ForeignKeyDef::new(
                    format!("{}_{column}_fkey", E::TABLE.name()),
                    column,
                    target,
                    foreign_key.target_column(),
                )
                .on_delete(foreign_key.actions().on_delete())
                .on_update(foreign_key.actions().on_update()),
            );
        }
        table.with_primary_key(
            E::PRIMARY_KEY
                .iter()
                .map(|index| E::COLUMNS[*index].name().to_owned())
                .collect::<Vec<_>>(),
        )
    }

    /// Returns the qualified table identity.
    #[must_use]
    pub const fn name(&self) -> &TableName {
        &self.name
    }

    /// Iterates columns in alphabetical order.
    pub fn columns(&self) -> impl Iterator<Item = &ColumnDef> {
        self.columns.values()
    }

    /// Returns one column by name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.get(name)
    }

    /// Returns primary-key column names in key order.
    #[must_use]
    pub fn primary_key(&self) -> &[String] {
        &self.primary_key
    }

    /// Iterates foreign keys in constraint-name order.
    pub fn foreign_keys(&self) -> impl Iterator<Item = &ForeignKeyDef> {
        self.foreign_keys.values()
    }

    /// Returns one foreign key by constraint name.
    #[must_use]
    pub fn foreign_key(&self, name: &str) -> Option<&ForeignKeyDef> {
        self.foreign_keys.get(name)
    }

    /// Clones this definition without its foreign keys.
    ///
    /// [`crate::diff`] emits `CREATE TABLE` without constraints and adds
    /// them separately once every referenced table exists, so creation
    /// order can never break a reference.
    #[must_use]
    pub fn without_foreign_keys(&self) -> Self {
        Self {
            foreign_keys: BTreeMap::new(),
            ..self.clone()
        }
    }

    pub(crate) fn columns_mut(&mut self) -> &mut BTreeMap<String, ColumnDef> {
        &mut self.columns
    }

    pub(crate) fn foreign_keys_mut(&mut self) -> &mut BTreeMap<String, ForeignKeyDef> {
        &mut self.foreign_keys
    }

    pub(crate) fn set_primary_key(&mut self, columns: Vec<String>) {
        self.primary_key = columns;
    }

    pub(crate) fn set_name(&mut self, name: TableName) {
        self.name = name;
    }
}

/// One complete schema state: every table, deterministically ordered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSet {
    tables: BTreeMap<TableName, TableDef>,
}

impl SchemaSet {
    /// Creates an empty schema state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces one table definition.
    pub fn insert(&mut self, table: TableDef) {
        self.tables.insert(table.name().clone(), table);
    }

    /// Inserts the table described by an entity's metadata.
    pub fn insert_entity<E>(&mut self)
    where
        E: Entity,
    {
        self.insert(TableDef::from_entity::<E>());
    }

    /// Returns one table by identity.
    #[must_use]
    pub fn table(&self, name: &TableName) -> Option<&TableDef> {
        self.tables.get(name)
    }

    /// Iterates tables in deterministic `(schema, name)` order.
    pub fn tables(&self) -> impl Iterator<Item = &TableDef> {
        self.tables.values()
    }

    /// Returns the number of tables.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// Reports whether the schema contains no tables.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub(crate) fn table_mut(&mut self, name: &TableName) -> Option<&mut TableDef> {
        self.tables.get_mut(name)
    }

    pub(crate) fn tables_values_mut(&mut self) -> impl Iterator<Item = &mut TableDef> {
        self.tables.values_mut()
    }

    pub(crate) fn remove(&mut self, name: &TableName) -> Option<TableDef> {
        self.tables.remove(name)
    }
}
