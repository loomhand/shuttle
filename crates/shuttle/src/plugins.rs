//! Tools that a directory adds as JSON files: script tools that run with `script_run`'s contract and their own program in place of Node, and services, long-lived programs that serve their own tools.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use shuttle_models::config::Config;

use crate::log;

const MAX_DESCRIPTION: usize = 2048;
const MAX_SERVICE_NAME: usize = 32;
const MAX_TOOL_NAME: usize = 64;
const SUFFIX: &str = "_run";
const DIR: &str = "tools.d";
const SERVICE_KEY: &str = "service";
const RESERVED: [&str; 3] = ["desk_", "model_", "safety_"];

/// The plugins of one directory: the script tools and the services.
#[derive(Debug, Default)]
pub struct Plugins {
    pub runs: Vec<Plugin>,
    pub services: Vec<Service>,
}

impl Plugins {
    /// Whether the MCP server lists the service tool `name`.
    pub fn lists(&self, name: &str) -> bool {
        self.services
            .iter()
            .flat_map(|s| &s.tools)
            .any(|t| t.mcp && t.name == name)
    }
}

/// One plugin tool, as its JSON file spells it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    /// The program that runs the tool's scripts in place of Node.
    pub bin: PathBuf,
    #[serde(default)]
    pub resources: Vec<Resource>,
}

impl Plugin {
    fn check(&self, builtins: &[&str]) -> Result<(), String> {
        if !is_name(&self.name) {
            return Err(format!("name {:?} is not ^[a-z][a-z0-9_]*_run$", self.name));
        }
        if builtins.contains(&self.name.as_str()) {
            return Err(format!("name {} is a built-in tool", self.name));
        }
        check_description(&self.description)?;
        if !self.bin.is_absolute() {
            return Err(format!("bin {} is not absolute", self.bin.display()));
        }
        if let Some(r) = self.resources.iter().find(|r| !r.path.is_absolute()) {
            return Err(format!(
                "resource path {} is not absolute",
                r.path.display()
            ));
        }
        Ok(())
    }
}

/// A file the plugin serves as an MCP resource.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub uri: String,
    pub path: PathBuf,
    pub mime: String,
    pub description: String,
}

/// A long-lived program that serves `tools` on the unix socket or Windows named pipe the daemon names in `SHUTTLE_SERVICE_SOCKET`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub service: String,
    pub bin: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub tools: Vec<ServiceTool>,
}

impl Service {
    fn check(&self, builtins: &[&str]) -> Result<(), String> {
        if !is_word(&self.service) || self.service.len() > MAX_SERVICE_NAME {
            return Err(format!(
                "service {:?} is not ^[a-z][a-z0-9_]*$ of at most {MAX_SERVICE_NAME} characters",
                self.service
            ));
        }
        if !self.bin.is_absolute() {
            return Err(format!("bin {} is not absolute", self.bin.display()));
        }
        if self.tools.is_empty() {
            return Err("no tools".to_owned());
        }
        for (i, t) in self.tools.iter().enumerate() {
            t.check(builtins)?;
            if self.tools[..i].iter().any(|u| u.name == t.name) {
                return Err(format!("tool {} is named twice", t.name));
            }
        }
        Ok(())
    }
}

/// One tool a service serves; `mcp = false` routes it on the desk wire only.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default = "listed")]
    pub mcp: bool,
}

impl ServiceTool {
    fn check(&self, builtins: &[&str]) -> Result<(), String> {
        let name = self.name.as_str();
        if !is_word(name) || name.len() > MAX_TOOL_NAME {
            return Err(format!(
                "tool {name:?} is not ^[a-z][a-z0-9_]*$ of at most {MAX_TOOL_NAME} characters"
            ));
        }
        if builtins.contains(&name) {
            return Err(format!("tool {name} is a built-in tool"));
        }
        if let Some(p) = RESERVED.iter().find(|p| name.starts_with(*p)) {
            return Err(format!("tool {name} takes the reserved prefix {p}"));
        }
        if name.ends_with(SUFFIX) {
            return Err(format!(
                "tool {name} ends in {SUFFIX}, which script tools take"
            ));
        }
        check_description(&self.description)?;
        if self.input_schema["type"] != "object" {
            return Err(format!("tool {name}: input_schema is not an object schema"));
        }
        Ok(())
    }
}

/// The plugins of the configuration at `path`: from `[tools] plugins`, relative to the file's directory, else `tools.d` beside it; none while it fails to load.
pub fn configured(config: &Result<Config, String>, path: &Path, builtins: &[&str]) -> Plugins {
    let Ok(c) = config else {
        return Plugins::default();
    };
    let dir = path.with_file_name(c.tools.plugins.as_deref().unwrap_or(Path::new(DIR)));
    load(&dir, builtins)
}

/// Whether `name` has a plugin tool's form: `^[a-z][a-z0-9_]*_run$`.
pub fn is_name(name: &str) -> bool {
    name.len() > SUFFIX.len() && name.ends_with(SUFFIX) && is_word(name)
}

