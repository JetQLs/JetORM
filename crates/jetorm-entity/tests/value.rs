use chrono::{DateTime, NaiveDate, Utc};
use jetorm_entity::{ColumnMeta, ColumnType, SqlValue, TableMeta, Value};
use uuid::Uuid;

#[test]
fn scalar_values_round_trip() {
    assert_eq!(bool::from_value(true.into_value()), Ok(true));
    assert_eq!(i16::from_value(3i16.into_value()), Ok(3));
    assert_eq!(i32::from_value(4i32.into_value()), Ok(4));
    assert_eq!(i64::from_value(5i64.into_value()), Ok(5));
    assert_eq!(f64::from_value(1.5f64.into_value()), Ok(1.5));
    assert_eq!(
        String::from_value("alice".to_owned().into_value()),
        Ok("alice".to_owned())
    );
    assert_eq!(
        Vec::<u8>::from_value(vec![1u8, 2].into_value()),
        Ok(vec![1u8, 2])
    );

    let uuid = Uuid::from_u128(7);
    assert_eq!(Uuid::from_value(uuid.into_value()), Ok(uuid));

    let date = NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid date");
    assert_eq!(NaiveDate::from_value(date.into_value()), Ok(date));

    let timestamp = DateTime::<Utc>::from_timestamp(1_752_710_400, 0).expect("valid timestamp");
    assert_eq!(
        DateTime::<Utc>::from_value(timestamp.into_value()),
        Ok(timestamp)
    );

    let json = serde_json::json!({ "ok": true });
    assert_eq!(
        serde_json::Value::from_value(json.clone().into_value()),
        Ok(json)
    );
}

#[test]
fn option_maps_none_onto_typed_null() {
    let value = Option::<String>::None.into_value();
    assert_eq!(value, Value::Null(ColumnType::Text));
    assert!(value.is_null());
    assert_eq!(value.column_type(), ColumnType::Text);

    assert_eq!(Option::<String>::from_value(value), Ok(None));
    assert_eq!(Option::<i64>::from_value(9i64.into_value()), Ok(Some(9i64)));
    const {
        assert!(<Option<i64> as SqlValue>::NULLABLE);
        assert!(!<i64 as SqlValue>::NULLABLE);
    }
}

#[test]
fn mismatched_payload_kind_is_reported() {
    let error = i64::from_value(Value::Text("7".to_owned())).expect_err("kind mismatch");
    assert_eq!(error.expected(), ColumnType::Int64);
    assert_eq!(error.actual(), "text");
}

#[test]
fn metadata_builders_compose_in_const_context() {
    const TABLE: TableMeta = TableMeta::new("users").with_schema("public");
    const COLUMN: ColumnMeta = ColumnMeta::new("email", "email", ColumnType::Text)
        .nullable()
        .unique();

    assert_eq!(TABLE.name(), "users");
    assert_eq!(TABLE.schema(), Some("public"));
    assert_eq!(COLUMN.name(), "email");
    assert_eq!(COLUMN.rust_name(), "email");
    assert_eq!(COLUMN.column_type(), ColumnType::Text);
    assert!(COLUMN.is_nullable());
    assert!(COLUMN.is_unique());
    assert!(!COLUMN.is_primary_key());
    assert!(!COLUMN.is_auto_increment());
}
