use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use jetorm_entity::{Column, Entity, Model, Relation, SqlValue};
use jetorm_query::{ColumnExt, EntityQuery};

use crate::database::Executor;
use crate::error::ExecuteError;
use crate::select::SelectExecute;

/// Rows on the referencing side of a relation.
type SourceModel<R> = <<R as Relation>::Source as Entity>::Model;
/// Rows on the referenced side of a relation.
type TargetModel<R> = <<R as Relation>::Target as Entity>::Model;
/// Rust type of a relation's join key.
type Key<R> = <<R as Relation>::SourceColumn as Column>::Rust;

/// Reads one row's join key through [`Model::value`].
///
/// A `NULL` key — a nullable, unset foreign key — yields `None`: the row
/// participates in no edge rather than failing the whole load.
fn key_of<C, M>(row: &M) -> Result<Option<C::Rust>, ExecuteError>
where
    C: Column,
    M: Model,
{
    let value = row.value(C::INDEX).ok_or_else(|| ExecuteError::Relation {
        detail: format!(
            "the model exposes no value for column {:?}; implement Model::value",
            C::meta().name()
        ),
    })?;
    if value.is_null() {
        return Ok(None);
    }
    C::Rust::from_value(value)
        .map(Some)
        .map_err(|mismatch| ExecuteError::Relation {
            detail: format!("join key column {:?}: {mismatch}", C::meta().name()),
        })
}

/// Loads, for each target-side row, every source row referencing it.
///
/// This is the batched answer to the N+1 problem: one query fetches the
/// children of all `parents` through a single array-bound membership test,
/// so ten parents and ten thousand parents run the same statement. Children
/// are grouped back onto their parents by join key; output order follows
/// `parents`, and each group preserves the query's row order.
///
/// # Errors
///
/// Returns an error when the query fails or a row's join key cannot be
/// read.
pub async fn load_many<R, X>(
    parents: &[TargetModel<R>],
    executor: X,
) -> Result<Vec<Vec<SourceModel<R>>>, ExecuteError>
where
    R: Relation,
    X: Executor,
    R::SourceColumn: Default,
    Key<R>: Eq + Hash + Clone,
    R::TargetColumn: Column<Rust = Key<R>>,
    SourceModel<R>: Clone,
{
    let parent_keys: Vec<Option<Key<R>>> = parents
        .iter()
        .map(key_of::<R::TargetColumn, _>)
        .collect::<Result<_, _>>()?;

    // Set-based dedup keeps huge parent lists linear; the vector preserves
    // first-seen order so the bound statement stays deterministic. The key
    // count doubles as key multiplicity for the distribution below.
    let mut remaining: HashMap<Key<R>, usize> = HashMap::new();
    let mut wanted: Vec<Key<R>> = Vec::new();
    for key in parent_keys.iter().flatten() {
        let count = remaining.entry(key.clone()).or_insert(0);
        if *count == 0 {
            wanted.push(key.clone());
        }
        *count += 1;
    }
    if wanted.is_empty() {
        return Ok(parents.iter().map(|_| Vec::new()).collect());
    }

    let children = R::Source::find()
        .filter(R::SourceColumn::default().is_in(wanted))
        .all(executor)
        .await?;

    let mut grouped: HashMap<Key<R>, Vec<SourceModel<R>>> = HashMap::new();
    for child in children {
        if let Some(key) = key_of::<R::SourceColumn, _>(&child)? {
            grouped.entry(key).or_default().push(child);
        }
    }

    // Parents sharing a key — the same parent listed twice, or an inverse
    // edge whose key is not unique — each receive the full group. Only the
    // last taker moves the group out; earlier takers clone, so the common
    // all-unique case never clones at all.
    Ok(parent_keys
        .into_iter()
        .map(|key| {
            let Some(key) = key else {
                return Vec::new();
            };
            let takers = remaining
                .get_mut(&key)
                .expect("every parent key was counted above");
            *takers -= 1;
            if *takers == 0 {
                grouped.remove(&key).unwrap_or_default()
            } else {
                grouped.get(&key).cloned().unwrap_or_default()
            }
        })
        .collect())
}

/// Loads, for each source-side row, the target row it references.
///
/// The belongs-to fetch: one query resolves the referenced rows of all
/// `sources` through a single array-bound membership test. A row whose
/// foreign key is `NULL` — or whose referenced row is gone — maps to
/// `None`; output order follows `sources`.
///
/// # Errors
///
/// Returns an error when the query fails or a row's join key cannot be
/// read.
pub async fn load_one<R, X>(
    sources: &[SourceModel<R>],
    executor: X,
) -> Result<Vec<Option<TargetModel<R>>>, ExecuteError>
where
    R: Relation,
    X: Executor,
    R::TargetColumn: Default + Column<Rust = Key<R>>,
    Key<R>: Eq + Hash + Clone,
    TargetModel<R>: Clone,
{
    let source_keys: Vec<Option<Key<R>>> = sources
        .iter()
        .map(key_of::<R::SourceColumn, _>)
        .collect::<Result<_, _>>()?;

    // Set-based dedup keeps huge source lists linear; the vector preserves
    // first-seen order so the bound statement stays deterministic.
    let mut seen: HashSet<Key<R>> = HashSet::new();
    let mut wanted: Vec<Key<R>> = Vec::new();
    for key in source_keys.iter().flatten() {
        if seen.insert(key.clone()) {
            wanted.push(key.clone());
        }
    }
    if wanted.is_empty() {
        return Ok(sources.iter().map(|_| None).collect());
    }

    let targets = R::Target::find()
        .filter(R::TargetColumn::default().is_in(wanted))
        .all(executor)
        .await?;

    let mut by_key: HashMap<Key<R>, TargetModel<R>> = HashMap::new();
    for target in targets {
        if let Some(key) = key_of::<R::TargetColumn, _>(&target)? {
            by_key.insert(key, target);
        }
    }

    Ok(source_keys
        .into_iter()
        .map(|key| key.and_then(|key| by_key.get(&key).cloned()))
        .collect())
}
