# JetORM

A typed async ORM for PostgreSQL built on the AfterBurner query IR.

The throughline: **facts move to compile time, values move to bind
parameters, and one prepared statement serves every query of a shape.**

```toml
[dependencies]
jetorm = { git = "https://github.com/JetQLs/JetORM" }
tokio = { version = "1", features = ["full"] }
```

## Entities

One plain struct per table. The derive reads everything else off the
field types — any `SqlValue` type is a column type.

```rust
use jetorm::prelude::*;

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,        // nullable column
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "posts")]
pub struct Post {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub title: String,
    pub price: rust_decimal::Decimal, // exact `numeric`, never rounded
    #[jet(references = "user::Id", on_delete = "cascade")]
    pub author_id: i64,               // edge name "author" by convention
}
```

String-backed enums, typed JSON, and array columns need no annotation
on the field:

```rust
#[derive(Clone, Copy, Debug, PartialEq, JetEnum)]
pub enum Status { Draft, #[jet(rename = "live")] Published }

pub struct Prefs { /* any Serialize + Deserialize type */ }
// ... status: Status, prefs: Json<Prefs> as ordinary fields.

pub tags: Vec<String>,               // text[]  — any scalar element type
pub scores: Option<Vec<i32>>,        // integer[], the whole array nullable
```

Arrays are deliberately flat — `Vec<T>` of a scalar `T`, never nested —
and an empty array is not `NULL`.

**Native enums** — add `#[jet(native = "...")]` and the enum becomes a
real PostgreSQL `CREATE TYPE ... AS ENUM`: comparisons and `ORDER BY`
follow declaration order rather than the alphabet, the database rejects
undeclared values, and migrations evolve the type by appending variants.
Inside every statement the column is enum-typed; it crosses the driver
boundary as text, so decoding stays dialect-independent:

```rust
#[derive(Clone, Copy, Debug, PartialEq, JetEnum)]
#[jet(native = "article_status")]
pub enum Status { Draft, #[jet(rename = "live")] Published }
```

## Connecting

```rust
let db = Database::connect("postgres://localhost/app").await?;
// Every default is tunable:
let db = Database::connect_with(url, DatabaseOptions::new()
    .max_connections(32)
    .plan_cache_capacity(50_000)).await?;
```

Executions emit `jetorm.query` tracing spans (SQL text and row counts,
never bind values) and plan-cache miss events.

## Reading

```rust
let adults: Vec<User> = UserEntity::find()
    .filter(user::Email.like("%@example.com").and(user::Id.gt(100)))
    .order_by(user::Id.desc())
    .limit(20)
    .all(&db).await?;

let one:   Option<User> = UserEntity::find_by_id(7).one(&db).await?;
let total: u64          = UserEntity::find().filter(...).count(&db).await?;
let any:   bool         = UserEntity::find().filter(...).exists().get(&db).await?;
```

Every user value is a bind parameter; queries differing only in values
share one cached prepared statement.

**Projections** — tuples for one-offs, named structs for anything that
crosses a function boundary. Field types are checked against the entity
at compile time (wrong nullability is a compile error, not a decode
surprise):

```rust
let pairs: Vec<(i64, String)> = UserEntity::find()
    .select((user::Id, user::Name)).all(&db).await?;

#[derive(JetPartial)]
#[jet(columns = "user")]
struct Summary { id: i64, name: String }
let rows: Vec<Summary> = UserEntity::find().select_as::<Summary>().all(&db).await?;
```

**Relations** — edges are named marker types, so two foreign keys to one
entity (or a self-reference) stay distinct:

```rust
// One query per edge, any parent count (the N+1 answer):
let posts: Vec<Vec<Post>> = load_many::<post::Author, _>(&users, &db).await?;
// One LEFT JOIN fetching both sides:
let pairs: Vec<(Post, Option<User>)> = PostEntity::find()
    .also::<post::Author>()
    .filter_related(user::Name.eq("alice"))   // typed against the joined side
    .all(&db).await?;
// Inverse edges come free: .also::<Inverse<post::Author>>() on users.
```

