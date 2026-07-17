//! Query construction and rendering benchmarks: JetORM vs SeaORM vs Diesel.
//!
//! Every contender produces the same logical statement for `public.users`:
//! `WHERE email LIKE $1 AND id > $2 ORDER BY id DESC LIMIT 20`, measured
//! from builder construction through to SQL text plus bind values. See the
//! crate documentation for how to read the two groups honestly.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use jetorm::PlanCache;
use jetorm::prelude::*;

/// JetORM entity for the benchmark table.
mod jet {
    use jetorm::prelude::*;

    /// One row of `public.users`.
    #[derive(Clone, Debug, JetModel)]
    #[jet(table = "users", schema = "public")]
    pub struct User {
        /// Generated primary key.
        #[jet(primary_key, auto_increment)]
        pub id: i64,
        /// Display name.
        pub name: String,
        /// Optional contact address.
        pub email: Option<String>,
    }
}

/// SeaORM entity for the same table.
mod sea {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "users", schema_name = "public")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub email: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Diesel schema for the same table.
mod schema {
    diesel::table! {
        public.users (id) {
            id -> BigInt,
            name -> Text,
            email -> Nullable<Text>,
        }
    }
}

fn jetorm_select() -> Select<jet::UserEntity> {
    use jet::user;
    jet::UserEntity::find()
        .filter(user::Email.like("%@example.com").and(user::Id.gt(100)))
        .order_by(user::Id.desc())
        .limit(20)
}

fn seaorm_statement() -> sea_orm::Statement {
    use sea_orm::{
        ColumnTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
    };
    sea::Entity::find()
        .filter(sea::Column::Email.like("%@example.com"))
        .filter(sea::Column::Id.gt(100))
        .order_by_desc(sea::Column::Id)
        .limit(20)
        .build(DbBackend::Postgres)
}

fn diesel_sql() -> String {
    use diesel::pg::Pg;
    use diesel::prelude::*;
    use schema::users::dsl::{email, id, users};

    let query = users
        .filter(email.like("%@example.com"))
        .filter(id.gt(100))
        .order(id.desc())
        .limit(20);
    // Caveat: `debug_query` is Diesel's only way to render SQL text without a
    // connection. It walks the same `QueryFragment` AST as execution, but the
    // debug bind formatting is extra work Diesel does not do in production —
    // read Diesel's numbers as an upper bound on its build cost.
    diesel::debug_query::<Pg, _>(&query).to_string()
}

/// First execution of a query shape: the plan-cache miss path.
///
/// This is exactly what [`jetorm::SelectExecute`] runs before its first fetch
/// — shape lookup, lowering, one verification (inside `render_query`), SQL
/// rendering, and the cache insert. Lowering and verifying an IR module is
/// genuinely more work than assembling a string, so JetORM is expected to
/// lose this group; the number exists to keep that one-time cost bounded.
fn cold_build(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("cold_build");

    group.bench_function("jetorm", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let query = jetorm_select();
            let statement = cache.statement(&query).expect("miss renders");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        bencher.iter(|| {
            let statement = seaorm_statement();
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        bencher.iter(|| black_box(diesel_sql().len()));
    });

    group.finish();
}

/// Steady state: the same query shape repeated, values changing per call.
///
/// JetORM resolves the statement through the fingerprint-keyed plan cache;
/// the others have no equivalent and rebuild the SQL text every call.
fn warm_repeat(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("warm_repeat");

    group.bench_function("jetorm", |bencher| {
        let cache = PlanCache::new();
        // Populate the cache once, as a long-lived server process would.
        cache
            .statement(&jetorm_select())
            .expect("query renders on the first call");

        bencher.iter(|| {
            let query = jetorm_select();
            let statement = cache.statement(&query).expect("cache resolves");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        bencher.iter(|| {
            let statement = seaorm_statement();
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        bencher.iter(|| black_box(diesel_sql().len()));
    });

    group.finish();
}

criterion_group!(benches, cold_build, warm_repeat);
criterion_main!(benches);
