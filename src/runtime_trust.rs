//! Explicit runtime identities and least-authority child environments.
//!
//! The gardener resolves tools once while it is enabled, records the exact
//! canonical executable and version identity, and verifies the file identity
//! again immediately before every spawn. Child processes never inherit the
//! daemon environment: each tool receives a small role-specific environment.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::process::{
    EffectRisk, ProcessError, ProcessHeartbeat, ProcessOutcome, ProcessSupervisor,
};

const VERSION_OUTPUT_BYTES: usize = 16 * 1024;
const EXECUTABLE_FILE_BUSY_ATTEMPTS: usize = 4;
const EXECUTABLE_FILE_BUSY_BACKOFF: Duration = Duration::from_millis(5);
const BOT_NAME: &str = "Bokkie Gardener";
const BOT_EMAIL: &str = "bokkie-gardener@users.noreply.github.com";

/// Semantic role of an executable in a persisted invocation manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableRole {
    Codex,
    Git,
    GitHub,
    GitHubPublicObserver,
    CandidateCheck,
    CandidateSandbox,
}

/// Canonical, content-bound identity of one executable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutableIdentity {
    role: ExecutableRole,
    invocation_path: PathBuf,
    path: PathBuf,
    sha256: String,
    version: String,
}

impl ExecutableIdentity {
    pub fn role(&self) -> ExecutableRole {
        self.role
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn invocation_path(&self) -> &Path {
        &self.invocation_path
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Resolves through only the supplied search path and obtains a bounded
    /// version identity through the shared process supervisor.
    pub fn resolve(
        role: ExecutableRole,
        configured: impl AsRef<Path>,
        version_arguments: &[&str],
        environment: &ChildEnvironment,
        supervisor: &ProcessSupervisor,
        timeout: Duration,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Self, RuntimeTrustError> {
        if timeout.is_zero() {
            return Err(RuntimeTrustError::InvalidEnvironment(
                "tool identity timeout must be positive".to_owned(),
            ));
        }
        let (invocation_path, path) =
            resolve_executable(configured.as_ref(), environment.search_path())?;
        let sha256 = executable_digest(&path)?;
        let mut command = Command::new(&invocation_path);
        command.args(version_arguments);
        environment.apply(&mut command, ProcessPolicy::for_role(role), None)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(RuntimeTrustError::DeadlineOutOfRange)?;
        let mut attempts = 0;
        let mut child = loop {
            match supervisor.spawn(&mut command, deadline, EffectRisk::None) {
                Ok(child) => break child,
                Err(ProcessError::Spawn(source))
                    if source.raw_os_error() == Some(26)
                        && attempts + 1 < EXECUTABLE_FILE_BUSY_ATTEMPTS =>
                {
                    attempts += 1;
                    thread::sleep(EXECUTABLE_FILE_BUSY_BACKOFF);
                }
                Err(source) => {
                    return Err(RuntimeTrustError::VersionSpawn {
                        path: path.clone(),
                        source,
                    });
                }
            }
        };
        child.close_stdin();
        let outcome = child
            .wait(heartbeat)
            .map_err(|source| RuntimeTrustError::VersionSpawn {
                path: path.clone(),
                source,
            })?;
        let evidence = match outcome {
            ProcessOutcome::Completed { status, evidence } if status.success() => evidence,
            outcome => {
                return Err(RuntimeTrustError::VersionProbe {
                    path,
                    detail: outcome.to_string(),
                });
            }
        };
        if evidence.stdout.truncated || evidence.stdout.total_bytes as usize > VERSION_OUTPUT_BYTES
        {
            return Err(RuntimeTrustError::VersionProbe {
                path,
                detail: "version output exceeded its bound".to_owned(),
            });
        }
        let version = evidence.stdout.tail.trim().to_owned();
        if version.is_empty() {
            return Err(RuntimeTrustError::VersionProbe {
                path,
                detail: "version output was empty".to_owned(),
            });
        }
        Ok(Self {
            role,
            invocation_path,
            path,
            sha256,
            version,
        })
    }

    /// Rejects replacement, permission changes, or retargeting before spawn.
    pub fn verify_unchanged(&self) -> Result<(), RuntimeTrustError> {
        let canonical = fs::canonicalize(&self.invocation_path).map_err(|source| {
            RuntimeTrustError::ExecutableFilesystem {
                path: self.invocation_path.clone(),
                source,
            }
        })?;
        if canonical != self.path {
            return Err(RuntimeTrustError::ExecutableChanged {
                path: self.path.clone(),
            });
        }
        validate_executable_file(&canonical)?;
        if executable_digest(&canonical)? != self.sha256 {
            return Err(RuntimeTrustError::ExecutableChanged { path: canonical });
        }
        Ok(())
    }
}

/// Explicit executable names or paths resolved at gardener startup.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GardenerExecutablePaths {
    pub codex: PathBuf,
    pub git: PathBuf,
    pub gh: PathBuf,
    pub github_public_observer: PathBuf,
}

impl GardenerExecutablePaths {
    pub fn new(
        codex: impl Into<PathBuf>,
        git: impl Into<PathBuf>,
        gh: impl Into<PathBuf>,
        github_public_observer: impl Into<PathBuf>,
    ) -> Self {
        Self {
            codex: codex.into(),
            git: git.into(),
            gh: gh.into(),
            github_public_observer: github_public_observer.into(),
        }
    }
}

/// Startup-resolved identities for the gardener's privileged tool boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GardenerExecutableIdentities {
    pub codex: ExecutableIdentity,
    pub git: ExecutableIdentity,
    pub gh: ExecutableIdentity,
    pub github_public_observer: ExecutableIdentity,
}

impl GardenerExecutableIdentities {
    pub fn resolve(
        configured: &GardenerExecutablePaths,
        environment: &ChildEnvironment,
        supervisor: &ProcessSupervisor,
        timeout: Duration,
        heartbeat: &mut dyn ProcessHeartbeat,
    ) -> Result<Self, RuntimeTrustError> {
        Ok(Self {
            codex: ExecutableIdentity::resolve(
                ExecutableRole::Codex,
                &configured.codex,
                &["--version"],
                environment,
                supervisor,
                timeout,
                heartbeat,
            )?,
            git: ExecutableIdentity::resolve(
                ExecutableRole::Git,
                &configured.git,
                &["--version"],
                environment,
                supervisor,
                timeout,
                heartbeat,
            )?,
            gh: ExecutableIdentity::resolve(
                ExecutableRole::GitHub,
                &configured.gh,
                &["--version"],
                environment,
                supervisor,
                timeout,
                heartbeat,
            )?,
            github_public_observer: ExecutableIdentity::resolve(
                ExecutableRole::GitHubPublicObserver,
                &configured.github_public_observer,
                &["--version"],
                environment,
                supervisor,
                timeout,
                heartbeat,
            )?,
        })
    }
}

/// Explicit directories and executable search path supplied to children.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChildEnvironment {
    home: PathBuf,
    xdg_config_home: PathBuf,
    xdg_cache_home: PathBuf,
    github_config_home: PathBuf,
    path: Vec<PathBuf>,
}

