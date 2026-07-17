//! Live-database schema introspection.
//!
//! `jet db pull` and baseline generation reconstruct a [`SchemaSet`] from a
//! running PostgreSQL database's own catalog. The result speaks the same
//! model the differ and the DDL renderer speak, so a pulled schema diffs,
//! serializes, and generates entities like any other.

use jetorm_entity::{ColumnType, ReferentialAction};
use jetorm_executor::Database;
use jetorm_schema::{ColumnDef, ForeignKeyDef, SchemaSet, TableDef, TableName};
use sqlx::Row;

use crate::error::MigrationError;
use crate::history::HISTORY_TABLE;

/// One column the introspection could not represent.
///
/// Skipping is reported, never silent: a pulled schema that quietly lost a
/// column would generate entities that lie about the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedColumn {
    /// Table owning the column.
    pub table: String,
    /// Column name.
    pub column: String,
    /// The database type JetORM has no mapping for.
    pub data_type: String,
}

/// A schema reconstructed from a live database.
#[derive(Clone, Debug)]
pub struct Introspection {
    /// Every representable table, keyed and ordered like any schema state.
    pub schema: SchemaSet,
    /// Columns skipped for lack of a type mapping, in catalog order.
    pub skipped: Vec<SkippedColumn>,
}

/// Maps a PostgreSQL type name onto JetORM's column type.
fn column_type_of(udt_name: &str) -> Option<ColumnType> {
    Some(match udt_name {
        "bool" => ColumnType::Boolean,
        "int2" => ColumnType::Int16,
        "int4" => ColumnType::Int32,
        "int8" => ColumnType::Int64,
        "float4" => ColumnType::Float32,
        "float8" => ColumnType::Float64,
        "numeric" => ColumnType::Decimal,
        "text" | "varchar" | "bpchar" => ColumnType::Text,
        "bytea" => ColumnType::Bytes,
        "date" => ColumnType::Date,
        "time" => ColumnType::Time,
        "timestamp" => ColumnType::Timestamp,
        "timestamptz" => ColumnType::TimestampUtc,
        "uuid" => ColumnType::Uuid,
        "json" | "jsonb" => ColumnType::Json,
        _ => return None,
    })
}

fn referential_action_of(rule: &str) -> ReferentialAction {
    match rule {
        "CASCADE" => ReferentialAction::Cascade,
        "RESTRICT" => ReferentialAction::Restrict,
        "SET NULL" => ReferentialAction::SetNull,
        "SET DEFAULT" => ReferentialAction::SetDefault,
        _ => ReferentialAction::NoAction,
    }
}

