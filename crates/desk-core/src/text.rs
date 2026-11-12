//! Desk text: label comparison, quoting and timings.

use serde_json::{Value, json};

/// The characters of a name or value a snapshot shows.
pub const NAME_CHARS: usize = 80;
/// The most elements a find returns.
pub const MAX_FOUND: usize = 20;
pub const READ_CHARS: usize = 64 << 10;
pub const READ_TAIL: &str = "read: tail needs text: true";

/// An element a find compares by name and role.
pub trait Named {
    fn name(&self) -> &str;
    fn role(&self) -> &str;
}

/// The indexes of the nodes named `name`, else of those that contain it, ignoring case and punctuation.
pub fn matches<N: Named>(nodes: &[N], name: &str, role: Option<&str>) -> Vec<usize> {
    let want = norm(name);
    if want.is_empty() {
        return Vec::new();
    }
    let (mut exact, mut contained) = (Vec::new(), Vec::new());
    let kept = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| role.is_none_or(|r| r.split('|').any(|r| r == n.role())));
    for (i, n) in kept {
        let have = norm(n.name());
        if have == want {
            exact.push(i);
        } else if have.contains(&want) {
            contained.push(i);
        }
    }
    if exact.is_empty() { contained } else { exact }
}

/// Lower case without the punctuation that labels add: underscores, ellipses, a trailing colon.
pub fn norm(s: &str) -> String {
    let mut s = s.to_lowercase();
    s.retain(|c| c != '_' && c != '…');
    if s.contains("...") {
        s = s.replace("...", "");
    }
    one_line(s.trim().trim_end_matches(':'))
}

/// The words of `s` joined by single spaces.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// At most `max` characters of `s`, escaped for a double-quoted outline field.
pub fn quote(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    for c in s.chars().take(max) {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// `s` cut to at most `max` characters, in place: its first ones, or with `tail` its last ones.
pub fn clip(mut s: String, max: usize, tail: bool) -> String {
    if !tail {
        if let Some((i, _)) = s.char_indices().nth(max) {
            s.truncate(i);
        }
    } else if let Some(skip) = s.chars().count().checked_sub(max).filter(|&k| k > 0) {
        let i = s.char_indices().nth(skip).map_or(s.len(), |(i, _)| i);
        s.drain(..i);
    }
    s
}

pub fn cut_read(value: &mut Option<String>, max: usize, tail: bool) -> bool {
    let cut = value.as_ref().is_some_and(|v| v.chars().nth(max).is_some());
    if cut {
        *value = value.take().map(|v| clip(v, max, tail));
    }
    cut
}

/// Microseconds as milliseconds with one decimal.
pub fn ms(us: u128) -> Value {
    json!((us as f64 / 100.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_compare_without_label_punctuation() {
        assert_eq!(norm("  _Save As...  "), "save as");
        assert_eq!(norm("Tab width:"), "tab width");
        assert_eq!(norm("Long  line\tmargin…"), "long line margin");
        assert_eq!(norm("Go._.. on: :"), "go on:");
    }

    #[test]
    fn names_match_exactly_before_they_match_contained() {
        let nodes = [
            N("Save As…", "menuitem"),
            N("Save", "menuitem"),
            N("save", "button"),
            N("", "button"),
        ];
        assert_eq!(matches(&nodes, "save", None), [1, 2]);
        assert_eq!(matches(&nodes, "save as", None), [0]);
        assert_eq!(matches(&nodes, "save", Some("button")), [2]);
        assert_eq!(matches(&nodes, "save", Some("tab|button")), [2]);
        assert!(matches(&nodes, "", None).is_empty());
        assert!(matches(&nodes, "...", Some("button")).is_empty());
    }

    #[test]
    fn quoting_cuts_and_escapes() {
        assert_eq!(quote("a \"b\"\nc\\", 80), "a \\\"b\\\"\\nc\\\\");
        assert_eq!(quote("héllo", 2), "hé");
        assert_eq!(clip("héllo".into(), 2, false), "hé");
        assert_eq!(clip("hé".into(), 5, false), "hé");
        assert_eq!(ms(1_234_567), json!(1234.6));
    }

    #[test]
    fn a_tail_clip_keeps_the_last_characters_whole() {
        assert_eq!(clip("ab中🙂".into(), 2, true), "中🙂");
        assert_eq!(clip("ab中🙂".into(), 3, true), "b中🙂");
        assert_eq!(clip("中🙂".into(), 1, true), "🙂");
        assert_eq!(clip("中🙂".into(), 2, true), "中🙂");
        assert_eq!(clip("中🙂".into(), 5, true), "中🙂");
        assert_eq!(clip("中🙂".into(), 0, true), "");
        assert_eq!(clip("中🙂".into(), 1, false), "中");
    }

    struct N(&'static str, &'static str);

    impl Named for N {
        fn name(&self) -> &str {
            self.0
        }
        fn role(&self) -> &str {
            self.1
        }
    }
}
