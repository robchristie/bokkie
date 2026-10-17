//! The qualification data layout, Codex configuration and probe plans.
//!
//! It mirrors the plan's layout: `/data/users/<id>/codex` is the user's
//! `CODEX_HOME`, `/data/users/<id>/workspace` the Bots' computer, and
//! `/data/bokkie.db` stands in for Bokkie's database.

use std::path::{Path, PathBuf};

use crate::{Expect, PathSpec, PeerSpec, ProbeSpec, USERS_DIR_LABEL};

/// The permission profile the probe qualifies: system read, own workspace
/// write, nothing else under `/data`, no network.
pub const PROFILE: &str = "bokkie";
/// The same profile with sandbox network access.
pub const PROFILE_NET: &str = "bokkie-net";
/// Codex's built-in default profile, kept as a control.
pub const PROFILE_BUILTIN: &str = ":workspace";

#[derive(Debug, Clone)]
pub struct Layout {
    pub data: PathBuf,
}

impl Layout {
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self { data: data.into() }
    }

    pub fn users_dir(&self) -> PathBuf {
        self.data.join("users")
    }

    pub fn user(&self, user: &str) -> PathBuf {
        self.users_dir().join(user)
    }

    pub fn codex_home(&self, user: &str) -> PathBuf {
        self.user(user).join("codex")
    }

    pub fn workspace(&self, user: &str) -> PathBuf {
        self.user(user).join("workspace")
    }

    /// The `HOME` given to the user's app server and sandboxed commands.
    pub fn home(&self, user: &str) -> PathBuf {
        self.user(user).join("home")
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("bokkie.db")
    }

    /// The probe's own state, standing in for gateway state.
    pub fn state(&self, user: &str) -> PathBuf {
        self.data.join("qualify").join(user)
    }
}

/// The `config.toml` written to every qualification `CODEX_HOME`.
///
/// `extra_read` adds readable roots, for running the harness outside the
/// image where Codex and the probe are not under `/usr`.
pub fn codex_config(layout: &Layout, extra_read: &[String]) -> String {
    let data = layout.data.display();
    let extra: String = extra_read
        .iter()
        .map(|path| format!("\"{path}\" = \"read\"\n"))
        .collect();
    format!(
        r#"# Written by bokkie-qualify. Phase 0 qualification only.
cli_auth_credentials_store = "file"
check_for_update_on_startup = false
model_reasoning_effort = "low"
default_permissions = "{PROFILE}"

[analytics]
enabled = false

[permissions.{PROFILE}]
description = "System read, own workspace write, no other /data access, no network"

[permissions.{PROFILE}.filesystem]
":minimal" = "read"
{extra}"{data}" = "deny"
":workspace_roots" = "write"

[permissions.{PROFILE}.network]
enabled = false

[permissions.{PROFILE_NET}]
description = "As {PROFILE}, with network access"
extends = "{PROFILE}"

[permissions.{PROFILE_NET}.network]
enabled = true
"#
    )
}

/// What the plan for one sandboxed probe run needs to know.
#[derive(Debug, Clone)]
pub struct PlanInput<'a> {
    pub layout: &'a Layout,
    /// The user whose sandbox runs the probe.
    pub user: &'a str,
    /// Every user on this data volume, including `user`.
    pub users: &'a [String],
    pub peers: Vec<PeerSpec>,
    pub outer_pid1_cmdline: String,
    /// `host:port` for the network check, and whether it should connect.
    pub connect: Option<(String, Expect)>,
}

/// Labels for writes that may succeed only into storage private to the
/// sandbox. The harness checks whether the file is visible outside.
pub const SHARED_WRITE_PREFIX: &str = "shared:";

