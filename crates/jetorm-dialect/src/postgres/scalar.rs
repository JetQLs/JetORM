use std::collections::HashMap;

use afterburner::ir::{
    BinaryOperator, BlockId, EffectSet, FunctionRef, Literal, Module, OperationKind, ScalarOp,
    SqlType, TimeZone, Type, UnaryOperator, ValueDefinition, ValueId,
};

use crate::error::RenderError;
use crate::postgres::quote_identifier;
use crate::postgres::types::type_name;

/// Nanoseconds in one 24-hour day.
const NANOS_PER_DAY: i64 = 86_400_000_000_000;

/// Dense placeholder assignment for frontend bind positions.
///
/// Each distinct bind position receives one `$n` placeholder, numbered by
/// first appearance in the rendered SQL, so a reused parameter binds once.
#[derive(Debug, Default)]
pub(crate) struct ParamMap {
    order: Vec<u32>,
    indices: HashMap<u32, usize>,
}

impl ParamMap {
    /// Returns the 1-based placeholder number for one bind position.
    fn placeholder(&mut self, position: u32) -> usize {
        if let Some(index) = self.indices.get(&position) {
            return *index + 1;
        }
        let index = self.order.len();
        self.order.push(position);
        self.indices.insert(position, index);
        index + 1
    }

    /// Returns bind positions in placeholder order.
    pub(crate) fn into_bind_order(self) -> Vec<u32> {
        self.order
    }
}

/// Row-lambda context mapping block arguments onto FROM-item columns.
pub(crate) struct RowScope<'a> {
    block: BlockId,
    alias: &'a str,
    columns: &'a [String],
}

impl<'a> RowScope<'a> {
    pub(crate) const fn new(block: BlockId, alias: &'a str, columns: &'a [String]) -> Self {
        Self {
            block,
            alias,
            columns,
        }
    }
}

/// Renders one scalar SSA value as a PostgreSQL expression.
///
/// Sub-expressions are always parenthesized, so rendered SQL never depends
/// on an operator-precedence table. Values reused across several consumers
/// re-render at each use site; that is sound only for non-volatile
/// operations, which the renderer enforces.
pub(crate) fn render_value(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    value_id: ValueId,
) -> Result<String, RenderError> {
    let value = module
        .value(value_id)
        .ok_or_else(|| RenderError::inconsistent(format!("stale value {value_id}")))?;
    match value.definition() {
        ValueDefinition::BlockArgument { block, index } => {
            if block != scope.block {
                return Err(RenderError::unsupported(
                    "expression captures a value from an enclosing region",
                ));
            }
            let column = scope.columns.get(index as usize).ok_or_else(|| {
                RenderError::inconsistent(format!("block argument {index} has no source column"))
            })?;
            Ok(format!(
                "{}.{}",
                quote_identifier(scope.alias)?,
                quote_identifier(column)?
            ))
        }
        ValueDefinition::OperationResult { operation, .. } => {
            let defining = module
                .operation(operation)
                .ok_or_else(|| RenderError::inconsistent(format!("stale operation {operation}")))?;
            if defining.effects().contains(EffectSet::VOLATILE) && value.uses().len() > 1 {
                return Err(RenderError::unsupported(
                    "a volatile expression result is consumed more than once; SQL would \
                     re-evaluate it per use site",
                ));
            }
            let OperationKind::Scalar(scalar) = defining.kind() else {
                return Err(RenderError::inconsistent(
                    "scalar context references a non-scalar operation result",
                ));
            };
            render_scalar_op(
                module,
                params,
                scope,
                scalar,
                defining.operands(),
                scalar_result_kind(module, value_id)?,
            )
        }
    }
}

