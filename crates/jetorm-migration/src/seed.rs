//! Declarative TOML seed data applied idempotently through upserts.
//!
//! A seed file names a table, the key columns that identify each row, and
//! the rows themselves. Applying seeds inserts absent rows and updates the
//! non-key columns of present ones, so running `jet seed` twice — or after
//! editing a value — always converges on the file's contents. The same
//! replay-invariant stance migrations take: the file is the truth, the
//! command makes the database agree with it.

use jetorm_entity::{ColumnType, ElementType};

use jetorm_executor::Database;
use jetorm_schema::{ColumnDef, SchemaSet, TableDef, TableName};
use serde::Deserialize;

use crate::error::MigrationError;

/// One parsed seed file: any number of table sections.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedFile {
    /// Table sections in file order.
    #[serde(default)]
    pub rows: Vec<SeedTable>,
}

/// Rows destined for one table, keyed by the named columns.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedTable {
    /// Table name, optionally schema-qualified as `schema.table`.
    pub table: String,
    /// Columns identifying a row — the upsert's conflict arbiter. They
    /// must be covered by a unique index or constraint on the live table.
    pub key: Vec<String>,
    /// The rows themselves; each maps column names onto TOML values.
    #[serde(default)]
    pub values: Vec<toml::Table>,
}

impl SeedFile {
    /// Parses one seed file.
    ///
    /// # Errors
    ///
    /// Returns an error when the contents are not a valid seed document.
    pub fn parse(path: &str, contents: &str) -> Result<Self, MigrationError> {
        toml::from_str(contents).map_err(|error| MigrationError::File {
            path: path.to_owned(),
            detail: error.to_string(),
        })
    }
}

/// Outcome of applying seed data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// `(table, rows)` pairs in application order.
    pub applied: Vec<(String, usize)>,
}

