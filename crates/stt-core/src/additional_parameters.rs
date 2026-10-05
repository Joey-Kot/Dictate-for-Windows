//! Shared JSON merge semantics for audio multipart fields and rewrite requests.
use serde_json::{Map, Value};

pub fn parse(input: &str) -> Result<Map<String, Value>, serde_json::Error> {
    if input.trim().is_empty() {
        Ok(Map::new())
    } else {
        serde_json::from_str(input)
    }
}

/// Objects merge recursively, arrays replace, and null object members delete.
/// Null array elements remain values, including in nested arrays.
pub fn merge(base: &Map<String, Value>, extra: &Map<String, Value>) -> Map<String, Value> {
    let mut result = base.clone();
    for (key, value) in extra {
        if value.is_null() {
            result.remove(key);
        } else if let Value::Object(object) = value {
            let original = result
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            result.insert(key.clone(), Value::Object(merge(&original, object)));
        } else {
            result.insert(key.clone(), replacement(value));
        }
    }
    result
}

fn replacement(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(merge(&Map::new(), object)),
        Value::Array(array) => Value::Array(array.iter().map(replacement).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recursive_merge_preserves_siblings_and_replaces_arrays_and_types() {
        let base = json!({"nested":{"keep":1,"change":2},"array":[1,2],"scalar":1});
        let extra = json!({"nested":{"change":3},"array":[{"x":4}],"scalar":{"x":5}});
        assert_eq!(
            Value::Object(merge(base.as_object().unwrap(), extra.as_object().unwrap())),
            json!({"nested":{"keep":1,"change":3},"array":[{"x":4}],"scalar":{"x":5}})
        );
        assert_eq!(base["nested"]["change"], 2);
    }

    #[test]
    fn deletion_applies_to_new_objects_and_objects_inside_arrays() {
        let extra = parse(r#"{"gone":null,"new":{"gone":null},"array":[null,{"gone":null,"keep":2},[null,{"gone":null}]]}"#).unwrap();
        assert_eq!(
            Value::Object(merge(&Map::new(), &extra)),
            json!({"new":{},"array":[null,{"keep":2},[null,{}]]})
        );
    }

    #[test]
    fn requires_one_object_or_blank_input() {
        assert!(parse(" \n ").unwrap().is_empty());
        for invalid in ["[]", "null", "42", "{} trailing", "{"] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }
}