fn render_scalar_op(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    scalar: &ScalarOp,
    operands: &[ValueId],
    result_kind: SqlType,
) -> Result<String, RenderError> {
    match scalar {
        ScalarOp::Literal(literal) => literal_sql(literal, &result_kind, false),
        ScalarOp::Parameter { position, .. } => {
            let placeholder = params.placeholder(*position);
            Ok(format!("${placeholder}::{}", type_name(&result_kind)?))
        }
        ScalarOp::Unary(operator) => {
            let operand = operand_sql(module, params, scope, operands, 0)?;
            Ok(match operator {
                UnaryOperator::Not => format!("(NOT {operand})"),
                UnaryOperator::Negate => format!("(- {operand})"),
                UnaryOperator::IsNull => format!("({operand} IS NULL)"),
                UnaryOperator::IsNotNull => format!("({operand} IS NOT NULL)"),
            })
        }
        ScalarOp::Binary(operator) => {
            let left = operand_sql(module, params, scope, operands, 0)?;
            let right = operand_sql(module, params, scope, operands, 1)?;
            Ok(format!(
                "({left} {} {right})",
                binary_operator_sql(*operator)
            ))
        }
        ScalarOp::Cast { to } => {
            let operand_id = *operands.first().ok_or_else(|| {
                RenderError::inconsistent("cast operation is missing its operand")
            })?;
            let operand = render_value(module, params, scope, operand_id)?;
            let Type::Scalar(target) = to else {
                return Err(RenderError::unsupported(
                    "cast targets must be scalar SQL types",
                ));
            };
            // A cast that only changes nullability is a frontend typing
            // artifact with no SQL counterpart; the operand renders as-is.
            if scalar_result_kind(module, operand_id)? == *target.kind() {
                return Ok(operand);
            }
            Ok(format!("CAST({operand} AS {})", type_name(target.kind())?))
        }
        ScalarOp::Case { arms } => {
            let expected = (*arms as usize).saturating_mul(2).saturating_add(1);
            if operands.len() != expected {
                return Err(RenderError::inconsistent(
                    "case operand count disagrees with its arm count",
                ));
            }
            let mut sql = String::from("(CASE");
            for arm in 0..*arms as usize {
                let condition = operand_sql(module, params, scope, operands, arm * 2)?;
                let value = operand_sql(module, params, scope, operands, arm * 2 + 1)?;
                sql.push_str(&format!(" WHEN {condition} THEN {value}"));
            }
            let otherwise = operand_sql(module, params, scope, operands, expected - 1)?;
            sql.push_str(&format!(" ELSE {otherwise} END)"));
            Ok(sql)
        }
        ScalarOp::Call { function, .. } => {
            let mut arguments = Vec::with_capacity(operands.len());
            for operand in operands {
                arguments.push(render_value(module, params, scope, *operand)?);
            }
            Ok(format!(
                "{}({})",
                function_name(function)?,
                arguments.join(", ")
            ))
        }
        ScalarOp::AggregateCall { .. } | ScalarOp::WindowCall { .. } => Err(
            RenderError::unsupported("aggregate and window calls are not rendered yet"),
        ),
    }
}

fn operand_sql(
    module: &Module,
    params: &mut ParamMap,
    scope: &RowScope<'_>,
    operands: &[ValueId],
    index: usize,
) -> Result<String, RenderError> {
    let operand = *operands.get(index).ok_or_else(|| {
        RenderError::inconsistent(format!("scalar operation is missing operand {index}"))
    })?;
    render_value(module, params, scope, operand)
}

fn scalar_result_kind(module: &Module, value_id: ValueId) -> Result<SqlType, RenderError> {
    module
        .value(value_id)
        .map(afterburner::ir::Value::ty)
        .and_then(Type::as_scalar)
        .map(|scalar| scalar.kind().clone())
        .ok_or_else(|| RenderError::inconsistent("expected a scalar-typed SSA value"))
}

const fn binary_operator_sql(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Modulo => "%",
        BinaryOperator::Equal => "=",
        BinaryOperator::NotEqual => "<>",
        BinaryOperator::LessThan => "<",
        BinaryOperator::LessThanOrEqual => "<=",
        BinaryOperator::GreaterThan => ">",
        BinaryOperator::GreaterThanOrEqual => ">=",
        BinaryOperator::And => "AND",
        BinaryOperator::Or => "OR",
        BinaryOperator::Concat => "||",
        BinaryOperator::Like => "LIKE",
        BinaryOperator::CaseInsensitiveLike => "ILIKE",
        BinaryOperator::IsDistinctFrom => "IS DISTINCT FROM",
    }
}

fn function_name(function: &FunctionRef) -> Result<String, RenderError> {
    match function.namespace() {
        Some(namespace) => Ok(format!(
            "{}.{}",
            quote_identifier(namespace)?,
            quote_identifier(function.name())?
        )),
        None => quote_identifier(function.name()),
    }
}

