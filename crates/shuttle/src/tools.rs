//! The MCP tools: their schemas, and their answers, the desk tools through the daemon.

use desk_core::wire::{Call, Output};
use serde_json::{Value, json};

use crate::{NAME, VERSION, proxy};

pub const DESK_VERBS: [&str; 15] = [
    "windows", "snapshot", "find", "read", "press", "act", "at", "key", "type", "click", "focus",
    "close", "launch", "shot", "screen",
];

const NOT_YET: &str = "this release lists the tool but does not run it yet; see docs/platforms.md for when it lands on this OS";

/// The result of `tools/list`.
pub fn list() -> Value {
    json!({"tools": [
        {
            "name": "gui_plan",
            "description": "Run a desktop-app GUI plan in one call: each step runs on the accessibility tree, is checked, and the plan stops at the first surprise. One op per step: {app}, {menu}, {click}, {check, on}, {set, value}, {type}, {key}, {wait}, {verify}, {close: true}. A passing plan can be saved as a flow with save, and replayed with flow and params.",
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
            "description": "Run one JavaScript or TypeScript ES module on the bundled Node runtime with the desk API: windows, accessibility snapshots, find and press elements, keys, typing, clicks, screen crops and model calls. A module that exports main(params) is called with params and its return value comes back as result.",
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
            "description": "One desk call on this desktop: list windows, snapshot or search a window's accessibility tree, read, press or act on an element, send keys, type, click, focus, close or launch, or crop the screen.",
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
            "description": "This host's version, OS and architecture, and which tools this release runs.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ]})
}

/// Answers a `tools/call` of `name` with `args`: an MCP tool result, or `None` for an unknown tool.
pub async fn call(name: &str, mut args: Value, endpoint: &str) -> Option<Value> {
    let (tool, args) = match name {
        "status" => return Some(result(status().to_string(), false)),
        "gui_plan" | "script_run" => return Some(result(NOT_YET.to_owned(), true)),
        "desk" => match args["verb"].as_str().filter(|v| DESK_VERBS.contains(v)) {
            Some(verb) => (
                format!("desk_{verb}"),
                args.get_mut("args").map_or_else(|| json!({}), Value::take),
            ),
            None => {
                return Some(result(
                    format!("desk needs a verb of {}", DESK_VERBS.join(", ")),
                    true,
                ));
            }
        },
        "screenshot" => ("desk_shot".to_owned(), args),
        _ => return None,
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

fn status() -> Value {
    json!({
        "name": NAME,
        "version": VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "runs": ["status", "desk", "screenshot"],
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
