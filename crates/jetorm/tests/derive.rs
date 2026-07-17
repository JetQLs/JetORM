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

/// A user-defined identifier stored as `bigint`.
///
/// Implementing [`SqlValue`] is the whole extension mechanism: the derive
/// resolves column types through the trait, so any implementing type is a
/// column type — no registry, no macro attribute required.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountId(pub i64);

impl SqlValue for AccountId {
    const COLUMN_TYPE: ColumnType = ColumnType::Int64;

    fn into_value(self) -> Value {
        Value::Int64(self.0)
    }

    fn from_value(value: Value) -> Result<Self, jetorm::ValueTypeMismatch> {
        i64::from_value(value).map(Self)
    }
}

/// The derive must see through type aliases: resolution is trait dispatch on
/// the aliased type, not a match on the alias's name.
pub type EmailAddress = String;

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "accounts")]
pub struct Account {
    #[jet(primary_key)]
    pub id: AccountId,
    pub contact: Option<EmailAddress>,
    /// Stored as text despite the `String` default already being text; the
    /// override attribute pins the SQL kind explicitly.
    #[jet(column_type = "Json")]
    pub settings: String,
}

#[test]
fn user_types_and_aliases_resolve_through_the_sql_value_trait() {
    assert_eq!(
        AccountEntity::COLUMNS,
        [
            ColumnMeta::new("id", "id", ColumnType::Int64).primary_key(),
            ColumnMeta::new("contact", "contact", ColumnType::Text).nullable(),
            ColumnMeta::new("settings", "settings", ColumnType::Json),
        ]
    );

    let account = Account {
        id: AccountId(7),
        contact: Some("a@example.com".to_owned()),
        settings: "{}".to_owned(),
    };
    let values = account.clone().into_values();
    assert_eq!(values[0], Value::Int64(7));
    assert_eq!(Account::from_values(values), Ok(account));
}
