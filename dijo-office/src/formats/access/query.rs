//! Jackcess's `QueryImpl` and its subclasses: a query's SQL as `toSQLString` writes it from the
//! query's rows of `MSysQueries`.

use super::buffer::{Context, Thrown};
use super::database::{QueryDefinition, QueryRow};
use crate::java::{equals_ignore_case, LINE_SEPARATOR as NEWLINE};

const OBJECT_FLAG_MASK: i32 = 0xF0;
const TYPE_ATTRIBUTE: i8 = 1;
const PARAMETER_ATTRIBUTE: i8 = 2;
const FLAG_ATTRIBUTE: i8 = 3;
const REMOTEDB_ATTRIBUTE: i8 = 4;
const TABLE_ATTRIBUTE: i8 = 5;
const COLUMN_ATTRIBUTE: i8 = 6;
const JOIN_ATTRIBUTE: i8 = 7;
const WHERE_ATTRIBUTE: i8 = 8;
const GROUPBY_ATTRIBUTE: i8 = 9;
const HAVING_ATTRIBUTE: i8 = 10;
const ORDERBY_ATTRIBUTE: i8 = 11;
const UNION_FLAG: i16 = 0x02;
const TEXT_FLAG: i16 = 0x0A;
const DESCENDING_FLAG: &str = "D";
const SELECT_STAR_SELECT_TYPE: i16 = 0x01;
const DISTINCT_SELECT_TYPE: i16 = 0x02;
const OWNER_ACCESS_SELECT_TYPE: i16 = 0x04;
const DISTINCT_ROW_SELECT_TYPE: i16 = 0x08;
const TOP_SELECT_TYPE: i16 = 0x10;
const PERCENT_SELECT_TYPE: i16 = 0x20;
const APPEND_VALUE_FLAG: i16 = 0x8000u16 as i16;
const CROSSTAB_PIVOT_FLAG: i16 = 0x01;
const CROSSTAB_NORMAL_FLAG: i16 = 0x02;
const UNION_PART1: &str = "X7YZ_____1";
const UNION_PART2: &str = "X7YZ_____2";
const DEFAULT_SEPARATOR: &str = ", ";

/// `Query.Type`: a query's type by its value in a type row and its object flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Type {
    Select,
    MakeTable,
    Append,
    Update,
    Delete,
    CrossTab,
    DataDefinition,
    Passthrough,
    Union,
}

const TYPES: [(Type, i32, i16); 9] = [
    (Type::Select, 0, 1),
    (Type::MakeTable, 80, 2),
    (Type::Append, 64, 3),
    (Type::Update, 48, 4),
    (Type::Delete, 32, 5),
    (Type::CrossTab, 16, 6),
    (Type::DataDefinition, 96, 7),
    (Type::Passthrough, 112, 8),
    (Type::Union, 128, 9),
];

/// What `toSQLString` gives: the SQL, or nothing for a query of no type Jackcess knows, whose
/// `toSQLString` throws `UnsupportedOperationException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Sql {
    Text(String),
    Unsupported,
}

/// An `IllegalStateException` `QueryImpl` throws with the name of the query it is about.
fn illegal_state(query: &str, message: impl Into<String>) -> Thrown {
    Thrown::illegal_state(message).in_context(Context::Query(query.to_owned()))
}

/// `QueryImpl.create` and `toSQLString`.
pub(super) fn sql(definition: &QueryDefinition) -> Result<Sql, Thrown> {
    let query = Query {
        name: &definition.name,
        rows: &definition.rows,
        kind: kind(definition),
    };
    query.to_sql()
}

/// `QueryImpl.create`'s type: by the object flag, or by the type row where the flag is a
/// select's; `None` where the constructor of the type throws, for `UnknownQueryImpl`.
fn kind(definition: &QueryDefinition) -> Option<Type> {
    let rows = &definition.rows;
    let type_value = |rows: &[QueryRow]| {
        rows.iter()
            .find(|row| row.attribute == Some(TYPE_ATTRIBUTE))
            .and_then(|row| row.flag)
    };
    let mut object_flag = definition.flags & OBJECT_FLAG_MASK;
    if object_flag == 0 {
        let row_type = type_value(rows).unwrap_or(object_flag as i16);
        if let Some(&(_, flag, _)) = TYPES.iter().find(|(_, _, value)| *value == row_type) {
            object_flag = flag;
        }
    }
    let &(kind, _, value) = TYPES.iter().find(|(_, flag, _)| *flag == object_flag)?;
    (type_value(rows).unwrap_or(value) == value).then_some(kind)
}

