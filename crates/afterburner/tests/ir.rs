use afterburner::ir::{
    AttachmentError, Attribute, BinaryOperator, BlockId, EditError, EffectSet, Field, FunctionRef,
    JoinKind, Literal, LogicalOp, Module, NullOrder, OperationId, OperationKind, OperationSpec,
    ProfileSiteId, RegionId, ScalarOp, Schema, SortDirection, SortKey, SourceSpan, SqlType,
    TerminatorOp, Type, ValueId, VerificationLocation, Volatility, WalkOrder, WindowFrame,
    WindowFrameBound, WindowFrameUnit, WindowSpec, collect_operations, structural_fingerprint,
    verify_module,
};

#[derive(Debug)]
struct FilterFixture {
    module: Module,
    scan: OperationId,
    scan_value: ValueId,
    filter: OperationId,
    filter_value: ValueId,
    expression_region: RegionId,
    expression_block: BlockId,
    age: ValueId,
    literal: OperationId,
    literal_value: ValueId,
    predicate: OperationId,
    yield_operation: OperationId,
    query_return: OperationId,
}

#[derive(Debug, PartialEq, Eq)]
struct EstimatedRows(u64);

#[derive(Debug, PartialEq, Eq)]
struct PassNote(&'static str);

fn i64_type() -> Type {
    Type::scalar(
        SqlType::Integer {
            bits: 64,
            signed: true,
        },
        false,
    )
}

fn build_filter_fixture() -> FilterFixture {
    let mut module = Module::new();
    let root = module.root_block();
    let (
        scan,
        scan_value,
        filter,
        filter_value,
        expression_region,
        expression_block,
        age,
        literal,
        literal_value,
        predicate,
        yield_operation,
        query_return,
    ) = {
        let mut editor = module.editor();
        let schema = editor.intern_schema(Schema::new(vec![
            Field::new("id", i64_type()),
            Field::new("age", i64_type()),
        ]));
        let relation = Type::relation(schema);
        let scan = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Scan {
                    table: afterburner::ir::TableRef::new("users"),
                    columns: vec!["id".into(), "age".into()],
                })
                .with_result(relation.clone()),
            )
            .unwrap();
        let scan_value = editor.result(scan, 0).unwrap();
        let filter = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Filter)
                    .with_operands(vec![scan_value])
                    .with_result(relation.clone()),
            )
            .unwrap();
        let expression_region = editor.add_region(filter).unwrap();
        let expression_block = editor
            .append_block(expression_region, vec![i64_type(), i64_type()])
            .unwrap();
        let age = editor.block_argument(expression_block, 1).unwrap();
        let literal = editor
            .append_operation(
                expression_block,
                OperationSpec::new(ScalarOp::Literal(Literal::Integer(18))).with_result(i64_type()),
            )
            .unwrap();
        let literal_value = editor.result(literal, 0).unwrap();
        let predicate = editor
            .append_operation(
                expression_block,
                OperationSpec::new(ScalarOp::Binary(BinaryOperator::GreaterThanOrEqual))
                    .with_operands(vec![age, literal_value])
                    .with_result(Type::boolean(false)),
            )
            .unwrap();
        let predicate_value = editor.result(predicate, 0).unwrap();
        let yield_operation = editor
            .append_operation(
                expression_block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate_value]),
            )
            .unwrap();
        let filter_value = editor.result(filter, 0).unwrap();
        let query_return = editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![filter_value]),
            )
            .unwrap();
        (
            scan,
            scan_value,
            filter,
            filter_value,
            expression_region,
            expression_block,
            age,
            literal,
            literal_value,
            predicate,
            yield_operation,
            query_return,
        )
    };

    FilterFixture {
        module,
        scan,
        scan_value,
        filter,
        filter_value,
        expression_region,
        expression_block,
        age,
        literal,
        literal_value,
        predicate,
        yield_operation,
        query_return,
    }
}

#[test]
fn relational_ssa_with_nested_scalar_region_verifies() {
    let fixture = build_filter_fixture();

    verify_module(&fixture.module).unwrap();
    let literal = fixture.module.value(fixture.literal_value).unwrap();
    assert_eq!(literal.uses().len(), 1);
    assert_eq!(literal.uses()[0].user(), fixture.predicate);
}

