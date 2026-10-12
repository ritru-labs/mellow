//! Local semantic review for JSON and a deliberately bounded YAML subset.
//! No validator executes code or contacts services here. Ambiguous constructs
//! stop review instead of hiding a change behind a guessed interpretation.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeSet;
use tree_sitter::{Node, Parser};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticChange {
    pub path: String,
    pub before: String,
    pub after: String,
}

pub fn review(language: &str, before: &str, after: &str) -> Result<Vec<SemanticChange>> {
    let before = validate(language, before)?;
    let after = validate(language, after)?;
    let mut changes = Vec::new();
    diff("", &before, &after, &mut changes);
    Ok(changes)
}

pub fn validate(language: &str, text: &str) -> Result<Value> {
    if text.len() > 2 * 1024 * 1024 {
        bail!("configuration exceeds the 2 MiB semantic review limit");
    }
    let mut parser = Parser::new();
    match language {
        "JSON" => {
            parser.set_language(&tree_sitter_json::LANGUAGE.into())?;
            let tree = parser.parse(text, None).context("JSON parse cancelled")?;
            if tree.root_node().has_error() {
                bail!("invalid JSON; semantic review is unavailable");
            }
            reject_json_duplicates(tree.root_node(), text, 0)?;
            serde_json::from_str(text).context("invalid JSON")
        }
        "YAML" => {
            parser.set_language(&tree_sitter_yaml::LANGUAGE.into())?;
            let tree = parser.parse(text, None).context("YAML parse cancelled")?;
            if tree.root_node().has_error() {
                bail!("invalid YAML; semantic review is unavailable");
            }
            yaml_value(tree.root_node(), text, 0)
        }
        _ => bail!("safe configuration review supports JSON and YAML"),
    }
}

fn reject_json_duplicates(node: Node<'_>, text: &str, depth: usize) -> Result<()> {
    if depth > 100 {
        bail!("configuration nesting exceeds the review limit");
    }
    let mut keys = BTreeSet::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if node.kind() == "object" && child.kind() == "pair" {
            let key = child
                .child_by_field_name("key")
                .context("missing JSON key")?;
            let key: String = serde_json::from_str(&text[key.byte_range()])?;
            if !keys.insert(key.clone()) {
                bail!("duplicate JSON key {key:?}; semantic review stopped");
            }
        }
        reject_json_duplicates(child, text, depth + 1)?;
    }
    Ok(())
}

fn yaml_value(node: Node<'_>, text: &str, depth: usize) -> Result<Value> {
    if depth > 100 {
        bail!("configuration nesting exceeds the review limit");
    }
    let raw = &text[node.byte_range()];
    let mut cursor = node.walk();
    let children: Vec<_> = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .collect();
    match node.kind() {
        "stream" | "document" | "block_node" | "flow_node" | "block_sequence_item" => {
            if children.is_empty() { return Ok(Value::Null); }
            if children.len()!=1 { bail!("YAML multi-document streams, anchors, tags or directives require an approved external validator; semantic review stopped"); }
            yaml_value(children[0],text,depth+1)
        },
        "block_mapping" | "flow_mapping" => {
            let mut object=serde_json::Map::new();
            for pair in children {
                if !matches!(pair.kind(),"block_mapping_pair"|"flow_pair") { bail!("unsupported YAML mapping"); }
                let key_node=pair.child_by_field_name("key").context("YAML complex or empty keys are unsupported")?;
                let key=yaml_key(key_node,text,depth+1)?;
                if key=="<<" { bail!("YAML merge keys require an approved external validator; semantic review stopped"); }
                let value=match pair.child_by_field_name("value") { Some(node)=>yaml_value(node,text,depth+1)?, None=>Value::Null };
                if object.insert(key.clone(),value).is_some() { bail!("duplicate YAML key {key:?}; semantic review stopped"); }
            }
            Ok(Value::Object(object))
        },
        "block_sequence" | "flow_sequence" => children.into_iter().map(|child|yaml_value(child,text,depth+1)).collect::<Result<Vec<_>>>().map(Value::Array),
        "plain_scalar" => plain_scalar(raw),
        "double_quote_scalar" => serde_json::from_str(raw).context("unsupported YAML quoted escape; use JSON-compatible quotes or an approved external validator"),
        "single_quote_scalar" => Ok(Value::String(raw.strip_prefix('\'').and_then(|raw|raw.strip_suffix('\'')).context("invalid YAML string")?.replace("''","'"))),
        "integer_scalar" | "float_scalar" => {
            match serde_json::from_str::<Value>(raw) {
                Ok(Value::Number(number)) => Ok(Value::Number(number)),
                _ => bail!("noncanonical YAML numeric scalar {raw:?}; use a canonical decimal number or quote it"),
            }
        },
        "string_scalar" | "boolean_scalar" | "null_scalar" => plain_scalar(raw),
        _ => bail!("YAML {} is unsupported by local semantic review; use an approved external validator",node.kind()),
    }
}