/// A query's rows and its type.
struct Query<'r> {
    name: &'r str,
    rows: &'r [QueryRow],
    kind: Option<Type>,
}

/// A source of a query's `FROM` clause: a table or a join of two.
enum TableSource {
    Simple {
        name: Option<String>,
        expression: String,
    },
    Join {
        from: Box<TableSource>,
        to: Box<TableSource>,
        join_type: i16,
        on: Vec<Option<String>>,
    },
}

impl TableSource {
    /// `new SimpleTable(tableName)`.
    fn named(name: Option<&str>) -> Result<Self, Thrown> {
        let name = name.ok_or_else(Thrown::null_pointer)?;
        Ok(TableSource::Simple {
            name: Some(name.to_owned()),
            expression: optional_quoted(name, true),
        })
    }

    /// `containsTable`.
    fn contains(&self, table: Option<&str>) -> Result<bool, Thrown> {
        match self {
            TableSource::Simple { name, .. } => {
                let name = name.as_deref().ok_or_else(Thrown::null_pointer)?;
                Ok(table.is_some_and(|table| equals_ignore_case(name, table)))
            }
            TableSource::Join { from, to, .. } => Ok(from.contains(table)? || to.contains(table)?),
        }
    }

    fn write(&self, text: &mut String, top_level: bool, query: &str) -> Result<(), Thrown> {
        match self {
            TableSource::Simple { expression, .. } => text.push_str(expression),
            TableSource::Join {
                from,
                to,
                join_type,
                on,
            } => {
                let join = match join_type {
                    1 => " INNER JOIN ",
                    2 => " LEFT JOIN ",
                    3 => " RIGHT JOIN ",
                    other => {
                        return Err(illegal_state(query, format!("Unknown join type {other}")))
                    }
                };
                if !top_level {
                    text.push('(');
                }
                from.write(text, false, query)?;
                text.push_str(join);
                to.write(text, false, query)?;
                text.push_str(" ON ");
                let multiple = on.len() > 1;
                if multiple {
                    text.push('(');
                }
                let on = on
                    .iter()
                    .map(|on| on.as_deref().ok_or_else(Thrown::null_pointer))
                    .collect::<Result<Vec<_>, _>>()?;
                text.push_str(&on.join(") AND ("));
                if multiple {
                    text.push(')');
                }
                if !top_level {
                    text.push(')');
                }
            }
        }
        Ok(())
    }
}

/// `StringBuilder.append(String)`: a null as `null`.
fn or_null(text: &Option<String>) -> &str {
    text.as_deref().unwrap_or("null")
}

