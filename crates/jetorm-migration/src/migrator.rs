use chrono::{DateTime, Utc};
use jetorm_dialect::postgres::ddl::{render_change, render_change_staged};
use jetorm_executor::Database;
use jetorm_executor::sqlx::{self, Row};
use jetorm_schema::SchemaChange;

use crate::error::MigrationError;
use crate::history::{AppliedMigration, HISTORY_TABLE, history_table};
use crate::migration::{Migration, MigrationSet, MigrationStep};

/// Where one migration stands relative to the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationState {
    /// Recorded in the database's history.
    Applied,
    /// Present in the set but not yet applied.
    Pending,
}

/// One migration paired with its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationStatus {
    version: String,
    state: MigrationState,
    applied_at: Option<DateTime<Utc>>,
}

impl MigrationStatus {
    /// Returns the migration's version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns whether the migration is applied.
    #[must_use]
    pub const fn state(&self) -> MigrationState {
        self.state
    }

    /// Returns when the migration was applied, if it was.
    #[must_use]
    pub const fn applied_at(&self) -> Option<DateTime<Utc>> {
        self.applied_at
    }
}

/// Applies and reverts a migration set against one database.
///
/// Every mutating method wraps one migration's steps and its history row in a
/// single transaction. PostgreSQL applies DDL transactionally, so a failed
/// migration leaves neither a half-changed schema nor a history row claiming
/// success.
pub struct Migrator<'a> {
    database: &'a Database,
    migrations: &'a MigrationSet,
}

impl<'a> Migrator<'a> {
    /// Binds a migration set to a database.
    #[must_use]
    pub const fn new(database: &'a Database, migrations: &'a MigrationSet) -> Self {
        Self {
            database,
            migrations,
        }
    }

    /// Creates the history table when it does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the database rejects the statement.
    pub async fn install(&self) -> Result<(), MigrationError> {
        if self.history_exists().await? {
            return Ok(());
        }
        for statement in
            render_change(&SchemaChange::CreateTable(history_table())).map_err(|source| {
                MigrationError::Render {
                    version: HISTORY_TABLE.to_owned(),
                    source,
                }
            })?
        {
            sqlx::query(&statement)
                .execute(self.database.pool())
                .await?;
        }
        Ok(())
    }

