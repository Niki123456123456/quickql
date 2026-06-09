use crate::*;
use anyhow::{bail, Context, Result};
use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "ql.pest"]
struct QlParser;

pub fn parse_query(input: &str) -> Result<Query> {
    let parsed =
        QlParser::parse(Rule::query, input).with_context(|| "Parsing QuickQL query".to_string())?;
    let steps = parsed
        .flatten()
        .filter(|pair| pair.as_rule() == Rule::statement)
        .map(parse_statement)
        .collect::<Result<Vec<_>>>()?;
    Ok(Query { steps })
}

fn parse_statement(pair: Pair<'_, Rule>) -> Result<SubQuery> {
    let statement = pair
        .into_inner()
        .next()
        .context("Missing query statement")?;
    match statement.as_rule() {
        Rule::source_stmt => Ok(SubQuery::Source(parse_source_stmt(statement)?)),
        Rule::map_stmt => Ok(SubQuery::Map(parse_map_stmt(statement)?)),
        Rule::filter_stmt => Ok(SubQuery::Filter(parse_filter_stmt(statement)?)),
        Rule::map_many_stmt => Ok(SubQuery::MapMany(parse_map_many_stmt(statement)?)),
        Rule::group_by_stmt => parse_group_by_stmt(statement),
        Rule::sort_by_stmt => parse_sort_by_stmt(statement),
        rule => bail!("Unsupported query statement: {rule:?}"),
    }
}

fn parse_source_stmt(pair: Pair<'_, Rule>) -> Result<Vec<CaluculatedValue>> {
    let sources = collect_value_list(pair)?;
    if sources.is_empty() {
        bail!("SOURCE must include at least one source");
    }
    Ok(sources)
}

fn parse_map_stmt(pair: Pair<'_, Rule>) -> Result<Vec<MapExpr>> {
    let list = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::map_item_list)
        .context("MAP requires items")?;
    collect_map_items(list)
}

fn parse_filter_stmt(pair: Pair<'_, Rule>) -> Result<CaluculatedValue> {
    let values = collect_value_list(pair)?;
    if values.is_empty() {
        bail!("FILTER must include at least one expression");
    }
    if values.len() == 1 {
        Ok(values.into_iter().next().unwrap())
    } else {
        Ok(CaluculatedValue::FunctionCall {
            function: "OR".to_string(),
            parameters: values,
        })
    }
}

fn parse_map_many_stmt(pair: Pair<'_, Rule>) -> Result<String> {
    let field = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::identifier)
        .context("MAP_MANY requires a column name")?
        .as_str()
        .to_string();
    if field.is_empty() {
        bail!("MAP_MANY requires a column name");
    }
    Ok(field)
}

fn parse_group_by_stmt(pair: Pair<'_, Rule>) -> Result<SubQuery> {
    let mut children = pair.into_inner();

    let key_list = children.next().context("GROUP_BY: missing key list")?;
    let keys: Vec<String> = key_list
        .into_inner()
        .filter(|p| p.as_rule() == Rule::identifier)
        .map(|p| p.as_str().to_string())
        .collect();

    if keys.is_empty() {
        bail!("GROUP_BY requires at least one key column");
    }

    let mapping = match children.find(|p| p.as_rule() == Rule::map_item_list) {
        Some(list) => collect_map_items(list)?,
        None => Vec::new(),
    };

    Ok(SubQuery::GroupBy { keys, mapping })
}

fn parse_sort_by_stmt(pair: Pair<'_, Rule>) -> Result<SubQuery> {
    let key_list = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::sort_key_list)
        .context("SORT_BY requires at least one column")?;

    let keys = key_list
        .into_inner()
        .filter(|p| p.as_rule() == Rule::sort_key)
        .map(parse_sort_key)
        .collect::<Result<Vec<_>>>()?;

    if keys.is_empty() {
        bail!("SORT_BY requires at least one column");
    }

    Ok(SubQuery::SortBy(keys))
}