/// `toOptionalQuotedExpr`: an identifier's parts, or an expression, in brackets where they hold
/// a character no word does.
fn optional_quoted(expression: &str, identifier: bool) -> String {
    let parts = if identifier {
        crate::java::split(expression, '.')
    } else {
        vec![expression]
    };
    parts
        .iter()
        .map(|part| {
            if part
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
            {
                quoted(part)
            } else {
                (*part).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// `toOptionalQuotedExpr` of a value Java may hand it as null, which throws.
fn optional_quoted_value(expression: &Option<String>, identifier: bool) -> Result<String, Thrown> {
    expression
        .as_deref()
        .map(|expression| optional_quoted(expression, identifier))
        .ok_or_else(Thrown::null_pointer)
}

/// `toQuotedExpr`: in brackets unless it is already.
fn quoted(expression: &str) -> String {
    if expression.len() >= 2 && expression.starts_with('[') && expression.ends_with(']') {
        expression.to_owned()
    } else {
        format!("[{expression}]")
    }
}

/// `toAlias`.
fn alias(text: &mut String, alias: &Option<String>) {
    if let Some(alias) = alias {
        text.push_str(" AS ");
        text.push_str(&optional_quoted(alias, false));
    }
}

/// `toRemoteDb`.
fn remote_db(text: &mut String, path: &Option<String>, db_type: &Option<String>) {
    if path.is_none() && db_type.is_none() {
        return;
    }
    text.push_str(" IN '");
    if let Some(path) = path {
        text.push_str(path);
    }
    text.push('\'');
    if let Some(db_type) = db_type {
        text.push_str(" [");
        text.push_str(db_type);
        text.push(']');
    }
}

/// `hasFlag(row, flagMask)`.
fn row_has_flag(row: &QueryRow, mask: i16) -> bool {
    i32::from(row.flag.unwrap_or(0)) & i32::from(mask) != 0
}

impl<'r> Query<'r> {
    fn rows_by(&self, attribute: i8) -> Vec<&'r QueryRow> {
        self.rows
            .iter()
            .filter(|row| row.attribute == Some(attribute))
            .collect()
    }

    /// `getUniqueRow`: the one row, an empty one for none, and a throw for more.
    fn unique(&self, rows: Vec<&'r QueryRow>) -> Result<QueryRow, Thrown> {
        match rows.as_slice() {
            [] => Ok(QueryRow::default()),
            [row] => Ok((*row).clone()),
            _ => Err(illegal_state(self.name, "Unexpected number of rows")),
        }
    }

    fn row_by(&self, attribute: i8) -> Result<QueryRow, Thrown> {
        self.unique(self.rows_by(attribute))
    }

    fn type_row(&self) -> Result<QueryRow, Thrown> {
        self.row_by(TYPE_ATTRIBUTE)
    }

    fn has_flag(&self, mask: i16) -> Result<bool, Thrown> {
        Ok(row_has_flag(&self.row_by(FLAG_ATTRIBUTE)?, mask))
    }

    /// `getColumnRows`, as each type overrides it.
    fn column_rows(&self) -> Vec<&'r QueryRow> {
        let rows = self.rows_by(COLUMN_ATTRIBUTE);
        match self.kind {
            Some(Type::Append) => rows
                .into_iter()
                .filter(|row| !row_has_flag(row, APPEND_VALUE_FLAG))
                .collect(),
            Some(Type::CrossTab) => rows
                .into_iter()
                .filter(|row| row_has_flag(row, CROSSTAB_NORMAL_FLAG))
                .collect(),
            _ => rows,
        }
    }

    /// `getGroupByRows`, as a cross tab query overrides it.
    fn group_by_rows(&self) -> Vec<&'r QueryRow> {
        let rows = self.rows_by(GROUPBY_ATTRIBUTE);
        match self.kind {
            Some(Type::CrossTab) => rows
                .into_iter()
                .filter(|row| row_has_flag(row, CROSSTAB_NORMAL_FLAG))
                .collect(),
            _ => rows,
        }
    }

    /// `toSQLString`.
    fn to_sql(&self) -> Result<Sql, Thrown> {
        let standard = !matches!(self.kind, Some(Type::DataDefinition | Type::Passthrough));
        let mut text = String::new();
        if standard {
            let parameters = self.parameters()?;
            if !parameters.is_empty() {
                text.push_str("PARAMETERS ");
                text.push_str(&parameters.join(DEFAULT_SEPARATOR));
                text.push(';');
                text.push_str(NEWLINE);
            }
        }
        let Some(kind) = self.kind else {
            return Ok(Sql::Unsupported);
        };
        self.write(kind, &mut text)?;
        if standard {
            if self.has_flag(OWNER_ACCESS_SELECT_TYPE)? {
                text.push_str(NEWLINE);
                text.push_str("WITH OWNERACCESS OPTION");
            }
            text.push(';');
        }
        Ok(Sql::Text(text))
    }

    /// `getParameters`.
    fn parameters(&self) -> Result<Vec<String>, Thrown> {
        self.rows_by(PARAMETER_ATTRIBUTE)
            .into_iter()
            .map(|row| {
                let type_name = match row.flag {
                    Some(0) => "Value",
                    Some(1) => "Bit",
                    Some(10) => "Text",
                    Some(2) => "Byte",
                    Some(3) => "Short",
                    Some(4) => "Long",
                    Some(5) => "Currency",
                    Some(6) => "IEEESingle",
                    Some(7) => "IEEEDouble",
                    Some(8) => "DateTime",
                    Some(9) => "Binary",
                    Some(11) => "LongBinary",
                    Some(15) => "Guid",
                    other => {
                        return Err(illegal_state(
                            self.name,
                            format!(
                                "Unknown param type {}",
                                other.map_or("null".to_owned(), |flag| flag.to_string())
                            ),
                        ))
                    }
                };
                let mut text = format!("{} {type_name}", or_null(&row.name1));
                if row.flag == Some(TEXT_FLAG) && row.extra.unwrap_or(0) > 0 {
                    text.push_str(&format!("({})", row.extra.unwrap_or(0)));
                }
                Ok(text)
            })
            .collect()
    }

    fn write(&self, kind: Type, text: &mut String) -> Result<(), Thrown> {
        match kind {
            Type::Select | Type::MakeTable => self.select(text, true),
            Type::Delete => {
                text.push_str("DELETE ");
                self.select(text, false)
            }
            Type::Append => self.append(text),
            Type::CrossTab => self.cross_tab(text),
            Type::Update => self.update(text),
            Type::Union => self.union(text),
            Type::DataDefinition | Type::Passthrough => {
                if let Some(expression) = self.type_row()?.expression {
                    text.push_str(&expression);
                }
                Ok(())
            }
        }
    }

    /// `toSQLSelectString`.
    fn select(&self, text: &mut String, prefix: bool) -> Result<(), Thrown> {
        if prefix {
            text.push_str("SELECT ");
            let select_type = self.select_type()?;
            if !select_type.is_empty() {
                text.push_str(&select_type);
                text.push(' ');
            }
        }
        let mut columns: Vec<String> = self
            .column_rows()
            .into_iter()
            .map(|row| {
                let mut column = or_null(&row.expression).to_owned();
                alias(&mut column, &row.name1);
                column
            })
            .collect();
        if self.has_flag(SELECT_STAR_SELECT_TYPE)? {
            columns.push("*".to_owned());
        }
        text.push_str(&columns.join(DEFAULT_SEPARATOR));
        if self.kind == Some(Type::MakeTable) {
            let type_row = self.type_row()?;
            text.push_str(" INTO ");
            text.push_str(&optional_quoted_value(&type_row.name1, true)?);
            remote_db(text, &type_row.name2, &type_row.expression);
        }
        let from = self.table_sources()?;
        if !from.is_empty() {
            text.push_str(NEWLINE);
            text.push_str("FROM ");
            text.push_str(&from.join(DEFAULT_SEPARATOR));
            let remote = self.row_by(REMOTEDB_ATTRIBUTE)?;
            remote_db(text, &remote.name1, &remote.expression);
        }
        if let Some(condition) = self.row_by(WHERE_ATTRIBUTE)?.expression {
            text.push_str(NEWLINE);
            text.push_str("WHERE ");
            text.push_str(&condition);
        }
        let groupings: Vec<&str> = self
            .group_by_rows()
            .into_iter()
            .map(|row| or_null(&row.expression))
            .collect();
        if !groupings.is_empty() {
            text.push_str(NEWLINE);
            text.push_str("GROUP BY ");
            text.push_str(&groupings.join(DEFAULT_SEPARATOR));
        }
        if let Some(condition) = self.row_by(HAVING_ATTRIBUTE)?.expression {
            text.push_str(NEWLINE);
            text.push_str("HAVING ");
            text.push_str(&condition);
        }
        self.order_by(text);
        Ok(())
    }

    /// `getOrderings` after `ORDER BY`, where there are any.
    fn order_by(&self, text: &mut String) {
        let orderings: Vec<String> = self
            .rows_by(ORDERBY_ATTRIBUTE)
            .into_iter()
            .map(|row| {
                let mut ordering = or_null(&row.expression).to_owned();
                if row
                    .name1
                    .as_deref()
                    .is_some_and(|name| equals_ignore_case(DESCENDING_FLAG, name))
                {
                    ordering.push_str(" DESC");
                }
                ordering
            })
            .collect();
        if !orderings.is_empty() {
            text.push_str(NEWLINE);
            text.push_str("ORDER BY ");
            text.push_str(&orderings.join(DEFAULT_SEPARATOR));
        }
    }

    /// `getSelectType`.
    fn select_type(&self) -> Result<String, Thrown> {
        if self.has_flag(DISTINCT_SELECT_TYPE)? {
            return Ok("DISTINCT".to_owned());
        }
        if self.has_flag(DISTINCT_ROW_SELECT_TYPE)? {
            return Ok("DISTINCTROW".to_owned());
        }
        if self.has_flag(TOP_SELECT_TYPE)? {
            let mut top = format!("TOP {}", or_null(&self.row_by(FLAG_ATTRIBUTE)?.name1));
            if self.has_flag(PERCENT_SELECT_TYPE)? {
                top.push_str(" PERCENT");
            }
            return Ok(top);
        }
        Ok(String::new())
    }

    /// `getFromTables`: the tables, joined as the join rows say.
    fn table_sources(&self) -> Result<Vec<String>, Thrown> {
        let mut sources = Vec::new();
        for table in self.rows_by(TABLE_ATTRIBUTE) {
            let mut expression = String::new();
            if let Some(database) = &table.expression {
                expression.push_str(&quoted(database));
                expression.push('.');
            }
            if let Some(name) = &table.name1 {
                expression.push_str(&optional_quoted(name, true));
            }
            alias(&mut expression, &table.name2);
            sources.push(TableSource::Simple {
                name: table.name2.clone().or_else(|| table.name1.clone()),
                expression,
            });
        }
        for join in self.rows_by(JOIN_ATTRIBUTE) {
            let (from_name, to_name) = (join.name1.as_deref(), join.name2.as_deref());
            let mut from: Option<TableSource> = None;
            let mut to: Option<TableSource> = None;
            let mut shared = None;
            let mut i = 0;
            while i < sources.len() && (from.is_none() || to.is_none()) {
                if from.is_none() && sources[i].contains(from_name)? {
                    if to.is_none() && sources[i].contains(to_name)? {
                        shared = Some(i);
                        break;
                    }
                    from = Some(sources.remove(i));
                } else if to.is_none() && sources[i].contains(to_name)? {
                    to = Some(sources.remove(i));
                } else {
                    i += 1;
                }
            }
            let join_type = |join: &QueryRow| join.flag.ok_or_else(Thrown::null_pointer);
            if let Some(index) = shared {
                let join_type = join_type(join)?;
                match &mut sources[index] {
                    TableSource::Join {
                        join_type: existing,
                        on,
                        ..
                    } if *existing == join_type => {
                        on.insert(0, join.expression.clone());
                        continue;
                    }
                    _ => {
                        return Err(illegal_state(
                            self.name,
                            format!(
                                "Inconsistent join types for {} and {}",
                                or_null(&join.name1),
                                or_null(&join.name2)
                            ),
                        ))
                    }
                }
            }
            let from = match from {
                Some(source) => source,
                None => TableSource::named(from_name)?,
            };
            let to = match to {
                Some(source) => source,
                None => TableSource::named(to_name)?,
            };
            let join_type = join_type(join)?;
            sources.push(TableSource::Join {
                from: Box::new(from),
                to: Box::new(to),
                join_type,
                on: vec![join.expression.clone()],
            });
        }
        sources
            .iter()
            .map(|source| {
                let mut text = String::new();
                source.write(&mut text, true, self.name)?;
                Ok(text)
            })
            .collect()
    }

    /// `AppendQueryImpl.toSQLString`.
    fn append(&self, text: &mut String) -> Result<(), Thrown> {
        let type_row = self.type_row()?;
        text.push_str("INSERT INTO ");
        text.push_str(&optional_quoted_value(&type_row.name1, true)?);
        let all_columns = self.rows_by(COLUMN_ATTRIBUTE);
        let targets = all_columns
            .iter()
            .filter(|row| row.name2.is_some())
            .map(|row| optional_quoted_value(&row.name2, true))
            .collect::<Result<Vec<_>, _>>()?;
        if !targets.is_empty() {
            text.push_str(" (");
            text.push_str(&targets.join(DEFAULT_SEPARATOR));
            text.push(')');
        }
        remote_db(text, &type_row.name2, &type_row.expression);
        text.push_str(NEWLINE);
        let values: Vec<&str> = all_columns
            .iter()
            .filter(|row| row_has_flag(row, APPEND_VALUE_FLAG))
            .map(|row| or_null(&row.expression))
            .collect();
        if values.is_empty() {
            return self.select(text, true);
        }
        text.push_str("VALUES (");
        text.push_str(&values.join(DEFAULT_SEPARATOR));
        text.push(')');
        Ok(())
    }

    /// `CrossTabQueryImpl.toSQLString`.
    fn cross_tab(&self, text: &mut String) -> Result<(), Thrown> {
        let all_columns = self.rows_by(COLUMN_ATTRIBUTE);
        let transform = self.unique(
            all_columns
                .iter()
                .copied()
                .filter(|row| !row_has_flag(row, CROSSTAB_PIVOT_FLAG | CROSSTAB_NORMAL_FLAG))
                .collect(),
        )?;
        if let Some(expression) = &transform.expression {
            let mut transform_text = expression.clone();
            alias(&mut transform_text, &transform.name1);
            text.push_str("TRANSFORM ");
            text.push_str(&transform_text);
            text.push_str(NEWLINE);
        }
        self.select(text, true)?;
        let pivot = self.unique(
            all_columns
                .into_iter()
                .filter(|row| row_has_flag(row, CROSSTAB_PIVOT_FLAG))
                .collect(),
        )?;
        text.push_str(NEWLINE);
        text.push_str("PIVOT ");
        text.push_str(or_null(&pivot.expression));
        Ok(())
    }

    /// `UpdateQueryImpl.toSQLString`.
    fn update(&self, text: &mut String) -> Result<(), Thrown> {
        text.push_str("UPDATE ");
        text.push_str(&self.table_sources()?.join(DEFAULT_SEPARATOR));
        let remote = self.row_by(REMOTEDB_ATTRIBUTE)?;
        remote_db(text, &remote.name1, &remote.expression);
        text.push_str(NEWLINE);
        text.push_str("SET ");
        let values = self
            .column_rows()
            .into_iter()
            .map(|row| {
                Ok(format!(
                    "{} = {}",
                    optional_quoted_value(&row.name2, true)?,
                    or_null(&row.expression)
                ))
            })
            .collect::<Result<Vec<_>, Thrown>>()?;
        text.push_str(&values.join(DEFAULT_SEPARATOR));
        if let Some(condition) = self.row_by(WHERE_ATTRIBUTE)?.expression {
            text.push_str(NEWLINE);
            text.push_str("WHERE ");
            text.push_str(&condition);
        }
        Ok(())
    }

    /// `UnionQueryImpl.toSQLString`.
    fn union(&self, text: &mut String) -> Result<(), Thrown> {
        let part = |id: &str| -> Result<String, Thrown> {
            let row = self
                .rows_by(TABLE_ATTRIBUTE)
                .into_iter()
                .find(|row| row.name2.as_deref() == Some(id))
                .ok_or_else(|| {
                    Thrown::illegal_state(format!("Could not find union query with id {id}"))
                })?;
            let expression = row.expression.as_deref().ok_or_else(Thrown::null_pointer)?;
            Ok(clean_union(expression))
        };
        text.push_str(&part(UNION_PART1)?);
        text.push_str(NEWLINE);
        text.push_str("UNION ");
        if !self.has_flag(UNION_FLAG)? {
            text.push_str("ALL ");
        }
        text.push_str(&part(UNION_PART2)?);
        self.order_by(text);
        Ok(())
    }
}

/// `cleanUnionString`: trimmed, each run of line breaks one line separator.
fn clean_union(text: &str) -> String {
    let mut cleaned = String::new();
    let mut in_break = false;
    for c in crate::java::trim(text).chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                cleaned.push_str(NEWLINE);
            }
            in_break = true;
        } else {
            cleaned.push(c);
            in_break = false;
        }
    }
    cleaned
}
