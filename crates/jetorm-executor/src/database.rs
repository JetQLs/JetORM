use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use jetorm_dialect::Statement;
use jetorm_entity::{ColumnType, Value};
use sqlx::postgres::{PgConnection, PgPool, PgPoolOptions};
use tracing::Instrument;

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
    /// positional values of the given types — the statement's output
    /// columns, which a projection may have narrowed below the entity's.
    ///
    /// No driver type crosses this boundary: implementations convert their
    /// native rows into [`JetRow`]s.
    #[doc(hidden)]
    fn fetch_rows(
        self,
        statement: Arc<Statement>,
        binds: Vec<Value>,
        columns: Vec<ColumnType>,
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
    max_connections: Option<u32>,
    min_connections: Option<u32>,
    acquire_timeout: Option<Duration>,
    idle_timeout: Option<Option<Duration>>,
    max_lifetime: Option<Option<Duration>>,
}

impl DatabaseOptions {
    /// Number of cached statements retained by default.
    ///
    /// Statements are a few hundred bytes each, so the default costs
    /// megabytes at most while bounding dynamically generated query shapes.
    pub const DEFAULT_PLAN_CACHE_CAPACITY: u64 = 10_000;

    /// Creates the default configuration.
    ///
    /// Pool knobs left unset keep the driver's own defaults rather than
    /// restating them here, so upgrading the driver never silently pins
    /// stale values.
    pub fn new() -> Self {
        Self {
            plan_cache_capacity: Self::DEFAULT_PLAN_CACHE_CAPACITY,
            max_connections: None,
            min_connections: None,
            acquire_timeout: None,
            idle_timeout: None,
            max_lifetime: None,
        }
    }

    /// Sets how many rendered statements the plan cache retains before
    /// evicting the least recently used.
    pub const fn plan_cache_capacity(mut self, capacity: u64) -> Self {
        self.plan_cache_capacity = capacity;
        self
    }

    /// Sets the largest number of pooled connections.
    pub const fn max_connections(mut self, connections: u32) -> Self {
        self.max_connections = Some(connections);
        self
    }

    /// Sets the number of connections the pool keeps open when idle.
    pub const fn min_connections(mut self, connections: u32) -> Self {
        self.min_connections = Some(connections);
        self
    }

    /// Sets how long acquiring a connection may wait before failing.
    pub const fn acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = Some(timeout);
        self
    }

    /// Sets how long a connection may sit idle before closing; `None`
    /// keeps idle connections forever.
    pub const fn idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Sets how long a connection may live before being replaced; `None`
    /// reuses connections forever.
    pub const fn max_lifetime(mut self, lifetime: Option<Duration>) -> Self {
        self.max_lifetime = Some(lifetime);
        self
    }

    /// Builds the driver pool configuration from the set knobs.
    fn pool_options(&self) -> PgPoolOptions {
        let mut pool = PgPoolOptions::new();
        if let Some(connections) = self.max_connections {
            pool = pool.max_connections(connections);
        }
        if let Some(connections) = self.min_connections {
            pool = pool.min_connections(connections);
        }
        if let Some(timeout) = self.acquire_timeout {
            pool = pool.acquire_timeout(timeout);
        }
        if let Some(timeout) = self.idle_timeout {
            pool = pool.idle_timeout(timeout);
        }
        if let Some(lifetime) = self.max_lifetime {
            pool = pool.max_lifetime(lifetime);
        }
        pool
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
        let pool = options.pool_options().connect(url).await?;
        Ok(Self::from_pool_with(pool, options))
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
        columns: Vec<ColumnType>,
    ) -> Result<Vec<JetRow>, ExecuteError> {
        let span = query_span(&statement, binds.len());
        async {
            let query = build_query(&statement, &binds)?;
            let rows = query.fetch_all(&self.pool).await?;
            tracing::Span::current().record("db.response.returned_rows", rows.len());
            rows.iter().map(|row| decode_row(row, &columns)).collect()
        }
        .instrument(span)
        .await
    }
}

/// One query execution span, named after OpenTelemetry's database
/// conventions so existing collectors pick the fields up unchanged. Bound
/// values are never recorded — only their count — because binds routinely
/// carry user data.
fn query_span(statement: &Statement, binds: usize) -> tracing::Span {
    tracing::debug_span!(
        "jetorm.query",
        db.system.name = "postgresql",
        db.query.text = statement.sql(),
        db.operation.parameter_count = binds,
        db.response.returned_rows = tracing::field::Empty,
    )
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
        columns: Vec<ColumnType>,
    ) -> Result<Vec<JetRow>, ExecuteError> {
        let span = query_span(&statement, binds.len());
        async {
            let query = build_query(&statement, &binds)?;
            let rows = query.fetch_all(&mut *self.inner).await?;
            tracing::Span::current().record("db.response.returned_rows", rows.len());
            rows.iter().map(|row| decode_row(row, &columns)).collect()
        }
        .instrument(span)
        .await
    }
}
