use std::future::Future;
use std::sync::Arc;

use jetorm_dialect::Statement;
use jetorm_entity::{ColumnMeta, Value};
use sqlx::postgres::{PgConnection, PgPool};

use crate::error::ExecuteError;
use crate::plan::PlanCache;
use crate::row::JetRow;
use crate::value::{build_query, decode_row};

mod sealed {
    /// Prevents downstream `Executor` implementations, keeping the driver
    /// boundary free to evolve.
    pub trait Sealed {}

    impl Sealed for &super::Database {}
    impl Sealed for &mut super::Transaction<'_> {}
}

/// Connection-like values a query can execute against.
///
/// Implemented for `&Database` (pooled execution) and `&mut Transaction`
/// (execution inside one open transaction), following the reference-based
/// executor pattern established by `sqlx`. The trait is sealed; its methods
/// are plumbing for [`crate::SelectExecute`] rather than a public API.
pub trait Executor: sealed::Sealed + Send + Sized {
    /// Returns the plan cache shared through this executor.
    #[doc(hidden)]
    fn plan_cache(&self) -> &PlanCache;

    /// Fetches every row produced by one bound statement, decoded into
    /// positional values for the given column layout.
    ///
    /// No driver type crosses this boundary: implementations convert their
    /// native rows into [`JetRow`]s.
    #[doc(hidden)]
    fn fetch_rows(
        self,
        statement: Arc<Statement>,
        binds: Vec<Value>,
        columns: &'static [ColumnMeta],
    ) -> impl Future<Output = Result<Vec<JetRow>, ExecuteError>> + Send;
}

/// A PostgreSQL connection pool with its shared plan cache.
///
/// Cloning is cheap and shares both the pool and the cache.
#[derive(Clone, Debug)]
pub struct Database {
    pool: PgPool,
    plans: Arc<PlanCache>,
}

/// Tuning knobs for a [`Database`].
///
/// Every default JetORM picks is overridable here, so operators can tune the
/// runtime without forking constants. Connection-level options — pool sizing,
/// timeouts, search path — join this type as they are implemented; driver
/// details beyond it remain reachable through [`Database::from_pool`].
#[derive(Clone, Debug)]
#[must_use = "options do nothing until passed to a Database constructor"]
pub struct DatabaseOptions {
    plan_cache_capacity: u64,
}

impl DatabaseOptions {
    /// Number of cached statements retained by default.
    ///
    /// Statements are a few hundred bytes each, so the default costs
    /// megabytes at most while bounding dynamically generated query shapes.
    pub const DEFAULT_PLAN_CACHE_CAPACITY: u64 = 10_000;

    /// Creates the default configuration.
    pub fn new() -> Self {
        Self {
            plan_cache_capacity: Self::DEFAULT_PLAN_CACHE_CAPACITY,
        }
    }

    /// Sets how many rendered statements the plan cache retains before
    /// evicting the least recently used.
    pub const fn plan_cache_capacity(mut self, capacity: u64) -> Self {
        self.plan_cache_capacity = capacity;
        self
    }
}

impl Default for DatabaseOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl Database {
    /// Connects a pool to the given PostgreSQL URL with default options.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL is invalid or the server is unreachable.
    pub async fn connect(url: &str) -> Result<Self, ExecuteError> {
        Self::connect_with(url, DatabaseOptions::new()).await
    }

    /// Connects a pool to the given PostgreSQL URL with explicit options.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL is invalid or the server is unreachable.
    pub async fn connect_with(url: &str, options: DatabaseOptions) -> Result<Self, ExecuteError> {
        Ok(Self::from_pool_with(PgPool::connect(url).await?, options))
    }

    /// Wraps an externally configured pool with default options.
    ///
    /// Use this when pool sizing, timeouts, or TLS need driver-level control.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self::from_pool_with(pool, DatabaseOptions::new())
    }

    /// Wraps an externally configured pool with explicit options.
    #[must_use]
    pub fn from_pool_with(pool: PgPool, options: DatabaseOptions) -> Self {
        Self {
            pool,
            plans: Arc::new(PlanCache::with_capacity(options.plan_cache_capacity)),
        }
    }

    /// Begins a database transaction sharing this database's plan cache.
    ///
    /// # Errors
    ///
    /// Returns an error when a connection cannot be acquired.
    pub async fn begin(&self) -> Result<Transaction<'_>, ExecuteError> {
        Ok(Transaction {
            inner: self.pool.begin().await?,
            plans: &self.plans,
        })
    }

    /// Returns the underlying driver pool.
    ///
    /// This is the supported escape hatch for statements JetORM cannot build
    /// yet; queries executed through it bypass the plan cache.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Closes every pooled connection.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

impl Executor for &Database {
    fn plan_cache(&self) -> &PlanCache {
        &self.plans
    }

    async fn fetch_rows(
        self,
        statement: Arc<Statement>,
        binds: Vec<Value>,
        columns: &'static [ColumnMeta],
    ) -> Result<Vec<JetRow>, ExecuteError> {
        let query = build_query(&statement, &binds)?;
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(|row| decode_row(row, columns)).collect()
    }
}

/// One open database transaction.
///
/// Dropping the value without calling [`Transaction::commit`] rolls the
/// transaction back, so an early `?` return can never leave changes applied.
/// That also makes an ignored transaction silently discard its work, which is
/// why the type is `#[must_use]`.
#[derive(Debug)]
#[must_use = "a dropped transaction rolls back; call commit() to keep its changes"]
pub struct Transaction<'database> {
    inner: sqlx::Transaction<'database, sqlx::Postgres>,
    plans: &'database PlanCache,
}

impl Transaction<'_> {
    /// Makes every change in this transaction durable.
    ///
    /// # Errors
    ///
    /// Returns an error when the database rejects the commit.
    pub async fn commit(self) -> Result<(), ExecuteError> {
        Ok(self.inner.commit().await?)
    }

    /// Discards every change in this transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the rollback statement itself fails.
    pub async fn rollback(self) -> Result<(), ExecuteError> {
        Ok(self.inner.rollback().await?)
    }

    /// Returns the underlying driver connection.
    ///
    /// This is the supported escape hatch for statements JetORM cannot build
    /// yet; queries executed through it bypass the plan cache but stay inside
    /// this transaction.
    #[must_use]
    pub fn connection(&mut self) -> &mut PgConnection {
        &mut self.inner
    }
}

impl Executor for &mut Transaction<'_> {
    fn plan_cache(&self) -> &PlanCache {
        self.plans
    }

    async fn fetch_rows(
        self,
        statement: Arc<Statement>,
        binds: Vec<Value>,
        columns: &'static [ColumnMeta],
    ) -> Result<Vec<JetRow>, ExecuteError> {
        let query = build_query(&statement, &binds)?;
        let rows = query.fetch_all(&mut *self.inner).await?;
        rows.iter().map(|row| decode_row(row, columns)).collect()
    }
}