fn yaml_key(node: Node<'_>, text: &str, depth: usize) -> Result<String> {
    match yaml_value(node, text, depth)? {
        Value::String(key) => Ok(key),
        _ => bail!("YAML keys must be unambiguous strings; quote numeric or boolean keys"),
    }
}

fn plain_scalar(raw: &str) -> Result<Value> {
    let raw = raw.trim();
    match raw {
        "null" | "~" | "" => Ok(Value::Null),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => {
            let lower = raw.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "yes"
                    | "no"
                    | "on"
                    | "off"
                    | "y"
                    | "n"
                    | "true"
                    | "false"
                    | "null"
                    | ".nan"
                    | ".inf"
                    | "-.inf"
                    | "+.inf"
            ) {
                bail!(
                    "ambiguous YAML scalar {raw:?}; quote the intended string or use canonical JSON scalars"
                );
            }
            if raw.starts_with(|ch: char| ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.')) {
                if let Ok(Value::Number(number)) = serde_json::from_str::<Value>(raw) {
                    return Ok(Value::Number(number));
                }
                // Dates, octal/hex and noncanonical numbers vary by YAML schema.
                if lower.starts_with("0x")
                    || lower.starts_with("0o")
                    || lower.starts_with("0b")
                    || raw.chars().all(|ch| {
                        ch.is_ascii_digit()
                            || matches!(ch, '-' | '+' | '.' | 'x' | 'o' | 'e' | 'E' | ':')
                    })
                {
                    bail!(
                        "ambiguous YAML numeric/date scalar {raw:?}; quote it or use a canonical decimal number"
                    );
                }
            }
            Ok(Value::String(raw.to_owned()))
        }
    }
}

fn diff(path: &str, before: &Value, after: &Value, changes: &mut Vec<SemanticChange>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let suffix = if !key.is_empty()
                    && key
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    if path.is_empty() {
                        key.clone()
                    } else {
                        format!(".{key}")
                    }
                } else {
                    format!("[{}]", serde_json::to_string(key).unwrap_or_default())
                };
                let next = format!("{path}{suffix}");
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => diff(&next, a, b, changes),
                    (a, b) => changes.push(change(&next, a, b)),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for index in 0..a.len().max(b.len()) {
                let next = format!("{path}[{index}]");
                match (a.get(index), b.get(index)) {
                    (Some(a), Some(b)) => diff(&next, a, b, changes),
                    (a, b) => changes.push(change(&next, a, b)),
                }
            }
        }
        _ => changes.push(change(
            if path.is_empty() { "$" } else { path },
            Some(before),
            Some(after),
        )),
    }
}