/// Reads the `*.json` plugins in `dir` in file-name order: a file with a `service` key is a service, any other a script tool. A missing directory has none, and a file that breaks a rule, names a built-in tool or repeats a name is skipped whole with one log line.
pub fn load(dir: &Path, builtins: &[&str]) -> Plugins {
    let mut plugins = Plugins::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return plugins;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for file in files {
        if let Err(e) = add(&mut plugins, &file, builtins) {
            log(format_args!("plugins: skip {}: {e}", file.display()));
        }
    }
    plugins
}

fn add(plugins: &mut Plugins, file: &Path, builtins: &[&str]) -> Result<(), String> {
    let raw = std::fs::read(file).map_err(|e| e.to_string())?;
    let spec: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if spec.get(SERVICE_KEY).is_none() {
        let p: Plugin = serde_json::from_value(spec).map_err(|e| e.to_string())?;
        p.check(builtins)?;
        if plugins.runs.iter().any(|q| q.name == p.name) {
            return Err(format!("name {} is taken", p.name));
        }
        plugins.runs.push(p);
        return Ok(());
    }
    let s: Service = serde_json::from_value(spec).map_err(|e| e.to_string())?;
    s.check(builtins)?;
    if plugins.services.iter().any(|t| t.service == s.service) {
        return Err(format!("service {} is taken", s.service));
    }
    let taken = |name: &str| {
        plugins
            .services
            .iter()
            .flat_map(|t| &t.tools)
            .any(|t| t.name == name)
    };
    if let Some(t) = s.tools.iter().find(|t| taken(&t.name)) {
        return Err(format!("tool {} is taken", t.name));
    }
    plugins.services.push(s);
    Ok(())
}

fn is_word(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn check_description(description: &str) -> Result<(), String> {
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(format!("description over {MAX_DESCRIPTION} characters"));
    }
    Ok(())
}

