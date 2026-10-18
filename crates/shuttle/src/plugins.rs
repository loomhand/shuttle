//! Script tools that a directory adds as JSON files: each runs with `script_run`'s contract and its own program in place of Node.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use shuttle_models::config::Config;

use crate::log;

const MAX_DESCRIPTION: usize = 2048;
const SUFFIX: &str = "_run";
const DIR: &str = "tools.d";

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

/// A file the plugin serves as an MCP resource.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub uri: String,
    pub path: PathBuf,
    pub mime: String,
    pub description: String,
}

impl Plugin {
    fn check(&self, builtins: &[&str]) -> Result<(), String> {
        if !is_name(&self.name) {
            return Err(format!("name {:?} is not ^[a-z][a-z0-9_]*_run$", self.name));
        }
        if builtins.contains(&self.name.as_str()) {
            return Err(format!("name {} is a built-in tool", self.name));
        }
        if self.description.chars().count() > MAX_DESCRIPTION {
            return Err(format!("description over {MAX_DESCRIPTION} characters"));
        }
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

/// The plugins of the configuration at `path`: from `[tools] plugins`, else `tools.d` next to the file; none while it fails to load.
pub fn configured(config: &Result<Config, String>, path: &Path, builtins: &[&str]) -> Vec<Plugin> {
    match config {
        Ok(c) => load(
            c.tools
                .plugins
                .as_deref()
                .unwrap_or(&path.with_file_name(DIR)),
            builtins,
        ),
        Err(_) => Vec::new(),
    }
}

/// Whether `name` has a plugin tool's form: `^[a-z][a-z0-9_]*_run$`.
pub fn is_name(name: &str) -> bool {
    name.len() > SUFFIX.len()
        && name.ends_with(SUFFIX)
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Reads the `*.json` plugins in `dir` in file-name order; a missing directory has none, and a file that breaks a rule, names a built-in tool or repeats a name is skipped whole with one log line.
pub fn load(dir: &Path, builtins: &[&str]) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut plugins: Vec<Plugin> = Vec::new();
    for file in files {
        let plugin = std::fs::read(&file)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_slice::<Plugin>(&raw).map_err(|e| e.to_string()))
            .and_then(|p| p.check(builtins).map(|()| p))
            .and_then(|p| {
                if plugins.iter().any(|q| q.name == p.name) {
                    Err(format!("name {} is taken", p.name))
                } else {
                    Ok(p)
                }
            });
        match plugin {
            Ok(p) => plugins.push(p),
            Err(e) => log(format_args!("plugins: skip {}: {e}", file.display())),
        }
    }
    plugins
}

#[cfg(test)]
mod tests {
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
        let got = load(d, &BUILTINS);
        let names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["alpha_run", "beta_run"]);
        assert_eq!(got[0].resources[0].uri, "demo://a/GUIDE.md");
        assert!(load(&d.join("missing"), &BUILTINS).is_empty());
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
                .into_iter()
                .map(|p| p.name)
                .collect()
        };
        assert_eq!(names(&Ok(Config::default())), ["demo_run"]);
        let mut moved = Config::default();
        moved.tools.plugins = Some(elsewhere);
        assert_eq!(names(&Ok(moved)), ["other_run"]);
        assert!(names(&Err("broken".into())).is_empty());
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