fn change(path: &str, before: Option<&Value>, after: Option<&Value>) -> SemanticChange {
    fn display(path: &str, value: Option<&Value>) -> String {
        let Some(value) = value else {
            return "(absent)".to_owned();
        };
        let lower = path.to_ascii_lowercase();
        if [
            "password",
            "secret",
            "token",
            "api_key",
            "apikey",
            "credential",
            "private_key",
        ]
        .iter()
        .any(|key| lower.contains(key))
        {
            return "[redacted]".to_owned();
        }
        // Redact sensitive descendants even when a whole object was added.
        fn redact(value: &Value) -> Value {
            match value {
                Value::Object(object) => Value::Object(
                    object
                        .iter()
                        .map(|(key, value)| {
                            let lower = key.to_ascii_lowercase();
                            (
                                key.clone(),
                                if [
                                    "password",
                                    "secret",
                                    "token",
                                    "api_key",
                                    "apikey",
                                    "credential",
                                    "private_key",
                                ]
                                .iter()
                                .any(|needle| lower.contains(needle))
                                {
                                    Value::String("[redacted]".to_owned())
                                } else {
                                    redact(value)
                                },
                            )
                        })
                        .collect(),
                ),
                Value::Array(values) => Value::Array(values.iter().map(redact).collect()),
                value => value.clone(),
            }
        }
        serde_json::to_string(&redact(value)).unwrap_or_else(|_| "(unavailable)".to_owned())
    }
    SemanticChange {
        path: path.to_owned(),
        before: display(path, before),
        after: display(path, after),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_review_tracks_keys_arrays_and_redacts_secrets() {
        let changes = review(
            "JSON",
            r#"{"spec":{"replicas":1},"items":[1,2],"token":"old"}"#,
            r#"{"spec":{"replicas":3},"items":[2,1],"token":"dummy"}"#,
        )
        .unwrap();
        assert!(changes.iter().any(|change| change.path == "spec.replicas"
            && change.before == "1"
            && change.after == "3"));
        assert_eq!(
            changes
                .iter()
                .filter(|change| change.path.starts_with("items["))
                .count(),
            2
        );
        let token = changes
            .iter()
            .find(|change| change.path == "token")
            .unwrap();
        assert_eq!(token.after, "[redacted]");
        assert_eq!(token.before, "[redacted]");
    }
    #[test]
    fn duplicate_and_escaped_duplicate_json_keys_are_rejected() {
        for text in [r#"{"a":1,"a":2}"#, r#"{"a":1,"\u0061":2}"#] {
            assert!(
                validate("JSON", text)
                    .unwrap_err()
                    .to_string()
                    .contains("duplicate")
            );
        }
    }
    #[test]
    fn yaml_nested_maps_sequences_and_quotes_are_reviewed() {
        let changes = review(
            "YAML",
            "spec:\n  replicas: 1\n  names:\n    - 'old'\n",
            "spec:\n  replicas: 2\n  names:\n    - 'new'\n",
        )
        .unwrap();
        assert!(
            changes
                .iter()
                .any(|change| change.path == "spec.replicas" && change.after == "2")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.path == "spec.names[0]" && change.after == "\"new\"")
        );
    }
    #[test]
    fn yaml_ambiguous_constructs_stop_semantic_review() {
        for text in [
            "a: 1\na: 2\n",
            "a: yes\n",
            "a: 012\n",
            "a: &anchor 1\nb: *anchor\n",
            "a: 1\n---\na: 2\n",
            "a: [\n",
        ] {
            assert!(validate("YAML", text).is_err(), "accepted {text:?}");
        }
    }
    #[test]
    fn added_secret_objects_are_redacted() {
        let changes = review(
            "JSON",
            "{}",
            r#"{"auth":{"token":"dummy-token","user":"ann"}}"#,
        )
        .unwrap();
        assert!(!changes[0].after.contains("dummy-token"));
        assert!(changes[0].after.contains("ann"));
    }
    #[test]
    fn unchanged_object_key_order_is_not_a_change() {
        assert!(
            review("JSON", r#"{"a":1,"b":2}"#, r#"{"b":2,"a":1}"#)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn empty_keys_have_explicit_paths_and_nondecimal_yaml_is_not_guessed() {
        let changes = review("JSON", r#"{"":1}"#, r#"{"":2}"#).unwrap();
        assert_eq!(changes[0].path, "[\"\"]");
        for value in ["0xCAFE", "0xdeadbeef", "0o777", "0b1010"] {
            assert!(
                validate("YAML", &format!("mode: {value}\n")).is_err(),
                "accepted {value}"
            );
        }
        assert!(validate("YAML", "mode: '0xCAFE'\n").is_ok());
    }
}
