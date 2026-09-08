//! Streaming aggregation retains group keys and accumulators, not all input rows.
use super::*;

#[derive(Clone, Default)]
struct Accumulator {
    count: i128,
    integers: i128,
    decimals: i128,
    any_decimal: bool,
    extreme: Option<Value>,
}

impl Accumulator {
    fn add(&mut self, func: AggFunc, value: Option<&Value>) -> Result<()> {
        if matches!(value, Some(Value::Null)) {
            return Ok(());
        }
        if func == AggFunc::Count {
            self.count += 1;
            return Ok(());
        }
        let value = value.expect("non-COUNT aggregates require a column");
        if matches!(func, AggFunc::Min | AggFunc::Max) {
            let replace = self.extreme.as_ref().is_none_or(|old| {
                if func == AggFunc::Min {
                    numeric_cmp(value, old).is_lt()
                } else {
                    numeric_cmp(value, old).is_gt()
                }
            });
            if replace {
                self.extreme = Some(value.clone());
            }
            return Ok(());
        }
        let label = if func == AggFunc::Sum { "SUM" } else { "AVG" };
        match value {
            Value::Int(n) => {
                self.integers = self
                    .integers
                    .checked_add(*n as i128)
                    .ok_or_else(|| PvError::Schema(format!("{label} overflowed")))?
            }
            Value::Decimal(n) => {
                self.decimals = self
                    .decimals
                    .checked_add(*n)
                    .ok_or_else(|| PvError::Schema(format!("{label} overflowed")))?;
                self.any_decimal = true;
            }
            _ => {
                return Err(PvError::Schema(format!(
                    "{label} requires numeric values, found {value:?}"
                )))
            }
        }
        self.count += 1;
        Ok(())
    }

    fn finish(&self, func: AggFunc) -> Result<Value> {
        Ok(match func {
            AggFunc::Count => Value::Int(self.count as i64),
            AggFunc::Min | AggFunc::Max => self.extreme.clone().unwrap_or(Value::Null),
            _ if self.count == 0 => Value::Null,
            AggFunc::Avg => Value::Decimal(round_div_half_away(
                combine_mantissa(self.integers, self.decimals)?,
                self.count,
            )),
            AggFunc::Sum if self.any_decimal => {
                Value::Decimal(combine_mantissa(self.integers, self.decimals)?)
            }
            AggFunc::Sum => Value::Int(
                i64::try_from(self.integers)
                    .map_err(|_| PvError::Schema("SUM overflowed i64".into()))?,
            ),
        })
    }
}

fn charge_group(budget: &mut Option<&mut QueryBudget>, key: &Row, states: usize) -> Result<()> {
    if let Some(budget) = budget.as_deref_mut() {
        budget.materialize(key)?;
        budget.materialized_bytes = budget
            .materialized_bytes
            .saturating_add(states.saturating_mul(std::mem::size_of::<Accumulator>()));
        if budget.materialized_bytes > budget.limits.max_materialized_bytes {
            return Err(PvError::ResourceLimit(
                "aggregate groups exceed materialization budget".into(),
            ));
        }
        budget.checkpoint()?;
    }
    Ok(())
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn select_streaming_aggregate(
        &self,
        table_name: &str,
        before: Option<u64>,
        items: Vec<SelectItem>,
        group_by: Vec<String>,
        order: Vec<OrderBy>,
        distinct: bool,
        limit: Option<usize>,
        offset: usize,
        mut budget: Option<&mut QueryBudget>,
    ) -> Result<QueryResult> {
        let table = self
            .tables
            .get(table_name)
            .ok_or_else(|| PvError::TableNotFound(table_name.into()))?;
        let columns = &table.columns;
        // Reuse existing validation/labels so optimized and general paths have
        // identical grouping, projection and ordering rules even for empty input.
        let QueryResult::Rows {
            columns: out_columns,
            ..
        } = project_grouped(
            columns.clone(),
            Vec::new(),
            items.clone(),
            group_by.clone(),
            None,
            order.clone(),
            distinct,
            limit,
            offset,
        )?
        else {
            unreachable!()
        };
        let group_indexes = group_by
            .iter()
            .map(|name| projection_col_pos(columns, name))
            .collect::<Result<Vec<_>>>()?;
        let indexes = items
            .iter()
            .map(|item| match &item.expr {
                SelectExpr::Column(name) => Ok(Some(projection_col_pos(columns, name)?)),
                SelectExpr::Aggregate(agg) => agg
                    .column
                    .as_ref()
                    .map(|name| projection_col_pos(columns, name))
                    .transpose(),
                _ => unreachable!("validated projection"),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut groups: BTreeMap<Row, Vec<Accumulator>> = BTreeMap::new();
        if group_by.is_empty() {
            charge_group(&mut budget, &Vec::new(), items.len())?;
            groups.insert(Vec::new(), vec![Accumulator::default(); items.len()]);
        }
        let snapshot = Snapshot::as_of(before.unwrap_or_else(|| self.txm.current()));
        scan(
            &mut self.cache.borrow_mut(),
            table,
            &self.cas,
            |_, env, row| {
                if let Some(budget) = budget.as_deref_mut() {
                    budget.scan_row()?;
                }
                if !snapshot.sees(env) {
                    return Ok(());
                }
                let key: Row = group_indexes.iter().map(|&i| row[i].clone()).collect();
                if !groups.contains_key(&key) {
                    charge_group(&mut budget, &key, items.len())?;
                    groups.insert(key.clone(), vec![Accumulator::default(); items.len()]);
                }
                let states = groups.get_mut(&key).expect("group inserted");
                for (i, item) in items.iter().enumerate() {
                    if let SelectExpr::Aggregate(agg) = &item.expr {
                        // Conservatively charge nonnumeric MIN/MAX replacements;
                        // integer/decimal state stays fixed regardless of row count.
                        let value = indexes[i].map(|ix| &row[ix]);
                        if matches!(agg.func, AggFunc::Min | AggFunc::Max) {
                            if let (Some(budget), Some(v @ (Value::Text(_) | Value::Blob(_)))) =
                                (budget.as_deref_mut(), value)
                            {
                                budget.materialize_value(v, "aggregate extreme")?;
                            }
                        }
                        states[i].add(agg.func, value)?;
                    }
                }
                Ok(())
            },
        )?;
        let mut rows = Vec::with_capacity(groups.len());
        for (key, states) in groups {
            let mut row = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                row.push(match &item.expr {
                    SelectExpr::Column(_) => key[group_indexes
                        .iter()
                        .position(|ix| Some(*ix) == indexes[i])
                        .expect("validated group column")]
                    .clone(),
                    SelectExpr::Aggregate(agg) => states[i].finish(agg.func)?,
                    _ => unreachable!("validated projection"),
                });
            }
            if let Some(budget) = budget.as_deref_mut() {
                budget.materialize(&row)?;
            }
            rows.push(row);
        }
        sort_rows(&mut rows, &out_columns, &order)?;
        if distinct {
            dedup_rows(&mut rows);
        }
        if offset > 0 {
            rows = rows.into_iter().skip(offset).collect();
        }
        if let Some(n) = limit {
            rows.truncate(n);
        }
        Ok(QueryResult::Rows {
            columns: out_columns,
            rows,
        })
    }
}
