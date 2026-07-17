//! Extended column types: string-backed enums, exact decimals, typed JSON.

use jetorm::prelude::*;
use jetorm::{ColumnType, Dialect, IntoAfterBurnerIr, Postgres};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, JetEnum)]
pub enum Status {
    Draft,
    InReview,
    #[jet(rename = "live")]
    Published,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    pub theme: String,
    pub page_size: u32,
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "articles")]
pub struct Article {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub status: Status,
    pub price: Decimal,
    pub preferences: Json<Preferences>,
    pub discount: Option<Decimal>,
}

#[test]
fn enums_store_stable_names_and_reject_unknown_text() {
    assert_eq!(Status::COLUMN_TYPE, ColumnType::Text);
    assert_eq!(
        Status::Draft.into_value(),
        Value::Text("draft".to_owned()),
        "variant names snake_case by default"
    );
    assert_eq!(
        Status::InReview.into_value(),
        Value::Text("in_review".to_owned())
    );
    assert_eq!(
        Status::Published.into_value(),
        Value::Text("live".to_owned()),
        "renames override the stored name"
    );

    assert_eq!(
        Status::from_value(Value::Text("in_review".to_owned())),
        Ok(Status::InReview)
    );
    assert!(
        Status::from_value(Value::Text("retracted".to_owned())).is_err(),
        "unknown stored text is a decode error, not a silent default"
    );
}

#[test]
fn typed_json_round_trips_through_the_value_currency() {
    let preferences = Json(Preferences {
        theme: "dark".to_owned(),
        page_size: 50,
    });
    let value = preferences.clone().into_value();
    assert_eq!(value.column_type(), ColumnType::Json);
    assert_eq!(Json::<Preferences>::from_value(value), Ok(preferences));

    // A document that does not match the target type is a decode error.
    let wrong = Value::Json(serde_json::json!({ "unexpected": true }));
    assert!(Json::<Preferences>::from_value(wrong).is_err());
}

#[test]
fn decimals_are_exact_and_metadata_flows_through_the_derive() {
    let value = Decimal::new(1999, 2).into_value(); // 19.99 exactly
    assert_eq!(value.column_type(), ColumnType::Decimal);
    assert_eq!(Decimal::from_value(value), Ok(Decimal::new(1999, 2)));

    // The derive read every column type off the field types alone.
    let types: Vec<ColumnType> = ArticleEntity::COLUMNS
        .iter()
        .map(|column| column.column_type())
        .collect();
    assert_eq!(
        types,
        [
            ColumnType::Int64,
            ColumnType::Text,
            ColumnType::Decimal,
            ColumnType::Json,
            ColumnType::Decimal,
        ]
    );
    assert!(ArticleEntity::COLUMNS[4].is_nullable());
}

#[test]
fn extended_types_bind_and_render_like_any_column() {
    let query = ArticleEntity::find()
        .filter(article::Status.eq(Status::Published))
        .filter(article::Price.le(Decimal::new(5000, 2)));
    assert_eq!(
        query.binds(),
        [
            Value::Text("live".to_owned()),
            Value::Decimal(Decimal::new(5000, 2)),
        ],
        "an enum operand binds its stored name, a decimal binds exactly"
    );

    let module = query.into_afterburner_ir().expect("query lowers");
    let statement = Postgres.render_query(&module).expect("query renders");
    assert!(
        statement.sql().contains("$2::numeric"),
        "the decimal bind casts to unconstrained numeric: {}",
        statement.sql()
    );
}
