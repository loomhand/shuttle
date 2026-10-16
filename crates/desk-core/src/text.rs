//! Desk text: label comparison, quoting, timings and the `desk` CLI's arguments.

use serde_json::{Map, Value, json};

/// The characters of a name or value a snapshot shows.
pub const NAME_CHARS: usize = 80;
/// The most elements a find returns.
pub const MAX_FOUND: usize = 20;

/// An element a find compares by name and role.
pub trait Named {
    fn name(&self) -> &str;
    fn role(&self) -> &str;
}

/// The indexes of the nodes named `name`, else of those that contain it, ignoring case and punctuation.
pub fn matches<N: Named>(nodes: &[N], name: &str, role: Option<&str>) -> Vec<usize> {
    let want = norm(name);
    let pool: Vec<usize> = (0..nodes.len())
        .filter(|&i| role.is_none_or(|r| r.split('|').any(|r| r == nodes[i].role())))
        .collect();
    let exact: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|&i| norm(nodes[i].name()) == want)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    pool.into_iter()
        .filter(|&i| !want.is_empty() && norm(nodes[i].name()).contains(&want))
        .collect()
}

/// Lower case without the punctuation that labels add: underscores, ellipses, a trailing colon.
pub fn norm(s: &str) -> String {
    let s = s.to_lowercase().replace(['_', '…'], "").replace("...", "");
    let s = s.trim().trim_end_matches(':');
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters of `s`, escaped for a double-quoted outline field.
pub fn quote(s: &str, max: usize) -> String {
    let cut: String = s.chars().take(max).collect();
    cut.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Microseconds as milliseconds with one decimal.
pub fn ms(us: u128) -> Value {
    json!((us as f64 / 100.0).round() / 10.0)
}

/// One JSON object, or `key=value` pairs whose values parse as JSON when they can.
pub fn cli_args(rest: &[String]) -> Result<Value, String> {
    if let [one] = rest
        && one.trim_start().starts_with('{')
    {
        return serde_json::from_str(one).map_err(|e| format!("arguments: {e}"));
    }
    let mut args = Map::new();
    for pair in rest {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("{pair:?} is not key=value"))?;
        let v = serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_owned()));
        args.insert(k.to_owned(), v);
    }
    Ok(Value::Object(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct N(&'static str, &'static str);

    impl Named for N {
        fn name(&self) -> &str {
            self.0
        }
        fn role(&self) -> &str {
            self.1
        }
    }

    #[test]
    fn names_compare_without_label_punctuation() {
        assert_eq!(norm("  _Save As...  "), "save as");
        assert_eq!(norm("Tab width:"), "tab width");
        assert_eq!(norm("Long  line\tmargin…"), "long line margin");
    }

    #[test]
    fn names_match_exactly_before_they_match_contained() {
        let nodes = [
            N("Save As…", "menuitem"),
            N("Save", "menuitem"),
            N("save", "button"),
        ];
        assert_eq!(matches(&nodes, "save", None), [1, 2]);
        assert_eq!(matches(&nodes, "save as", None), [0]);
        assert_eq!(matches(&nodes, "save", Some("button")), [2]);
        assert_eq!(matches(&nodes, "save", Some("tab|button")), [2]);
        assert!(matches(&nodes, "", None).is_empty());
    }

    #[test]
    fn quoting_cuts_and_escapes() {
        assert_eq!(quote("a \"b\"\nc\\", 80), "a \\\"b\\\"\\nc\\\\");
        assert_eq!(quote("héllo", 2), "hé");
        assert_eq!(ms(1_234_567), json!(1234.6));
    }

    #[test]
    fn cli_arguments_are_json_or_pairs_with_json_values() {
        let pairs =
            cli_args(&["window=Save As".into(), "text=true".into(), "pid=7".into()]).unwrap();
        assert_eq!(pairs, json!({"window": "Save As", "text": true, "pid": 7}));
        assert_eq!(
            cli_args(&[r#"{"id": "1/2"}"#.into()]).unwrap(),
            json!({"id": "1/2"})
        );
        assert!(cli_args(&["nope".into()]).is_err());
    }
}