#[test]
fn replace_all_uses_maintains_inverse_use_lists() {
    let mut fixture = build_filter_fixture();
    let replacement_value = {
        let mut editor = fixture.module.editor();
        let replacement = editor
            .insert_operation_before(
                fixture.predicate,
                OperationSpec::new(ScalarOp::Literal(Literal::Integer(21))).with_result(i64_type()),
            )
            .unwrap();
        let replacement_value = editor.result(replacement, 0).unwrap();
        editor
            .replace_all_uses(fixture.literal_value, replacement_value)
            .unwrap();
        editor.erase_operation(fixture.literal).unwrap();
        replacement_value
    };

    verify_module(&fixture.module).unwrap();
    assert!(fixture.module.operation(fixture.literal).is_none());
    assert_eq!(
        fixture
            .module
            .value(replacement_value)
            .unwrap()
            .uses()
            .len(),
        1
    );
}

#[test]
fn swap_remove_repairs_moved_inverse_use_backlinks() {
    let mut fixture = build_filter_fixture();
    let relation = fixture
        .module
        .value(fixture.scan_value)
        .unwrap()
        .ty()
        .clone();
    {
        let mut editor = fixture.module.editor();
        let first = editor
            .insert_operation_before(
                fixture.filter,
                OperationSpec::new(LogicalOp::Distinct)
                    .with_operands(vec![fixture.scan_value])
                    .with_result(relation.clone()),
            )
            .unwrap();
        let first_value = editor.result(first, 0).unwrap();
        let second = editor
            .insert_operation_before(
                fixture.filter,
                OperationSpec::new(LogicalOp::Distinct)
                    .with_operands(vec![fixture.scan_value])
                    .with_result(relation),
            )
            .unwrap();

        // Removing the first use moves `second` into its use-list slot. Erasing
        // `second` then exercises the repaired backlink rather than a linear scan.
        editor
            .replace_operand(fixture.filter, 0, first_value)
            .unwrap();
        editor.erase_operation(second).unwrap();
    }

    verify_module(&fixture.module).unwrap();
    assert_eq!(
        fixture
            .module
            .value(fixture.scan_value)
            .unwrap()
            .uses()
            .len(),
        1
    );
}

#[test]
fn recursive_erase_rejects_escaping_values_without_partial_mutation() {
    let mut fixture = build_filter_fixture();
    let revision = fixture.module.revision();

    let error = fixture
        .module
        .editor()
        .erase_operation(fixture.filter)
        .unwrap_err();

    assert_eq!(error, EditError::ValueStillUsed(fixture.filter_value));
    assert_eq!(fixture.module.revision(), revision);
    assert!(fixture.module.operation(fixture.filter).is_some());
    assert!(fixture.module.operation(fixture.literal).is_some());
    verify_module(&fixture.module).unwrap();
}

#[test]
fn recursive_erase_removes_owned_ir_and_attachments() {
    let mut fixture = build_filter_fixture();
    fixture
        .module
        .insert_attachment(fixture.filter, EstimatedRows(32))
        .unwrap();
    fixture
        .module
        .insert_attachment(fixture.literal, PassNote("foldable"))
        .unwrap();
    {
        let mut editor = fixture.module.editor();
        editor
            .replace_operand(fixture.query_return, 0, fixture.scan_value)
            .unwrap();
        editor.erase_operation(fixture.filter).unwrap();
    }

    assert!(fixture.module.operation(fixture.scan).is_some());
    assert!(fixture.module.operation(fixture.filter).is_none());
    assert!(fixture.module.operation(fixture.literal).is_none());
    assert!(fixture.module.operation(fixture.predicate).is_none());
    assert!(fixture.module.operation(fixture.yield_operation).is_none());
    assert!(fixture.module.region(fixture.expression_region).is_none());
    assert!(fixture.module.block(fixture.expression_block).is_none());
    assert!(fixture.module.value(fixture.age).is_none());
    assert!(fixture.module.value(fixture.literal_value).is_none());
    assert!(fixture.module.value(fixture.filter_value).is_none());
    assert_eq!(
        fixture.module.attachment::<EstimatedRows>(fixture.filter),
        Err(AttachmentError::UnknownOperation(fixture.filter))
    );
    verify_module(&fixture.module).unwrap();
}

