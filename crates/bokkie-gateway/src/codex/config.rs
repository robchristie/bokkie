//! The `config.toml` the gateway writes into every user's `CODEX_HOME`.
//!
//! Until Phase 4's sandbox gate passes, every Codex built-in tool stays off:
//! no command execution, no file changes, no shell, and no other built-in
//! tool that reaches outside the conversation. A thread's only tools are
//! the Bokkie tools of its grants ([`crate::tools`]), which run in the
//! gateway. Two things together keep Codex's tools off with the pinned
//! Codex, and [`super::guard`] checks both at startup and for every thread:
//!
//! - the feature flags in [`DISABLED_FEATURES`] and `web_search = "disabled"`;
//! - Bokkie's own model catalog ([`super::catalog`]), which removes the
//!   tools that catalog entries add.
//!
//! Neither alone is enough. The live test in `tests/live_codex.rs` proves
//! the combination by capturing the tools that the app server actually
//! offers a model.

use std::path::Path;

/// The permission profile every thread must run under.
pub const PROFILE: &str = "bokkie";

/// Features that must be off. Each would offer the model a tool that acts
/// outside the conversation: a shell, files, images, the network, other
/// agents, apps, plugins, hooks or memories. The last, `use_agent_identity`,
/// would make each app server store its own agent identity in the shared
/// `auth.json`, where the household's app servers would overwrite each
/// other's.
pub const DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "shell_snapshot",
    "view_image",
    "image_generation",
    "multi_agent",
    "multi_agent_v2",
    "goals",
    "apps",
    "plugins",
    "remote_plugin",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "code_mode_host",
    "sleep_tool",
    "skill_search",
    "tool_suggest",
    "hooks",
    "memories",
    "realtime_conversation",
    "use_agent_identity",
];

/// Where Codex keeps the ChatGPT sign-in: in `auth.json`, so that the
/// household's file and the links to it are what every app server uses,
/// never an operating-system keyring.
pub const CREDENTIALS_STORE: &str = "file";

/// TOML-quotes a string. JSON's string escapes are valid in TOML basic
/// strings.
fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialise")
}

/// Renders the configuration. `data_dir` is denied to the sandbox outright;
/// `catalog` is the gateway's stripped model catalog.
pub fn render(data_dir: &Path, catalog: &Path) -> String {
    let mut out = String::from(
        "# Written by the Bokkie gateway each time it starts this app server.\n\
         # Do not edit: the gateway refuses to serve threads if it changes.\n\n",
    );
    out.push_str(&format!("default_permissions = {}\n", quote(PROFILE)));
    out.push_str("approval_policy = \"never\"\n");
    out.push_str("web_search = \"disabled\"\n");
    out.push_str(&format!(
        "model_catalog_json = {}\n",
        quote(&catalog.display().to_string())
    ));
    // Codex reads AGENTS.md through its sandbox helper, which needs a working
    // Bubblewrap. Threads that run no command need neither, and a workspace
    // file must not add instructions.
    out.push_str("project_doc_max_bytes = 0\n");
    out.push_str("check_for_update_on_startup = false\n");
    out.push_str(&format!(
        "cli_auth_credentials_store = {}\n\n",
        quote(CREDENTIALS_STORE)
    ));
    out.push_str("[features]\n");
    for feature in DISABLED_FEATURES {
        out.push_str(&format!("{feature} = false\n"));
    }
    out.push_str(&format!("\n[permissions.{PROFILE}.filesystem]\n"));
    out.push_str("\":minimal\" = \"read\"\n");
    out.push_str(&format!(
        "{} = \"deny\"\n",
        quote(&data_dir.display().to_string())
    ));
    out.push_str("\":workspace_roots\" = \"write\"\n");
    out.push_str(&format!(
        "\n[permissions.{PROFILE}.network]\nenabled = false\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rendering_names_the_profile_catalog_and_every_feature() {
        let text = render(Path::new("/data"), Path::new("/data/codex/models.json"));
        assert!(text.contains("default_permissions = \"bokkie\"\n"));
        assert!(text.contains("model_catalog_json = \"/data/codex/models.json\"\n"));
        assert!(text.contains("\"/data\" = \"deny\"\n"));
        assert!(text.contains("project_doc_max_bytes = 0\n"));
        assert!(text.contains("cli_auth_credentials_store = \"file\"\n"));
        assert!(text.contains("\nuse_agent_identity = false\n"));
        for feature in DISABLED_FEATURES {
            assert!(
                text.contains(&format!("\n{feature} = false\n")),
                "{feature}"
            );
        }
    }

    #[test]
    fn paths_are_quoted() {
        let text = render(Path::new("/da\"ta"), Path::new("/x\\y.json"));
        assert!(text.contains(r#""/da\"ta" = "deny""#), "{text}");
        assert!(
            text.contains(r#"model_catalog_json = "/x\\y.json""#),
            "{text}"
        );
    }
}
