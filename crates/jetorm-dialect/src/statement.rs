/// Shape of the result produced by executing a rendered statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatementResult {
    /// The statement returns rows that must be fetched and decoded.
    Rows,
    /// The statement returns only its affected-row count.
    AffectedRows,
}

/// One rendered SQL statement with its parameter layout and result contract.
///
/// Placeholder numbering is dialect-native (`$1`, `$2`, ... for PostgreSQL)
/// and dense: each distinct frontend bind position receives exactly one
/// placeholder, in order of first appearance in the rendered SQL. A bind
/// value reused by several IR parameters therefore binds once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    sql: String,
    bind_order: Vec<u32>,
    result: StatementResult,
}

impl Statement {
    pub(crate) const fn new(sql: String, bind_order: Vec<u32>, result: StatementResult) -> Self {
        Self {
            sql,
            bind_order,
            result,
        }
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

    /// Returns whether execution yields rows or an affected-row count.
    #[must_use]
    pub const fn result(&self) -> StatementResult {
        self.result
    }

    /// Decomposes the statement into SQL, bind order, and result contract.
    #[must_use]
    pub fn into_parts(self) -> (String, Vec<u32>, StatementResult) {
        (self.sql, self.bind_order, self.result)
    }
}