#[test]
fn native_attachments_are_typed_shared_and_non_semantic() {
    let mut fixture = build_filter_fixture();
    let revision = fixture.module.revision();
    let fingerprint = structural_fingerprint(&fixture.module).unwrap();

    assert!(
        fixture
            .module
            .insert_attachment(fixture.filter, EstimatedRows(128))
            .unwrap()
            .is_none()
    );
    fixture
        .module
        .insert_attachment(fixture.filter, PassNote("hot path"))
        .unwrap();
    assert_eq!(
        fixture
            .module
            .attachment::<EstimatedRows>(fixture.filter)
            .unwrap(),
        Some(&EstimatedRows(128))
    );
    assert_eq!(
        fixture
            .module
            .attachment::<PassNote>(fixture.filter)
            .unwrap(),
        Some(&PassNote("hot path"))
    );
    assert_eq!(fixture.module.revision(), revision);
    assert_eq!(
        structural_fingerprint(&fixture.module).unwrap(),
        fingerprint
    );

    let cloned = fixture.module.clone();
    assert!(std::ptr::eq(
        fixture
            .module
            .attachment::<EstimatedRows>(fixture.filter)
            .unwrap()
            .unwrap(),
        cloned
            .attachment::<EstimatedRows>(fixture.filter)
            .unwrap()
            .unwrap(),
    ));

    let previous = fixture
        .module
        .insert_attachment(fixture.filter, EstimatedRows(256))
        .unwrap()
        .unwrap();
    assert_eq!(*previous, EstimatedRows(128));
    assert_eq!(
        fixture
            .module
            .remove_attachment::<PassNote>(fixture.filter)
            .unwrap()
            .as_deref(),
        Some(&PassNote("hot path"))
    );
}

#[test]
fn verifier_rejects_a_value_that_does_not_dominate_its_use() {
    let mut fixture = build_filter_fixture();
    let expression_block = fixture.module.operation(fixture.literal).unwrap().parent();
    {
        let mut editor = fixture.module.editor();
        let yield_operation = *editor
            .module()
            .block(expression_block)
            .unwrap()
            .operations()
            .last()
            .unwrap();
        let late_literal = editor
            .insert_operation_before(
                yield_operation,
                OperationSpec::new(ScalarOp::Literal(Literal::Integer(21))).with_result(i64_type()),
            )
            .unwrap();
        let late_value = editor.result(late_literal, 0).unwrap();
        editor
            .replace_operand(fixture.predicate, 1, late_value)
            .unwrap();
    }

    let errors = verify_module(&fixture.module).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.location() == VerificationLocation::Operation(fixture.predicate)
            && error.message().contains("does not dominate")
    }));
}

#[test]
fn fingerprints_ignore_diagnostics_but_include_semantic_attributes() {
    let mut fixture = build_filter_fixture();
    let baseline = structural_fingerprint(&fixture.module).unwrap();
    {
        let mut editor = fixture.module.editor();
        editor
            .set_profile_site(fixture.filter, Some(ProfileSiteId::new(7)))
            .unwrap();
        editor
            .set_source_span(fixture.filter, Some(SourceSpan::new("query.sql", 4, 31)))
            .unwrap();
    }
    assert_eq!(structural_fingerprint(&fixture.module).unwrap(), baseline);

    fixture
        .module
        .editor()
        .set_attribute(
            fixture.filter,
            "optimizer.barrier",
            Attribute::Boolean(true),
        )
        .unwrap();
    assert_ne!(structural_fingerprint(&fixture.module).unwrap(), baseline);
}

#[test]
fn walker_has_explicit_nested_region_order() {
    let fixture = build_filter_fixture();
    let preorder = collect_operations(
        &fixture.module,
        fixture.module.root_region(),
        WalkOrder::PreOrder,
    )
    .unwrap();
    let postorder = collect_operations(
        &fixture.module,
        fixture.module.root_region(),
        WalkOrder::PostOrder,
    )
    .unwrap();

    let filter_pre = preorder
        .iter()
        .position(|id| *id == fixture.filter)
        .unwrap();
    let literal_pre = preorder
        .iter()
        .position(|id| *id == fixture.literal)
        .unwrap();
    let filter_post = postorder
        .iter()
        .position(|id| *id == fixture.filter)
        .unwrap();
    let literal_post = postorder
        .iter()
        .position(|id| *id == fixture.literal)
        .unwrap();
    assert!(filter_pre < literal_pre);
    assert!(literal_post < filter_post);
}