impl ChildEnvironment {
    pub fn new(
        home: impl Into<PathBuf>,
        xdg_config_home: impl Into<PathBuf>,
        xdg_cache_home: impl Into<PathBuf>,
        path: Vec<PathBuf>,
    ) -> Result<Self, RuntimeTrustError> {
        let home = home.into();
        let xdg_config_home = xdg_config_home.into();
        let environment = Self {
            github_config_home: xdg_config_home.join("bokkie-gh-empty"),
            home,
            xdg_config_home,
            xdg_cache_home: xdg_cache_home.into(),
            path,
        };
        for directory in [
            &environment.home,
            &environment.xdg_config_home,
            &environment.xdg_cache_home,
            &environment.github_config_home,
        ] {
            validate_absolute_directory_shape(directory)?;
        }
        if environment.path.is_empty() {
            return Err(RuntimeTrustError::InvalidEnvironment(
                "child PATH must contain at least one absolute directory".to_owned(),
            ));
        }
        for directory in &environment.path {
            validate_absolute_directory_shape(directory)?;
        }
        env::join_paths(&environment.path).map_err(|source| {
            RuntimeTrustError::InvalidEnvironment(format!("child PATH is invalid: {source}"))
        })?;
        Ok(environment)
    }

    /// Compatibility profile for existing tests and callers. Production code
    /// should persist and pass an explicitly constructed profile instead.
    pub fn captured_current() -> Result<Self, RuntimeTrustError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/var/empty"));
        let config = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        let cache = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".cache"));
        let path = env::var_os("PATH")
            .map(|value| {
                env::split_paths(&value)
                    .filter(|path| path.is_absolute())
                    .collect::<Vec<_>>()
            })
            .filter(|paths| !paths.is_empty())
            .unwrap_or_else(|| {
                vec![
                    PathBuf::from("/usr/local/bin"),
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/bin"),
                ]
            });
        Self::new(home, config, cache, path)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn xdg_config_home(&self) -> &Path {
        &self.xdg_config_home
    }

    pub fn xdg_cache_home(&self) -> &Path {
        &self.xdg_cache_home
    }

    pub fn search_path(&self) -> &[PathBuf] {
        &self.path
    }

    pub fn github_config_home(&self) -> &Path {
        &self.github_config_home
    }

    /// Clears the entire ambient environment and applies one role-specific
    /// policy. A credential is accepted only for a GitHub mutation policy.
    pub fn apply(
        &self,
        command: &mut Command,
        policy: ProcessPolicy,
        credential: Option<&GitHubCredential>,
    ) -> Result<(), RuntimeTrustError> {
        if credential.is_some() && !policy.accepts_github_credential() {
            return Err(RuntimeTrustError::CredentialScope);
        }
        command.env_clear();
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config_home)
            .env("XDG_CACHE_HOME", &self.xdg_cache_home)
            .env(
                "PATH",
                env::join_paths(&self.path).map_err(|source| {
                    RuntimeTrustError::InvalidEnvironment(format!(
                        "child PATH is invalid: {source}"
                    ))
                })?,
            )
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("NO_COLOR", "1");

        match policy {
            ProcessPolicy::Codex => {
                command.env("CODEX_HOME", self.home.join(".codex"));
                apply_git_policy(command, None)?;
            }
            ProcessPolicy::GitLocal
            | ProcessPolicy::GitRemoteRead
            | ProcessPolicy::GitHubMutationGit => {
                apply_git_policy(command, credential)?;
            }
            ProcessPolicy::GitHubRead | ProcessPolicy::GitHubMutationCli => {
                require_empty_or_missing_directory(&self.github_config_home)?;
                command
                    .env("GH_CONFIG_DIR", &self.github_config_home)
                    .env("GH_PROMPT_DISABLED", "1")
                    .env("GH_NO_UPDATE_NOTIFIER", "1")
                    .env("GH_FORCE_TTY", "0");
                if let Some(credential) = credential {
                    command.env("GH_TOKEN", credential.expose());
                }
            }
            ProcessPolicy::GitHubPublicRead => {}
            ProcessPolicy::CandidateCheck => {
                command
                    .env("CARGO_HOME", self.home.join(".cargo"))
                    .env("RUSTUP_HOME", self.home.join(".rustup"))
                    .env("CARGO_NET_OFFLINE", "true")
                    .env("CARGO_TERM_COLOR", "never");
                apply_git_policy(command, None)?;
            }
            ProcessPolicy::CandidateSandbox => {}
        }
        Ok(())
    }
}

