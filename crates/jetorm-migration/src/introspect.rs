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
    /// Facts that were captured but changed shape on the way — normalized
    /// constraint names, serial columns pulled as identity, length
    /// modifiers mapped away. Present so nothing is lost silently.
    pub notes: Vec<String>,
}

/// Maps a PostgreSQL type name onto JetORM's column type.
fn column_type_of(udt_name: &str) -> Option<ColumnType> {
    use jetorm_entity::ElementType;
    // The catalog spells array types with a leading underscore.
    if let Some(base) = udt_name.strip_prefix('_') {
        let element = match base {
            "bool" => ElementType::Boolean,
            "int2" => ElementType::Int16,
            "int4" => ElementType::Int32,
            "int8" => ElementType::Int64,
            "float4" => ElementType::Float32,
            "float8" => ElementType::Float64,
            "numeric" => ElementType::Decimal,
            "text" | "varchar" | "bpchar" => ElementType::Text,
            "date" => ElementType::Date,
            "time" => ElementType::Time,
            "timestamp" => ElementType::Timestamp,
            "timestamptz" => ElementType::TimestampUtc,
            "uuid" => ElementType::Uuid,
            _ => return None,
        };
        return Some(ColumnType::ArrayOf(element));
    }
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
    let mut notes = Vec::new();

    // Native enum types first: the columns below resolve their names
    // against this set, and a pulled schema must carry the definitions
    // for its diff to create the types before any column takes them.
    let enum_rows = sqlx::query(
        "SELECT t.typname, e.enumlabel
         FROM pg_type t
         JOIN pg_namespace n ON n.oid = t.typnamespace
         JOIN pg_enum e ON e.enumtypid = t.oid
         WHERE n.nspname = $1 AND t.typtype = 'e'
         ORDER BY t.typname, e.enumsortorder",
    )
    .bind(schema_name)
    .fetch_all(database.pool())
    .await?;
    let mut enum_variants: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for row in enum_rows {
        enum_variants
            .entry(row.get(0))
            .or_default()
            .push(row.get(1));
    }
    // Only enums a pulled column actually references enter the schema:
    // entity-derived schemas carry exactly the referenced enums, so an
    // unused type recorded here would diff as perpetual DropEnum churn.
    // Unused ones are noted below instead.
    let mut used_enums: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

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
            "SELECT column_name, udt_name, is_nullable, identity_generation,
                    column_default, character_maximum_length, udt_schema,
                    domain_name
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
            let udt_schema: String = column_row.get(6);
            // The catalog resolves a domain column to its base type;
            // the domain's identity, CHECKs, and default vanish in the
            // flattening and must be said out loud.
            let domain_name: Option<String> = column_row.get(7);
            if let Some(domain) = &domain_name {
                notes.push(format!(
                    "{table_name}.{column_name}: domain {domain:?} pulled as \
                     its base type; domain constraints and defaults are not \
                     represented"
                ));
            }
            // Names alone don't identify a type: the same udt_name can
            // name an enum here, a built-in in pg_catalog, and something
            // else in a third namespace. Enums must live in the pulled
            // schema, built-ins in pg_catalog; anything else is skipped
            // with its full name, never guessed.
            let enum_name = (udt_schema == schema_name && enum_variants.contains_key(&udt_name))
                .then(|| qualified_type(schema_name, &udt_name));
            let builtin = (udt_schema == "pg_catalog")
                .then(|| column_type_of(&udt_name))
                .flatten();
            let column_type = match (&enum_name, builtin) {
                (Some(_), _) => ColumnType::Text,
                (None, Some(column_type)) => column_type,
                (None, None) => {
                    skipped.push(SkippedColumn {
                        table: table_name.clone(),
                        column: column_name,
                        data_type: format!("{udt_schema}.{udt_name}"),
                    });
                    continue;
                }
            };
            let mut definition = ColumnDef::new(&column_name, column_type);
            if let Some(enum_name) = enum_name {
                used_enums.insert(udt_name.clone());
                definition = definition.with_type_name(enum_name);
            }
            let is_nullable: String = column_row.get(2);
            if is_nullable == "YES" {
                definition = definition.nullable();
            }
            let identity: Option<String> = column_row.get(3);
            let default: Option<String> = column_row.get(4);
            if identity.is_some() {
                definition = definition.auto_increment();
            } else if let Some(default) = &default {
                // A serial column is a nextval() default in the catalog;
                // it is database-generated all the same. Any other default
                // has no model representation and must be said out loud.
                if default.starts_with("nextval(") {
                    definition = definition.auto_increment();
                    notes.push(format!(
                        "{table_name}.{column_name}: serial pulled as an \
                         identity column"
                    ));
                } else {
                    notes.push(format!(
                        "{table_name}.{column_name}: column default \
                         {default:?} is not representable and was dropped"
                    ));
                }
            }
            let length: Option<i32> = column_row.get(5);
            if let Some(length) = length
                && matches!(udt_name.as_str(), "varchar" | "bpchar")
            {
                notes.push(format!(
                    "{table_name}.{column_name}: {udt_name}({length}) mapped \
                     to text; the length limit is not represented"
                ));
            }
            // Arrays report no character_maximum_length, so the length
            // check above can never fire for them; the loss is the same
            // and gets the same note.
            if matches!(udt_name.as_str(), "_varchar" | "_bpchar") {
                notes.push(format!(
                    "{table_name}.{column_name}: {} array mapped to text[]; \
                     element length limits are not represented",
                    udt_name.trim_start_matches('_')
                ));
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
        for (constraint, columns) in &unique_columns {
            if let [column] = columns.as_slice() {
                if let Some(definition) = table.column(column) {
                    let definition = definition.clone().unique();
                    table = table.with_column(definition);
                    let default_name = format!("{table_name}_{column}_key");
                    if *constraint != default_name {
                        notes.push(format!(
                            "{table_name}: unique constraint {constraint:?} \
                             recorded under the default name {default_name:?}; \
                             later drops will use the default name"
                        ));
                    }
                }
            } else {
                // Multi-column uniqueness has no model representation yet;
                // absence must be reported, never silent.
                skipped.push(SkippedColumn {
                    table: table_name.clone(),
                    column: columns.join(", "),
                    data_type: format!("composite unique constraint {constraint}"),
                });
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

    for (name, variants) in &enum_variants {
        if used_enums.contains(name) {
            schema.insert_enum(qualified_type(schema_name, name), variants.clone());
        } else {
            notes.push(format!(
                "enum type {name:?} is not used by any pulled column and was \
                 not recorded; it would otherwise diff as a perpetual drop"
            ));
        }
    }

    // Foreign keys, single-column only; the referencing and referenced
    // column lists come from the same constraint, aligned by position.
    let foreign_keys = sqlx::query(
        "SELECT c.conname, t.relname, a.attname, ft.relname, fa.attname,
                c.confdeltype::text, c.confupdtype::text, cardinality(c.conkey),
                fn.nspname
         FROM pg_constraint c
         JOIN pg_class t ON t.oid = c.conrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         JOIN pg_class ft ON ft.oid = c.confrelid
         JOIN pg_namespace fn ON fn.oid = ft.relnamespace
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
        let live_name: String = row.get(0);
        let column: String = row.get(2);
        // Entity metadata always regenerates the default constraint name,
        // and the differ matches constraints by name, so a live name kept
        // verbatim would show as perpetual churn on every generate. The
        // pull normalizes the name and says so; the live database keeps
        // its own name until the operator renames it.
        let default_name = format!("{owner}_{column}_fkey");
        if live_name != default_name {
            notes.push(format!(
                "{owner}: foreign key {live_name:?} recorded under the \
                 default name {default_name:?}; rename the live constraint \
                 to match, or later drops will miss it"
            ));
        }
        let target_schema: String = row.get(8);
        let delete_code: String = row.get(5);
        let update_code: String = row.get(6);
        let foreign_key = ForeignKeyDef::new(
            default_name,
            column,
            qualified(&target_schema, &row.get::<String, _>(3)),
            row.get::<String, _>(4),
        )
        .on_delete(action_code(&delete_code))
        .on_update(action_code(&update_code));
        schema.insert(table.with_foreign_key(foreign_key));
    }

    Ok(Introspection {
        schema,
        skipped,
        notes,
    })
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

/// Spells a type name the way the schema records it: bare in `public`,
/// schema-qualified elsewhere — mirroring table naming, so a baseline
/// replayed on a fresh database creates the type in its own schema.
fn qualified_type(schema_name: &str, type_name: &str) -> String {
    if schema_name == "public" {
        type_name.to_owned()
    } else {
        format!("{schema_name}.{type_name}")
    }
}

fn qualified(schema_name: &str, table: &str) -> TableName {
    if schema_name == "public" {
        TableName::new(table)
    } else {
        TableName::qualified(schema_name, table)
    }
}