#[test]
fn call_effects_combine_explicit_and_volatility_constraints() {
    let call = OperationKind::from(ScalarOp::Call {
        function: FunctionRef::new("may_fail"),
        volatility: Volatility::Stable,
        effects: EffectSet::MAY_ERROR,
    });

    assert!(call.effects().contains(EffectSet::READS_DATABASE));
    assert!(call.effects().contains(EffectSet::MAY_ERROR));
    assert!(!call.effects().is_pure());

    let division = OperationKind::from(ScalarOp::Binary(BinaryOperator::Divide));
    assert_eq!(division.effects(), EffectSet::MAY_ERROR);
}

#[test]
fn verifier_requires_unique_profile_sites() {
    let mut fixture = build_filter_fixture();
    {
        let mut editor = fixture.module.editor();
        editor
            .set_profile_site(fixture.filter, Some(ProfileSiteId::new(42)))
            .unwrap();
        editor
            .set_profile_site(fixture.literal, Some(ProfileSiteId::new(42)))
            .unwrap();
    }

    let errors = verify_module(&fixture.module).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.location() == VerificationLocation::Operation(fixture.literal)
            && error.message().contains("already assigned")
    }));
}

#[test]
fn verifier_requires_join_results_to_match_positional_join_semantics() {
    let mut module = Module::new();
    let root = module.root_block();
    let join = {
        let mut editor = module.editor();
        let schema = editor.intern_schema(Schema::new(vec![Field::new("id", i64_type())]));
        let relation = Type::relation(schema);
        let mut scans = Vec::new();
        for table in ["left_items", "right_items"] {
            let scan = editor
                .append_operation(
                    root,
                    OperationSpec::new(LogicalOp::Scan {
                        table: afterburner::ir::TableRef::new(table),
                        columns: vec!["id".into()],
                    })
                    .with_result(relation.clone()),
                )
                .unwrap();
            scans.push(editor.result(scan, 0).unwrap());
        }
        let join = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Join {
                    kind: JoinKind::Inner,
                    has_condition: false,
                })
                .with_operands(scans)
                // An inner join must return the left and right rows, but this
                // deliberately reuses the one-column input schema.
                .with_result(relation),
            )
            .unwrap();
        let joined = editor.result(join, 0).unwrap();
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![joined]),
            )
            .unwrap();
        join
    };

    let errors = verify_module(&module).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.location() == VerificationLocation::Operation(join)
            && error.message().contains("null-extended input rows")
    }));
}

fn module_with_misplaced_scalar(scalar: ScalarOp) -> (Module, OperationId) {
    let mut module = Module::new();
    let root = module.root_block();
    let scalar_operation = {
        let mut editor = module.editor();
        let schema = editor.intern_schema(Schema::new(vec![Field::new("id", i64_type())]));
        let relation = Type::relation(schema);
        let scan = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Scan {
                    table: afterburner::ir::TableRef::new("items"),
                    columns: vec!["id".into()],
                })
                .with_result(relation.clone()),
            )
            .unwrap();
        let input = editor.result(scan, 0).unwrap();
        let project = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Project)
                    .with_operands(vec![input])
                    .with_result(relation),
            )
            .unwrap();
        let region = editor.add_region(project).unwrap();
        let block = editor.append_block(region, vec![i64_type()]).unwrap();
        let argument = editor.block_argument(block, 0).unwrap();
        let scalar_operation = editor
            .append_operation(
                block,
                OperationSpec::new(scalar)
                    .with_operands(vec![argument])
                    .with_result(i64_type()),
            )
            .unwrap();
        let value = editor.result(scalar_operation, 0).unwrap();
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![value]),
            )
            .unwrap();
        let projected = editor.result(project, 0).unwrap();
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![projected]),
            )
            .unwrap();
        scalar_operation
    };
    (module, scalar_operation)
}

#[test]
fn verifier_restricts_aggregate_and_window_calls_to_their_regions() {
    let cases = [
        (
            ScalarOp::AggregateCall {
                function: FunctionRef::new("sum"),
                distinct: false,
                volatility: Volatility::Immutable,
                effects: EffectSet::PURE,
            },
            "aggregate calls are valid only inside an aggregate region",
        ),
        (
            ScalarOp::WindowCall {
                function: FunctionRef::new("lag"),
                argument_count: 1,
                window: WindowSpec::global(),
                volatility: Volatility::Immutable,
                effects: EffectSet::PURE,
            },
            "window calls are valid only inside a window region",
        ),
    ];

    for (scalar, message) in cases {
        let (module, operation) = module_with_misplaced_scalar(scalar);
        let errors = verify_module(&module).unwrap_err();
        assert!(errors.iter().any(|error| {
            error.location() == VerificationLocation::Operation(operation)
                && error.message() == message
        }));
    }
}

