//! Rendering for relational operations that establish their own SQL scope.

use afterburner::ir::{JoinKind, OperationId, SetOperator};

use super::{Renderer, SelectBuilder, Stage};
use crate::error::RenderError;
use crate::postgres::quote_identifier;
use crate::postgres::scalar::{RowScope, render_value};

impl Renderer<'_> {
    pub(super) fn join_relation(
        &mut self,
        operation_id: OperationId,
        kind: JoinKind,
        has_condition: bool,
    ) -> Result<SelectBuilder, RenderError> {
        let inputs = self.input_relations(operation_id)?;
        if inputs.len() != 2 {
            return Err(RenderError::inconsistent(
                "join operation must have exactly two relation inputs",
            ));
        }
        let left_builder = self.build_relation(inputs[0])?;
        let left = self.wrap(left_builder)?;
        let right_builder = self.build_relation(inputs[1])?;
        let right = self.wrap(right_builder)?;
        let output_columns = self.result_columns(operation_id)?;

        let condition = if has_condition {
            self.join_condition_sql(operation_id, &left, &right)?
        } else {
            "TRUE".to_owned()
        };
        let left_width = left.columns.len();
        let expected_width = if matches!(kind, JoinKind::Semi | JoinKind::Anti) {
            left_width
        } else {
            left_width + right.columns.len()
        };
        if output_columns.len() != expected_width {
            return Err(RenderError::inconsistent(format!(
                "join output has {} columns but its inputs require {expected_width}",
                output_columns.len()
            )));
        }

        let mut projection = Vec::with_capacity(output_columns.len());
        for (index, output) in output_columns.iter().enumerate() {
            let (alias, source) = if index < left_width {
                (&left.alias, &left.columns[index])
            } else {
                (&right.alias, &right.columns[index - left_width])
            };
            projection.push(format!(
                "{}.{} AS {}",
                quote_identifier(alias)?,
                quote_identifier(source)?,
                quote_identifier(output)?
            ));
        }

        let left_alias = left.alias.clone();
        let from_item;
        let where_sql;
        match kind {
            JoinKind::Semi | JoinKind::Anti => {
                let negation = if kind == JoinKind::Anti { "NOT " } else { "" };
                from_item = left.from_item;
                where_sql = Some(format!(
                    "{negation}EXISTS (SELECT 1 FROM {} WHERE {condition})",
                    right.from_item
                ));
            }
            JoinKind::Cross => {
                from_item = format!("{} CROSS JOIN {}", left.from_item, right.from_item);
                where_sql = None;
            }
            JoinKind::Inner | JoinKind::Left | JoinKind::Right | JoinKind::Full => {
                let keyword = match kind {
                    JoinKind::Inner => "INNER JOIN",
                    JoinKind::Left => "LEFT JOIN",
                    JoinKind::Right => "RIGHT JOIN",
                    JoinKind::Full => "FULL JOIN",
                    JoinKind::Semi | JoinKind::Anti | JoinKind::Cross => unreachable!(),
                };
                from_item = format!(
                    "{} {keyword} {} ON {condition}",
                    left.from_item, right.from_item
                );
                where_sql = None;
            }
        }

        let mut joined = SelectBuilder::from_source(from_item, left_alias, output_columns);
        joined.projection = Some(projection);
        joined.where_sql = where_sql;
        joined.stage = Stage::Projection;
        self.wrap(joined)
    }

    fn join_condition_sql(
        &mut self,
        operation_id: OperationId,
        left: &SelectBuilder,
        right: &SelectBuilder,
    ) -> Result<String, RenderError> {
        let (block, values) = self.yielded_values(operation_id)?;
        let value = *values.first().ok_or_else(|| {
            RenderError::inconsistent("join condition region yields no predicate")
        })?;
        if values.len() != 1 {
            return Err(RenderError::inconsistent(
                "join condition region must yield one predicate",
            ));
        }
        let scope = RowScope::joined(
            block,
            &left.alias,
            &left.columns,
            &right.alias,
            &right.columns,
        );
        render_value(self.module, &mut self.params, &scope, value)
    }

    pub(super) fn set_relation(
        &mut self,
        operation_id: OperationId,
        operator: SetOperator,
        all: bool,
    ) -> Result<SelectBuilder, RenderError> {
        let inputs = self.input_relations(operation_id)?;
        if inputs.len() < 2 {
            return Err(RenderError::inconsistent(
                "set operation must have at least two relation inputs",
            ));
        }
        let keyword = match operator {
            SetOperator::Union => "UNION",
            SetOperator::Intersect => "INTERSECT",
            SetOperator::Except => "EXCEPT",
        };
        let all = if all { " ALL" } else { "" };
        let separator = format!(" {keyword}{all} ");
        let mut rendered = Vec::with_capacity(inputs.len());
        for input in inputs {
            let builder = self.build_relation(input)?;
            if !builder.order_sql.is_empty() && builder.fetch.is_none() && builder.offset.is_none()
            {
                return Err(RenderError::unsupported(
                    "an ORDER BY without a row limit cannot be preserved inside a set operand",
                ));
            }
            rendered.push(format!("({})", builder.render()?));
        }
        let alias = self.next_alias();
        let from_item = format!(
            "({}) AS {}",
            rendered.join(&separator),
            quote_identifier(&alias)?
        );
        Ok(SelectBuilder::from_source(
            from_item,
            alias,
            self.result_columns(operation_id)?,
        ))
    }

    pub(super) fn aggregate_relation(
        &mut self,
        operation_id: OperationId,
        group_keys: u32,
    ) -> Result<SelectBuilder, RenderError> {
        let input = self.input_relation(operation_id)?;
        let input_builder = self.build_relation(input)?;
        let mut builder = self.wrap(input_builder)?;
        let output_columns = self.result_columns(operation_id)?;
        let (block, values) = self.yielded_values(operation_id)?;
        let group_count = usize::try_from(group_keys).map_err(|_| {
            RenderError::inconsistent("aggregate group-key count does not fit this target")
        })?;
        let expected_count = group_count
            .checked_add(output_columns.len())
            .ok_or_else(|| RenderError::inconsistent("aggregate yield count overflow"))?;
        if values.len() != expected_count {
            return Err(RenderError::inconsistent(
                "aggregate yield count disagrees with its explicit group keys and output schema",
            ));
        }
        let (group_values, output_values) = values.split_at(group_count);

        let scope = RowScope::new(block, &builder.alias, &builder.columns);
        let mut group_sql = Vec::with_capacity(group_values.len());
        for value in group_values {
            group_sql.push(render_value(self.module, &mut self.params, &scope, *value)?);
        }
        let mut projection = Vec::with_capacity(output_values.len());
        for (value, output) in output_values.iter().zip(&output_columns) {
            let expression = render_value(self.module, &mut self.params, &scope, *value)?;
            projection.push(format!("{expression} AS {}", quote_identifier(output)?));
        }
        builder.projection = Some(projection);
        builder.columns = output_columns;
        builder.group_sql = Some(group_sql);
        builder.stage = Stage::Projection;
        Ok(builder)
    }

    pub(super) fn window_relation(
        &mut self,
        operation_id: OperationId,
    ) -> Result<SelectBuilder, RenderError> {
        let input = self.input_relation(operation_id)?;
        let input_builder = self.build_relation(input)?;
        let mut builder = self.wrap(input_builder)?;
        let output_columns = self.result_columns(operation_id)?;
        let (block, values) = self.yielded_values(operation_id)?;
        if values.len() != output_columns.len() {
            return Err(RenderError::inconsistent(
                "window yield count disagrees with its output schema",
            ));
        }
        let scope = RowScope::new(block, &builder.alias, &builder.columns);
        let mut projection = Vec::with_capacity(values.len());
        for (value, output) in values.iter().zip(&output_columns) {
            let expression = render_value(self.module, &mut self.params, &scope, *value)?;
            projection.push(format!("{expression} AS {}", quote_identifier(output)?));
        }
        builder.projection = Some(projection);
        builder.columns = output_columns;
        builder.stage = Stage::Projection;
        Ok(builder)
    }
}
