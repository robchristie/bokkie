//! The pinned `codex` binary with a synthetic household sign-in, for live
//! tests.
//!
//! Every app server a live test starts is pointed at local mocks only: the
//! model ([`MockModel`]), ChatGPT's sign-in authority and backend
//! ([`MockChatgpt`]: token, revoke and account-check endpoints, over HTTPS
//! with a test certificate authority), and a proxy that refuses and
//! records anything else ([`bokkie_mock_model::chatgpt::DenyProxy`]). The
//! sign-in is synthetic, issued by the mock authority. The gateway itself
//! gets no test seam: the overrides and environment reach app servers only
//! through the test's own [`Launcher`].
//!
//! The binary runs through a small wrapper that names each app server in
//! the `client_id` of its token requests (`CODEX_APP_SERVER_LOGIN_CLIENT_ID`)
//! as `household` or `user-<id>`, so a test can tell which app server
//! refreshed.

#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bokkie_gateway::codex::Launcher;
use bokkie_mock_model::MockModel;
use bokkie_mock_model::chatgpt::MockChatgpt;

pub struct Live {
    /// The wrapper around the pinned binary.
    pub program: PathBuf,
    pub model: MockModel,
    pub base_url: String,
    pub chatgpt: MockChatgpt,
    pub dir: tempfile::TempDir,
}

impl Live {
    /// The mocks and the wrapper, or `None` when `BOKKIE_CODEX_BIN` is not
    /// set.
    pub async fn start(reply: &[&str]) -> Option<Self> {
        let Some(codex) = std::env::var_os("BOKKIE_CODEX_BIN").map(PathBuf::from) else {
            eprintln!("BOKKIE_CODEX_BIN is not set; skipping");
            return None;
        };
        let dir = tempfile::tempdir().unwrap();
        let (model, base_url) = MockModel::start(reply).await;
        let chatgpt = MockChatgpt::start(dir.path()).await;
        let program = wrap(&codex, dir.path());
        Some(Self {
            program,
            model,
            base_url,
            chatgpt,
            dir,
        })
    }

    /// The `-c` overrides: the mock model, requiring the sign-in, and the
    /// mock backend.
    pub fn overrides(&self) -> Vec<String> {
        let mut overrides = MockModel::signed_in_overrides(&self.base_url);
        overrides.extend(self.chatgpt.overrides());
        overrides
    }

    pub fn env(&self) -> Vec<(OsString, OsString)> {
        self.chatgpt.env()
    }

    pub fn launcher(&self) -> Launcher {
        self.launcher_with(Vec::new())
    }

    /// The launcher, with more overrides after the mocks'.
    pub fn launcher_with(&self, more: Vec<String>) -> Launcher {
        let mut overrides = self.overrides();
        overrides.extend(more);
        Launcher::Process {
            program: self.program.clone(),
            overrides,
            env: self.env(),
        }
    }

    /// Signs the household in under `data_dir` with a new generation of
    /// synthetic tokens whose access token expires in `expires_in` seconds.
    pub fn sign_in_household(&self, data_dir: &Path, expires_in: i64) {
        use std::os::unix::fs::DirBuilderExt;
        let codex_home = data_dir.join("household/codex");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&codex_home)
            .unwrap();
        self.chatgpt.sign_in(&codex_home, expires_in);
    }

    /// The generation of each bearer token the model saw, in order.
    pub fn model_generations(&self) -> Vec<Option<u64>> {
        self.model
            .bearers
            .lock()
            .unwrap()
            .iter()
            .map(|bearer| {
                bearer
                    .as_deref()
                    .and_then(|token| self.chatgpt.generation_of(token))
            })
            .collect()
    }

    /// Who refreshed, in order: `household` or `user-<id>`.
    pub fn refreshers(&self) -> Vec<String> {
        self.chatgpt
            .refreshes()
            .into_iter()
            .map(|r| r.client_id)
            .collect()
    }

    /// Nothing any app server tried to send went anywhere but the mocks.
    pub fn assert_nothing_left_the_machine(&self) {
        let attempts = self.chatgpt.proxy().attempts();
        assert!(
            attempts.is_empty(),
            "app servers tried to reach other hosts: {attempts:?}"
        );
    }
}

/// Writes the wrapper: the pinned binary, with a login client id that names
/// the app server from its `CODEX_HOME`.
fn wrap(codex: &Path, dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let wrapper = dir.join("codex");
    let quoted = codex.display().to_string().replace('\'', r"'\''");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             case \"$CODEX_HOME\" in\n\
             \x20 */household/codex) id=household ;;\n\
             \x20 */users/*/codex) id=\"user-$(basename \"$(dirname \"$CODEX_HOME\")\")\" ;;\n\
             \x20 *) id=other ;;\n\
             esac\n\
             CODEX_APP_SERVER_LOGIN_CLIENT_ID=\"$id\" exec '{quoted}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Until every child forked while the file was open for writing has run
    // its own program, executing it fails with "Text file busy".
    for attempt in 0.. {
        match std::process::Command::new(&wrapper)
            .arg("--version")
            .output()
        {
            Ok(output) if output.status.success() => break,
            Err(error) if error.raw_os_error() == Some(26) && attempt < 100 => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => panic!("the wrapper does not run: {other:?}"),
        }
    }
    wrapper
}
