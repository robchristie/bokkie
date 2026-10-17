//! Bokkie's own model catalog.
//!
//! Codex decides some of a model's tools from the model's catalog entry, not
//! from feature flags: with every relevant feature disabled, the pinned
//! release still offers `apply_patch` (file changes), a JavaScript `exec`
//! tool that orchestrates nested tools, and multi-agent spawning, because the
//! bundled entries set `apply_patch_tool_type`, `tool_mode` and
//! `multi_agent_version`.
//!
//! So the gateway exports the pinned binary's bundled catalog with
//! `codex debug models --bundled`, strips the entries' tool metadata, writes
//! the result to a file it owns, and points every app server at it with
//! `model_catalog_json`. A static catalog also stops Codex replacing it with
//! a remotely fetched one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::guard::Refusal;

/// Entry fields that add tools, and the value each must have.
const TOOL_FIELDS: &[(&str, Value)] = &[
    ("apply_patch_tool_type", Value::Null),
    ("tool_mode", Value::Null),
    ("multi_agent_version", Value::Null),
    ("supports_search_tool", Value::Bool(false)),
    ("shell_type", Value::String(String::new())),
];

/// The value of `shell_type` that offers no shell tool.
pub const SHELL_TYPE_DISABLED: &str = "disabled";

/// The catalog file every app server uses.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub path: PathBuf,
    pub slugs: BTreeSet<String>,
}

fn expected(field: &str, value: &Value) -> Value {
    if field == "shell_type" {
        Value::String(SHELL_TYPE_DISABLED.into())
    } else {
        value.clone()
    }
}

/// Removes every tool-adding field's effect from a catalog. The input is the
/// JSON that `codex debug models --bundled` prints.
pub fn strip_tools(mut catalog: Value) -> Result<Value, Refusal> {
    let models = catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .filter(|models| !models.is_empty())
        .ok_or_else(|| Refusal::new("the bundled model catalog has no models"))?;
    for model in models {
        let entry = model
            .as_object_mut()
            .ok_or_else(|| Refusal::new("a catalog entry is not an object"))?;
        for (field, value) in TOOL_FIELDS {
            entry.insert((*field).into(), expected(field, value));
        }
        entry.insert("experimental_supported_tools".into(), Value::Array(vec![]));
    }
    Ok(catalog)
}

/// Checks a catalog that an app server will use: every entry has a slug and
/// none adds tools. Returns the slugs.
pub fn check(catalog: &Value) -> Result<BTreeSet<String>, Refusal> {
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .filter(|models| !models.is_empty())
        .ok_or_else(|| Refusal::new("the model catalog has no models"))?;
    let mut slugs = BTreeSet::new();
    for model in models {
        let slug = model
            .get("slug")
            .and_then(Value::as_str)
            .ok_or_else(|| Refusal::new("a catalog entry has no slug"))?;
        for (field, value) in TOOL_FIELDS {
            if model.get(*field) != Some(&expected(field, value)) {
                return Err(Refusal::new(format!(
                    "catalog entry {slug} sets {field} to {}",
                    model.get(*field).unwrap_or(&Value::Null)
                )));
            }
        }
        if model.get("experimental_supported_tools") != Some(&Value::Array(vec![])) {
            return Err(Refusal::new(format!(
                "catalog entry {slug} lists experimental tools"
            )));
        }
        slugs.insert(slug.to_owned());
    }
    Ok(slugs)
}

/// Writes a stripped catalog to `path`, private to the gateway, and checks
/// what was written.
pub fn write(path: &Path, bundled: Value) -> Result<Catalog, Refusal> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let stripped = strip_tools(bundled)?;
    let bytes = serde_json::to_vec_pretty(&stripped).map_err(Refusal::new)?;
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(Refusal::new)?;
    file.write_all(&bytes).map_err(Refusal::new)?;
    file.sync_all().map_err(Refusal::new)?;
    std::fs::rename(&temporary, path).map_err(Refusal::new)?;
    load(path)
}

fn private_dir(path: &Path) -> Result<(), Refusal> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(Refusal::new)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(Refusal::new)
}

/// Reads back and checks the catalog at `path`.
pub fn load(path: &Path) -> Result<Catalog, Refusal> {
    let bytes = std::fs::read(path).map_err(Refusal::new)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(Refusal::new)?;
    Ok(Catalog {
        path: path.to_owned(),
        slugs: check(&value)?,
    })
}

/// Exports the bundled catalog from the `codex` binary. Runs it with an
/// empty environment and a scratch `CODEX_HOME`, so no user's configuration
/// or credentials take part.
pub async fn export_bundled(program: &Path, scratch_home: &Path) -> Result<Value, Refusal> {
    private_dir(scratch_home)?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(["debug", "models", "--bundled"])
        .env_clear()
        .env("CODEX_HOME", scratch_home)
        .env("HOME", scratch_home)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .map_err(|_| Refusal::new("codex debug models timed out"))?
        .map_err(Refusal::new)?;
    if !output.status.success() {
        return Err(Refusal::new(format!(
            "codex debug models exited with {}",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(Refusal::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bundled() -> Value {
        json!({"models": [
            {"slug": "gpt-6.1-sol", "apply_patch_tool_type": "freeform",
             "tool_mode": "code_mode_only", "multi_agent_version": "v2",
             "experimental_supported_tools": ["clock"], "supports_search_tool": true,
             "shell_type": "unified_exec", "display_name": "GPT-6.1-Sol"},
            {"slug": "gpt-5.5", "apply_patch_tool_type": "freeform",
             "experimental_supported_tools": [], "supports_search_tool": true,
             "shell_type": "shell_command"},
        ]})
    }

    #[test]
    fn stripping_removes_every_tool_field_and_keeps_the_rest() {
        let stripped = strip_tools(bundled()).unwrap();
        let slugs = check(&stripped).unwrap();
        assert_eq!(
            slugs.into_iter().collect::<Vec<_>>(),
            ["gpt-5.5", "gpt-6.1-sol"]
        );
        let sol = &stripped["models"][0];
        assert_eq!(sol["display_name"], "GPT-6.1-Sol");
        assert_eq!(sol["tool_mode"], Value::Null);
        assert_eq!(sol["shell_type"], "disabled");
        // A field that was absent is now present and null.
        assert!(
            stripped["models"][1]
                .as_object()
                .unwrap()
                .contains_key("tool_mode")
        );
    }

    #[test]
    fn the_check_refuses_any_tool_metadata() {
        assert!(check(&bundled()).is_err());
        for (field, value) in [
            ("apply_patch_tool_type", json!("freeform")),
            ("tool_mode", json!("code_mode_only")),
            ("multi_agent_version", json!("v1")),
            ("supports_search_tool", json!(true)),
            ("shell_type", json!("unified_exec")),
            ("experimental_supported_tools", json!(["clock"])),
        ] {
            let mut catalog = strip_tools(bundled()).unwrap();
            catalog["models"][1][field] = value;
            let refusal = check(&catalog).unwrap_err();
            assert!(refusal.to_string().contains("gpt-5.5"), "{refusal}");
        }
        assert!(check(&json!({"models": []})).is_err());
        assert!(strip_tools(json!({})).is_err());
    }

    #[test]
    fn written_catalogs_are_private_and_checked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex/models.json");
        let catalog = write(&path, bundled()).unwrap();
        assert_eq!(catalog.slugs.len(), 2);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Tampering is caught on reload.
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["models"][0]["tool_mode"] = json!("code_mode_only");
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(load(&path).is_err());
    }
}