/// Renders one IR literal as PostgreSQL SQL.
///
/// With `exact` set, the literal is additionally cast to its precise SQL
/// type; row sources (`VALUES`) use this so derived column types match the
/// IR schema instead of PostgreSQL's literal-inference defaults.
pub(crate) fn literal_sql(
    literal: &Literal,
    kind: &SqlType,
    exact: bool,
) -> Result<String, RenderError> {
    let rendered = match literal {
        Literal::Null => return Ok(format!("CAST(NULL AS {})", type_name(kind)?)),
        Literal::Boolean(true) => "TRUE".to_owned(),
        Literal::Boolean(false) => "FALSE".to_owned(),
        Literal::Integer(value) => value.to_string(),
        Literal::Unsigned(value) => value.to_string(),
        Literal::Float(bits) => return float_sql(bits.to_f64(), kind),
        Literal::Decimal { coefficient, scale } => decimal_digits(*coefficient, *scale),
        Literal::String(text) => quote_string(text)?,
        Literal::Bytes(bytes) => return Ok(format!("'\\x{}'::bytea", hex_lower(bytes))),
        Literal::Date(days) => return Ok(format!("(DATE '1970-01-01' + ({days}))")),
        Literal::Time(nanos) => return time_sql(*nanos),
        Literal::Timestamp(micros) => return timestamp_sql(*micros, kind),
        Literal::Interval {
            months,
            days,
            nanos,
        } => return interval_sql(*months, *days, *nanos),
        Literal::Uuid(bytes) => return Ok(format!("'{}'::uuid", format_uuid(bytes))),
        Literal::Json(text) => return Ok(format!("{}::jsonb", quote_string(text)?)),
    };
    if exact {
        return Ok(format!("CAST({rendered} AS {})", type_name(kind)?));
    }
    Ok(rendered)
}

fn float_sql(value: f64, kind: &SqlType) -> Result<String, RenderError> {
    let name = type_name(kind)?;
    if value.is_nan() {
        return Ok(format!("'NaN'::{name}"));
    }
    if value.is_infinite() {
        let sign = if value.is_sign_negative() { "-" } else { "" };
        return Ok(format!("'{sign}Infinity'::{name}"));
    }
    // `{:?}` prints the shortest decimal that round-trips the exact bits.
    Ok(format!("CAST({value:?} AS {name})"))
}

fn quote_string(text: &str) -> Result<String, RenderError> {
    if text.contains('\0') {
        return Err(RenderError::unsupported(
            "PostgreSQL text values cannot contain NUL bytes",
        ));
    }
    Ok(format!("'{}'", text.replace('\'', "''")))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let hex = hex_lower(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn decimal_digits(coefficient: i128, scale: i16) -> String {
    let sign = if coefficient < 0 { "-" } else { "" };
    let digits = coefficient.unsigned_abs().to_string();
    if scale <= 0 {
        let zeros = "0".repeat(scale.unsigned_abs() as usize);
        return format!("{sign}{digits}{zeros}");
    }
    let scale = scale as usize;
    let padded = if digits.len() > scale {
        digits
    } else {
        format!("{}{digits}", "0".repeat(scale - digits.len() + 1))
    };
    let split = padded.len() - scale;
    format!("{sign}{}.{}", &padded[..split], &padded[split..])
}

fn time_sql(nanos: i64) -> Result<String, RenderError> {
    if !(0..=NANOS_PER_DAY).contains(&nanos) {
        return Err(RenderError::unsupported(
            "time literal is outside the 24-hour range",
        ));
    }
    let micros = whole_micros(nanos)?;
    let seconds = micros / 1_000_000;
    let fraction = micros % 1_000_000;
    Ok(format!(
        "TIME '{:02}:{:02}:{:02}.{fraction:06}'",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    ))
}

fn timestamp_sql(micros: i64, kind: &SqlType) -> Result<String, RenderError> {
    let epoch = match kind {
        SqlType::Timestamp {
            timezone: TimeZone::Naive,
            ..
        } => "TIMESTAMP '1970-01-01 00:00:00'",
        SqlType::Timestamp {
            timezone: TimeZone::Utc,
            ..
        } => "TIMESTAMPTZ '1970-01-01 00:00:00+00'",
        SqlType::Timestamp {
            timezone: TimeZone::Named(zone),
            ..
        } => {
            return Err(RenderError::unsupported(format!(
                "PostgreSQL types carry no fixed time zone; cannot render zone {zone:?}"
            )));
        }
        other => {
            return Err(RenderError::inconsistent(format!(
                "timestamp literal carries non-timestamp type {other:?}"
            )));
        }
    };
    Ok(format!("({epoch} + ({micros}) * INTERVAL '1 microsecond')"))
}

fn interval_sql(months: i32, days: i32, nanos: i64) -> Result<String, RenderError> {
    let micros = whole_micros(nanos)?;
    let sign = if micros < 0 { "-" } else { "" };
    let magnitude = micros.unsigned_abs();
    Ok(format!(
        "INTERVAL 'P{months}M{days}DT{sign}{}.{:06}S'",
        magnitude / 1_000_000,
        magnitude % 1_000_000
    ))
}

fn whole_micros(nanos: i64) -> Result<i64, RenderError> {
    if nanos % 1000 != 0 {
        return Err(RenderError::unsupported(
            "PostgreSQL temporal values have microsecond resolution; sub-microsecond \
             literals would silently lose precision",
        ));
    }
    Ok(nanos / 1000)
}
