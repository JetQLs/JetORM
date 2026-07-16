use std::future::Future;
use std::sync::Arc;

use jetorm_dialect::Statement;
use jetorm_entity::Value;
use sqlx::postgres::{PgConnection, PgPool, PgRow};

use crate::error::ExecuteError;
use crate::plan::PlanCache;
use crate::value::build_query;

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

    /// Fetches every row produced by one bound statement.
    #[doc(hidden)]
    fn fetch_rows(
        self,
        statement: Arc<Statement>,
        binds: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<PgRow>, ExecuteError>> + Send;
}

/// A PostgreSQL connection pool with its shared plan cache.
///
/// Cloning is cheap and shares both the pool and the cache.
#[derive(Clone, Debug)]
pub struct Database {
    pool: PgPool,
    plans: Arc<PlanCache>,
}

impl Database {
    /// Connects a pool to the given PostgreSQL URL.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL is invalid or the server is unreachable.
    pub async fn connect(url: &str) -> Result<Self, ExecuteError> {
        Ok(Self::from_pool(PgPool::connect(url).await?))
    }

    /// Wraps an externally configured pool.
    ///
    /// Use this when pool sizing, timeouts, or TLS need driver-level control.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            plans: Arc::new(PlanCache::new()),
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
    ) -> Result<Vec<PgRow>, ExecuteError> {
        let query = build_query(&statement, &binds)?;
        Ok(query.fetch_all(&self.pool).await?)
    }
}

/// One open database transaction.
///
/// Dropping the value without calling [`Transaction::commit`] rolls the
/// transaction back, so an early `?` return can never leave changes applied.
#[derive(Debug)]
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
    ) -> Result<Vec<PgRow>, ExecuteError> {
        let query = build_query(&statement, &binds)?;
        Ok(query.fetch_all(&mut *self.inner).await?)
    }
}
