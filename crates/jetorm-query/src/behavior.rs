//! Typed existence queries derived from a select pipeline.

use jetorm_entity::{Entity, Value};

use crate::Select;
use crate::select::{CacheableQuery, QueryShape};

/// Scalar existence test for one select pipeline.
#[derive(Clone, Debug)]
pub struct Exists<E: Entity> {
    pub(crate) select: Select<E>,
}

impl<E: Entity> Exists<E> {
    pub(crate) const fn new(select: Select<E>) -> Self {
        Self { select }
    }

    /// Returns captured values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        self.select.binds()
    }

    /// Consumes the query and returns captured values in bind order.
    #[must_use]
    pub fn into_binds(self) -> Vec<Value> {
        self.select.into_binds()
    }
}

impl<E: Entity> CacheableQuery for Exists<E> {
    fn shape(&self) -> QueryShape {
        QueryShape::for_exists(&self.select)
    }
}
