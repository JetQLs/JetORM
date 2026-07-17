//! Feature-query construction benchmarks: JetORM vs SeaORM vs Diesel.
//!
//! One group per feature — counts, relation joins, projections — and inside
//! each group every contender builds the same logical statement, measured
//! from builder construction to SQL text plus bind values. JetORM appears
//! twice per group: `jetorm_cold` pays the plan-cache miss (lowering,
//! verification, rendering), `jetorm_warm` is the steady state a server
//! process actually runs. SeaORM and Diesel rebuild their SQL every call,
//! so their single number serves both readings. The Diesel numbers carry
//! the same `debug_query` caveat as `query_build.rs`: it is Diesel's only
//! connectionless renderer and formats binds production code would not.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use jetorm::PlanCache;
use jetorm::prelude::*;

/// JetORM entities for the benchmark tables.
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

    /// One row of `public.posts`.
    #[derive(Clone, Debug, JetModel)]
    #[jet(table = "posts", schema = "public")]
    pub struct Post {
        /// Generated primary key.
        #[jet(primary_key, auto_increment)]
        pub id: i64,
        /// Post title.
        pub title: String,
        /// Authoring user.
        #[jet(references = "user::Id", relation = "author")]
        pub author_id: i64,
    }

    /// Named projection over `public.users`. The bench only builds the
    /// query, so the fields are never read back.
    #[allow(dead_code)]
    #[derive(Clone, Debug, JetPartial)]
    #[jet(columns = "user")]
    pub struct UserSummary {
        /// Generated primary key.
        pub id: i64,
        /// Display name.
        pub name: String,
    }

    /// One row of `public.boards`, carrying array columns.
    #[derive(Clone, Debug, JetModel)]
    #[jet(table = "boards", schema = "public")]
    pub struct Board {
        /// Generated primary key.
        #[jet(primary_key, auto_increment)]
        pub id: i64,
        /// Text array column.
        pub tags: Vec<String>,
        /// Integer array column.
        pub scores: Vec<i32>,
    }
}

/// SeaORM entities for the same tables.
mod sea_user {
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

mod sea_post {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "posts", schema_name = "public")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub author_id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::sea_user::Entity",
            from = "Column::AuthorId",
            to = "super::sea_user::Column::Id"
        )]
        User,
    }

    impl Related<super::sea_user::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::User.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod sea_board {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "boards", schema_name = "public")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub tags: Vec<String>,
        pub scores: Vec<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Diesel schema for the same tables.
mod schema {
    diesel::table! {
        public.users (id) {
            id -> BigInt,
            name -> Text,
            email -> Nullable<Text>,
        }
    }

    diesel::table! {
        public.posts (id) {
            id -> BigInt,
            title -> Text,
            author_id -> BigInt,
        }
    }

    diesel::table! {
        public.boards (id) {
            id -> BigInt,
            tags -> Array<Text>,
            scores -> Array<Integer>,
        }
    }

    diesel::joinable!(posts -> users (author_id));
    diesel::allow_tables_to_appear_in_same_query!(posts, users);
}

