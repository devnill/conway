//! Canonical JSON bytes — the one canonicalizer this workspace uses.
//!
//! Moved here from `conway-runtime/src/context/prefix.rs` (board item
//! `01M00QDYK4T5MZNCTQ0ZXEBSZX`, DESIGN-context-path §2.3) so that
//! `conway-core`'s `SelectionKey` can hash its projection through the exact
//! same canonical bytes `prefix_key` hashes its segment projection through.
//! Two canonicalizers is the drift hazard that makes two hashes disagree for
//! reasons nobody can find; this retire the second copy (the third lived in
//! `conway-plugin-stepguard` and was byte-identical).
//!
//! This is a pure canonicalizer with no policy in it, so it fits the contract
//! crate's charter. `blake3` and `serde_json` are already `conway-core` deps.
//! `conway-runtime` and `conway` (the facade) both depend on `conway-core`,
//! so each consumes it from here directly.

/// Canonicalize `value`: recursively sort object keys and serialize
/// without insignificant whitespace, so semantically identical JSON always
/// hashes to the same bytes. `serde_json`'s default `Map` is already
/// key-sorted (no `preserve_order` feature is enabled anywhere in this
/// workspace), but this function does not rely on that remaining true.
pub fn canonical_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    fn canon(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<(String, serde_json::Value)> =
                    map.iter().map(|(k, v)| (k.clone(), canon(v))).collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::Value::Object(entries.into_iter().collect())
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(canon).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_vec(&canon(value)).expect("canonical json serialization never fails")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Object-key order must not change the bytes: the whole point of the
    /// canonicalizer.
    #[test]
    fn key_order_does_not_change_bytes() {
        let a = serde_json::json!({"b": 2, "a": 1, "c": [1, 2, 3]});
        let b = serde_json::json!({"c": [1, 2, 3], "a": 1, "b": 2});
        assert_eq!(canonical_json_bytes(&a), canonical_json_bytes(&b));
    }

    /// Nested objects are sorted recursively, not just the top level.
    #[test]
    fn nested_objects_are_sorted_recursively() {
        let a = serde_json::json!({"outer": {"z": 1, "a": 2}});
        let b = serde_json::json!({"outer": {"a": 2, "z": 1}});
        assert_eq!(canonical_json_bytes(&a), canonical_json_bytes(&b));
    }

    /// No insignificant whitespace: the bytes are dense.
    #[test]
    fn no_insignificant_whitespace() {
        let v = serde_json::json!({"a": 1, "b": [2, 3]});
        let bytes = canonical_json_bytes(&v);
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), r#"{"a":1,"b":[2,3]}"#);
    }

    /// Arrays are order-sensitive: reordering elements changes the bytes.
    #[test]
    fn array_order_is_significant() {
        let a = serde_json::json!([1, 2, 3]);
        let b = serde_json::json!([3, 2, 1]);
        assert_ne!(canonical_json_bytes(&a), canonical_json_bytes(&b));
    }
}