    async fn history_exists(&self) -> Result<bool, MigrationError> {
        let row = sqlx::query(
            "SELECT EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_schema = current_schema() AND table_name = $1
             )",
        )
        .bind(HISTORY_TABLE)
        .fetch_one(self.database.pool())
        .await?;
        Ok(row.get::<bool, _>(0))
    }

    /// Returns applied migrations, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the history table cannot be read.
    pub async fn applied(&self) -> Result<Vec<AppliedMigration>, MigrationError> {
        self.install().await?;
        let rows = sqlx::query(&format!(
            "SELECT version, applied_at FROM \"{HISTORY_TABLE}\" ORDER BY applied_at, version"
        ))
        .fetch_all(self.database.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| AppliedMigration::new(row.get(0), row.get(1)))
            .collect())
    }

    /// Returns every migration with its state, in application order.
    ///
    /// # Errors
    ///
    /// Returns an error when history cannot be read, or
    /// [`MigrationError::UnknownAppliedVersion`] when the database has a
    /// migration applied that this set does not contain.
    pub async fn status(&self) -> Result<Vec<MigrationStatus>, MigrationError> {
        let applied = self.applied().await?;
        for record in &applied {
            if self.migrations.get(record.version()).is_none() {
                return Err(MigrationError::UnknownAppliedVersion {
                    version: record.version().to_owned(),
                });
            }
        }
        Ok(self
            .migrations
            .migrations()
            .iter()
            .map(|migration| {
                let record = applied
                    .iter()
                    .find(|record| record.version() == migration.version());
                MigrationStatus {
                    version: migration.version().to_owned(),
                    state: record.map_or(MigrationState::Pending, |_| MigrationState::Applied),
                    applied_at: record.map(AppliedMigration::applied_at),
                }
            })
            .collect())
    }

    /// Returns pending migrations in application order.
    ///
    /// # Errors
    ///
    /// Returns an error when history cannot be read or interpreted.
    pub async fn pending(&self) -> Result<Vec<&'a Migration>, MigrationError> {
        let status = self.status().await?;
        Ok(status
            .iter()
            .filter(|entry| entry.state() == MigrationState::Pending)
            .filter_map(|entry| self.migrations.get(entry.version()))
            .collect())
    }

    /// Reconstructs the schema the database should currently have.
    ///
    /// This is the baseline the code-first differ compares entity metadata
    /// against.
    ///
    /// # Errors
    ///
    /// Returns an error when history cannot be read or replayed.
    pub async fn replay_applied(&self) -> Result<jetorm_schema::SchemaSet, MigrationError> {
        let applied: Vec<String> = self
            .applied()
            .await?
            .into_iter()
            .map(|record| record.version().to_owned())
            .collect();
        self.migrations.replay_applied(&applied)
    }

    /// Applies pending migrations, oldest first.
    ///
    /// `count` limits how many are applied; `None` applies all of them.
    /// Returns the versions applied, in order.
    ///
    /// # Errors
    ///
    /// Returns an error when a migration cannot be rendered or the database
    /// rejects it. Migrations already applied in this call stay applied;
    /// the failing one is rolled back whole.
    pub async fn up(&self, count: Option<usize>) -> Result<Vec<String>, MigrationError> {
        let pending = self.pending().await?;
        let selected = count.unwrap_or(pending.len()).min(pending.len());
        let mut applied = Vec::with_capacity(selected);
        for migration in pending.into_iter().take(selected) {
            self.apply(migration).await?;
            applied.push(migration.version().to_owned());
        }
        Ok(applied)
    }

    /// Applies exactly the given versions, verifying they are the leading
    /// pending migrations at application time.
    ///
    /// This closes the gap between reviewing a plan and applying it: when
    /// another process applies or adds migrations in between, the plan no
    /// longer matches and this fails instead of applying something the
    /// caller never vetted.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending set no longer starts with the
    /// given versions, a migration cannot be rendered, or the database
    /// rejects it.
    pub async fn up_versions(&self, versions: &[String]) -> Result<Vec<String>, MigrationError> {
        let pending = self.pending().await?;
        if pending.len() < versions.len() {
            return Err(MigrationError::InvalidVersion {
                version: versions[pending.len().min(versions.len() - 1)].clone(),
                detail: "no longer pending; the plan is stale".to_owned(),
            });
        }
        for (expected, actual) in versions.iter().zip(&pending) {
            if actual.version() != expected {
                return Err(MigrationError::InvalidVersion {
                    version: expected.clone(),
                    detail: format!(
                        "pending migrations changed since the plan was reviewed; \
                         {} is next now",
                        actual.version()
                    ),
                });
            }
        }
        let mut applied = Vec::with_capacity(versions.len());
        for migration in pending.into_iter().take(versions.len()) {
            self.apply(migration).await?;
            applied.push(migration.version().to_owned());
        }
        Ok(applied)
    }

    /// Reverts applied migrations, newest first.
    ///
    /// Returns the versions reverted, in the order they were reverted.
    ///
    /// # Errors
    ///
    /// Returns an error when a migration is irreversible, cannot be
    /// rendered, or the database rejects it.
    pub async fn down(&self, count: usize) -> Result<Vec<String>, MigrationError> {
        // Recency order, not version order: with gap-filled application a
        // lower version can be the most recently applied, and "revert the
        // last migration" must mean the one that ran last.
        let history = self.applied().await?;
        let mut applied = Vec::with_capacity(history.len());
        for record in &history {
            let Some(migration) = self.migrations.get(record.version()) else {
                return Err(MigrationError::UnknownAppliedVersion {
                    version: record.version().to_owned(),
                });
            };
            applied.push(migration);
        }

        let mut reverted = Vec::new();
        for migration in applied.into_iter().rev().take(count) {
            if !migration.is_reversible() {
                return Err(MigrationError::InvalidVersion {
                    version: migration.version().to_owned(),
                    detail: "the migration declares no down steps and cannot be reverted"
                        .to_owned(),
                });
            }
            self.revert(migration).await?;
            reverted.push(migration.version().to_owned());
        }
        Ok(reverted)
    }

    /// Runs one migration's up steps and records it, in one transaction.
    async fn apply(&self, migration: &Migration) -> Result<(), MigrationError> {
        self.apply_staged(migration, false).await
    }

    /// Runs one migration, optionally staging constraint validation.
    ///
    /// Staged, a foreign-key addition takes effect `NOT VALID` inside the
    /// migration's transaction — new writes are constrained immediately —
    /// and existing rows validate afterwards, each constraint in its own
    /// transaction under a weaker lock. A failed validation leaves the
    /// migration recorded and the constraint enforced for new writes; the
    /// error names the statement to rerun once the data is repaired.
    async fn apply_staged(
        &self,
        migration: &Migration,
        stage_constraints: bool,
    ) -> Result<(), MigrationError> {
        let mut immediate = Vec::new();
        let mut deferred = Vec::new();
        for step in migration.up() {
            match step {
                MigrationStep::Change(change) => {
                    let staged = if stage_constraints {
                        render_change_staged(change)
                    } else {
                        render_change(change).map(|statements| {
                            jetorm_dialect::postgres::ddl::StagedStatements {
                                immediate: statements,
                                deferred: Vec::new(),
                            }
                        })
                    }
                    .map_err(|source| MigrationError::Render {
                        version: migration.version().to_owned(),
                        source,
                    })?;
                    immediate.extend(staged.immediate);
                    deferred.extend(staged.deferred);
                }
                MigrationStep::Sql { sql, .. } => immediate.push(sql.clone()),
            }
        }

        let mut transaction = self.database.begin().await?;
        for statement in &immediate {
            sqlx::query(statement)
                .execute(transaction.connection())
                .await?;
        }
        sqlx::query(&format!(
            "INSERT INTO \"{HISTORY_TABLE}\" (version, applied_at) VALUES ($1, $2)"
        ))
        .bind(migration.version())
        .bind(Utc::now())
        .execute(transaction.connection())
        .await?;
        transaction.commit().await?;

        // Every deferred validation runs even when an earlier one fails:
        // stopping early would leave later constraints unvalidated behind
        // an error that names only the first, and the operator's repair
        // pass deserves the full list.
        let mut failures = Vec::new();
        for statement in &deferred {
            if let Err(error) = sqlx::query(statement).execute(self.database.pool()).await {
                failures.push((statement.clone(), error.to_string()));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(MigrationError::Validation { failures })
        }
    }

    /// Applies pending migrations with constraint validation staged.
    ///
    /// The staged counterpart of [`Migrator::up_versions`], with the same
    /// stale-plan guard.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending set no longer starts with the
    /// given versions, a migration cannot be rendered or applied, or a
    /// deferred validation fails.
    pub async fn up_versions_staged(
        &self,
        versions: &[String],
    ) -> Result<Vec<String>, MigrationError> {
        let pending = self.pending().await?;
        // A rename or raw-SQL step after a staged constraint could change
        // the very names the deferred validation addresses; refusing is
        // honest where reordering would guess.
        for migration in &pending {
            let mut staged_constraint_seen = false;
            for step in migration.up() {
                match step {
                    MigrationStep::Change(change) => {
                        if matches!(change, jetorm_schema::SchemaChange::AddForeignKey { .. }) {
                            staged_constraint_seen = true;
                        } else if staged_constraint_seen
                            && matches!(
                                change,
                                jetorm_schema::SchemaChange::RenameTable { .. }
                                    | jetorm_schema::SchemaChange::RenameColumn { .. }
                                    | jetorm_schema::SchemaChange::DropTable(_)
                            )
                        {
                            return Err(MigrationError::InvalidVersion {
                                version: migration.version().to_owned(),
                                detail: "a rename or drop follows a staged \
                                         constraint; apply this migration \
                                         without --stage-constraints"
                                    .to_owned(),
                            });
                        }
                    }
                    MigrationStep::Sql { .. } if staged_constraint_seen => {
                        return Err(MigrationError::InvalidVersion {
                            version: migration.version().to_owned(),
                            detail: "a raw SQL step follows a staged constraint \
                                     and could rename what the deferred \
                                     validation addresses; apply this migration \
                                     without --stage-constraints"
                                .to_owned(),
                        });
                    }
                    MigrationStep::Sql { .. } => {}
                }
            }
        }
        if pending.len() < versions.len() {
            return Err(MigrationError::InvalidVersion {
                version: versions[pending.len().min(versions.len() - 1)].clone(),
                detail: "no longer pending; the plan is stale".to_owned(),
            });
        }
        for (expected, actual) in versions.iter().zip(&pending) {
            if actual.version() != expected {
                return Err(MigrationError::InvalidVersion {
                    version: expected.clone(),
                    detail: format!(
                        "pending migrations changed since the plan was reviewed; \
                         {} is next now",
                        actual.version()
                    ),
                });
            }
        }
        let mut applied = Vec::with_capacity(versions.len());
        for migration in pending.into_iter().take(versions.len()) {
            self.apply_staged(migration, true).await?;
            applied.push(migration.version().to_owned());
        }
        Ok(applied)
    }

    /// Validates every `NOT VALID` constraint in the given schema.
    ///
    /// Returns the constraints validated. Fails on the first constraint
    /// whose existing rows still violate it, naming it.
    ///
    /// # Errors
    ///
    /// Returns an error when the catalog cannot be read or a validation
    /// fails.
    pub async fn validate_pending_constraints(
        &self,
        schema_name: &str,
    ) -> Result<Vec<String>, MigrationError> {
        // Foreign keys only: staging creates nothing else, and a user's
        // own deliberately-unvalidated CHECK is a pattern this tool must
        // not silently flip.
        let rows = sqlx::query(
            "SELECT n.nspname, t.relname, c.conname
             FROM pg_constraint c
             JOIN pg_class t ON t.oid = c.conrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             WHERE n.nspname = $1 AND NOT c.convalidated AND c.contype = 'f'
             ORDER BY t.relname, c.conname",
        )
        .bind(schema_name)
        .fetch_all(self.database.pool())
        .await?;

        let mut validated = Vec::with_capacity(rows.len());
        let mut failures = Vec::new();
        for row in rows {
            let (schema, table, constraint): (String, String, String) =
                (row.get(0), row.get(1), row.get(2));
            let statement = format!(
                "ALTER TABLE {}.{} VALIDATE CONSTRAINT {}",
                quote_identifier(&schema),
                quote_identifier(&table),
                quote_identifier(&constraint),
            );
            match sqlx::query(&statement).execute(self.database.pool()).await {
                Ok(_) => validated.push(format!("{table}.{constraint}")),
                Err(error) => failures.push((format!("{table}.{constraint}"), error.to_string())),
            }
        }
        if failures.is_empty() {
            Ok(validated)
        } else {
            Err(MigrationError::Validation { failures })
        }
    }

    /// Runs one migration's down steps and forgets it, in one transaction.
    async fn revert(&self, migration: &Migration) -> Result<(), MigrationError> {
        let statements = render_steps(migration.version(), migration.down())?;
        let mut transaction = self.database.begin().await?;
        for statement in statements {
            sqlx::query(&statement)
                .execute(transaction.connection())
                .await?;
        }
        sqlx::query(&format!(
            "DELETE FROM \"{HISTORY_TABLE}\" WHERE version = $1"
        ))
        .bind(migration.version())
        .execute(transaction.connection())
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

/// Renders every step of one migration into executable statements.
/// Quotes one identifier read from the catalog, doubling embedded quotes.
///
/// Catalog names are legal PostgreSQL identifiers, which may contain
/// double quotes; interpolating them raw would produce broken — or worse,
/// injectable — statements.
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn render_steps(version: &str, steps: &[MigrationStep]) -> Result<Vec<String>, MigrationError> {
    let mut statements = Vec::with_capacity(steps.len());
    for step in steps {
        match step {
            MigrationStep::Change(change) => {
                statements.extend(render_change(change).map_err(|source| {
                    MigrationError::Render {
                        version: version.to_owned(),
                        source,
                    }
                })?);
            }
            MigrationStep::Sql { sql, .. } => statements.push(sql.clone()),
        }
    }
    Ok(statements)
}
