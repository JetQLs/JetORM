use afterburner::ir::{
    LogicalOp, Module, NullOrder, OperationId, OperationKind, SchemaId, SortDirection, SortKey,
    TableRef, TerminatorOp, Type, ValueDefinition, ValueId,
};

use crate::error::RenderError;
use crate::postgres::quote_identifier;
use crate::postgres::scalar::{ParamMap, RowScope, literal_sql, render_value};

mod advanced;

/// SQL clause slots in evaluation order.
///
/// A relational operation fuses into the current `SELECT` only while clause
/// evaluation order matches operation order; otherwise the accumulated query
/// is wrapped as a derived table. The ordering of this enum is semantic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    From,
    Where,
    Projection,
    Distinct,
    OrderBy,
    Limit,
}

/// One flat `SELECT` under construction.
struct SelectBuilder {
    from_item: String,
    alias: String,
    columns: Vec<String>,
    projection: Option<Vec<String>>,
    where_sql: Option<String>,
    group_sql: Option<Vec<String>>,
    distinct: bool,
    order_sql: Vec<String>,
    offset_sql: Option<String>,
    fetch_sql: Option<String>,
    stage: Stage,
}

impl SelectBuilder {
    fn from_source(from_item: String, alias: String, columns: Vec<String>) -> Self {
        Self {
            from_item,
            alias,
            columns,
            projection: None,
            where_sql: None,
            group_sql: None,
            distinct: false,
            order_sql: Vec::new(),
            offset_sql: None,
            fetch_sql: None,
            stage: Stage::From,
        }
    }

    fn render(&self) -> Result<String, RenderError> {
        if self.columns.is_empty() {
            return Err(RenderError::unsupported(
                "relations without columns cannot be rendered as SQL",
            ));
        }
        let projection = match &self.projection {
            Some(projection) => projection.clone(),
            None => {
                let alias = quote_identifier(&self.alias)?;
                let mut projection = Vec::with_capacity(self.columns.len());
                for column in &self.columns {
                    projection.push(format!("{alias}.{}", quote_identifier(column)?));
                }
                projection
            }
        };

        let mut sql = String::from("SELECT ");
        if self.distinct {
            sql.push_str("DISTINCT ");
        }
        sql.push_str(&projection.join(", "));
        sql.push_str(" FROM ");
        sql.push_str(&self.from_item);
        if let Some(where_sql) = &self.where_sql {
            sql.push_str(" WHERE ");
            sql.push_str(where_sql);
        }
        if let Some(group_sql) = &self.group_sql {
            sql.push_str(" GROUP BY ");
            if group_sql.is_empty() {
                sql.push_str("()");
            } else {
                sql.push_str(&group_sql.join(", "));
            }
        }
        if !self.order_sql.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.order_sql.join(", "));
        }
        if let Some(fetch) = &self.fetch_sql {
            sql.push_str(" LIMIT ");
            sql.push_str(fetch);
        }
        if let Some(offset) = &self.offset_sql {
            sql.push_str(" OFFSET ");
            sql.push_str(offset);
        }
        Ok(sql)
    }
}

/// Stateful walk of one verified module into PostgreSQL SQL.
pub(crate) struct Renderer<'module> {
    module: &'module Module,
    params: ParamMap,
    alias_counter: usize,
}

impl<'module> Renderer<'module> {
    pub(crate) fn new(module: &'module Module) -> Self {
        Self {
            module,
            params: ParamMap::default(),
            alias_counter: 0,
        }
    }

    /// Returns bind positions in placeholder order.
    pub(crate) fn into_bind_order(self) -> Vec<u32> {
        self.params.into_bind_order()
    }

