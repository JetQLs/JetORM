use std::path::Path;

use jetorm_schema::{SchemaChange, SchemaSet};
use serde::{Deserialize, Serialize};

use crate::error::MigrationError;

/// One step of a migration.
///
/// Steps are data. SQL is rendered from [`MigrationStep::Change`] at apply
/// time rather than stored, so a migration file and the statements it runs
/// cannot disagree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MigrationStep {
    /// A typed schema change, rendered to DDL when applied.
    Change(SchemaChange),
    /// Raw SQL, for the things a schema differ cannot express — triggers,
    /// partitioning, data backfills, database-specific features.
    ///
    /// Raw SQL is opaque to the schema model, so a step that changes the
    /// schema must declare that effect in `state`. Otherwise replay would
    /// drift from the real database and every later diff would be wrong.
    /// A step that only touches data leaves `state` empty.
    Sql {
        /// Statement to execute verbatim.
        sql: String,
        /// What this SQL does to the schema model, for replay.
        #[serde(default)]
        state: Vec<SchemaChange>,
    },
}

impl MigrationStep {
    /// Returns the changes this step makes to the schema model.
    #[must_use]
    pub fn state_changes(&self) -> &[SchemaChange] {
        match self {
            Self::Change(change) => std::slice::from_ref(change),
            Self::Sql { state, .. } => state,
        }
    }

    /// Reports whether applying this step can lose data or fail on existing
    /// rows.
    ///
    /// Raw SQL is always treated as destructive: its contents are opaque, so
    /// the safe assumption is the cautious one.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        match self {
            Self::Change(change) => change.is_destructive(),
            Self::Sql { .. } => true,
        }
    }
}

/// One named, ordered migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    version: String,
    #[serde(default)]
    up: Vec<MigrationStep>,
    #[serde(default)]
    down: Vec<MigrationStep>,
}

impl Migration {
    /// Creates a migration from its version and steps.
    #[must_use]
    pub fn new(
        version: impl Into<String>,
        up: Vec<MigrationStep>,
        down: Vec<MigrationStep>,
    ) -> Self {
        Self {
            version: version.into(),
            up,
            down,
        }
    }

    /// Returns the version, which is this migration's identity in history.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the steps that apply this migration.
    #[must_use]
    pub fn up(&self) -> &[MigrationStep] {
        &self.up
    }

    /// Returns the steps that revert this migration.
    ///
    /// An empty list means the migration is irreversible.
    #[must_use]
    pub fn down(&self) -> &[MigrationStep] {
        &self.down
    }

    /// Reports whether this migration can be reverted.
    #[must_use]
    pub fn is_reversible(&self) -> bool {
        !self.down.is_empty()
    }

    /// Reports whether applying this migration can lose data.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        self.up.iter().any(MigrationStep::is_destructive)
    }

    /// Renders this migration as the contents of a migration file.
    ///
    /// # Errors
    ///
    /// Returns an error when the steps cannot be serialized.
    pub fn to_toml(&self) -> Result<String, MigrationError> {
        toml::to_string_pretty(self).map_err(|error| MigrationError::File {
            path: format!("{}.toml", self.version),
            detail: error.to_string(),
        })
    }

    /// Parses a migration from the contents of a migration file.
    ///
    /// # Errors
    ///
    /// Returns an error when the contents are not a valid migration.
    pub fn from_toml(path: &Path, contents: &str) -> Result<Self, MigrationError> {
        toml::from_str(contents).map_err(|error| MigrationError::File {
            path: path.display().to_string(),
            detail: error.to_string(),
        })
    }
}

/// Every migration of a project, in application order.
///
/// A set validates its own identity rules on construction — unique, non-empty
/// versions — and orders migrations by version, so the order on disk cannot
/// depend on directory listing order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationSet {
    migrations: Vec<Migration>,
}