**Grouped aggregates** — result types are the database's own promotions,
at compile time (`sum(i32) -> i64`, `avg(int) -> Decimal`, nullable
column -> `Option`):

```rust
let by_customer: Vec<(String, (i64, i64))> = OrderEntity::find()
    .group_by((order::Customer,))
    .select_agg((count_rows(), sum(order::Quantity)))
    .order_by_keys()
    .all(&db).await?;
```

**Pagination** — offset with totals, or keyset for depth:

```rust
let pager = ItemEntity::find().order_by(item::Id.asc()).paginate(&db, 50);
let (items, pages) = (pager.fetch_page(0).await?, pager.num_pages().await?);

// Page 1000 costs what page 1 costs:
let page = ItemEntity::find().cursor_by((item::Id,)).after(last_seen).first(50)
    .all(&db).await?;
```

## Writing

```rust
let created: Vec<Post> = PostEntity::insert(post)
    .returning().all(&db).await?;

let changed: u64 = PostEntity::update()
    .set(post::Title, "renamed")
    .filter(post::Id.eq(7))
    .execute(&db).await?;

PostEntity::delete().filter(post::Id.eq(7)).execute(&db).await?;
```

**Upserts** — choose the conflict arbiter and what a conflict does:

```rust
// Explicit arbiter and update list:
ItemEntity::insert(item)
    .on_conflict((item::Sku,))
    .update_columns((item::Price,))     // ON CONFLICT ("sku") DO UPDATE SET "price" = EXCLUDED."price"
    .returning().all(&db).await?;
ItemEntity::insert(item).on_conflict((item::Sku,)).ignore().execute(&db).await?;

// Natural-key convenience: insert or update every non-key column.
SettingEntity::upsert(setting).execute(&db).await?;
```

Nonsense is refused before the database sees it: assigning a target
column to itself, assigning a generated column, or arbitrating on an
auto-incrementing key (which is never inserted, so it could never
conflict) are lowering errors.

An update or delete **without a filter refuses to run** unless you write
`.all_rows()` — unbounded writes are said in code, not assumed.
Transactions: `let mut tx = db.begin().await?;` then pass `&mut tx`
anywhere an executor goes; drop rolls back, `tx.commit()` keeps.

## Migrations

The schema's source of truth is your entities. The workflow:

```rust
// Emit the target schema (a tiny bin or test in your app):
let mut target = SchemaSet::new();
target.insert_entity::<UserEntity>();
target.insert_entity::<PostEntity>();
std::fs::write("schema.toml", toml::to_string_pretty(&target)?)?;
```

```console
$ jet migrate generate add_posts --schema schema.toml   # diff -> TOML file
$ jet migrate status
$ jet migrate up                    # --allow-destructive to ack flagged steps
$ jet check --schema schema.toml    # CI: exit 0 in sync, 1 pending/drift
```

Renames are never guessed — the differ surfaces candidates and you
confirm by editing the file. Generated migrations have empty `down`
steps (reversal is not derivable); `jet migrate down --count 1 --yes`
refuses migrations without them.

**Existing database?** Start from it:

```console
$ jet db pull --out src/entities.rs --schema-out schema.toml
```

Native enum types pull as `JetEnum` definitions with their exact stored
labels; array columns pull as `Vec<T>` fields. Unmappable columns are
reported and skipped, never silently lost.

## Why JetORM

- **Compile-time honesty**: wrong projection types, mismatched relation
  columns, aggregate promotions, cross-entity predicates — compile
  errors here, runtime surprises elsewhere.
- **The plan cache**: build cost is one shape probe after first
  execution — measured 657ns for a join build vs 5.11µs (SeaORM) and
  2.82µs (Diesel) under equal conditions; the gap grows with query
  complexity because a shape probe doesn't care how much SQL it names.
- **No silent behavior**: ambiguous combinations (DISTINCT over a
  projection, LIMIT under GROUP BY) are rejected, not guessed;
  destructive operations demand acknowledgement flags; anything a tool
  cannot represent is reported.