/// Counting rows matching a filter: `count(*) WHERE email LIKE $1`.
fn count_build(criterion: &mut Criterion) {
    use jet::user;

    let mut group = criterion.benchmark_group("count_build");

    let jetorm_count = || {
        jet::UserEntity::find()
            .filter(user::Email.like("%@example.com"))
            .into_count()
    };

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let query = jetorm_count();
            let statement = cache.statement(&query).expect("miss renders");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_count())
            .expect("query renders on the first call");
        bencher.iter(|| {
            let query = jetorm_count();
            let statement = cache.statement(&query).expect("cache resolves");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::{ColumnTrait, DbBackend, EntityTrait, QueryFilter, QuerySelect, QueryTrait};
        bencher.iter(|| {
            // SeaORM's paginator builds its count as a subquery wrap at
            // execution time; the connectionless equivalent is an explicit
            // count expression over the filtered select.
            let statement = sea_user::Entity::find()
                .filter(sea_user::Column::Email.like("%@example.com"))
                .select_only()
                .column_as(sea_user::Column::Id.count(), "count")
                .build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::users::dsl::{email, users};
        bencher.iter(|| {
            let query = users.filter(email.like("%@example.com")).count();
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

/// One `LEFT JOIN` pairing posts with their author:
/// `posts ⟕ users WHERE title LIKE $1 ORDER BY posts.id`.
fn join_build(criterion: &mut Criterion) {
    use jet::post;

    let mut group = criterion.benchmark_group("join_build");

    let jetorm_join = || {
        jet::PostEntity::find()
            .filter(post::Title.like("intro%"))
            .order_by(post::Id.asc())
            .also::<post::Author>()
    };

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let query = jetorm_join();
            let statement = cache.statement(&query).expect("miss renders");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_join())
            .expect("query renders on the first call");
        bencher.iter(|| {
            let query = jetorm_join();
            let statement = cache.statement(&query).expect("cache resolves");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::{ColumnTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QueryTrait};
        bencher.iter(|| {
            let statement = sea_post::Entity::find()
                .find_also_related(sea_user::Entity)
                .filter(sea_post::Column::Title.like("intro%"))
                .order_by_asc(sea_post::Column::Id)
                .build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::posts::dsl::{id, posts, title};
        use schema::users::dsl::users;
        bencher.iter(|| {
            let query = posts
                .left_join(users)
                .filter(title.like("intro%"))
                .order(id.asc());
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

/// A named two-column projection: `SELECT id, name WHERE email LIKE $1`.
fn projection_build(criterion: &mut Criterion) {
    use jet::user;

    let mut group = criterion.benchmark_group("projection_build");

    let jetorm_projection = || {
        jet::UserEntity::find()
            .select_as::<jet::UserSummary>()
            .filter(user::Email.like("%@example.com"))
            .into_select()
    };

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let query = jetorm_projection();
            let statement = cache.statement(&query).expect("miss renders");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_projection())
            .expect("query renders on the first call");
        bencher.iter(|| {
            let query = jetorm_projection();
            let statement = cache.statement(&query).expect("cache resolves");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::{ColumnTrait, DbBackend, EntityTrait, QueryFilter, QuerySelect, QueryTrait};
        bencher.iter(|| {
            let statement = sea_user::Entity::find()
                .select_only()
                .columns([sea_user::Column::Id, sea_user::Column::Name])
                .filter(sea_user::Column::Email.like("%@example.com"))
                .build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::users::dsl::{email, id, name, users};
        bencher.iter(|| {
            let query = users.select((id, name)).filter(email.like("%@example.com"));
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

/// One-row insert with RETURNING: `INSERT ... VALUES ... RETURNING *`.
fn insert_build(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("insert_build");

    let jet_user = || jet::User {
        id: 0,
        name: "alice".to_owned(),
        email: Some("alice@example.com".to_owned()),
    };
    let jetorm_insert = || jet::UserEntity::insert(jet_user()).returning();

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let mutation = jetorm_insert();
            let statement = cache.statement(&mutation).expect("miss renders");
            black_box((statement.sql().len(), mutation.binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_insert())
            .expect("statement renders on the first call");
        bencher.iter(|| {
            let mutation = jetorm_insert();
            let statement = cache.statement(&mutation).expect("cache resolves");
            black_box((statement.sql().len(), mutation.binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::{ActiveValue, DbBackend, EntityTrait, QueryTrait};
        bencher.iter(|| {
            let row = sea_user::ActiveModel {
                id: ActiveValue::NotSet,
                name: ActiveValue::Set("alice".to_owned()),
                email: ActiveValue::Set(Some("alice@example.com".to_owned())),
            };
            let statement = sea_user::Entity::insert(row).build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::users::dsl::{email, name, users};
        bencher.iter(|| {
            let query = diesel::insert_into(users)
                .values((name.eq("alice"), email.eq(Some("alice@example.com"))));
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

/// A filtered two-column update: `UPDATE ... SET ... WHERE id = $n`.
fn update_build(criterion: &mut Criterion) {
    use jet::user;

    let mut group = criterion.benchmark_group("update_build");

    let jetorm_update = || {
        jet::UserEntity::update()
            .set(user::Name, "renamed")
            .set_null(user::Email)
            .filter(user::Id.eq(7))
    };

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let mutation = jetorm_update();
            let statement = cache.statement(&mutation).expect("miss renders");
            black_box((statement.sql().len(), mutation.binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_update())
            .expect("statement renders on the first call");
        bencher.iter(|| {
            let mutation = jetorm_update();
            let statement = cache.statement(&mutation).expect("cache resolves");
            black_box((statement.sql().len(), mutation.binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, DbBackend, EntityTrait, QueryFilter, QueryTrait};
        bencher.iter(|| {
            let statement = sea_user::Entity::update_many()
                .col_expr(sea_user::Column::Name, Expr::value("renamed"))
                .col_expr(
                    sea_user::Column::Email,
                    Expr::value(sea_orm::Value::String(None)),
                )
                .filter(sea_user::Column::Id.eq(7))
                .build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::users::dsl::{email, id, name, users};
        bencher.iter(|| {
            let query = diesel::update(users.filter(id.eq(7)))
                .set((name.eq("renamed"), email.eq(None::<String>)));
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

/// One-row insert of array columns with RETURNING:
/// `INSERT ... VALUES ($1::text[], $2::integer[]) RETURNING *`.
fn array_insert_build(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("array_insert_build");

    let jet_board = || jet::Board {
        id: 0,
        tags: vec!["rust".to_owned(), "orm".to_owned()],
        scores: vec![10, 20, 30],
    };
    let jetorm_insert = || jet::BoardEntity::insert(jet_board()).returning();

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let mutation = jetorm_insert();
            let statement = cache.statement(&mutation).expect("miss renders");
            black_box((statement.sql().len(), mutation.binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_insert())
            .expect("statement renders on the first call");
        bencher.iter(|| {
            let mutation = jetorm_insert();
            let statement = cache.statement(&mutation).expect("cache resolves");
            black_box((statement.sql().len(), mutation.binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::{ActiveValue, DbBackend, EntityTrait, QueryTrait};
        bencher.iter(|| {
            let row = sea_board::ActiveModel {
                id: ActiveValue::NotSet,
                tags: ActiveValue::Set(vec!["rust".to_owned(), "orm".to_owned()]),
                scores: ActiveValue::Set(vec![10, 20, 30]),
            };
            let statement = sea_board::Entity::insert(row).build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::boards::dsl::{boards, scores, tags};
        bencher.iter(|| {
            let query = diesel::insert_into(boards).values((
                tags.eq(vec!["rust".to_owned(), "orm".to_owned()]),
                scores.eq(vec![10, 20, 30]),
            ));
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

/// Whole-array equality filter: `SELECT ... WHERE scores = $1::integer[]`.
fn array_filter_build(criterion: &mut Criterion) {
    use jet::board;

    let mut group = criterion.benchmark_group("array_filter_build");

    let jetorm_filter = || jet::BoardEntity::find().filter(board::Scores.eq(vec![10, 20, 30]));

    group.bench_function("jetorm_cold", |bencher| {
        bencher.iter(|| {
            let cache = PlanCache::new();
            let query = jetorm_filter();
            let statement = cache.statement(&query).expect("miss renders");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("jetorm_warm", |bencher| {
        let cache = PlanCache::new();
        cache
            .statement(&jetorm_filter())
            .expect("query renders on the first call");
        bencher.iter(|| {
            let query = jetorm_filter();
            let statement = cache.statement(&query).expect("cache resolves");
            black_box((statement.sql().len(), query.into_binds().len()))
        });
    });

    group.bench_function("seaorm", |bencher| {
        use sea_orm::{ColumnTrait, DbBackend, EntityTrait, QueryFilter, QueryTrait};
        bencher.iter(|| {
            let statement = sea_board::Entity::find()
                .filter(sea_board::Column::Scores.eq(vec![10, 20, 30]))
                .build(DbBackend::Postgres);
            black_box((
                statement.sql.len(),
                statement.values.as_ref().map_or(0, |values| values.0.len()),
            ))
        });
    });

    group.bench_function("diesel", |bencher| {
        use diesel::pg::Pg;
        use diesel::prelude::*;
        use schema::boards::dsl::{boards, scores};
        bencher.iter(|| {
            let query = boards.filter(scores.eq(vec![10, 20, 30]));
            black_box(diesel::debug_query::<Pg, _>(&query).to_string().len())
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    count_build,
    join_build,
    projection_build,
    insert_build,
    update_build,
    array_insert_build,
    array_filter_build
);
criterion_main!(benches);