impl MigrationSet {
    /// Creates a set, sorting by version and rejecting duplicates.
    ///
    /// # Errors
    ///
    /// Returns an error when a version is empty or repeated.
    pub fn new(mut migrations: Vec<Migration>) -> Result<Self, MigrationError> {
        migrations.sort_by(|left, right| left.version.cmp(&right.version));
        for pair in migrations.windows(2) {
            if pair[0].version == pair[1].version {
                return Err(MigrationError::DuplicateVersion {
                    version: pair[0].version.clone(),
                });
            }
        }
        for migration in &migrations {
            if migration.version.trim().is_empty() {
                return Err(MigrationError::InvalidVersion {
                    version: migration.version.clone(),
                    detail: "a version must not be empty".to_owned(),
                });
            }
        }
        Ok(Self { migrations })
    }

    /// Loads every `*.toml` migration in one directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be read or a file is not a
    /// valid migration.
    pub fn from_directory(directory: &Path) -> Result<Self, MigrationError> {
        let entries = std::fs::read_dir(directory).map_err(|error| MigrationError::File {
            path: directory.display().to_string(),
            detail: error.to_string(),
        })?;
        let mut migrations = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| MigrationError::File {
                    path: directory.display().to_string(),
                    detail: error.to_string(),
                })?
                .path();
            if path.extension().is_none_or(|extension| extension != "toml") {
                continue;
            }
            let contents =
                std::fs::read_to_string(&path).map_err(|error| MigrationError::File {
                    path: path.display().to_string(),
                    detail: error.to_string(),
                })?;
            migrations.push(Migration::from_toml(&path, &contents)?);
        }
        Self::new(migrations)
    }

    /// Returns migrations in application order.
    #[must_use]
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }

    /// Returns one migration by version.
    #[must_use]
    pub fn get(&self, version: &str) -> Option<&Migration> {
        self.migrations
            .iter()
            .find(|migration| migration.version == version)
    }

    /// Returns the number of migrations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.migrations.len()
    }

    /// Reports whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }

    /// Reconstructs the schema this set describes, in application order.
    ///
    /// This is the "current" side of the code-first diff and the baseline a
    /// drift check compares against.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::Replay`] when a migration's steps do not
    /// apply to the schema its predecessors produced, which means the set is
    /// internally inconsistent.
    pub fn replay(&self) -> Result<SchemaSet, MigrationError> {
        self.replay_through(self.migrations.len())
    }

    /// Reconstructs the schema after the first `count` migrations.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::Replay`] on an inconsistent set.
    pub fn replay_through(&self, count: usize) -> Result<SchemaSet, MigrationError> {
        let mut schema = SchemaSet::new();
        for migration in self.migrations.iter().take(count) {
            for step in &migration.up {
                schema.apply_all(step.state_changes()).map_err(|source| {
                    MigrationError::Replay {
                        version: migration.version.clone(),
                        source: Box::new(source),
                    }
                })?;
            }
        }
        Ok(schema)
    }

    /// Reconstructs the schema produced by exactly the applied versions.
    ///
    /// Versions are replayed in set order, not in the order given.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::UnknownAppliedVersion`] when history names a
    /// migration this set does not contain, or [`MigrationError::Replay`] on
    /// an inconsistent set.
    pub fn replay_applied(&self, applied: &[String]) -> Result<SchemaSet, MigrationError> {
        for version in applied {
            if self.get(version).is_none() {
                return Err(MigrationError::UnknownAppliedVersion {
                    version: version.clone(),
                });
            }
        }
        let mut schema = SchemaSet::new();
        for migration in &self.migrations {
            if !applied.contains(&migration.version) {
                continue;
            }
            for step in &migration.up {
                schema.apply_all(step.state_changes()).map_err(|source| {
                    MigrationError::Replay {
                        version: migration.version.clone(),
                        source: Box::new(source),
                    }
                })?;
            }
        }
        Ok(schema)
    }
}