#[test]
fn verifier_rejects_invalid_window_frame_boundaries_and_modes() {
    let mut module = Module::new();
    let root = module.root_block();
    let window_call = {
        let mut editor = module.editor();
        let schema = editor.intern_schema(Schema::new(vec![Field::new("id", i64_type())]));
        let relation = Type::relation(schema);
        let scan = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Scan {
                    table: afterburner::ir::TableRef::new("items"),
                    columns: vec!["id".into()],
                })
                .with_result(relation.clone()),
            )
            .unwrap();
        let input = editor.result(scan, 0).unwrap();
        let window = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Window)
                    .with_operands(vec![input])
                    .with_result(relation),
            )
            .unwrap();
        let region = editor.add_region(window).unwrap();
        let block = editor.append_block(region, vec![i64_type()]).unwrap();
        let id = editor.block_argument(block, 0).unwrap();
        let offset = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::Literal(Literal::Integer(1))).with_result(i64_type()),
            )
            .unwrap();
        let offset = editor.result(offset, 0).unwrap();
        let specification = WindowSpec::new(1, Vec::new()).with_frame(WindowFrame::new(
            WindowFrameUnit::Groups,
            WindowFrameBound::Following,
        ));
        let window_call = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::WindowCall {
                    function: FunctionRef::new("row_number"),
                    argument_count: 0,
                    window: specification,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                // Partition key followed by the start-bound offset.
                .with_operands(vec![id, offset])
                .with_result(i64_type()),
            )
            .unwrap();
        let ordinal = editor.result(window_call, 0).unwrap();
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![ordinal]),
            )
            .unwrap();
        let result = editor.result(window, 0).unwrap();
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![result]),
            )
            .unwrap();
        window_call
    };

    let errors = verify_module(&module).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.location() == VerificationLocation::Operation(window_call)
            && error.message().contains("end cannot precede")
    }));
    assert!(errors.iter().any(|error| {
        error.location() == VerificationLocation::Operation(window_call)
            && error.message().contains("GROUPS frames require")
    }));
}

#[test]
fn window_spec_preserves_ordering_metadata() {
    let specification = WindowSpec::new(
        2,
        vec![SortKey::new(SortDirection::Descending, NullOrder::First)],
    );
    assert_eq!(specification.partition_key_count(), 2);
    assert_eq!(
        specification.order_keys(),
        [SortKey::new(SortDirection::Descending, NullOrder::First,)]
    );
}

fn window_fingerprint_module(direction: SortDirection) -> Module {
    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let schema = editor.intern_schema(Schema::new(vec![Field::new("id", i64_type())]));
        let relation = Type::relation(schema);
        let scan = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Scan {
                    table: afterburner::ir::TableRef::new("items"),
                    columns: vec!["id".into()],
                })
                .with_result(relation.clone()),
            )
            .unwrap();
        let input = editor.result(scan, 0).unwrap();
        let window = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Window)
                    .with_operands(vec![input])
                    .with_result(relation),
            )
            .unwrap();
        let region = editor.add_region(window).unwrap();
        let block = editor.append_block(region, vec![i64_type()]).unwrap();
        let id = editor.block_argument(block, 0).unwrap();
        let call = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::WindowCall {
                    function: FunctionRef::new("row_number"),
                    argument_count: 0,
                    window: WindowSpec::new(
                        0,
                        vec![SortKey::new(direction, NullOrder::DialectDefault)],
                    ),
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_operands(vec![id])
                .with_result(i64_type()),
            )
            .unwrap();
        let ordinal = editor.result(call, 0).unwrap();
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![ordinal]),
            )
            .unwrap();
        let result = editor.result(window, 0).unwrap();
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![result]),
            )
            .unwrap();
    }
    module
}

#[test]
fn fingerprints_include_window_specification_metadata() {
    let ascending = window_fingerprint_module(SortDirection::Ascending);
    let descending = window_fingerprint_module(SortDirection::Descending);

    assert_ne!(
        structural_fingerprint(&ascending).unwrap(),
        structural_fingerprint(&descending).unwrap()
    );
}
