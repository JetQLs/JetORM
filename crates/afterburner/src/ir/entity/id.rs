use std::fmt;

pub(crate) trait ArenaId: Copy + Eq {
    fn from_parts(slot: u32, generation: u32) -> Self;
    fn parts(self) -> (u32, u32);
}

macro_rules! arena_id {
    ($name:ident, $label:literal) => {
        /// Generational handle into one module-local IR arena.
        ///
        /// Equality is meaningful only within the module that created the handle.
        /// A generation change distinguishes a reused slot from the entity that
        /// previously occupied it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name {
            slot: u32,
            generation: u32,
        }

        impl $name {
            /// Returns the module-local arena slot.
            ///
            /// Slots are useful for diagnostics and dense side tables, but are
            /// not stable identities across modules, rewrites, or serialization.
            #[must_use]
            pub const fn slot(self) -> u32 {
                self.slot
            }

            /// Returns the generation used to reject stale slot references.
            #[must_use]
            pub const fn generation(self) -> u32 {
                self.generation
            }

            #[must_use]
            /// Reconstructs a raw module-local handle.
            ///
            /// This does not validate module ownership or entity existence. It
            /// is intended for diagnostics and internal adapters, not persistent
            /// IR identity.
            pub const fn from_raw_parts(slot: u32, generation: u32) -> Self {
                Self { slot, generation }
            }
        }

        impl ArenaId for $name {
            fn from_parts(slot: u32, generation: u32) -> Self {
                Self { slot, generation }
            }

            fn parts(self) -> (u32, u32) {
                (self.slot, self.generation)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    formatter,
                    concat!($label, "{}g{}"),
                    self.slot, self.generation
                )
            }
        }
    };
}

arena_id!(OperationId, "%op");
arena_id!(RegionId, "^region");
arena_id!(BlockId, "^block");
arena_id!(ValueId, "%v");
arena_id!(SchemaId, "!schema");

/// Instrumentation identity that remains independent of arena allocation.
///
/// The verifier requires profile-site ids to be unique within a module. Profile
/// artifacts need additional build, target, and structural-fingerprint identity
/// before they can be admitted safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileSiteId(u64);

impl ProfileSiteId {
    /// Creates a profile-site identity from its externally managed number.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying externally managed number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ProfileSiteId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pgo{}", self.0)
    }
}
