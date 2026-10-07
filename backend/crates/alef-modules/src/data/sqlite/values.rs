// SPDX-License-Identifier: MIT OR Apache-2.0
//! Values between SQLite and JSON. A JSON number, text, `null` or boolean is what it looks like;
//! what JSON cannot hold goes in a tag of one key: `{"$int": "9007199254740993"}` for an INTEGER a
//! JS number cannot keep exactly, `{"$blob": "<base64>"}` for a BLOB, `{"$real": "Infinity"}` for the
//! REALs JSON has no number for.
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{
    types::{Value as Sql, ValueRef},
    ToSql,
};
use serde_json::{json, Map, Value};

use alef_core::{AlefError, ErrorCode};

/// The integers a JS number holds exactly: up to 2^53 - 1.
const SAFE: u64 = (1 << 53) - 1;

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// A value SQLite gives, as JSON.
pub(super) fn to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => integer(number),
        ValueRef::Real(number) => match serde_json::Number::from_f64(number) {
            Some(number) => Value::Number(number),
            None => {
                json!({ "$real": if number.is_sign_negative() { "-Infinity" } else { "Infinity" } })
            }
        },
        ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => json!({ "$blob": STANDARD.encode(bytes) }),
    }
}

/// An INTEGER as JSON: a number when a JS number keeps it, a tag when it does not.
pub(super) fn integer(number: i64) -> Value {
    if number.unsigned_abs() <= SAFE {
        json!(number)
    } else {
        json!({ "$int": number.to_string() })
    }
}

/// A value of a parameter, as SQLite takes it.
fn from_json(value: &Value) -> Result<Sql, AlefError> {
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Bool(flag) => Sql::Integer(i64::from(*flag)),
        Value::Number(number) => {
            // What a JS number cannot be as an integer is a REAL (a program that means a whole
            // number of 64 bits above 2^53 says it with `$int`).
            match number.as_i64() {
                Some(whole) => Sql::Integer(whole),
                None => Sql::Real(number.as_f64().unwrap_or(f64::NAN)),
            }
        }
        Value::String(text) => Sql::Text(text.clone()),
        Value::Array(_) => return Err(invalid("a parameter is a single value, not a list")),
        Value::Object(tag) => {
            let mut entries = tag.iter();
            match (entries.next(), entries.next()) {
                (Some((key, Value::String(text))), None) => match key.as_str() {
                    "$int" => Sql::Integer(
                        text.parse()
                            .map_err(|_| invalid("$int holds a whole number of 64 bits"))?,
                    ),
                    "$blob" => Sql::Blob(
                        STANDARD
                            .decode(text)
                            .map_err(|_| invalid("$blob holds base64"))?,
                    ),
                    "$real" => Sql::Real(match text.as_str() {
                        "Infinity" => f64::INFINITY,
                        "-Infinity" => f64::NEG_INFINITY,
                        _ => return Err(invalid("$real holds Infinity or -Infinity")),
                    }),
                    _ => {
                        return Err(invalid(
                            "a parameter is a value or a $int, $blob, $real tag",
                        ))
                    }
                },
                _ => {
                    return Err(invalid(
                        "a parameter is a value or a $int, $blob, $real tag",
                    ))
                }
            }
        }
    })
}

/// The parameters of a statement: none, by position (`?`), or by name (`:name`, `@name`, `$name`).
#[derive(Debug, Default)]
pub(super) enum Params {
    #[default]
    None,
    Positional(Vec<Sql>),
    Named(Vec<(String, Sql)>),
}

impl Params {
    pub(super) fn from_json(value: Option<&Value>) -> Result<Self, AlefError> {
        match value {
            None | Some(Value::Null) => Ok(Self::None),
            Some(Value::Array(items)) => Ok(Self::Positional(
                items.iter().map(from_json).collect::<Result<_, _>>()?,
            )),
            Some(Value::Object(named)) => Ok(Self::Named(
                named
                    .iter()
                    .map(|(name, value)| {
                        // A name without a prefix is `:name`.
                        let full = if name.starts_with([':', '@', '$']) {
                            name.clone()
                        } else {
                            format!(":{name}")
                        };
                        Ok((full, from_json(value)?))
                    })
                    .collect::<Result<_, AlefError>>()?,
            )),
            Some(_) => Err(invalid("parameters are a list or an object")),
        }
    }

