//! PostgreSQL spelling for explicit window specifications.

use std::slice::Iter;

use afterburner::ir::{
    FunctionRef, Module, NullOrder, SortDirection, ValueId, WindowFrame, WindowFrameBound,
    WindowFrameExclusion, WindowFrameUnit, WindowSpec,
};

use super::{ParamMap, RowScope, render_function_call, render_value};
use crate::error::RenderError;

pub(super) fn render_window_call(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    function: &FunctionRef,
    argument_count: u32,
    window: &WindowSpec,
    operands: &[ValueId],
) -> Result<String, RenderError> {
    let argument_count = usize::try_from(argument_count).map_err(|_| {
        RenderError::inconsistent("window function-argument count does not fit this target")
    })?;
    let partition_count = usize::try_from(window.partition_key_count()).map_err(|_| {
        RenderError::inconsistent("window partition-key count does not fit this target")
    })?;
    let order_count = window.order_keys().len();
    let offset_count = window.frame().map_or(0, |frame| frame.offset_count());
    let expected_count = argument_count
        .checked_add(partition_count)
        .and_then(|count| count.checked_add(order_count))
        .and_then(|count| count.checked_add(offset_count))
        .ok_or_else(|| RenderError::inconsistent("window operand layout overflow"))?;
    if operands.len() != expected_count {
        return Err(RenderError::inconsistent(
            "window operand count disagrees with its explicit layout",
        ));
    }
    let partition_end = argument_count + partition_count;
    let order_end = partition_end + order_count;
    let arguments = &operands[..argument_count];
    let partitions = &operands[argument_count..partition_end];
    let ordering = &operands[partition_end..order_end];
    let offsets = &operands[order_end..];

    let call = render_function_call(module, params, scope, function, arguments, false, false)?;
    let mut clauses = Vec::new();
    if !partitions.is_empty() {
        let mut expressions = Vec::with_capacity(partitions.len());
        for value in partitions {
            expressions.push(render_value(module, params, scope, *value)?);
        }
        clauses.push(format!("PARTITION BY {}", expressions.join(", ")));
    }
    if !ordering.is_empty() {
        let mut expressions = Vec::with_capacity(ordering.len());
        for (value, key) in ordering.iter().zip(window.order_keys()) {
            let mut expression = render_value(module, params, scope, *value)?;
            expression.push_str(match key.direction() {
                SortDirection::Ascending => " ASC",
                SortDirection::Descending => " DESC",
            });
            expression.push_str(match key.null_order() {
                NullOrder::First => " NULLS FIRST",
                NullOrder::Last => " NULLS LAST",
                NullOrder::DialectDefault => "",
            });
            expressions.push(expression);
        }
        clauses.push(format!("ORDER BY {}", expressions.join(", ")));
    }
    if let Some(frame) = window.frame() {
        clauses.push(render_window_frame(module, params, scope, *frame, offsets)?);
    }

    Ok(format!("{call} OVER ({})", clauses.join(" ")))
}

fn render_window_frame(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    frame: WindowFrame,
    offsets: &[ValueId],
) -> Result<String, RenderError> {
    let unit = match frame.unit() {
        WindowFrameUnit::Rows => "ROWS",
        WindowFrameUnit::Range => "RANGE",
        WindowFrameUnit::Groups => "GROUPS",
    };
    let mut offsets = offsets.iter();
    let start = render_window_bound(module, params, scope, frame.start(), &mut offsets)?;
    let bounds = if let Some(end) = frame.end() {
        let end = render_window_bound(module, params, scope, end, &mut offsets)?;
        format!("BETWEEN {start} AND {end}")
    } else {
        start
    };
    if offsets.next().is_some() {
        return Err(RenderError::inconsistent(
            "window frame has unused offset operands",
        ));
    }
    let exclusion = match frame.exclusion() {
        WindowFrameExclusion::NoOthers => "",
        WindowFrameExclusion::CurrentRow => " EXCLUDE CURRENT ROW",
        WindowFrameExclusion::Group => " EXCLUDE GROUP",
        WindowFrameExclusion::Ties => " EXCLUDE TIES",
    };
    Ok(format!("{unit} {bounds}{exclusion}"))
}

fn render_window_bound(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    bound: WindowFrameBound,
    offsets: &mut Iter<'_, ValueId>,
) -> Result<String, RenderError> {
    match bound {
        WindowFrameBound::UnboundedPreceding => Ok("UNBOUNDED PRECEDING".to_owned()),
        WindowFrameBound::Preceding => {
            let offset = render_frame_offset(module, params, scope, offsets)?;
            Ok(format!("{offset} PRECEDING"))
        }
        WindowFrameBound::CurrentRow => Ok("CURRENT ROW".to_owned()),
        WindowFrameBound::Following => {
            let offset = render_frame_offset(module, params, scope, offsets)?;
            Ok(format!("{offset} FOLLOWING"))
        }
        WindowFrameBound::UnboundedFollowing => Ok("UNBOUNDED FOLLOWING".to_owned()),
    }
}

fn render_frame_offset(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    offsets: &mut Iter<'_, ValueId>,
) -> Result<String, RenderError> {
    let value = offsets
        .next()
        .ok_or_else(|| RenderError::inconsistent("window frame is missing an offset operand"))?;
    render_value(module, params, scope, *value)
}
