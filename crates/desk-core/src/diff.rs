//! Snapshot diffs: a window's nodes against the last ones a caller read of that window.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use serde_json::Value;

/// The most windows whose last nodes one caller keeps.
pub const CALLER_WINDOWS: usize = 8;
/// The most last reads all callers keep.
pub const KEPT_READS: usize = 32;
pub const SINCE_TEXT: &str =
    "snapshot: since diffs the nodes, not the text outline; leave out text or since";

/// The last nodes the desk returned to each caller for each window, oldest first.
#[derive(Default)]
pub struct Baselines(VecDeque<Baseline>);

impl Baselines {
    /// `head` with the diff of `nodes` against the last ones `caller` read of `window`, or with all of them and `diff: false` when there are none or the diff is not shorter; `nodes` become that read.
    pub fn answer(
        &mut self,
        caller: &str,
        window: &str,
        head: &Value,
        nodes: Vec<Value>,
    ) -> Result<String, String> {
        let held = self
            .0
            .iter()
            .position(|b| b.caller == caller && b.window == window)
            .and_then(|i| self.0.remove(i));
        let full = serde_json::to_string(&Full {
            head,
            diff: false,
            nodes: &nodes,
        })
        .map_err(|e| format!("snapshot: {e}"))?;
        let diffed = held
            .map(|b| serde_json::to_string(&diff(head, &b.nodes, &nodes)))
            .transpose()
            .map_err(|e| format!("snapshot: {e}"))?;
        self.keep(caller, window, nodes);
        Ok(diffed.filter(|d| d.len() < full.len()).unwrap_or(full))
    }

    /// Drops every read `caller` holds.
    pub fn release(&mut self, caller: &str) {
        self.0.retain(|b| b.caller != caller);
    }

    fn keep(&mut self, caller: &str, window: &str, nodes: Vec<Value>) {
        if self.0.iter().filter(|b| b.caller == caller).count() >= CALLER_WINDOWS
            && let Some(i) = self.0.iter().position(|b| b.caller == caller)
        {
            self.0.remove(i);
        }
        if self.0.len() >= KEPT_READS {
            self.0.pop_front();
        }
        self.0.push_back(Baseline {
            caller: caller.to_owned(),
            window: window.to_owned(),
            nodes,
        });
    }
}

struct Baseline {
    caller: String,
    window: String,
    nodes: Vec<Value>,
}

#[derive(Serialize)]
struct Full<'a> {
    #[serde(flatten)]
    head: &'a Value,
    diff: bool,
    nodes: &'a [Value],
}

#[derive(Serialize)]
struct Diff<'a> {
    #[serde(flatten)]
    head: &'a Value,
    diff: bool,
    focused: Option<&'a Value>,
    added: Vec<&'a Value>,
    removed: Vec<Gone<'a>>,
    changed: Vec<&'a Value>,
}

#[derive(Serialize)]
struct Gone<'a> {
    id: &'a Value,
    role: &'a Value,
    name: &'a Value,
}

fn diff<'a>(head: &'a Value, old: &'a [Value], new: &'a [Value]) -> Diff<'a> {
    let mut interned = HashMap::new();
    let was = keys(old, &mut interned);
    let now = keys(new, &mut interned);
    let mut at = vec![None; interned.len()];
    for (i, &k) in was.iter().enumerate() {
        at[k] = Some(i);
    }
    let (mut added, mut changed) = (Vec::new(), Vec::new());
    for (n, &k) in new.iter().zip(&now) {
        match at[k].take() {
            Some(i) if same(&old[i], n) => {}
            Some(_) => changed.push(n),
            None => added.push(n),
        }
    }
    let removed = old
        .iter()
        .zip(&was)
        .filter(|&(_, &k)| at[k].is_some())
        .map(|(n, _)| Gone {
            id: &n["id"],
            role: &n["role"],
            name: &n["name"],
        })
        .collect();
    let focused = new.iter().find(|n| {
        n["states"]
            .as_array()
            .is_some_and(|s| s.iter().any(|s| s == "focused"))
    });
    Diff {
        head,
        diff: true,
        focused,
        added,
        removed,
        changed,
    }
}

