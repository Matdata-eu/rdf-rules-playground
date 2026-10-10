//! Literal comparison and datatype checks, following XSD and SPARQL semantics
//! closely enough for SHACL's value range and datatype constraints.

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNodeRef, Term};
use oxsdatatypes::{
    Boolean, Date, DateTime, Decimal, Double, Duration, Float, GDay, GMonth, GMonthDay, GYear,
    GYearMonth, Time,
};
use std::cmp::Ordering;
use std::str::FromStr;

/// The string value of a term as SPARQL's `str()` gives it; `None` for blank nodes.
pub fn str_of(t: &Term) -> Option<&str> {
    match t {
        Term::NamedNode(n) => Some(n.as_str()),
        Term::Literal(l) => Some(l.value()),
        _ => None,
    }
}

const INTEGERS: &[(&str, Option<i128>, Option<i128>)] = &[
    ("integer", None, None),
    ("nonNegativeInteger", Some(0), None),
    ("positiveInteger", Some(1), None),
    ("nonPositiveInteger", None, Some(0)),
    ("negativeInteger", None, Some(-1)),
    ("long", Some(i64::MIN as i128), Some(i64::MAX as i128)),
    ("int", Some(i32::MIN as i128), Some(i32::MAX as i128)),
    ("short", Some(i16::MIN as i128), Some(i16::MAX as i128)),
    ("byte", Some(i8::MIN as i128), Some(i8::MAX as i128)),
    ("unsignedLong", Some(0), Some(u64::MAX as i128)),
    ("unsignedInt", Some(0), Some(u32::MAX as i128)),
    ("unsignedShort", Some(0), Some(u16::MAX as i128)),
    ("unsignedByte", Some(0), Some(u8::MAX as i128)),
];

fn xsd_local(dt: NamedNodeRef<'_>) -> Option<&str> {
    dt.as_str()
        .strip_prefix("http://www.w3.org/2001/XMLSchema#")
}

fn is_integer_lexical(s: &str) -> bool {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn is_decimal_lexical(s: &str) -> bool {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    (!int.is_empty() || !frac.is_empty())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
        && !(body.contains('.') && int.is_empty() && frac.is_empty())
}

/// True when the literal's lexical form is valid for its datatype. Unknown
/// datatypes are accepted.
pub fn well_formed(l: &Literal) -> bool {
    let dt = l.datatype();
    let v = l.value();
    if dt == rdf::LANG_STRING {
        return l.language().is_some_and(|t| !t.is_empty());
    }
    let Some(local) = xsd_local(dt) else {
        return true;
    };
    if let Some((_, min, max)) = INTEGERS.iter().find(|(n, _, _)| *n == local) {
        if !is_integer_lexical(v) {
            return false;
        }
        return match v.parse::<i128>() {
            Ok(n) => min.is_none_or(|m| n >= m) && max.is_none_or(|m| n <= m),
            // Out of i128 range: only unbounded types can hold it.
            Err(_) => {
                let negative = v.starts_with('-');
                match local {
                    "integer" => true,
                    "nonNegativeInteger" | "positiveInteger" => !negative,
                    "nonPositiveInteger" | "negativeInteger" => negative,
                    _ => false,
                }
            }
        };
    }
    match local {
        "decimal" => is_decimal_lexical(v),
        "double" => Double::from_str(v).is_ok(),
        "float" => Float::from_str(v).is_ok(),
        "boolean" => Boolean::from_str(v).is_ok(),
        "dateTime" => DateTime::from_str(v).is_ok(),
        "dateTimeStamp" => DateTime::from_str(v).is_ok_and(|d| d.timezone().is_some()),
        "date" => Date::from_str(v).is_ok(),
        "time" => Time::from_str(v).is_ok(),
        "gYear" => GYear::from_str(v).is_ok(),
        "gYearMonth" => GYearMonth::from_str(v).is_ok(),
        "gMonth" => GMonth::from_str(v).is_ok(),
        "gMonthDay" => GMonthDay::from_str(v).is_ok(),
        "gDay" => GDay::from_str(v).is_ok(),
        "duration" => Duration::from_str(v).is_ok(),
        "string" => l.language().is_none(),
        _ => true,
    }
}

enum Value<'a> {
    Decimal(Decimal),
    Double(f64),
    Str(&'a str),
    Bool(bool),
    DateTime(DateTime),
    Date(Date),
    Time(Time),
}

fn value_of(l: &Literal) -> Option<Value<'_>> {
    if !well_formed(l) {
        return None;
    }
    let v = l.value();
    let dt = l.datatype();
    if dt == xsd::STRING {
        return Some(Value::Str(v));
    }
    let local = xsd_local(dt)?;
    if INTEGERS.iter().any(|(n, _, _)| *n == local) || local == "decimal" {
        return match Decimal::from_str(v) {
            Ok(d) => Some(Value::Decimal(d)),
            Err(_) => v.parse::<f64>().ok().map(Value::Double),
        };
    }
    match local {
        "double" => Double::from_str(v).ok().map(|d| Value::Double(d.into())),
        "float" => Float::from_str(v)
            .ok()
            .map(|f| Value::Double(f32::from(f) as f64)),
        "boolean" => Boolean::from_str(v).ok().map(|b| Value::Bool(b.into())),
        "dateTime" | "dateTimeStamp" => DateTime::from_str(v).ok().map(Value::DateTime),
        "date" => Date::from_str(v).ok().map(Value::Date),
        "time" => Time::from_str(v).ok().map(Value::Time),
        _ => None,
    }
}

/// Compare two terms like SPARQL's `<`; `None` when they are not comparable.
pub fn compare(a: &Term, b: &Term) -> Option<Ordering> {
    let (Term::Literal(a), Term::Literal(b)) = (a, b) else {
        return None;
    };
    match (value_of(a)?, value_of(b)?) {
        (Value::Decimal(x), Value::Decimal(y)) => Some(x.cmp(&y)),
        (Value::Decimal(x), Value::Double(y)) => f64::from(Double::from(x)).partial_cmp(&y),
        (Value::Double(x), Value::Decimal(y)) => x.partial_cmp(&f64::from(Double::from(y))),
        (Value::Double(x), Value::Double(y)) => x.partial_cmp(&y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(&y)),
        (Value::DateTime(x), Value::DateTime(y)) => x.partial_cmp(&y),
        (Value::Date(x), Value::Date(y)) => x.partial_cmp(&y),
        (Value::Time(x), Value::Time(y)) => x.partial_cmp(&y),
        _ => None,
    }
}

/// SPARQL `langMatches` with a basic language range.
pub fn lang_matches(tag: &str, range: &str) -> bool {
    if range == "*" {
        return !tag.is_empty();
    }
    let tag = tag.to_ascii_lowercase();
    let range = range.to_ascii_lowercase();
    tag == range || tag.starts_with(&format!("{range}-"))
}
