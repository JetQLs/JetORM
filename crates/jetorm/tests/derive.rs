use chrono::{DateTime, Utc};
use jetorm::prelude::*;
use jetorm::{ColumnMeta, ColumnType, DecodeError, TableMeta};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users", schema = "public")]
pub struct User {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub name: String,
    #[jet(column = "email_address", unique)]
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "audit_events", module = "audit")]
pub struct AuditEvent {
    #[jet(primary_key)]
    pub id: Uuid,
    pub payload: serde_json::Value,
    pub actor_id: Option<i64>,
}

#[test]
fn derive_emits_table_and_column_metadata() {
    assert_eq!(
        UserEntity::TABLE,
        TableMeta::new("users").with_schema("public")
    );
    assert_eq!(UserEntity::PRIMARY_KEY, [0]);
    assert_eq!(
        UserEntity::COLUMNS,
        [
            ColumnMeta::new("id", "id", ColumnType::Int64)
                .primary_key()
                .auto_increment(),
            ColumnMeta::new("name", "name", ColumnType::Text),
            ColumnMeta::new("email_address", "email", ColumnType::Text)
                .nullable()
                .unique(),
            ColumnMeta::new("created_at", "created_at", ColumnType::TimestampUtc),
        ]
    );
}

#[test]
fn derive_emits_typed_column_markers() {
    const {
        assert!(user::Id::INDEX == 0);
        assert!(!user::Id::NULLABLE);
        assert!(user::Email::INDEX == 2);
        assert!(user::Email::NULLABLE);
    }
    assert_eq!(user::Email::meta().name(), "email_address");
    assert_eq!(audit::ActorId::INDEX, 2);
    assert_eq!(audit::Payload::meta().column_type(), ColumnType::Json);
}

#[test]
fn derived_model_round_trips_positional_values() {
    let created_at = DateTime::<Utc>::from_timestamp(1_752_710_400, 0).expect("valid timestamp");
    let user = User {
        id: 1,
        name: "alice".to_owned(),
        email: None,
        created_at,
    };
    let values = user.clone().into_values();
    assert_eq!(values.len(), UserEntity::COLUMNS.len());
    assert_eq!(User::from_values(values), Ok(user));
}

#[test]
fn derived_model_reports_width_and_kind_mismatches() {
    assert_eq!(
        User::from_values(Vec::new()),
        Err(DecodeError::ColumnCount {
            expected: 4,
            actual: 0,
        })
    );

    let created_at = DateTime::<Utc>::from_timestamp(1_752_710_400, 0).expect("valid timestamp");
    let mut values = User {
        id: 1,
        name: "alice".to_owned(),
        email: None,
        created_at,
    }
    .into_values();
    values[1] = Value::Int64(3);
    let error = User::from_values(values).expect_err("kind mismatch");
    assert!(matches!(error, DecodeError::Column { name: "name", .. }));
}