/// Builds the probe specification: real reads and writes for invariants 1-3
/// and 5, and peer processes for invariant 4.
pub fn probe_spec(input: &PlanInput<'_>) -> ProbeSpec {
    let layout = input.layout;
    let me = input.user;
    let others: Vec<&String> = input.users.iter().filter(|u| *u != me).collect();
    let path = |p: PathBuf| p.display().to_string();
    let spec = |label: String, p: PathBuf, expect| PathSpec {
        label,
        path: path(p),
        expect,
    };

    let mut reads = vec![
        // Sanity: the sandbox can still read its own workspace and the system.
        spec("own:workspace".into(), layout.workspace(me), Expect::Allow),
        spec(
            "own:system".into(),
            PathBuf::from("/usr/bin"),
            Expect::Allow,
        ),
    ];
    for user in input.users {
        let home = layout.codex_home(user);
        reads.push(spec(
            format!("inv1:{user}-codex-home"),
            home.clone(),
            Expect::Deny,
        ));
        reads.push(spec(
            format!("inv1:{user}-auth"),
            secret_file(&home),
            Expect::Deny,
        ));
    }
    for other in &others {
        reads.push(spec(
            format!("inv3:{other}-workspace"),
            layout.workspace(other),
            Expect::Deny,
        ));
        reads.push(spec(
            format!("inv3:{other}-workspace-file"),
            layout.workspace(other).join("README.md"),
            Expect::Deny,
        ));
        reads.push(spec(
            format!("inv3:{other}-home"),
            layout.home(other),
            Expect::Deny,
        ));
    }
    reads.push(spec(
        "inv3:database".into(),
        layout.database(),
        Expect::Deny,
    ));
    reads.push(spec(
        USERS_DIR_LABEL.into(),
        layout.users_dir(),
        Expect::Deny,
    ));
    reads.push(spec(
        "inv3:gateway-state".into(),
        layout.state(me),
        Expect::Deny,
    ));

    let mut writes = vec![
        spec("own:workspace".into(), layout.workspace(me), Expect::Allow),
        spec(
            "inv2:own-codex-home".into(),
            layout.codex_home(me),
            Expect::Deny,
        ),
        spec("inv2:own-home".into(), layout.home(me), Expect::Deny),
        spec("inv2:data".into(), layout.data.clone(), Expect::Deny),
        spec("inv2:database".into(), layout.database(), Expect::Deny),
        spec("inv2:gateway-state".into(), layout.state(me), Expect::Deny),
        spec(
            format!("{SHARED_WRITE_PREFIX}root"),
            PathBuf::from("/"),
            Expect::Deny,
        ),
        spec(
            "inv2:usr".into(),
            PathBuf::from("/usr/local/bin"),
            Expect::Deny,
        ),
        // Shared temporary storage: allowed only when private to the sandbox.
        spec(
            format!("{SHARED_WRITE_PREFIX}tmp"),
            PathBuf::from("/tmp"),
            Expect::Deny,
        ),
        spec(
            format!("{SHARED_WRITE_PREFIX}dev-shm"),
            PathBuf::from("/dev/shm"),
            Expect::Deny,
        ),
    ];
    for other in &others {
        writes.push(spec(
            format!("inv2:{other}-workspace"),
            layout.workspace(other),
            Expect::Deny,
        ));
        writes.push(spec(
            format!("inv2:{other}-codex-home"),
            layout.codex_home(other),
            Expect::Deny,
        ));
    }

    ProbeSpec {
        keep_written: writes
            .iter()
            .filter(|w| w.label.starts_with(SHARED_WRITE_PREFIX))
            .map(|w| w.label.clone())
            .collect(),
        reads,
        writes,
        peers: input.peers.clone(),
        outer_pid1_cmdline: input.outer_pid1_cmdline.clone(),
        other_users: others.iter().map(|u| (*u).clone()).collect(),
        connect: input.connect.as_ref().map(|(target, expect)| PathSpec {
            label: "inv5:connect".into(),
            path: target.clone(),
            expect: *expect,
        }),
    }
}

/// `auth.json` when the user has signed in, otherwise the canary file that
/// stands in for it.
fn secret_file(codex_home: &Path) -> PathBuf {
    let auth = codex_home.join("auth.json");
    if auth.exists() {
        auth
    } else {
        codex_home.join(CANARY)
    }
}

/// A file in every `CODEX_HOME`, so invariant 1 is tested even before
/// sign-in.
pub const CANARY: &str = "bokkie-canary";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_covers_every_user_and_invariant() {
        let layout = Layout::new("/data");
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        let spec = probe_spec(&PlanInput {
            layout: &layout,
            user: "alice",
            users: &users,
            peers: vec![PeerSpec {
                pid: 1,
                comm: "docker-init".into(),
                start_time: Some(1),
            }],
            outer_pid1_cmdline: "/sbin/docker-init".into(),
            connect: Some(("1.1.1.1:443".into(), Expect::Deny)),
        });
        let labels: Vec<&str> = spec
            .reads
            .iter()
            .chain(&spec.writes)
            .map(|c| c.label.as_str())
            .collect();
        for label in [
            "inv1:alice-auth",
            "inv1:bob-auth",
            "inv2:bob-workspace",
            "inv2:database",
            "inv3:bob-workspace-file",
            "inv3:database",
            "shared:tmp",
        ] {
            assert!(labels.contains(&label), "missing {label}");
        }
        assert!(!labels.contains(&"inv3:alice-workspace"));
        assert_eq!(
            spec.keep_written,
            ["shared:root", "shared:tmp", "shared:dev-shm"]
        );
        assert_eq!(spec.connect.unwrap().expect, Expect::Deny);
    }

    #[test]
    fn the_config_denies_data_and_names_both_profiles() {
        let config = codex_config(&Layout::new("/data"), &[]);
        assert!(config.contains(r#""/data" = "deny""#));
        assert!(config.contains("[permissions.bokkie-net.network]\nenabled = true"));
        assert!(config.contains(r#"default_permissions = "bokkie""#));
    }
}
