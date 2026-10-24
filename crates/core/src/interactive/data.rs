//! Bounded data expressions and the `OpenUI` collection/numeric built-ins.

use super::{MAX_VALUE, eval, scalar_ok, scalar_text, spend};
use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;

pub(super) fn builtin(name: &str) -> Option<&'static str> {
    Some(match name {
        "@Count" => "count",
        "@First" => "first",
        "@Last" => "last",
        "@Sum" => "sum",
        "@Avg" => "avg",
        "@Min" => "min",
        "@Max" => "max",
        "@Filter" => "filter",
        "@Sort" => "sort",
        "@Round" => "round",
        "@Abs" => "abs",
        "@Floor" => "floor",
        "@Ceil" => "ceil",
        _ => return None,
    })
}

pub(super) fn arity(op: &str, count: usize) -> Result<()> {
    let valid = match op {
        "filter" => count == 4,
        "sort" => (2..=3).contains(&count),
        "round" => (1..=2).contains(&count),
        "count" | "first" | "last" | "sum" | "avg" | "min" | "max" | "abs" | "floor" | "ceil" => {
            count == 1
        }
        _ => bail!("unknown data operator: {op}"),
    };
    ensure!(valid, "wrong arity for {op}");
    Ok(())
}

pub(super) fn value_ok(value: &Value, budget: &mut usize, depth: usize) -> Result<()> {
    spend(budget)?;
    ensure!(depth <= 12, "UI data is too deep");
    match value {
        Value::Array(items) => {
            ensure!(items.len() <= 200, "UI array exceeds 200 items");
            for item in items {
                value_ok(item, budget, depth + 1)?;
            }
        }
        Value::Object(fields) => {
            ensure!(fields.len() <= 200, "UI object exceeds 200 fields");
            for (key, value) in fields {
                super::text_ok(key, MAX_VALUE)?;
                value_ok(value, budget, depth + 1)?;
            }
        }
        _ => scalar_ok(value)?,
    }
    Ok(())
}

pub(super) fn initial(value: &Value) -> Result<Value> {
    let mut value = value.clone();
    first_random(&mut value);
    eval(&value, &Map::new(), &mut 4096)
}
fn first_random(value: &mut Value) {
    if let Some(first) = value
        .get("random")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        *value = first.clone();
    } else if let Some(fields) = value.get_mut("object").and_then(Value::as_object_mut) {
        for value in fields.values_mut() {
            first_random(value);
        }
    } else if value.get("literal").is_none() {
        if let Some(object) = value.as_object_mut() {
            for value in object.values_mut() {
                first_random(value);
            }
        } else if let Some(array) = value.as_array_mut() {
            for value in array {
                first_random(value);
            }
        }
    }
}

pub(super) fn field(value: &Value, key: &str) -> Value {
    match value {
        Value::Array(items) if key == "length" => json!(items.len()),
        Value::String(text) if key == "length" => json!(text.chars().count()),
        Value::Array(items) => items
            .iter()
            .map(|item| item.get(key).cloned().unwrap_or(Value::Null))
            .collect(),
        Value::Object(fields) => fields.get(key).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}
fn path(value: &Value, key: &str) -> Value {
    if key.is_empty() {
        return value.clone();
    }
    key.split('.')
        .fold(value.clone(), |value, key| field(&value, key))
}
fn numeric(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .unwrap_or(0.0),
        Value::Bool(true) => 1.0,
        _ => 0.0,
    }
}
fn text(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        scalar_text(value)
    }
}
fn equal(a: &Value, b: &Value) -> bool {
    if a.is_null() || b.is_null() {
        return a == b;
    }
    if a.is_number() || b.is_number() || a.is_boolean() || b.is_boolean() {
        let number = |value: &Value| {
            if let Some(text) = value.as_str() {
                if text.trim().is_empty() {
                    Some(0.0)
                } else {
                    text.trim().parse::<f64>().ok()
                }
            } else {
                Some(numeric(value))
            }
        };
        number(a)
            .zip(number(b))
            .is_some_and(|(a, b)| a.partial_cmp(&b) == Some(Ordering::Equal))
    } else {
        a == b
    }
}
fn compare(a: &Value, b: &Value) -> Ordering {
    let number = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    if let (Some(a), Some(b)) = (number(a), number(b)) {
        a.partial_cmp(&b).unwrap_or(Ordering::Equal)
    } else {
        let (a, b) = (text(a), text(b));
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| b.cmp(&a))
    }
}

pub(super) fn evaluate(op: &str, args: &[Value]) -> Result<Value> {
    arity(op, args.len())?;
    let value = &args[0];
    let items = value.as_array().map_or(&[][..], Vec::as_slice);
    Ok(match op {
        "count" => json!(items.len()),
        "first" => items.first().cloned().unwrap_or(Value::Null),
        "last" => items.last().cloned().unwrap_or(Value::Null),
        "sum" => json!(items.iter().map(numeric).sum::<f64>()),
        "avg" => json!(if items.is_empty() {
            0.0
        } else {
            items.iter().map(numeric).sum::<f64>() / f64::from(u32::try_from(items.len())?)
        }),
        "min" | "max" => json!(
            items
                .iter()
                .map(numeric)
                .reduce(|a, b| if op == "min" { a.min(b) } else { a.max(b) })
                .unwrap_or(0.0)
        ),
        "abs" => json!(numeric(value).abs()),
        "floor" => json!(numeric(value).floor()),
        "ceil" => json!(numeric(value).ceil()),
        "round" => {
            let decimals = args.get(1).map_or(0.0, numeric).trunc();
            ensure!(
                (-12.0..=12.0).contains(&decimals),
                "round decimals must be -12–12"
            );
            let scale = 10_f64.powf(decimals);
            json!((numeric(value) * scale + 0.5).floor() / scale)
        }
        "filter" => {
            let key = args[1].as_str().unwrap_or("");
            let op = args[2].as_str().unwrap_or("==");
            let target = &args[3];
            items
                .iter()
                .filter(|item| {
                    let value = path(item, key);
                    match op {
                        "==" => equal(&value, target),
                        "!=" => !equal(&value, target),
                        ">" => numeric(&value) > numeric(target),
                        "<" => numeric(&value) < numeric(target),
                        ">=" => numeric(&value) >= numeric(target),
                        "<=" => numeric(&value) <= numeric(target),
                        "contains" => text(&value).contains(&text(target)),
                        _ => false,
                    }
                })
                .cloned()
                .collect()
        }
        "sort" => {
            if !value.is_array() {
                return Ok(value.clone());
            }
            let mut items = items.to_vec();
            let key = args[1].as_str().unwrap_or("");
            let descending = args.get(2).and_then(Value::as_str) == Some("desc");
            items.sort_by(|a, b| {
                let order = compare(&path(a, key), &path(b, key));
                if descending { order.reverse() } else { order }
            });
            items.into()
        }
        _ => unreachable!("checked data operator"),
    })
}