fn parse_sort_key(pair: Pair<'_, Rule>) -> Result<SortKey> {
    let mut children = pair.into_inner();
    let column = children
        .next()
        .context("SORT_BY: missing column name")?
        .as_str()
        .to_string();
    let direction = match children
        .next()
        .map(|p| p.as_str().to_ascii_uppercase())
        .as_deref()
    {
        Some(s) if s.starts_with("DESC") => SortDirection::Desc,
        _ => SortDirection::Asc,
    };
    Ok(SortKey { column, direction })
}

fn collect_value_list(pair: Pair<'_, Rule>) -> Result<Vec<CaluculatedValue>> {
    let list = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::value_list)
        .context("Missing value list")?;
    list.into_inner()
        .filter(|p| p.as_rule() == Rule::value)
        .map(parse_value)
        .collect()
}

fn collect_map_items(pair: Pair<'_, Rule>) -> Result<Vec<MapExpr>> {
    pair.into_inner()
        .filter(|p| p.as_rule() == Rule::map_item)
        .map(parse_map_item)
        .collect()
}

fn parse_map_item(pair: Pair<'_, Rule>) -> Result<MapExpr> {
    let inner = pair.into_inner().next().context("Empty map item")?;
    match inner.as_rule() {
        Rule::all_columns => Ok(MapExpr::All),
        Rule::assignment => {
            let (col, val) = parse_assignment(inner)?;
            Ok(MapExpr::Specific {
                column: path_parts(&col),
                value: val,
            })
        }
        Rule::value => match parse_value(inner)? {
            CaluculatedValue::Reference(parts) => Ok(MapExpr::Specific {
                column: parts.clone(),
                value: CaluculatedValue::Reference(parts),
            }),
            _ => bail!("MAP item without assignment must be an identifier"),
        },
        rule => bail!("Invalid map item: {rule:?}"),
    }
}

fn parse_assignment(pair: Pair<'_, Rule>) -> Result<(String, CaluculatedValue)> {
    let mut children = pair.into_inner();
    let col = children
        .next()
        .context("Assignment: missing column")?
        .as_str()
        .to_string();
    let val_pair = children.next().context("Assignment: missing value")?;
    Ok((col, parse_value(val_pair)?))
}

fn parse_value(pair: Pair<'_, Rule>) -> Result<CaluculatedValue> {
    let inner = pair.into_inner().next().context("Empty value")?;
    match inner.as_rule() {
        Rule::function_call => parse_function_call(inner),
        Rule::json_object => parse_json_object(inner),
        Rule::json_array => parse_json_array(inner),
        Rule::single_quoted_string | Rule::double_quoted_string => Ok(CaluculatedValue::Static(
            Value::String(pair_text(inner).to_string()),
        )),
        Rule::bool_literal => {
            let b = inner.as_str().to_ascii_lowercase() == "true";
            Ok(CaluculatedValue::Static(Value::Bool(b)))
        }
        Rule::number => parse_number(inner.as_str()),
        Rule::identifier => Ok(CaluculatedValue::Reference(path_parts(inner.as_str()))),
        rule => bail!("Unsupported value type: {rule:?}"),
    }
}

fn parse_function_call(pair: Pair<'_, Rule>) -> Result<CaluculatedValue> {
    let mut children = pair.into_inner();
    let name = children
        .next()
        .context("Function call: missing name")?
        .as_str()
        .to_string();
    let parameters = match children.next() {
        Some(args) if args.as_rule() == Rule::function_args => args
            .into_inner()
            .filter(|p| p.as_rule() == Rule::value)
            .map(parse_value)
            .collect::<Result<Vec<_>>>()?,
        _ => Vec::new(),
    };
    Ok(CaluculatedValue::FunctionCall {
        function: name,
        parameters,
    })
}