/// A typed bind value converted from TOML against the column's type.
#[derive(Clone, Debug)]
enum SeedBind {
    Boolean(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float64(f64),
    Decimal(rust_decimal::Decimal),
    Text(String),
    Date(chrono::NaiveDate),
    Time(chrono::NaiveTime),
    Timestamp(chrono::NaiveDateTime),
    TimestampUtc(chrono::DateTime<chrono::Utc>),
    Uuid(uuid::Uuid),
    Json(serde_json::Value),
    Array(ElementType, Vec<SeedBind>),
}

/// One executable statement with its binds.
#[derive(Clone, Debug)]
struct SeedStatement {
    sql: String,
    binds: Vec<SeedBind>,
}

/// Applies every seed file inside one transaction.
///
/// Each row upserts on its table's declared key: absent rows insert,
/// present rows update their non-key columns from the file. A failure
/// anywhere rolls the whole application back — a half-seeded database is
/// no state to leave behind.
///
/// # Errors
///
/// Returns an error when a seed row does not fit the schema, or when the
/// database rejects a statement.
pub async fn apply_seeds(
    database: &Database,
    schema: &SchemaSet,
    files: &[(String, SeedFile)],
) -> Result<SeedReport, MigrationError> {
    // Validate and build everything before touching the database.
    let mut planned: Vec<(String, Vec<SeedStatement>)> = Vec::new();
    for (path, file) in files {
        for section in &file.rows {
            let statements = section_statements(path, schema, section)?;
            planned.push((section.table.clone(), statements));
        }
    }

    let mut transaction = database.pool().begin().await?;
    let mut report = SeedReport::default();
    for (table, statements) in planned {
        let count = statements.len();
        for statement in statements {
            let mut query = sqlx::query(&statement.sql);
            for bind in statement.binds {
                query = bind_seed(query, bind);
            }
            query.execute(&mut *transaction).await?;
        }
        report.applied.push((table, count));
    }
    transaction.commit().await?;
    Ok(report)
}

fn section_statements(
    path: &str,
    schema: &SchemaSet,
    section: &SeedTable,
) -> Result<Vec<SeedStatement>, MigrationError> {
    let table_name = parse_table_name(&section.table);
    let table = schema.table(&table_name).ok_or_else(|| {
        seed_error(
            path,
            format!("table {:?} is not in the schema", section.table),
        )
    })?;
    if section.key.is_empty() {
        return Err(seed_error(
            path,
            format!("table {:?} declares no key columns", section.table),
        ));
    }
    for key in &section.key {
        if table.column(key).is_none() {
            return Err(seed_error(
                path,
                format!("key column {key:?} is not in table {:?}", section.table),
            ));
        }
    }

    let mut statements = Vec::with_capacity(section.values.len());
    for (index, row) in section.values.iter().enumerate() {
        statements.push(row_statement(path, table, section, index, row)?);
    }
    Ok(statements)
}

fn row_statement(
    path: &str,
    table: &TableDef,
    section: &SeedTable,
    index: usize,
    row: &toml::Table,
) -> Result<SeedStatement, MigrationError> {
    let context = |detail: String| {
        seed_error(
            path,
            format!("table {:?}, row {index}: {detail}", section.table),
        )
    };
    for key in &section.key {
        if !row.contains_key(key) {
            return Err(context(format!("key column {key:?} is missing")));
        }
    }

    let mut columns = Vec::with_capacity(row.len());
    let mut binds = Vec::with_capacity(row.len());
    for (name, value) in row {
        let column = table
            .column(name)
            .ok_or_else(|| context(format!("column {name:?} is not in the table")))?;
        columns.push(column);
        binds.push(
            convert(value, column.column_type())
                .map_err(|detail| context(format!("column {name:?}: {detail}")))?,
        );
    }

    let column_sql = columns
        .iter()
        .map(|column| quote_identifier(column.name()))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = columns
        .iter()
        .enumerate()
        .map(|(position, column)| placeholder(position + 1, column))
        .collect::<Vec<_>>()
        .join(", ");
    let key_sql = section
        .key
        .iter()
        .map(|key| quote_identifier(key))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!(
        "INSERT INTO {} ({column_sql}) VALUES ({placeholders}) ON CONFLICT ({key_sql})",
        table_sql(table.name()),
    );
    let assignments = columns
        .iter()
        .filter(|column| !section.key.iter().any(|key| key == column.name()))
        .map(|column| {
            format!(
                "{} = EXCLUDED.{}",
                quote_identifier(column.name()),
                quote_identifier(column.name())
            )
        })
        .collect::<Vec<_>>();
    if assignments.is_empty() {
        sql.push_str(" DO NOTHING");
    } else {
        sql.push_str(" DO UPDATE SET ");
        sql.push_str(&assignments.join(", "));
    }
    Ok(SeedStatement { sql, binds })
}

/// Spells one bind position, casting named types so a text parameter can
/// cross into an enum-typed column.
fn placeholder(position: usize, column: &ColumnDef) -> String {
    match column.type_name() {
        Some(name) => format!("${position}::{}", type_sql(name)),
        None => format!("${position}"),
    }
}

/// Converts one TOML value against the column's declared type.
fn convert(value: &toml::Value, column_type: ColumnType) -> Result<SeedBind, String> {
    let mismatch = |value: &toml::Value| {
        Err(format!(
            "{} does not fit column type {column_type:?}",
            value.type_str()
        ))
    };
    Ok(match column_type {
        ColumnType::Boolean => match value {
            toml::Value::Boolean(value) => SeedBind::Boolean(*value),
            other => return mismatch(other),
        },
        ColumnType::Int16 => match value {
            toml::Value::Integer(value) => SeedBind::Int16(
                i16::try_from(*value).map_err(|_| format!("{value} exceeds Int16"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::Int32 => match value {
            toml::Value::Integer(value) => SeedBind::Int32(
                i32::try_from(*value).map_err(|_| format!("{value} exceeds Int32"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::Int64 => match value {
            toml::Value::Integer(value) => SeedBind::Int64(*value),
            other => return mismatch(other),
        },
        ColumnType::Float32 => match value {
            toml::Value::Float(value) => {
                let narrowed = *value as f32;
                SeedBind::Float32(narrowed)
            }
            toml::Value::Integer(value) => SeedBind::Float32(*value as f32),
            other => return mismatch(other),
        },
        ColumnType::Float64 => match value {
            toml::Value::Float(value) => SeedBind::Float64(*value),
            toml::Value::Integer(value) => SeedBind::Float64(*value as f64),
            other => return mismatch(other),
        },
        // Floats are excluded on purpose: a binary float is already an
        // approximation, and an exact decimal seeded from one would
        // launder the error. Spell decimals as strings or integers.
        ColumnType::Decimal => match value {
            toml::Value::String(value) => SeedBind::Decimal(
                value
                    .parse()
                    .map_err(|error| format!("{value:?} is not a decimal: {error}"))?,
            ),
            toml::Value::Integer(value) => SeedBind::Decimal(rust_decimal::Decimal::from(*value)),
            other => return mismatch(other),
        },
        ColumnType::Text => match value {
            toml::Value::String(value) => SeedBind::Text(value.clone()),
            other => return mismatch(other),
        },
        ColumnType::Bytes => return Err("byte columns cannot be seeded from TOML".to_owned()),
        ColumnType::Date => match value {
            toml::Value::String(value) => SeedBind::Date(
                value
                    .parse()
                    .map_err(|error| format!("{value:?} is not a date: {error}"))?,
            ),
            toml::Value::Datetime(value) => SeedBind::Date(
                value
                    .to_string()
                    .parse()
                    .map_err(|error| format!("{value} is not a date: {error}"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::Time => match value {
            toml::Value::String(value) => SeedBind::Time(
                value
                    .parse()
                    .map_err(|error| format!("{value:?} is not a time: {error}"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::Timestamp => match value {
            toml::Value::String(value) => SeedBind::Timestamp(
                value
                    .parse()
                    .map_err(|error| format!("{value:?} is not a timestamp: {error}"))?,
            ),
            toml::Value::Datetime(value) => SeedBind::Timestamp(
                value
                    .to_string()
                    .parse()
                    .map_err(|error| format!("{value} is not a timestamp: {error}"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::TimestampUtc => match value {
            toml::Value::String(value) => SeedBind::TimestampUtc(
                value
                    .parse()
                    .map_err(|error| format!("{value:?} is not a UTC timestamp: {error}"))?,
            ),
            toml::Value::Datetime(value) => SeedBind::TimestampUtc(
                value
                    .to_string()
                    .parse()
                    .map_err(|error| format!("{value} is not a UTC timestamp: {error}"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::Uuid => match value {
            toml::Value::String(value) => SeedBind::Uuid(
                value
                    .parse()
                    .map_err(|error| format!("{value:?} is not a UUID: {error}"))?,
            ),
            other => return mismatch(other),
        },
        ColumnType::Json => SeedBind::Json(
            serde_json::to_value(value).map_err(|error| format!("not JSON: {error}"))?,
        ),
        ColumnType::ArrayOf(element) => match value {
            toml::Value::Array(values) => SeedBind::Array(
                element,
                values
                    .iter()
                    .map(|value| convert(value, element.as_column_type()))
                    .collect::<Result<_, _>>()?,
            ),
            other => return mismatch(other),
        },
    })
}

type PgQuery<'query> = sqlx::query::Query<'query, sqlx::Postgres, sqlx::postgres::PgArguments>;

fn bind_seed(query: PgQuery<'_>, bind: SeedBind) -> PgQuery<'_> {
    match bind {
        SeedBind::Boolean(value) => query.bind(value),
        SeedBind::Int16(value) => query.bind(value),
        SeedBind::Int32(value) => query.bind(value),
        SeedBind::Int64(value) => query.bind(value),
        SeedBind::Float32(value) => query.bind(value),
        SeedBind::Float64(value) => query.bind(value),
        SeedBind::Decimal(value) => query.bind(value),
        SeedBind::Text(value) => query.bind(value),
        SeedBind::Date(value) => query.bind(value),
        SeedBind::Time(value) => query.bind(value),
        SeedBind::Timestamp(value) => query.bind(value),
        SeedBind::TimestampUtc(value) => query.bind(value),
        SeedBind::Uuid(value) => query.bind(value),
        SeedBind::Json(value) => query.bind(value),
        SeedBind::Array(element, values) => {
            /// Unwraps one element kind back into its vector.
            macro_rules! typed {
                ($variant:ident) => {
                    query.bind(
                        values
                            .into_iter()
                            .filter_map(|bind| match bind {
                                SeedBind::$variant(value) => Some(value),
                                _ => None,
                            })
                            .collect::<Vec<_>>(),
                    )
                };
            }
            match element {
                ElementType::Boolean => typed!(Boolean),
                ElementType::Int16 => typed!(Int16),
                ElementType::Int32 => typed!(Int32),
                ElementType::Int64 => typed!(Int64),
                ElementType::Float32 => typed!(Float32),
                ElementType::Float64 => typed!(Float64),
                ElementType::Decimal => typed!(Decimal),
                ElementType::Text => typed!(Text),
                ElementType::Date => typed!(Date),
                ElementType::Time => typed!(Time),
                ElementType::Timestamp => typed!(Timestamp),
                ElementType::TimestampUtc => typed!(TimestampUtc),
                ElementType::Uuid => typed!(Uuid),
            }
        }
    }
}

fn seed_error(path: &str, detail: String) -> MigrationError {
    MigrationError::File {
        path: path.to_owned(),
        detail,
    }
}

fn parse_table_name(name: &str) -> TableName {
    match name.split_once('.') {
        Some((schema, table)) => TableName::qualified(schema, table),
        None => TableName::new(name),
    }
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn table_sql(name: &TableName) -> String {
    match name.schema() {
        Some(schema) => format!(
            "{}.{}",
            quote_identifier(schema),
            quote_identifier(name.name())
        ),
        None => quote_identifier(name.name()),
    }
}

/// Spells an optionally qualified type name for a cast.
fn type_sql(name: &str) -> String {
    name.split('.')
        .map(quote_identifier)
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jetorm_schema::ColumnDef;

    fn schema() -> SchemaSet {
        let mut schema = SchemaSet::new();
        schema.insert_enum("role_kind", ["human", "robot"].map(str::to_owned));
        schema.insert(
            TableDef::new(TableName::new("roles"))
                .with_column(ColumnDef::new("name", ColumnType::Text))
                .with_column(ColumnDef::new("rank", ColumnType::Int32))
                .with_column(ColumnDef::new("kind", ColumnType::Text).with_type_name("role_kind"))
                .with_column(
                    ColumnDef::new("tags", ColumnType::ArrayOf(ElementType::Text)).nullable(),
                )
                .with_primary_key(["name".to_owned()]),
        );
        schema
    }

    fn parse(contents: &str) -> SeedFile {
        SeedFile::parse("seeds/test.toml", contents).expect("the seed parses")
    }

    #[test]
    fn a_row_becomes_one_keyed_upsert() {
        let file = parse(
            r#"
            [[rows]]
            table = "roles"
            key = ["name"]

            [[rows.values]]
            name = "admin"
            rank = 1
            "#,
        );
        let statements =
            section_statements("seeds/test.toml", &schema(), &file.rows[0]).expect("plans");
        assert_eq!(statements.len(), 1);
        assert_eq!(
            statements[0].sql,
            "INSERT INTO \"roles\" (\"name\", \"rank\") VALUES ($1, $2) \
             ON CONFLICT (\"name\") DO UPDATE SET \"rank\" = EXCLUDED.\"rank\""
        );
    }

    #[test]
    fn key_only_rows_insert_or_do_nothing() {
        let file = parse(
            r#"
            [[rows]]
            table = "roles"
            key = ["name"]

            [[rows.values]]
            name = "admin"
            "#,
        );
        let statements =
            section_statements("seeds/test.toml", &schema(), &file.rows[0]).expect("plans");
        assert!(
            statements[0]
                .sql
                .ends_with("ON CONFLICT (\"name\") DO NOTHING")
        );
    }

    #[test]
    fn enum_columns_cast_their_text_binds() {
        let file = parse(
            r#"
            [[rows]]
            table = "roles"
            key = ["name"]

            [[rows.values]]
            name = "admin"
            kind = "human"
            "#,
        );
        let statements =
            section_statements("seeds/test.toml", &schema(), &file.rows[0]).expect("plans");
        assert!(
            statements[0].sql.contains("$1::\"role_kind\""),
            "{}",
            statements[0].sql
        );
    }

    #[test]
    fn rows_that_do_not_fit_the_schema_are_refused() {
        let missing_table = parse(
            r#"
            [[rows]]
            table = "nowhere"
            key = ["name"]
            "#,
        );
        assert!(section_statements("s", &schema(), &missing_table.rows[0]).is_err());

        let missing_key = parse(
            r#"
            [[rows]]
            table = "roles"
            key = ["name"]

            [[rows.values]]
            rank = 3
            "#,
        );
        let error = section_statements("s", &schema(), &missing_key.rows[0])
            .expect_err("a row without its key is refused");
        assert!(error.to_string().contains("key column"), "{error}");

        let wrong_type = parse(
            r#"
            [[rows]]
            table = "roles"
            key = ["name"]

            [[rows.values]]
            name = "admin"
            rank = "high"
            "#,
        );
        let error = section_statements("s", &schema(), &wrong_type.rows[0])
            .expect_err("a mistyped value is refused");
        assert!(error.to_string().contains("does not fit"), "{error}");
    }

    #[test]
    fn decimal_floats_are_refused_as_laundered_precision() {
        let mut schema = SchemaSet::new();
        schema.insert(
            TableDef::new(TableName::new("prices"))
                .with_column(ColumnDef::new("sku", ColumnType::Text))
                .with_column(ColumnDef::new("amount", ColumnType::Decimal))
                .with_primary_key(["sku".to_owned()]),
        );
        let file = parse(
            r#"
            [[rows]]
            table = "prices"
            key = ["sku"]

            [[rows.values]]
            sku = "a"
            amount = 12.5
            "#,
        );
        assert!(section_statements("s", &schema, &file.rows[0]).is_err());
    }
}
