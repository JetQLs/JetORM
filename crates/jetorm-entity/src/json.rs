use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::meta::ColumnType;
use crate::value::{SqlValue, Value};

/// A typed JSON column: any (de)serializable Rust type stored as `jsonb`.
///
/// The wrapper is the column type — `Json<Preferences>` as a field stores
/// the struct as a JSON document and decodes it back on fetch, so the
/// database sees `jsonb` while the model sees the real type. A raw
/// [`serde_json::Value`] field remains the untyped alternative.
///
/// # Panics
///
/// Converting into a bind value panics when the payload cannot be
/// serialized as JSON — a map with non-string keys, or a failing custom
/// `Serialize` impl. For plain data types serialization is infallible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Json<T>(pub T);

impl<T> Json<T> {
    /// Returns the wrapped payload.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Json<T> {
    fn from(payload: T) -> Self {
        Self(payload)
    }
}

impl<T> SqlValue for Json<T>
where
    T: Serialize + DeserializeOwned,
{
    const COLUMN_TYPE: ColumnType = ColumnType::Json;

    fn into_value(self) -> Value {
        Value::Json(
            serde_json::to_value(&self.0)
                .expect("the payload serializes as JSON; see the Json panic contract"),
        )
    }

    fn from_value(value: Value) -> Result<Self, crate::error::ValueTypeMismatch> {
        let document = serde_json::Value::from_value(value)?;
        serde_json::from_value(document).map(Self).map_err(|_| {
            crate::error::ValueTypeMismatch::new(
                ColumnType::Json,
                "json document not matching the target type",
            )
        })
    }
}