    pub(super) fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Runs the query of `statement` with these parameters.
    pub(super) fn query<'s>(
        &self,
        statement: &'s mut rusqlite::Statement<'_>,
    ) -> rusqlite::Result<rusqlite::Rows<'s>> {
        match self {
            Self::None => statement.query([]),
            Self::Positional(values) => statement.query(rusqlite::params_from_iter(values)),
            Self::Named(values) => {
                let named: Vec<(&str, &dyn ToSql)> = values
                    .iter()
                    .map(|(name, value)| (name.as_str(), value as &dyn ToSql))
                    .collect();
                statement.query(named.as_slice())
            }
        }
    }

    /// Executes `statement` with these parameters, the number of rows it changed.
    pub(super) fn execute(
        &self,
        statement: &mut rusqlite::Statement<'_>,
    ) -> rusqlite::Result<usize> {
        match self {
            Self::None => statement.execute([]),
            Self::Positional(values) => statement.execute(rusqlite::params_from_iter(values)),
            Self::Named(values) => {
                let named: Vec<(&str, &dyn ToSql)> = values
                    .iter()
                    .map(|(name, value)| (name.as_str(), value as &dyn ToSql))
                    .collect();
                statement.execute(named.as_slice())
            }
        }
    }
}

/// One row as an object of its columns (a name that comes twice keeps the last).
pub(super) fn row_to_json(names: &[String], row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = Map::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        object.insert(name.clone(), to_json(row.get_ref(index)?));
    }
    Ok(Value::Object(object))
}

/// Roughly how much memory a value of a row takes, to bound what one answer holds.
pub(super) fn weight(row: &Value) -> usize {
    match row {
        Value::Object(map) => map
            .iter()
            .map(|(name, value)| name.len() + weight(value))
            .sum(),
        Value::String(text) => text.len() + 2,
        Value::Array(items) => items.iter().map(weight).sum(),
        _ => 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_sqlite_gives_comes_as_json_and_what_json_cannot_hold_is_tagged() {
        assert_eq!(to_json(ValueRef::Null), Value::Null);
        assert_eq!(to_json(ValueRef::Integer(-7)), json!(-7));
        assert_eq!(
            to_json(ValueRef::Integer(9_007_199_254_740_991)),
            json!(9_007_199_254_740_991_i64)
        );
        assert_eq!(
            to_json(ValueRef::Integer(9_007_199_254_740_992)),
            json!({ "$int": "9007199254740992" })
        );
        assert_eq!(
            to_json(ValueRef::Integer(i64::MIN)),
            json!({ "$int": "-9223372036854775808" })
        );
        assert_eq!(to_json(ValueRef::Real(1.5)), json!(1.5));
        assert_eq!(
            to_json(ValueRef::Real(f64::INFINITY)),
            json!({ "$real": "Infinity" })
        );
        assert_eq!(
            to_json(ValueRef::Real(f64::NEG_INFINITY)),
            json!({ "$real": "-Infinity" })
        );
        assert_eq!(to_json(ValueRef::Text("é".as_bytes())), json!("é"));
        assert_eq!(
            to_json(ValueRef::Blob(&[0, 1, 255])),
            json!({ "$blob": "AAH/" })
        );
    }

    #[test]
    fn a_parameter_is_read_back_as_the_value_it_names() {
        let read = |value: Value| from_json(&value).unwrap();
        assert_eq!(read(Value::Null), Sql::Null);
        assert_eq!(read(json!(true)), Sql::Integer(1));
        assert_eq!(read(json!(false)), Sql::Integer(0));
        assert_eq!(read(json!(42)), Sql::Integer(42));
        assert_eq!(read(json!(2.5)), Sql::Real(2.5));
        assert_eq!(
            read(json!(18_446_744_073_709_551_615_u64)),
            Sql::Real(1.8446744073709552e19),
            "beyond 64 bits it is a REAL"
        );
        assert_eq!(read(json!("t")), Sql::Text("t".into()));
        assert_eq!(
            read(json!({ "$int": "-9223372036854775808" })),
            Sql::Integer(i64::MIN)
        );
        assert_eq!(read(json!({ "$blob": "AAH/" })), Sql::Blob(vec![0, 1, 255]));
        assert_eq!(
            read(json!({ "$real": "-Infinity" })),
            Sql::Real(f64::NEG_INFINITY)
        );
        for bad in [
            json!([1]),
            json!({}),
            json!({ "$int": "x" }),
            json!({ "$int": 5 }),
            json!({ "$blob": "***" }),
            json!({ "$real": "NaN" }),
            json!({ "$other": "1" }),
            json!({ "$int": "1", "extra": 1 }),
        ] {
            assert_eq!(
                from_json(&bad).unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{bad}"
            );
        }
    }

    #[test]
    fn parameters_are_a_list_or_names_with_or_without_the_colon() {
        assert!(Params::from_json(None).unwrap().is_none());
        assert!(Params::from_json(Some(&Value::Null)).unwrap().is_none());
        assert!(matches!(
            Params::from_json(Some(&json!([1, "a"]))).unwrap(),
            Params::Positional(values) if values.len() == 2
        ));
        let Params::Named(named) =
            Params::from_json(Some(&json!({ "a": 1, ":b": 2, "@c": 3, "$d": 4 }))).unwrap()
        else {
            panic!("named parameters");
        };
        let mut names: Vec<&str> = named.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["$d", ":a", ":b", "@c"]);
        assert_eq!(
            Params::from_json(Some(&json!("x"))).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
}