/// Reads one database schema into JetORM's model.
///
/// The migration-history table is excluded — it belongs to the tooling, not
/// the application's schema. Composite foreign keys and columns of unmapped
/// types are skipped and reported through [`Introspection::skipped`] (a
/// composite key's columns stay; only the constraint is dropped, since the
/// model speaks single-column constraints today).
///
/// # Errors
///
/// Returns an error when the catalog cannot be queried.
pub async fn introspect(
    database: &Database,
    schema_name: &str,
) -> Result<Introspection, MigrationError> {
    let mut schema = SchemaSet::new();
    let mut skipped = Vec::new();

    let tables = sqlx::query(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = $1 AND table_type = 'BASE TABLE'
         ORDER BY table_name",
    )
    .bind(schema_name)
    .fetch_all(database.pool())
    .await?;

    for table_row in tables {
        let table_name: String = table_row.get(0);
        if table_name == HISTORY_TABLE {
            continue;
        }
        let mut table = TableDef::new(qualified(schema_name, &table_name));

        let columns = sqlx::query(
            "SELECT column_name, udt_name, is_nullable, identity_generation
             FROM information_schema.columns
             WHERE table_schema = $1 AND table_name = $2
             ORDER BY ordinal_position",
        )
        .bind(schema_name)
        .bind(&table_name)
        .fetch_all(database.pool())
        .await?;
        for column_row in columns {
            let column_name: String = column_row.get(0);
            let udt_name: String = column_row.get(1);
            let Some(column_type) = column_type_of(&udt_name) else {
                skipped.push(SkippedColumn {
                    table: table_name.clone(),
                    column: column_name,
                    data_type: udt_name,
                });
                continue;
            };
            let mut definition = ColumnDef::new(&column_name, column_type);
            let is_nullable: String = column_row.get(2);
            if is_nullable == "YES" {
                definition = definition.nullable();
            }
            let identity: Option<String> = column_row.get(3);
            if identity.is_some() {
                definition = definition.auto_increment();
            }
            table = table.with_column(definition);
        }

        // Single-column unique constraints map onto the column flag;
        // multi-column ones have no model representation yet.
        let uniques = sqlx::query(
            "SELECT c.conname, a.attname
             FROM pg_constraint c
             JOIN pg_class t ON t.oid = c.conrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             JOIN unnest(c.conkey) AS k(attnum) ON true
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
             WHERE n.nspname = $1 AND t.relname = $2 AND c.contype = 'u'
             ORDER BY c.conname",
        )
        .bind(schema_name)
        .bind(&table_name)
        .fetch_all(database.pool())
        .await?;
        let mut unique_columns: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for row in uniques {
            unique_columns
                .entry(row.get(0))
                .or_default()
                .push(row.get(1));
        }
        for columns in unique_columns.values() {
            if let [column] = columns.as_slice()
                && let Some(definition) = table.column(column)
            {
                let definition = definition.clone().unique();
                table = table.with_column(definition);
            }
        }

        let key_columns = sqlx::query(
            "SELECT a.attname
             FROM pg_constraint c
             JOIN pg_class t ON t.oid = c.conrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             JOIN unnest(c.conkey) WITH ORDINALITY AS k(attnum, position) ON true
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
             WHERE n.nspname = $1 AND t.relname = $2 AND c.contype = 'p'
             ORDER BY k.position",
        )
        .bind(schema_name)
        .bind(&table_name)
        .fetch_all(database.pool())
        .await?;
        let primary_key: Vec<String> = key_columns.into_iter().map(|row| row.get(0)).collect();
        table = table.with_primary_key(primary_key);

        schema.insert(table);
    }

    // Foreign keys, single-column only; the referencing and referenced
    // column lists come from the same constraint, aligned by position.
    let foreign_keys = sqlx::query(
        "SELECT c.conname, t.relname, a.attname, ft.relname, fa.attname,
                c.confdeltype::text, c.confupdtype::text, cardinality(c.conkey)
         FROM pg_constraint c
         JOIN pg_class t ON t.oid = c.conrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         JOIN pg_class ft ON ft.oid = c.confrelid
         JOIN unnest(c.conkey) WITH ORDINALITY AS k(attnum, position) ON true
         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
         JOIN unnest(c.confkey) WITH ORDINALITY AS fk(attnum, position)
              ON fk.position = k.position
         JOIN pg_attribute fa ON fa.attrelid = ft.oid AND fa.attnum = fk.attnum
         WHERE n.nspname = $1 AND c.contype = 'f'
         ORDER BY c.conname",
    )
    .bind(schema_name)
    .fetch_all(database.pool())
    .await?;
    for row in foreign_keys {
        let arity: i32 = row.get(7);
        if arity != 1 {
            skipped.push(SkippedColumn {
                table: row.get(1),
                column: row.get(2),
                data_type: format!("composite foreign key {}", row.get::<String, _>(0)),
            });
            continue;
        }
        let owner: String = row.get(1);
        let Some(table) = schema.table(&qualified(schema_name, &owner)).cloned() else {
            continue;
        };
        let delete_code: String = row.get(5);
        let update_code: String = row.get(6);
        let foreign_key = ForeignKeyDef::new(
            row.get::<String, _>(0),
            row.get::<String, _>(2),
            qualified(schema_name, &row.get::<String, _>(3)),
            row.get::<String, _>(4),
        )
        .on_delete(action_code(&delete_code))
        .on_update(action_code(&update_code));
        schema.insert(table.with_foreign_key(foreign_key));
    }

    Ok(Introspection { schema, skipped })
}

/// Maps `pg_constraint`'s single-letter action codes.
fn action_code(code: &str) -> ReferentialAction {
    match code {
        "c" => ReferentialAction::Cascade,
        "r" => ReferentialAction::Restrict,
        "n" => ReferentialAction::SetNull,
        "d" => ReferentialAction::SetDefault,
        _ => referential_action_of(code),
    }
}

fn qualified(schema_name: &str, table: &str) -> TableName {
    if schema_name == "public" {
        TableName::new(table)
    } else {
        TableName::qualified(schema_name, table)
    }
}