    /// Renders the module's returned relation as one `SELECT` statement.
    pub(crate) fn render_module(&mut self) -> Result<String, RenderError> {
        let root_block = self
            .module
            .block(self.module.root_block())
            .ok_or_else(|| RenderError::inconsistent("root block handle is stale"))?;
        let terminator_id = *root_block
            .operations()
            .last()
            .ok_or_else(|| RenderError::inconsistent("root block has no terminator"))?;
        let terminator = self
            .module
            .operation(terminator_id)
            .ok_or_else(|| RenderError::inconsistent("root terminator handle is stale"))?;
        if terminator.kind() != &OperationKind::Terminator(TerminatorOp::QueryReturn) {
            return Err(RenderError::unsupported(
                "module root must end in a query return",
            ));
        }
        let returned = *terminator
            .operands()
            .first()
            .ok_or_else(|| RenderError::inconsistent("query return has no operand"))?;
        let source = self.defining_operation(returned)?;
        let builder = self.build_relation(source)?;
        builder.render()
    }

    fn build_relation(&mut self, operation_id: OperationId) -> Result<SelectBuilder, RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent(format!("stale operation {operation_id}")))?;
        let kind = operation.kind().clone();
        match kind {
            OperationKind::Logical(LogicalOp::Scan { table, columns }) => {
                let alias = self.next_alias();
                let from_item = format!("{} AS {}", table_sql(&table)?, quote_identifier(&alias)?);
                Ok(SelectBuilder::from_source(from_item, alias, columns))
            }
            OperationKind::Logical(LogicalOp::Values { rows }) => {
                self.values_source(operation_id, &rows)
            }
            OperationKind::Logical(LogicalOp::Empty) => {
                let schema = self.result_schema(operation_id)?;
                self.empty_source(schema)
            }
            OperationKind::Logical(LogicalOp::Filter) => {
                let input = self.input_relation(operation_id)?;
                let mut builder = self.build_relation(input)?;
                if builder.stage > Stage::Where {
                    builder = self.wrap(builder)?;
                }
                let predicate = self.single_yield_sql(operation_id, &builder)?;
                builder.where_sql = Some(match builder.where_sql.take() {
                    Some(existing) => format!("({existing} AND {predicate})"),
                    None => predicate,
                });
                builder.stage = Stage::Where;
                Ok(builder)
            }
            OperationKind::Logical(LogicalOp::Project) => {
                let input = self.input_relation(operation_id)?;
                let mut builder = self.build_relation(input)?;
                // A SELECT list coexists with WHERE, ORDER BY, and LIMIT in
                // one query level — PostgreSQL resolves sort keys against
                // the FROM row, so ordering by an unprojected column stays
                // valid. Only two cases force a derived table: a projection
                // already fused (the row shape changed), and DISTINCT
                // (whose ORDER BY must draw from the select list).
                if builder.projection.is_some() || builder.distinct {
                    builder = self.wrap(builder)?;
                }
                let schema_id = self.result_schema(operation_id)?;
                let schema = self
                    .module
                    .schema(schema_id)
                    .ok_or_else(|| RenderError::inconsistent("stale schema behind projection"))?;
                let output_names: Vec<String> = schema
                    .fields()
                    .iter()
                    .map(|field| field.name().to_owned())
                    .collect();
                let (block, values) = self.yielded_values(operation_id)?;
                if values.len() != output_names.len() {
                    return Err(RenderError::inconsistent(
                        "projection yield count disagrees with its output schema",
                    ));
                }
                let mut projection = Vec::with_capacity(values.len());
                {
                    let scope = RowScope::new(block, &builder.alias, &builder.columns);
                    for (value, name) in values.iter().zip(&output_names) {
                        let expression =
                            render_value(self.module, &mut self.params, &scope, *value)?;
                        projection.push(format!("{expression} AS {}", quote_identifier(name)?));
                    }
                }
                builder.projection = Some(projection);
                builder.columns = output_names;
                // Never lower the stage: a projection fused onto a sorted or
                // limited query must not reopen earlier clause slots.
                builder.stage = builder.stage.max(Stage::Projection);
                Ok(builder)
            }
            OperationKind::Logical(LogicalOp::Distinct) => {
                let input = self.input_relation(operation_id)?;
                let mut builder = self.build_relation(input)?;
                if builder.stage >= Stage::Distinct {
                    builder = self.wrap(builder)?;
                }
                builder.distinct = true;
                builder.stage = Stage::Distinct;
                Ok(builder)
            }
            OperationKind::Logical(LogicalOp::Sort { keys }) => {
                let input = self.input_relation(operation_id)?;
                let mut builder = self.build_relation(input)?;
                // Sort keys reference the row shape by position. Once a fused
                // projection changed that shape, the keys must address the
                // projected columns through a derived table.
                if builder.stage >= Stage::OrderBy || builder.projection.is_some() {
                    builder = self.wrap(builder)?;
                }
                builder.order_sql = self.sort_keys_sql(operation_id, &keys, &builder)?;
                builder.stage = Stage::OrderBy;
                Ok(builder)
            }
            OperationKind::Logical(LogicalOp::Limit {
                has_offset,
                has_fetch,
            }) => {
                let input = self.input_relation(operation_id)?;
                let mut builder = self.build_relation(input)?;
                if builder.stage >= Stage::Limit {
                    builder = self.wrap(builder)?;
                }
                // Row counts are scalar operands following the relation, so
                // parameterized counts render as placeholders and every page
                // of a paginated query shares one statement.
                let operation = self
                    .module
                    .operation(operation_id)
                    .ok_or_else(|| RenderError::inconsistent("stale limit operation"))?;
                let parent = operation.parent();
                let mut counts = operation.operands()[1..].iter().copied();

                let scope = RowScope::new(parent, &builder.alias, &builder.columns);
                let offset_sql = if has_offset {
                    let operand = counts.next().ok_or_else(|| {
                        RenderError::inconsistent("limit is missing its offset operand")
                    })?;
                    Some(render_value(
                        self.module,
                        &mut self.params,
                        &scope,
                        operand,
                    )?)
                } else {
                    None
                };
                let fetch_sql = if has_fetch {
                    let operand = counts.next().ok_or_else(|| {
                        RenderError::inconsistent("limit is missing its fetch operand")
                    })?;
                    Some(render_value(
                        self.module,
                        &mut self.params,
                        &scope,
                        operand,
                    )?)
                } else {
                    None
                };

                builder.offset_sql = offset_sql;
                builder.fetch_sql = fetch_sql;
                builder.stage = Stage::Limit;
                Ok(builder)
            }
            OperationKind::Logical(LogicalOp::Join {
                kind,
                has_condition,
            }) => self.join_relation(operation_id, kind, has_condition),
            OperationKind::Logical(LogicalOp::Aggregate { group_keys }) => {
                self.aggregate_relation(operation_id, group_keys)
            }
            OperationKind::Logical(LogicalOp::Window) => self.window_relation(operation_id),
            OperationKind::Logical(LogicalOp::Set { operator, all }) => {
                self.set_relation(operation_id, operator, all)
            }
            OperationKind::Extension(extension) => Err(RenderError::unsupported(format!(
                "extension operation {}.{} has no PostgreSQL rendering",
                extension.dialect(),
                extension.name()
            ))),
            OperationKind::Scalar(_) | OperationKind::Terminator(_) => {
                Err(RenderError::inconsistent(
                    "relation position references a non-relational operation",
                ))
            }
        }
    }

    /// Wraps the accumulated query as a derived table.
    ///
    /// An interior `ORDER BY` without a row limit is rejected: SQL does not
    /// guarantee that a derived table's ordering survives the enclosing
    /// query, so rendering it would silently drop the sort.
    fn wrap(&mut self, builder: SelectBuilder) -> Result<SelectBuilder, RenderError> {
        if !builder.order_sql.is_empty()
            && builder.fetch_sql.is_none()
            && builder.offset_sql.is_none()
        {
            return Err(RenderError::unsupported(
                "an interior ORDER BY without a row limit cannot be preserved through a \
                 derived table",
            ));
        }
        let alias = self.next_alias();
        let from_item = format!("({}) AS {}", builder.render()?, quote_identifier(&alias)?);
        Ok(SelectBuilder::from_source(
            from_item,
            alias,
            builder.columns,
        ))
    }

    fn values_source(
        &mut self,
        operation_id: OperationId,
        rows: &[Vec<afterburner::ir::Literal>],
    ) -> Result<SelectBuilder, RenderError> {
        let schema_id = self.result_schema(operation_id)?;
        if rows.is_empty() {
            return self.empty_source(schema_id);
        }
        let schema = self
            .module
            .schema(schema_id)
            .ok_or_else(|| RenderError::inconsistent("stale schema behind values source"))?;
        let mut kinds = Vec::with_capacity(schema.len());
        let mut columns = Vec::with_capacity(schema.len());
        for field in schema.fields() {
            let scalar = field.ty().as_scalar().ok_or_else(|| {
                RenderError::inconsistent("row schema field carries a non-scalar type")
            })?;
            kinds.push(scalar.kind().clone());
            columns.push(field.name().to_owned());
        }

        let mut rendered_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let mut rendered = Vec::with_capacity(row.len());
            for (literal, kind) in row.iter().zip(&kinds) {
                rendered.push(literal_sql(literal, kind, true)?);
            }
            rendered_rows.push(format!("({})", rendered.join(", ")));
        }

        let alias = self.next_alias();
        let column_aliases: Vec<String> = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Result<_, _>>()?;
        let from_item = format!(
            "(VALUES {}) AS {} ({})",
            rendered_rows.join(", "),
            quote_identifier(&alias)?,
            column_aliases.join(", ")
        );
        Ok(SelectBuilder::from_source(from_item, alias, columns))
    }

    fn empty_source(&mut self, schema_id: SchemaId) -> Result<SelectBuilder, RenderError> {
        let schema = self
            .module
            .schema(schema_id)
            .ok_or_else(|| RenderError::inconsistent("stale schema behind empty source"))?;
        if schema.is_empty() {
            return Err(RenderError::unsupported(
                "relations without columns cannot be rendered as SQL",
            ));
        }
        let mut projection = Vec::with_capacity(schema.len());
        let mut columns = Vec::with_capacity(schema.len());
        for field in schema.fields() {
            let scalar = field.ty().as_scalar().ok_or_else(|| {
                RenderError::inconsistent("row schema field carries a non-scalar type")
            })?;
            projection.push(format!(
                "{} AS {}",
                literal_sql(&afterburner::ir::Literal::Null, scalar.kind(), false)?,
                quote_identifier(field.name())?
            ));
            columns.push(field.name().to_owned());
        }
        let alias = self.next_alias();
        let from_item = format!(
            "(SELECT {} WHERE FALSE) AS {}",
            projection.join(", "),
            quote_identifier(&alias)?
        );
        Ok(SelectBuilder::from_source(from_item, alias, columns))
    }

    /// Renders the single scalar yielded by an operation's row lambda.
    fn single_yield_sql(
        &mut self,
        operation_id: OperationId,
        builder: &SelectBuilder,
    ) -> Result<String, RenderError> {
        let yielded = self.yielded_values(operation_id)?;
        let (block, values) = yielded;
        let value = *values
            .first()
            .ok_or_else(|| RenderError::inconsistent("row lambda yields no value"))?;
        let scope = RowScope::new(block, &builder.alias, &builder.columns);
        render_value(self.module, &mut self.params, &scope, value)
    }

    fn sort_keys_sql(
        &mut self,
        operation_id: OperationId,
        keys: &[SortKey],
        builder: &SelectBuilder,
    ) -> Result<Vec<String>, RenderError> {
        let (block, values) = self.yielded_values(operation_id)?;
        if values.len() != keys.len() {
            return Err(RenderError::inconsistent(
                "sort key count disagrees with yielded expressions",
            ));
        }
        let scope = RowScope::new(block, &builder.alias, &builder.columns);
        let mut rendered = Vec::with_capacity(keys.len());
        for (value, key) in values.iter().zip(keys) {
            let expression = render_value(self.module, &mut self.params, &scope, *value)?;
            let direction = match key.direction() {
                SortDirection::Ascending => "ASC",
                SortDirection::Descending => "DESC",
            };
            let nulls = match key.null_order() {
                NullOrder::First => " NULLS FIRST",
                NullOrder::Last => " NULLS LAST",
                NullOrder::DialectDefault => "",
            };
            rendered.push(format!("{expression} {direction}{nulls}"));
        }
        Ok(rendered)
    }

    /// Returns the row-lambda block and its yielded values for one operation.
    fn yielded_values(
        &self,
        operation_id: OperationId,
    ) -> Result<(afterburner::ir::BlockId, Vec<ValueId>), RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent(format!("stale operation {operation_id}")))?;
        let region_id = *operation
            .regions()
            .first()
            .ok_or_else(|| RenderError::inconsistent("operation owns no expression region"))?;
        let region = self
            .module
            .region(region_id)
            .ok_or_else(|| RenderError::inconsistent("stale expression region"))?;
        let block_id = *region
            .blocks()
            .first()
            .ok_or_else(|| RenderError::inconsistent("expression region has no block"))?;
        let block = self
            .module
            .block(block_id)
            .ok_or_else(|| RenderError::inconsistent("stale expression block"))?;
        let terminator_id = *block
            .operations()
            .last()
            .ok_or_else(|| RenderError::inconsistent("expression block has no terminator"))?;
        let terminator = self
            .module
            .operation(terminator_id)
            .ok_or_else(|| RenderError::inconsistent("stale expression terminator"))?;
        if terminator.kind() != &OperationKind::Terminator(TerminatorOp::Yield) {
            return Err(RenderError::inconsistent(
                "expression region must end in yield",
            ));
        }
        Ok((block_id, terminator.operands().to_vec()))
    }

    fn input_relation(&self, operation_id: OperationId) -> Result<OperationId, RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent(format!("stale operation {operation_id}")))?;
        let operand = *operation
            .operands()
            .first()
            .ok_or_else(|| RenderError::inconsistent("relational operation has no input"))?;
        self.defining_operation(operand)
    }

    fn input_relations(&self, operation_id: OperationId) -> Result<Vec<OperationId>, RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent(format!("stale operation {operation_id}")))?;
        operation
            .operands()
            .iter()
            .map(|operand| self.defining_operation(*operand))
            .collect()
    }

    fn defining_operation(&self, value_id: ValueId) -> Result<OperationId, RenderError> {
        let value = self
            .module
            .value(value_id)
            .ok_or_else(|| RenderError::inconsistent(format!("stale value {value_id}")))?;
        match value.definition() {
            ValueDefinition::OperationResult { operation, .. } => Ok(operation),
            ValueDefinition::BlockArgument { .. } => Err(RenderError::inconsistent(
                "relation values cannot be block arguments in the root region",
            )),
        }
    }

    fn result_schema(&self, operation_id: OperationId) -> Result<SchemaId, RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent(format!("stale operation {operation_id}")))?;
        let result = *operation
            .results()
            .first()
            .ok_or_else(|| RenderError::inconsistent("relational operation has no result"))?;
        self.module
            .value(result)
            .map(afterburner::ir::Value::ty)
            .and_then(Type::as_relation)
            .ok_or_else(|| RenderError::inconsistent("relational result must carry a schema"))
    }

    fn result_columns(&self, operation_id: OperationId) -> Result<Vec<String>, RenderError> {
        let schema_id = self.result_schema(operation_id)?;
        let schema = self
            .module
            .schema(schema_id)
            .ok_or_else(|| RenderError::inconsistent("stale relational result schema"))?;
        Ok(schema
            .fields()
            .iter()
            .map(|field| field.name().to_owned())
            .collect())
    }

    fn next_alias(&mut self) -> String {
        let alias = format!("t{}", self.alias_counter);
        self.alias_counter += 1;
        alias
    }
}

fn table_sql(table: &TableRef) -> Result<String, RenderError> {
    if table.catalog().is_some() {
        return Err(RenderError::unsupported(
            "PostgreSQL cannot reference tables in another catalog",
        ));
    }
    match table.schema() {
        Some(schema) => Ok(format!(
            "{}.{}",
            quote_identifier(schema)?,
            quote_identifier(table.name())?
        )),
        None => quote_identifier(table.name()),
    }
}