fn parse_json_object(pair: Pair<'_, Rule>) -> Result<CaluculatedValue> {
    let mut entries = Vec::new();
    for kv in pair.into_inner().filter(|p| p.as_rule() == Rule::json_kv) {
        let mut kv_children = kv.into_inner();
        let key = parse_json_key(kv_children.next().context("JSON object: missing key")?)?;
        let val_pair = kv_children.next().context("JSON object: missing value")?;
        entries.push((key, parse_value(val_pair)?));
    }
    Ok(CaluculatedValue::Object(entries))
}

fn parse_json_key(pair: Pair<'_, Rule>) -> Result<String> {
    let inner = pair.into_inner().next().context("JSON key is empty")?;
    match inner.as_rule() {
        Rule::ident => Ok(inner.as_str().to_string()),
        Rule::single_quoted_string | Rule::double_quoted_string => Ok(pair_text(inner).to_string()),
        rule => bail!("Invalid JSON key type: {rule:?}"),
    }
}

fn parse_json_array(pair: Pair<'_, Rule>) -> Result<CaluculatedValue> {
    let values = pair
        .into_inner()
        .filter(|p| p.as_rule() == Rule::value)
        .map(parse_value)
        .collect::<Result<Vec<_>>>()?;
    Ok(CaluculatedValue::Array(values))
}

fn parse_number(s: &str) -> Result<CaluculatedValue> {
    let v = if s.contains('.') {
        let f: f64 = s.parse().with_context(|| format!("Invalid float: {s}"))?;
        serde_json::json!(f)
    } else if let Ok(i) = s.parse::<i64>() {
        serde_json::json!(i)
    } else {
        let f: f64 = s.parse().with_context(|| format!("Invalid number: {s}"))?;
        serde_json::json!(f)
    };
    Ok(CaluculatedValue::Static(v))
}

fn path_parts(input: &str) -> Vec<String> {
    input
        .split('.')
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn pair_text(pair: Pair<'_, Rule>) -> &str {
    match pair.as_rule() {
        Rule::single_quoted_string | Rule::double_quoted_string => {
            let value = pair.as_str();
            &value[1..value.len() - 1]
        }
        _ => pair.as_str(),
    }
}

#[derive(Default)]
pub(crate) struct PartialQuery {
    pub(crate) sources: Vec<String>,
}

pub(crate) fn parse_query_lenient(input: &str) -> Result<PartialQuery> {
    let mut partial = PartialQuery::default();
    for raw_line in input.lines() {
        if let Ok(sources) = parse_sources(raw_line) {
            partial.sources = sources
                .into_iter()
                .filter_map(|source| match source {
                    CaluculatedValue::Static(Value::String(s)) => Some(s),
                    _ => None,
                })
                .collect();
        }
    }
    Ok(partial)
}

pub(crate) fn parse_sources(line: &str) -> Result<Vec<CaluculatedValue>> {
    let source_stmt = QlParser::parse(Rule::source_only, line)
        .with_context(|| "Parsing SOURCE clause".to_string())?
        .flatten()
        .find(|pair| pair.as_rule() == Rule::source_stmt)
        .context("SOURCE must include at least one source")?;
    parse_source_stmt(source_stmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_map_star_as_all_columns() {
        let query = parse_query("MAP *").unwrap();

        assert_eq!(
            query,
            Query {
                steps: vec![SubQuery::Map(vec![MapExpr::All])]
            }
        );
    }

    #[test]
    fn parses_function_call_inside_json_object() {
        let query = parse_query("MAP payload = { token: BASE64(name) }").unwrap();

        assert_eq!(
            query,
            Query {
                steps: vec![SubQuery::Map(vec![MapExpr::Specific {
                    column: vec!["payload".to_string()],
                    value: CaluculatedValue::Object(vec![(
                        "token".to_string(),
                        CaluculatedValue::FunctionCall {
                            function: "BASE64".to_string(),
                            parameters: vec![CaluculatedValue::Reference(vec!["name".to_string()])],
                        },
                    )]),
                }])]
            }
        );
    }
}
