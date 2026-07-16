/// One rendered SQL statement with its positional parameter layout.
///
/// Placeholder numbering is dialect-native (`$1`, `$2`, ... for PostgreSQL)
/// and dense: each distinct frontend bind position receives exactly one
/// placeholder, in order of first appearance in the rendered SQL. A bind
/// value reused by several IR parameters therefore binds once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    sql: String,
    bind_order: Vec<u32>,
}

impl Statement {
    pub(crate) const fn new(sql: String, bind_order: Vec<u32>) -> Self {
        Self { sql, bind_order }
    }

    /// Returns the rendered SQL text.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// Returns the frontend bind position for each placeholder.
    ///
    /// Index `n` holds the query bind-table position whose value must be
    /// bound to placeholder `n + 1`.
    #[must_use]
    pub fn bind_order(&self) -> &[u32] {
        &self.bind_order
    }

    /// Returns the number of placeholders in the statement.
    #[must_use]
    pub fn parameter_count(&self) -> usize {
        self.bind_order.len()
    }

    /// Decomposes the statement into its SQL text and bind order.
    #[must_use]
    pub fn into_parts(self) -> (String, Vec<u32>) {
        (self.sql, self.bind_order)
    }
}
