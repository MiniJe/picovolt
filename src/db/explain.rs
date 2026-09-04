use super::*;

impl Database {
    /// Describe a SELECT's physical execution steps without reading table rows.
    /// The result has `step`, `operation`, and `detail` columns. Bounded query
    /// callers should use `query_with_limits("EXPLAIN ...", ...)` so the plan
    /// reflects bounded execution's range-index fallback.
    pub fn explain(&self, sql: &str) -> Result<QueryResult> {
        let statement = parse(sql)?;
        match statement {
            Statement::Explain { statement } => self.explain_statement(&statement, false),
            statement => self.explain_statement(&statement, false),
        }
    }

    pub(super) fn explain_statement(
        &self,
        statement: &Statement,
        bounded: bool,
    ) -> Result<QueryResult> {
        let mut steps = Vec::<(&str, String)>::new();
        let (projection, distinct, filter, group_by, having, order, limit, offset, columns) =
            match statement {
                Statement::Select {
                    table,
                    projection,
                    distinct,
                    before,
                    filter,
                    group_by,
                    having,
                    order,
                    limit,
                    offset,
                } => {
                    let columns = self.column_names(table)?;
                    check_projection(&columns, projection)?;
                    if let Some(filter) = filter {
                        check_predicate_columns(&columns, filter)?;
                    }
                    let grouped = !group_by.is_empty()
                        || projection_has_aggregate(projection)
                        || having.is_some();
                    let count_only = filter.is_none()
                        && group_by.is_empty()
                        && having.is_none()
                        && order.is_empty()
                        && limit.is_none()
                        && *offset == 0
                        && !distinct
                        && count_star_only(projection).is_some();
                    steps.push((
                        "snapshot",
                        format!("transaction {}", before.unwrap_or(self.current_tx())),
                    ));
                    if count_only {
                        steps.push((
                            "count envelopes",
                            format!("{table}: MVCC visibility only; row bodies are not decoded"),
                        ));
                        return Ok(plan_result(steps));
                    }
                    if !grouped
                        && filter.is_none()
                        && !distinct
                        && order.len() == 1
                        && self.has_index(table, &order[0].column)
                    {
                        project_select(
                            columns,
                            Vec::new(),
                            projection.clone(),
                            &[],
                            false,
                            *limit,
                            *offset,
                        )?;
                        steps.push((
                            "ordered index scan",
                            format!(
                                "{table}.{} {}; early stop {}",
                                order[0].column,
                                if order[0].descending { "DESC" } else { "ASC" },
                                limit
                                    .map(|n| n.saturating_add(*offset).to_string())
                                    .unwrap_or_else(|| "none".into())
                            ),
                        ));
                        steps.push(("project", "select output columns".into()));
                        append_limit(&mut steps, *limit, *offset);
                        return Ok(plan_result(steps));
                    }
                    let access = filter
                        .as_ref()
                        .and_then(|p| index_access(&self.tables[table], p, bounded));
                    match access {
                        Some((column, operation)) => steps.push((
                            operation,
                            format!("{table}.{column}; recheck full predicate and MVCC visibility"),
                        )),
                        None => steps.push((
                            "table scan",
                            format!("{table}: scan row versions and check MVCC visibility"),
                        )),
                    }
                    (
                        projection, distinct, filter, group_by, having, order, limit, offset,
                        columns,
                    )
                }
                Statement::SelectJoin {
                    source,
                    joins,
                    before,
                    projection,
                    distinct,
                    filter,
                    group_by,
                    having,
                    order,
                    limit,
                    offset,
                } => {
                    let mut columns = self
                        .column_names(&source.name)?
                        .into_iter()
                        .map(|c| format!("{}.{c}", source.qualifier()))
                        .collect::<Vec<_>>();
                    steps.push((
                        "snapshot",
                        format!("transaction {}", before.unwrap_or(self.current_tx())),
                    ));
                    steps.push((
                        "table scan",
                        format!("{} AS {}", source.name, source.qualifier()),
                    ));
                    for join in joins {
                        let right = self
                            .column_names(&join.table.name)?
                            .into_iter()
                            .map(|c| format!("{}.{c}", join.table.qualifier()))
                            .collect::<Vec<_>>();
                        resolve_join_keys(
                            &columns,
                            &right,
                            &join.first_column,
                            &join.second_column,
                        )?;
                        steps.push((
                            "table scan",
                            format!("{} AS {}", join.table.name, join.table.qualifier()),
                        ));
                        steps.push((
                            if join.left_join {
                                "left equality join"
                            } else {
                                "inner equality join"
                            },
                            format!(
                                "{} = {}; build ordered map of right input",
                                join.first_column, join.second_column
                            ),
                        ));
                        columns.extend(right);
                    }
                    if let Some(filter) = filter {
                        check_predicate_columns(&columns, filter)?;
                    }
                    (
                        projection, distinct, filter, group_by, having, order, limit, offset,
                        columns,
                    )
                }
                _ => return Err(PvError::Query("EXPLAIN requires a SELECT statement".into())),
            };
        if filter.is_some() {
            steps.push(("filter", "evaluate WHERE with SQL NULL semantics".into()));
        }
        let grouped =
            !group_by.is_empty() || projection_has_aggregate(projection) || having.is_some();
        check_projection(&columns, projection)?;
        if grouped {
            project_grouped(
                columns,
                Vec::new(),
                projection_to_items(projection.clone())?,
                group_by.clone(),
                having.clone(),
                order.clone(),
                *distinct,
                *limit,
                *offset,
            )?;
            steps.push((
                "aggregate",
                if group_by.is_empty() {
                    "whole input".into()
                } else {
                    format!("group by {}", group_by.join(", "))
                },
            ));
            steps.push(("project", "select group output columns".into()));
            if having.is_some() {
                steps.push(("having", "filter groups".into()));
            }
        } else {
            project_select(
                columns,
                Vec::new(),
                projection.clone(),
                order,
                *distinct,
                *limit,
                *offset,
            )?;
        }
        if !order.is_empty() {
            steps.push((
                if !grouped && !distinct && limit.is_some() {
                    "top-N sort"
                } else {
                    "sort"
                },
                order
                    .iter()
                    .map(|o| format!("{} {}", o.column, if o.descending { "DESC" } else { "ASC" }))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        if !grouped {
            steps.push(("project", "select output columns".into()));
        }
        if *distinct {
            steps.push(("distinct", "deduplicate output rows".into()));
        }
        append_limit(&mut steps, *limit, *offset);
        Ok(plan_result(steps))
    }
}

fn check_projection(columns: &[String], projection: &Projection) -> Result<()> {
    match projection {
        Projection::All => {}
        Projection::Columns(names) => {
            for name in names {
                projection_col_pos(columns, name)?;
            }
        }
        Projection::Items(items) => {
            for item in items {
                match &item.expr {
                    SelectExpr::Column(name) => {
                        projection_col_pos(columns, name)?;
                    }
                    SelectExpr::Aggregate(agg) => {
                        if let Some(name) = &agg.column {
                            projection_col_pos(columns, name)?;
                        }
                    }
                    SelectExpr::Scalar(expr) => check_scalar(columns, expr)?,
                }
            }
        }
    }
    Ok(())
}

fn check_scalar(columns: &[String], expr: &ScalarExpr) -> Result<()> {
    match expr {
        ScalarExpr::Column(name) => {
            projection_col_pos(columns, name)?;
        }
        ScalarExpr::Literal(_) => {}
        ScalarExpr::Function { arguments, .. } => {
            for arg in arguments {
                check_scalar(columns, arg)?;
            }
        }
        ScalarExpr::Case {
            branches,
            else_expr,
        } => {
            for (predicate, expr) in branches {
                check_predicate_columns(columns, predicate)?;
                check_scalar(columns, expr)?;
            }
            if let Some(expr) = else_expr {
                check_scalar(columns, expr)?;
            }
        }
    }
    Ok(())
}

fn append_limit(steps: &mut Vec<(&'static str, String)>, limit: Option<usize>, offset: usize) {
    if offset > 0 {
        steps.push(("offset", offset.to_string()));
    }
    if let Some(limit) = limit {
        steps.push(("limit", limit.to_string()));
    }
}

fn plan_result(steps: Vec<(&str, String)>) -> QueryResult {
    QueryResult::Rows {
        columns: vec!["step".into(), "operation".into(), "detail".into()],
        rows: steps
            .into_iter()
            .enumerate()
            .map(|(i, (op, detail))| {
                vec![
                    Value::Int(i as i64),
                    Value::Text(op.into()),
                    Value::Text(detail),
                ]
            })
            .collect(),
    }
}

fn index_access<'a>(
    table: &'a Table,
    pred: &'a Predicate,
    bounded: bool,
) -> Option<(&'a str, &'static str)> {
    match pred {
        Predicate::Compare { column, op, value } if table.indexes.contains_key(column) => {
            match op {
                CompareOp::Eq => Some((column, "index lookup")),
                CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge
                    if !bounded && !matches!(value, Value::Int(_) | Value::Decimal(_)) =>
                {
                    Some((column, "index range scan"))
                }
                _ => None,
            }
        }
        Predicate::And(a, b) => {
            index_access(table, a, bounded).or_else(|| index_access(table, b, bounded))
        }
        _ => None,
    }
}