/// Authority profile for a child invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPolicy {
    Codex,
    GitLocal,
    GitRemoteRead,
    GitHubMutationGit,
    GitHubRead,
    GitHubMutationCli,
    GitHubPublicRead,
    CandidateCheck,
    CandidateSandbox,
}

impl ProcessPolicy {
    fn for_role(role: ExecutableRole) -> Self {
        match role {
            ExecutableRole::Codex => Self::Codex,
            ExecutableRole::Git => Self::GitLocal,
            ExecutableRole::GitHub => Self::GitHubRead,
            ExecutableRole::GitHubPublicObserver => Self::GitHubPublicRead,
            ExecutableRole::CandidateCheck => Self::CandidateCheck,
            ExecutableRole::CandidateSandbox => Self::CandidateSandbox,
        }
    }

    fn accepts_github_credential(self) -> bool {
        matches!(self, Self::GitHubMutationGit | Self::GitHubMutationCli)
    }
}

/// Secret supplied by an explicit credential source. Debug and Display never
/// expose its value, and it is injected only by mutation policy branches.
#[derive(Clone)]
pub struct GitHubCredential(OsString);

impl GitHubCredential {
    pub fn new(value: impl Into<OsString>) -> Result<Self, RuntimeTrustError> {
        let value = value.into();
        let Some(text) = value.to_str() else {
            return Err(RuntimeTrustError::InvalidCredential);
        };
        if text.is_empty() {
            return Err(RuntimeTrustError::EmptyCredential);
        }
        if !text.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(RuntimeTrustError::InvalidCredential);
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &OsStr {
        &self.0
    }

    pub(crate) fn redact_text(&self, value: &str) -> String {
        value.replace(
            self.0
                .to_str()
                .expect("credential construction requires UTF-8"),
            "[REDACTED]",
        )
    }

    pub(crate) fn redact_bytes(&self, value: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(value);
        self.redact_text(&text).into_bytes()
    }
}

impl fmt::Debug for GitHubCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitHubCredential([REDACTED])")
    }
}