fn keys<'a>(
    nodes: &'a [Value],
    interned: &mut HashMap<(usize, &'a str, &'a str, usize), usize>,
) -> Vec<usize> {
    let mut seen: HashMap<(usize, &str, &str), usize> = HashMap::new();
    let mut keys = Vec::with_capacity(nodes.len());
    for n in nodes {
        let parent = n["parent"]
            .as_u64()
            .and_then(|p| keys.get(usize::try_from(p).ok()?))
            .copied();
        let name = match parent {
            Some(_) => n["name"].as_str().unwrap_or_default(),
            None => "",
        };
        let parent = parent.unwrap_or(usize::MAX);
        let role = n["role"].as_str().unwrap_or_default();
        let nth = seen.entry((parent, role, name)).or_default();
        let next = interned.len();
        keys.push(*interned.entry((parent, role, name, *nth)).or_insert(next));
        *nth += 1;
    }
    keys
}

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_object(), b.as_object()) {
        (Some(a), Some(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| k == "ref" || k == "parent" || b.get(k) == Some(v))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_first_read_is_the_whole_tree_with_diff_false() {
        let mut bases = Baselines::default();
        let got = answer(&mut bases, "a", "w", editor("Doc", "x"));
        assert_eq!(got["diff"], false);
        assert_eq!(got["nodes"].as_array().map(Vec::len), Some(5));
        assert_eq!(got["window"], "w");
    }

    #[test]
    fn a_value_change_is_the_only_entry() {
        let mut bases = Baselines::default();
        answer(&mut bases, "a", "w", editor("Doc", "x"));
        let got = answer(&mut bases, "a", "w", editor("Doc", "xy"));
        assert_eq!(got["diff"], true);
        assert_eq!(got["added"], json!([]));
        assert_eq!(got["removed"], json!([]));
        assert_eq!(got["changed"], json!([editor("Doc", "xy")[3]]));
        assert_eq!(got["focused"], editor("Doc", "xy")[3]);
        let again = answer(&mut bases, "a", "w", editor("Doc", "xy"));
        assert_eq!(again["changed"], json!([]));
    }

    #[test]
    fn an_added_and_a_removed_node_show() {
        let mut bases = Baselines::default();
        answer(&mut bases, "a", "w", editor("Doc", "x"));
        let mut nodes = editor("Doc", "x");
        nodes.remove(4);
        nodes.insert(2, node(9, 1, "button", "Open", &[]));
        let nodes = renumber(nodes);
        let got = answer(&mut bases, "a", "w", nodes.clone());
        assert_eq!(got["diff"], true);
        assert_eq!(got["added"], json!([nodes[2]]));
        assert_eq!(
            got["removed"],
            json!([{"id": "n4", "role": "button", "name": "Save"}])
        );
        assert_eq!(got["changed"], json!([]));
    }

    #[test]
    fn another_window_is_the_whole_tree() {
        let mut bases = Baselines::default();
        answer(&mut bases, "a", "w", editor("Doc", "x"));
        let got = answer(&mut bases, "a", "v", editor("Doc", "xy"));
        assert_eq!(got["diff"], false);
        assert!(got["nodes"].is_array());
    }

    #[test]
    fn a_diff_longer_than_the_tree_is_the_whole_tree() {
        let mut bases = Baselines::default();
        answer(&mut bases, "a", "w", editor("Doc", "x"));
        let renamed: Vec<Value> = editor("Doc", "x")
            .into_iter()
            .map(|mut n| {
                n["role"] = "group".into();
                n
            })
            .collect();
        let got = answer(&mut bases, "a", "w", renamed);
        assert_eq!(got["diff"], false);
        assert_eq!(got["nodes"].as_array().map(Vec::len), Some(5));
        let after = answer(&mut bases, "a", "w", editor("Doc", "x"));
        assert_eq!(after["diff"], false, "the whole tree is the new baseline");
    }

    #[test]
    fn a_retitled_window_changes_only_its_root() {
        let mut bases = Baselines::default();
        answer(&mut bases, "a", "w", editor("Doc", "x"));
        let got = answer(&mut bases, "a", "w", editor("Doc - Edited", "x"));
        assert_eq!(got["diff"], true);
        assert_eq!(got["changed"], json!([editor("Doc - Edited", "x")[0]]));
        assert_eq!(got["added"], json!([]));
        assert_eq!(got["removed"], json!([]));
    }

    #[test]
    fn callers_keep_their_own_reads() {
        let mut bases = Baselines::default();
        answer(&mut bases, "a", "w", editor("Doc", "x"));
        let b = answer(&mut bases, "b", "w", editor("Doc", "xy"));
        assert_eq!(b["diff"], false);
        let a = answer(&mut bases, "a", "w", editor("Doc", "xy"));
        assert_eq!(a["changed"], json!([editor("Doc", "xy")[3]]));
        bases.release("a");
        assert_eq!(
            answer(&mut bases, "a", "w", editor("Doc", "xy"))["diff"],
            false
        );
        assert_eq!(
            answer(&mut bases, "b", "w", editor("Doc", "xy"))["diff"],
            true
        );
    }

    #[test]
    fn a_caller_keeps_its_last_eight_windows_and_all_callers_thirty_two_reads() {
        let mut bases = Baselines::default();
        for w in 0..=CALLER_WINDOWS {
            answer(&mut bases, "a", &w.to_string(), editor("Doc", "x"));
        }
        assert_eq!(
            answer(&mut bases, "a", "0", editor("Doc", "x"))["diff"],
            false
        );
        assert_eq!(
            answer(&mut bases, "a", "2", editor("Doc", "x"))["diff"],
            true
        );
        for c in 0..KEPT_READS {
            answer(&mut bases, &c.to_string(), "w", editor("Doc", "x"));
        }
        assert_eq!(bases.0.len(), KEPT_READS);
        assert!(bases.0.iter().all(|b| b.caller != "a"));
    }

    #[test]
    fn twin_siblings_are_told_apart_by_their_place() {
        let mut bases = Baselines::default();
        let twins = |second: &str| {
            let mut n = vec![
                node(0, -1, "window", "w", &[]),
                node(1, 0, "textbox", "", &[]),
            ];
            n.push(node(2, 0, "textbox", "", &[]));
            n[1]["value"] = "a".into();
            n[2]["value"] = second.into();
            n
        };
        answer(&mut bases, "a", "w", twins("b"));
        let got = answer(&mut bases, "a", "w", twins("c"));
        assert_eq!(got["changed"], json!([twins("c")[2]]));
    }

    fn answer(bases: &mut Baselines, caller: &str, window: &str, nodes: Vec<Value>) -> Value {
        let head = json!({"window": window, "truncated": false});
        let text = bases.answer(caller, window, &head, nodes).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn editor(title: &str, text: &str) -> Vec<Value> {
        let mut nodes = vec![
            node(0, -1, "window", title, &["showing"]),
            node(1, 0, "toolbar", "Tools", &["showing"]),
            node(2, 1, "button", "Bold", &["showing"]),
            node(3, 0, "textbox", "Body", &["focused", "showing"]),
            node(4, 0, "button", "Save", &["showing"]),
        ];
        nodes[3]["value"] = text.into();
        nodes
    }

    fn node(i: usize, parent: i64, role: &str, name: &str, states: &[&str]) -> Value {
        json!({"ref": i, "id": format!("n{i}"), "parent": parent, "depth": usize::from(parent >= 0),
            "role": role, "name": name, "states": states, "bounds": [0, 0, 10, 10]})
    }

    fn renumber(mut nodes: Vec<Value>) -> Vec<Value> {
        for (i, n) in nodes.iter_mut().enumerate() {
            n["ref"] = i.into();
        }
        nodes
    }
}
