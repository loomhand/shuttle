//! The MCP tools: their schemas, and their answers, the desk tools through the daemon.

use desk_core::wire::{Call, Output};
use serde_json::{Value, json};

use crate::{NAME, VERSION, proxy};

pub const DESK_VERBS: [&str; 16] = [
    "windows", "wait", "snapshot", "find", "read", "press", "act", "at", "key", "type", "click",
    "focus", "close", "launch", "shot", "screen",
];

/// The result of `tools/list`.
pub fn list() -> Value {
    json!({"tools": [
        {
            "name": "gui_plan",
            "description": "Run a desktop-app GUI plan in one call: each step runs on the accessibility tree, is checked, and the plan stops at the first surprise. One op per step: {app}, {menu}, {click}, {check, on}, {set, value}, {type}, {key}, {wait}, {expect}, {verify}, {close: true}. A passing plan can be saved as a flow with save, and replayed with flow and params.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "steps": {"type": "array", "items": {"type": "object"}, "description": "The plan, one op per step"},
                    "flow": {"type": "string", "description": "Replay this saved flow instead of steps"},
                    "params": {"type": "object", "description": "Values for the {{name}} slots"},
                    "save": {"type": "string", "description": "Flow name for this plan"},
                    "start": {"type": "integer", "description": "First step to run, default 0"},
                    "find": {"type": "string", "description": "A request: returns the saved flow that does it"},
                    "timeout_s": {"type": "integer", "description": "Default 90, max 110"}
                }
            }
        },
        {
            "name": "script_run",
            "description": "Run one JavaScript or TypeScript ES module on the bundled Node runtime with the desk API as the global `desk`: windows, accessibility snapshots, find and press elements, keys, typing, clicks, screen crops, and decide, look, ground and ask model calls. A module that exports main(params) is called with params and its return value comes back as result, with the logs, stderr and any error.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "The script as an ES module"},
                    "script": {"type": "string", "description": "Path of a script file"},
                    "params": {"type": "object", "description": "Passed to the script's exported main(params)"},
                    "timeout_s": {"type": "integer", "description": "Default 90, max 110"}
                }
            }
        },
        {
            "name": "desk",
            "description": "One desk call on this desktop: list windows or wait for them to change, snapshot or search a window's accessibility tree, read, press or act on an element, send keys, type, click, focus, close or launch, or crop the screen.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "verb": {"type": "string", "enum": DESK_VERBS},
                    "args": {"type": "object", "description": "The verb's arguments, as the desk wire takes them"}
                },
                "required": ["verb"]
            }
        },
        {
            "name": "screenshot",
            "description": "A PNG of the screen, or of the rectangle x, y, w, h in screen pixels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "w": {"type": "integer"},
                    "h": {"type": "integer"}
                }
            }
        },
        {
            "name": "status",
            "description": "This host's version, OS and architecture.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ]})
}

/// Answers a `tools/call` of `name` with `args`: an MCP tool result, or `None` for an unknown tool.
pub async fn call(name: &str, args: Value, endpoint: &str) -> Option<Value> {
    if name == "status" {
        return Some(result(status().to_string(), false));
    }
    let (tool, args) = match route(name, args)? {
        Ok(routed) => routed,
        Err(e) => return Some(result(e, true)),
    };
    let call = Call {
        agent_id: NAME.to_owned(),
        tool,
        args,
    };
    Some(match proxy::call(endpoint, &call).await {
        Ok(out) => output(out),
        Err(e) => result(e, true),
    })
}

fn route(name: &str, mut args: Value) -> Option<Result<(String, Value), String>> {
    Some(Ok(match name {
        "desk" => match args["verb"].as_str().filter(|v| DESK_VERBS.contains(v)) {
            Some(verb) => (
                format!("desk_{verb}"),
                args.get_mut("args").map_or_else(|| json!({}), Value::take),
            ),
            None => {
                return Some(Err(format!(
                    "desk needs a verb of {}",
                    DESK_VERBS.join(", ")
                )));
            }
        },
        "screenshot" => ("desk_shot".to_owned(), args),
        "script_run" => (name.to_owned(), absolute_script(args)),
        "gui_plan" => (name.to_owned(), args),
        _ => return None,
    }))
}

// a relative script path names a file under the agent's working directory, which the daemon does not share
fn absolute_script(mut args: Value) -> Value {
    let relative = args["script"]
        .as_str()
        .map(std::path::Path::new)
        .filter(|p| p.is_relative());
    if let (Some(path), Ok(cwd)) = (relative, std::env::current_dir()) {
        args["script"] = cwd.join(path).to_string_lossy().into_owned().into();
    }
    args
}

fn status() -> Value {
    json!({
        "name": NAME,
        "version": VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
}

fn output(out: Output) -> Value {
    let mut r = result(out.text, out.is_error);
    if let (Some(png), Some(content)) = (out.image_b64, r["content"].as_array_mut()) {
        let mut image = json!({"type": "image", "mimeType": "image/png"});
        image["data"] = png.into();
        content.push(image);
    }
    r
}

fn result(text: String, is_error: bool) -> Value {
    let mut out = json!({"content": [{"type": "text"}], "isError": is_error});
    out["content"][0]["text"] = text.into();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_mcp_tool_reaches_the_safety_controls() {
        let tools = list();
        let names: Vec<&str> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        let mut routed = Vec::new();
        for name in names.iter().chain(&["safety_stop", "safety_resume"]) {
            let verbs = DESK_VERBS
                .iter()
                .chain(&["../safety_stop", "safety_resume"]);
            for verb in verbs {
                if let Some(Ok((tool, _))) = route(name, json!({"verb": verb})) {
                    routed.push(tool);
                }
            }
        }
        assert!(routed.len() > DESK_VERBS.len(), "{routed:?}");
        assert!(!routed.iter().any(|t| t.contains("safety")), "{routed:?}");
    }

    #[test]
    fn the_desk_tool_takes_every_verb_the_desk_answers() {
        let answered: Vec<&str> = include_str!("../../desk/src/desk.rs")
            .lines()
            .filter(|l| l.contains("=> self."))
            .filter_map(|l| l.trim().strip_prefix("\"desk_")?.split_once('"'))
            .map(|(verb, _)| verb)
            .collect();
        assert!(answered.contains(&"windows"), "{answered:?}");
        for verb in answered {
            assert!(DESK_VERBS.contains(&verb), "{verb}");
            assert!(
                matches!(route("desk", json!({"verb": verb})), Some(Ok(_))),
                "{verb}"
            );
        }
    }
}