#[derive(Debug, Error)]
pub enum RuntimeTrustError {
    #[error("invalid child environment: {0}")]
    InvalidEnvironment(String),
    #[error("executable {configured} was not found on the controlled PATH")]
    ExecutableNotFound { configured: PathBuf },
    #[error("executable path must be absolute or a bare file name: {0}")]
    InvalidExecutablePath(PathBuf),
    #[error("executable is not a regular executable file: {0}")]
    NotExecutable(PathBuf),
    #[error("cannot inspect executable {path}: {source}")]
    ExecutableFilesystem {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("executable changed after startup validation: {path}")]
    ExecutableChanged { path: PathBuf },
    #[error("cannot run version probe for {path}: {source}")]
    VersionSpawn {
        path: PathBuf,
        #[source]
        source: ProcessError,
    },
    #[error("version probe for {path} failed: {detail}")]
    VersionProbe { path: PathBuf, detail: String },
    #[error("tool identity deadline is out of range")]
    DeadlineOutOfRange,
    #[error("GitHub credential may be supplied only to a GitHub mutation process")]
    CredentialScope,
    #[error("GitHub credential must not be empty")]
    EmptyCredential,
    #[error("GitHub credential must contain only non-whitespace ASCII characters")]
    InvalidCredential,
}

fn resolve_executable(
    configured: &Path,
    search_path: &[PathBuf],
) -> Result<(PathBuf, PathBuf), RuntimeTrustError> {
    let candidate = if configured.is_absolute() {
        configured.to_owned()
    } else if configured.components().count() == 1 {
        search_path
            .iter()
            .map(|directory| directory.join(configured))
            .find(|candidate| validate_executable_file(candidate).is_ok())
            .ok_or_else(|| RuntimeTrustError::ExecutableNotFound {
                configured: configured.to_owned(),
            })?
    } else {
        return Err(RuntimeTrustError::InvalidExecutablePath(
            configured.to_owned(),
        ));
    };
    let parent = candidate
        .parent()
        .ok_or_else(|| RuntimeTrustError::InvalidExecutablePath(candidate.clone()))?;
    let parent =
        fs::canonicalize(parent).map_err(|source| RuntimeTrustError::ExecutableFilesystem {
            path: parent.to_owned(),
            source,
        })?;
    let invocation_path = parent.join(
        candidate
            .file_name()
            .ok_or_else(|| RuntimeTrustError::InvalidExecutablePath(candidate.clone()))?,
    );
    let canonical =
        fs::canonicalize(&candidate).map_err(|source| RuntimeTrustError::ExecutableFilesystem {
            path: candidate.clone(),
            source,
        })?;
    validate_executable_file(&canonical)?;
    Ok((invocation_path, canonical))
}

