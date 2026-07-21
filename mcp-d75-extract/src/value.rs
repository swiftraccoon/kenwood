//! Small helpers over `serde_json::Value` objects.
//!
//! The extractor mirrors the reference implementation's dict-shaped schema:
//! every JSON object is built in explicit key insertion order (`serde_json`'s
//! `preserve_order` feature keeps that order through serialization).

use serde_json::{Map, Value};

use crate::error::{Result, extract_error};

/// Borrow a value's object map, failing on non-objects.
pub(crate) fn obj(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| extract_error!("internal schema value is not an object: {value}"))
}

/// Mutably borrow a value's object map, failing on non-objects.
pub(crate) fn obj_mut(value: &mut Value) -> Result<&mut Map<String, Value>> {
    if value.is_object() {
        value
            .as_object_mut()
            .ok_or_else(|| extract_error!("internal schema value is not an object"))
    } else {
        Err(extract_error!(
            "internal schema value is not an object: {value}"
        ))
    }
}

/// Insert (or replace in place) a key while preserving insertion order.
pub(crate) fn insert(value: &mut Value, key: &str, item: Value) -> Result<()> {
    drop(obj_mut(value)?.insert(key.to_owned(), item));
    Ok(())
}

/// Fetch a required key from an object value.
pub(crate) fn req<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    obj(value)?
        .get(key)
        .ok_or_else(|| extract_error!("internal schema value is missing key {key}"))
}

/// Fetch a required string value.
pub(crate) fn req_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    req(value, key)?
        .as_str()
        .ok_or_else(|| extract_error!("internal schema key {key} is not a string"))
}

/// Fetch a required integer value.
pub(crate) fn req_i64(value: &Value, key: &str) -> Result<i64> {
    req(value, key)?
        .as_i64()
        .ok_or_else(|| extract_error!("internal schema key {key} is not an integer"))
}

/// Fetch a required array value.
pub(crate) fn req_array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    req(value, key)?
        .as_array()
        .ok_or_else(|| extract_error!("internal schema key {key} is not an array"))
}

/// Copy of an object value with every `null` entry removed.
pub(crate) fn without_nulls(value: &Value) -> Result<Value> {
    let filtered: Map<String, Value> = obj(value)?
        .iter()
        .filter(|(_, item)| !item.is_null())
        .map(|(key, item)| (key.clone(), item.clone()))
        .collect();
    Ok(Value::Object(filtered))
}

/// Render a possibly-null name the way Python string-formats it.
pub(crate) fn display_name(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
