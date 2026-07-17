//! Partial models: derived projections decoding into named structs.

use jetorm::prelude::*;
use jetorm::{Dialect, IntoAfterBurnerIr, Postgres};

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub age: i32,
}

#[derive(Clone, Debug, PartialEq, JetPartial)]
#[jet(columns = "user")]
pub struct UserSummary {
    pub id: i64,
    pub name: String,
}

/// Field names may differ from the entity's through the `column` override,
/// which names the entity field, and nullable columns keep their `Option`.
#[derive(Clone, Debug, PartialEq, JetPartial)]
#[jet(columns = "user")]
pub struct Contact {
    #[jet(column = "email")]
    pub address: Option<String>,
    pub name: String,
}

#[test]
fn partials_project_their_columns_in_field_order() {
    assert_eq!(<UserSummary as ColumnList<UserEntity>>::indexes(), [0, 1]);
    assert_eq!(<Contact as ColumnList<UserEntity>>::indexes(), [2, 1]);
}

#[test]
fn partials_decode_rows_into_themselves() {
    let row = UserSummary::decode(vec![Value::Int64(7), Value::Text("alice".to_owned())])
        .expect("well-typed row decodes");
    assert_eq!(
        row,
        UserSummary {
            id: 7,
            name: "alice".to_owned(),
        }
    );

    let contact = Contact::decode(vec![
        Value::Null(jetorm::ColumnType::Text),
        Value::Text("bob".to_owned()),
    ])
    .expect("a NULL in the nullable column decodes");
    assert_eq!(
        contact,
        Contact {
            address: None,
            name: "bob".to_owned(),
        }
    );

    let error = UserSummary::decode(vec![Value::Int64(7)]).expect_err("width is checked");
    assert!(matches!(
        error,
        jetorm::DecodeError::ColumnCount {
            expected: 2,
            actual: 1,
        }
    ));
}

#[test]
fn select_as_renders_exactly_the_partial_columns() {
    let query = UserEntity::find()
        .select_as::<UserSummary>()
        .filter(user::Age.ge(18))
        .order_by(user::Name.asc())
        .into_select();
    let module = query.into_afterburner_ir().expect("partial select lowers");
    let statement = Postgres
        .render_query(&module)
        .expect("partial select renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\" AS \"id\", \"t0\".\"name\" AS \"name\" FROM \"users\" AS \"t0\" \
         WHERE (\"t0\".\"age\" >= $1::integer) \
         ORDER BY \"t0\".\"name\" ASC"
    );
}