fn listed() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const BUILTINS: [&str; 2] = ["script_run", "gui_plan"];

    fn write(dir: &Path, file: &str, body: &str) {
        let root = if cfg!(windows) { "\"C:\\\\" } else { "\"/" };
        std::fs::write(dir.join(file), body.replace("\"/", root)).unwrap();
    }

    #[test]
    fn loads_valid_plugins_in_file_name_order_and_skips_every_broken_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(
            d,
            "20-b.json",
            r#"{"name": "beta_run", "description": "b", "bin": "/usr/bin/b"}"#,
        );
        write(
            d,
            "10-a.json",
            r#"{"name": "alpha_run", "description": "a", "bin": "/usr/bin/a", "resources": [{"uri": "demo://a/GUIDE.md", "path": "/opt/a/GUIDE.md", "mime": "text/markdown", "description": "g"}]}"#,
        );
        for (file, body) in [
            (
                "30-badname.json",
                r#"{"name": "Gamma", "description": "c", "bin": "/usr/bin/c"}"#,
            ),
            (
                "31-builtin.json",
                r#"{"name": "script_run", "description": "c", "bin": "/usr/bin/c"}"#,
            ),
            (
                "32-relative.json",
                r#"{"name": "rel_run", "description": "c", "bin": "bin/c"}"#,
            ),
            (
                "33-dup.json",
                r#"{"name": "alpha_run", "description": "c", "bin": "/usr/bin/c"}"#,
            ),
            ("35-broken.json", "{"),
            (
                "36-unknown-field.json",
                r#"{"name": "typo_run", "description": "c", "bin": "/usr/bin/c", "resource": []}"#,
            ),
            (
                "37-half-resource.json",
                r#"{"name": "half_run", "description": "c", "bin": "/usr/bin/c", "resources": [{"uri": "demo://h", "path": "/opt/h.md"}]}"#,
            ),
            (
                "38-relative-resource.json",
                r#"{"name": "relres_run", "description": "c", "bin": "/usr/bin/c", "resources": [{"uri": "demo://r", "path": "r.md", "mime": "text/markdown", "description": "r"}]}"#,
            ),
            (
                "40-c.txt",
                r#"{"name": "text_run", "description": "c", "bin": "/usr/bin/c"}"#,
            ),
        ] {
            write(d, file, body);
        }
        write(
            d,
            "34-long.json",
            &format!(
                r#"{{"name": "long_run", "description": "{}", "bin": "/usr/bin/c"}}"#,
                "x".repeat(MAX_DESCRIPTION + 1)
            ),
        );
        let got = load(d, &BUILTINS).runs;
        let names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["alpha_run", "beta_run"]);
        assert_eq!(got[0].resources[0].uri, "demo://a/GUIDE.md");
        let missing = load(&d.join("missing"), &BUILTINS);
        assert!(missing.runs.is_empty() && missing.services.is_empty());
    }

    #[test]
    fn the_configured_directory_is_tools_d_beside_the_file_unless_tools_plugins_names_one() {
        let dir = tempfile::tempdir().unwrap();
        let spec = r#"{"name": "demo_run", "description": "d", "bin": "/usr/bin/d"}"#;
        std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
        write(&dir.path().join(DIR), "demo.json", spec);
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        write(
            &elsewhere,
            "other.json",
            &spec.replace("demo_run", "other_run"),
        );
        let path = dir.path().join("config.toml");
        let names = |config: &Result<Config, String>| -> Vec<String> {
            configured(config, &path, &BUILTINS)
                .runs
                .into_iter()
                .map(|p| p.name)
                .collect()
        };
        assert_eq!(names(&Ok(Config::default())), ["demo_run"]);
        let mut moved = Config::default();
        moved.tools.plugins = Some(elsewhere);
        assert_eq!(names(&Ok(moved)), ["other_run"]);
        let mut relative = Config::default();
        relative.tools.plugins = Some("elsewhere".into());
        assert_eq!(names(&Ok(relative)), ["other_run"]);
        assert!(names(&Err("broken".into())).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_service_file_loads_its_tools_and_a_broken_rule_or_a_taken_name_skips_it_whole() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let tool = |name: &str| json!({"name": name, "description": "d", "input_schema": {"type": "object"}});
        let service = |name: &str, tools: Value| {
            json!({"service": name, "bin": "/usr/bin/s", "tools": tools}).to_string()
        };
        write(
            d,
            "10-run.json",
            r#"{"name": "alpha_run", "description": "a", "bin": "/usr/bin/a"}"#,
        );
        let mut files = json!({"service": "files", "bin": "/usr/bin/f", "args": ["--serve"], "tools": [tool("read_file"), tool("exec")]});
        files["tools"][1]["mcp"] = false.into();
        write(d, "20-files.json", &files.to_string());
        let mut bad_schema = tool("other");
        bad_schema["input_schema"] = json!({"type": "string"});
        let mut camel = tool("camel");
        camel["inputSchema"] = json!({"type": "object"});
        for (file, body) in [
            (
                "21-same-service.json",
                service("files", json!([tool("other")])),
            ),
            (
                "22-taken-tool.json",
                service("other", json!([tool("read_file")])),
            ),
            (
                "23-builtin.json",
                service("other", json!([tool("gui_plan")])),
            ),
            (
                "24-reserved.json",
                service("other", json!([tool("desk_files")])),
            ),
            (
                "25-run-suffix.json",
                service("other", json!([tool("other_run")])),
            ),
            ("26-schema.json", service("other", json!([bad_schema]))),
            ("27-no-tools.json", service("other", json!([]))),
            (
                "28-twice.json",
                service("other", json!([tool("x"), tool("x")])),
            ),
            ("29-unknown-field.json", service("other", json!([camel]))),
            ("30-bad-name.json", service("Other", json!([tool("other")]))),
            (
                "31-relative.json",
                json!({"service": "other", "bin": "bin/s", "tools": [tool("other")]}).to_string(),
            ),
        ] {
            write(d, file, &body);
        }
        write(
            d,
            "40-browser.json",
            &service("browser", json!([tool("browser_open")])),
        );
        let got = load(d, &BUILTINS);
        let names: Vec<&str> = got.services.iter().map(|s| s.service.as_str()).collect();
        assert_eq!(names, ["files", "browser"]);
        assert_eq!(got.runs.len(), 1);
        let files = &got.services[0];
        assert_eq!(files.args, ["--serve"]);
        let tools: Vec<(&str, bool)> = files
            .tools
            .iter()
            .map(|t| (t.name.as_str(), t.mcp))
            .collect();
        assert_eq!(tools, [("read_file", true), ("exec", false)]);
        assert!(got.lists("read_file"));
        assert!(!got.lists("exec"));
    }

    #[cfg(windows)]
    #[test]
    fn a_service_file_loads_on_windows_with_an_absolute_bin() {
        let dir = tempfile::tempdir().unwrap();
        let spec = |bin: &str| {
            json!({"service": "files", "bin": bin, "tools": [{"name": "read_file", "description": "d", "input_schema": {"type": "object"}}]}).to_string()
        };
        write(
            dir.path(),
            "files.json",
            &spec(r"C:\Windows\System32\cmd.exe"),
        );
        let got = load(dir.path(), &BUILTINS);
        assert_eq!(got.services.len(), 1);
        assert!(got.lists("read_file"));
        write(dir.path(), "files.json", &spec("cmd.exe"));
        assert!(load(dir.path(), &BUILTINS).services.is_empty());
    }

    #[test]
    fn a_plugin_name_ends_in_run_and_has_lowercase_letters_digits_and_underscores() {
        for ok in ["demo_run", "a1_b_run", "x_run"] {
            assert!(is_name(ok), "{ok}");
        }
        for bad in [
            "_run",
            "run",
            "Demo_run",
            "demo-run",
            "demo_runs",
            "1x_run",
            "",
        ] {
            assert!(!is_name(bad), "{bad}");
        }
    }
}