fn validate_executable_file(path: &Path) -> Result<(), RuntimeTrustError> {
    let metadata =
        fs::metadata(path).map_err(|source| RuntimeTrustError::ExecutableFilesystem {
            path: path.to_owned(),
            source,
        })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(RuntimeTrustError::NotExecutable(path.to_owned()));
    }
    Ok(())
}

fn executable_digest(path: &Path) -> Result<String, RuntimeTrustError> {
    let mut file = File::open(path).map_err(|source| RuntimeTrustError::ExecutableFilesystem {
        path: path.to_owned(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let read =
            file.read(&mut buffer)
                .map_err(|source| RuntimeTrustError::ExecutableFilesystem {
                    path: path.to_owned(),
                    source,
                })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_absolute_directory_shape(path: &Path) -> Result<(), RuntimeTrustError> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(RuntimeTrustError::InvalidEnvironment(format!(
            "directory must be absolute: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_empty_or_missing_directory(path: &Path) -> Result<(), RuntimeTrustError> {
    match fs::read_dir(path) {
        Ok(mut entries) => match entries.next() {
            None => Ok(()),
            Some(_) => Err(RuntimeTrustError::InvalidEnvironment(format!(
                "GitHub CLI config directory must remain empty: {}",
                path.display()
            ))),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeTrustError::InvalidEnvironment(format!(
            "cannot inspect GitHub CLI config directory {}: {error}",
            path.display()
        ))),
    }
}

fn apply_git_policy(
    command: &mut Command,
    credential: Option<&GitHubCredential>,
) -> Result<(), RuntimeTrustError> {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("GIT_SSH_COMMAND", "/bin/false");
    let mut entries = vec![
        ("core.hooksPath", "/dev/null".to_owned()),
        ("commit.gpgSign", "false".to_owned()),
        ("tag.gpgSign", "false".to_owned()),
        ("user.name", BOT_NAME.to_owned()),
        ("user.email", BOT_EMAIL.to_owned()),
        ("credential.helper", String::new()),
        ("credential.interactive", "never".to_owned()),
        ("core.sshCommand", "/bin/false".to_owned()),
        ("http.https://github.com/.extraheader", String::new()),
    ];
    if let Some(credential) = credential {
        let token = credential.expose().to_str().ok_or_else(|| {
            RuntimeTrustError::InvalidEnvironment(
                "GitHub credential must be valid UTF-8 for Git HTTP authentication".to_owned(),
            )
        })?;
        entries.push((
            "http.https://github.com/.extraheader",
            format!("Authorization: Bearer {token}"),
        ));
    }
    command.env("GIT_CONFIG_COUNT", entries.len().to_string());
    for (index, (key, value)) in entries.into_iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::process::{CancellationToken, NoopHeartbeat, ProcessLimits};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn environment(root: &Path) -> ChildEnvironment {
        ChildEnvironment::new(
            root.join("home"),
            root.join("config"),
            root.join("cache"),
            vec![root.join("bin"), PathBuf::from("/bin")],
        )
        .unwrap()
    }

    fn executable(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn supervisor() -> ProcessSupervisor {
        ProcessSupervisor::new(
            Duration::from_millis(10),
            ProcessLimits::default(),
            CancellationToken::new(),
        )
        .unwrap()
    }

    #[test]
    fn resolves_only_the_controlled_path_and_binds_version_and_content() {
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("bin/tool");
        executable(&tool, "#!/bin/sh\nprintf 'tool 1.2.3\\n'\n");
        let environment = environment(root.path());

        let identity = ExecutableIdentity::resolve(
            ExecutableRole::CandidateCheck,
            "tool",
            &["--version"],
            &environment,
            &supervisor(),
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();

        assert_eq!(identity.path(), fs::canonicalize(tool).unwrap());
        assert_eq!(identity.version(), "tool 1.2.3");
        assert_eq!(identity.sha256().len(), 64);
        identity.verify_unchanged().unwrap();
    }

    #[test]
    fn detects_executable_replacement_after_startup() {
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("bin/tool");
        executable(&tool, "#!/bin/sh\nprintf 'tool 1\\n'\n");
        let identity = ExecutableIdentity::resolve(
            ExecutableRole::CandidateCheck,
            &tool,
            &["--version"],
            &environment(root.path()),
            &supervisor(),
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();

        executable(&tool, "#!/bin/sh\nprintf 'tool 2\\n'\n");
        assert!(matches!(
            identity.verify_unchanged(),
            Err(RuntimeTrustError::ExecutableChanged { .. })
        ));
    }

    #[test]
    fn canonical_identity_retains_a_multicall_invocation_name() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("bin/multicall");
        let link = root.path().join("bin/candidate-check");
        executable(
            &target,
            "#!/bin/sh\n[ \"${0##*/}\" = candidate-check ] || exit 19\nprintf 'candidate-check 3\\n'\n",
        );
        symlink("multicall", &link).unwrap();

        let identity = ExecutableIdentity::resolve(
            ExecutableRole::CandidateCheck,
            &link,
            &["--version"],
            &environment(root.path()),
            &supervisor(),
            Duration::from_secs(1),
            &mut NoopHeartbeat,
        )
        .unwrap();

        assert_eq!(identity.path(), fs::canonicalize(&target).unwrap());
        assert_eq!(identity.invocation_path(), link);
        assert_eq!(identity.version(), "candidate-check 3");
        identity.verify_unchanged().unwrap();
    }

    #[test]
    fn hostile_ambient_values_are_cleared_and_credential_scope_is_narrow() {
        let root = tempfile::tempdir().unwrap();
        let dump = root.path().join("dump");
        executable(&dump, "#!/bin/sh\nenv | sort\n");
        let environment = environment(root.path());
        let credential = GitHubCredential::new("explicit-token").unwrap();

        let run = |policy, credential: Option<&GitHubCredential>| {
            let mut command = Command::new(&dump);
            command
                .env("AWS_SECRET_ACCESS_KEY", "ambient-cloud-secret")
                .env("GH_TOKEN", "ambient-gh-secret")
                .env("GIT_CONFIG_GLOBAL", "/hostile/config")
                .env("SSH_AUTH_SOCK", "/hostile/agent");
            environment.apply(&mut command, policy, credential).unwrap();
            let output = command.output().unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        let local = run(ProcessPolicy::GitLocal, None);
        assert!(!local.contains("ambient-cloud-secret"));
        assert!(!local.contains("ambient-gh-secret"));
        assert!(!local.contains("SSH_AUTH_SOCK"));
        assert!(local.contains("GIT_CONFIG_GLOBAL=/dev/null"));
        assert!(local.contains("GIT_TERMINAL_PROMPT=0"));
        assert!(local.contains("GIT_CONFIG_VALUE_0=/dev/null"));
        assert!(!local.contains("explicit-token"));

        let gh_read = run(ProcessPolicy::GitHubRead, None);
        assert!(!gh_read.contains("GH_TOKEN="));
        let public_read = run(ProcessPolicy::GitHubPublicRead, None);
        assert!(!public_read.contains("GH_TOKEN="));
        let gh_mutation = run(ProcessPolicy::GitHubMutationCli, Some(&credential));
        assert!(gh_mutation.contains("GH_TOKEN=explicit-token"));
        assert!(matches!(
            environment.apply(
                &mut Command::new(&dump),
                ProcessPolicy::GitHubPublicRead,
                Some(&credential)
            ),
            Err(RuntimeTrustError::CredentialScope)
        ));
        assert!(matches!(
            environment.apply(
                &mut Command::new(&dump),
                ProcessPolicy::Codex,
                Some(&credential)
            ),
            Err(RuntimeTrustError::CredentialScope)
        ));
        assert_eq!(format!("{credential:?}"), "GitHubCredential([REDACTED])");
    }
}
