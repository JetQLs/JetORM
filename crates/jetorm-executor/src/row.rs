use jetorm_entity::Value;

/// One fetched row, decoded into positional values.
///
/// This is the executor's row currency: driver rows are converted into
/// `JetRow`s inside each [`crate::Executor`] implementation, so no driver
/// type appears in the execution contract. That seam is what makes
/// alternative executors — a recording mock, another driver — possible
/// without changing the query layer, and it is where multi-entity row
/// segmentation will attach when joins land.
#[derive(Clone, Debug, PartialEq)]
pub struct JetRow {
    values: Vec<Value>,
}

impl JetRow {
    /// Wraps positional values in the order the statement selected them.
    #[must_use]
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    /// Returns the values in selection order.
    #[must_use]
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Consumes the row, yielding its values in selection order.
    #[must_use]
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    /// Returns the number of values in the row.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Reports whether the row holds no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}
