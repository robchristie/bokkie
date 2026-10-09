//! Trusted host registration is separate from browser session protection.
use crate::{ManagedCapabilityProfile, StoreError, WorkspaceLimits, WorkspacePolicy};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHostConfig {
    pub hosts: Vec<WorkspaceHost>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHost {
    pub id: String,
    pub name: String,
    pub token_sha256: String,
    pub projects: Vec<WorkspaceHostProject>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHostProject {
    pub project_id: String,
    pub profile_revision: String,
    pub permitted_actions: Vec<String>,
    pub limits: WorkspaceLimits,
}

pub(crate) fn bounded_text(value: &str, max: usize, required: bool) -> bool {
    (!required || !value.trim().is_empty())
        && value.chars().count() <= max
        && !value
            .chars()
            .any(|c| c == '\0' || (c.is_control() && c != '\n' && c != '\t'))
}

pub(crate) fn validate_limits(limits: &WorkspaceLimits) -> Result<(), StoreError> {
    if !(1..=86400).contains(&limits.max_seconds)
        || !(1..=100).contains(&limits.max_turns)
        || !(1..=2_000_000).contains(&limits.max_tokens)
    {
        return Err(StoreError::Invalid(
            "workspace limits exceed finite execution bounds".into(),
        ));
    }
    Ok(())
}

impl WorkspaceHostConfig {
    /// The operator supplies a bounded canonical regular file. This config is
    /// trusted registration, separate from every model and browser payload.
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        let invalid =
            |error: String| StoreError::Invalid(format!("workspace host configuration: {error}"));
        let metadata = std::fs::symlink_metadata(path).map_err(|e| invalid(e.to_string()))?;
        if !path.is_absolute()
            || !metadata.file_type().is_file()
            || metadata.len() > 131072
            || std::fs::canonicalize(path).map_err(|e| invalid(e.to_string()))? != path
        {
            return Err(invalid(
                "use an absolute canonical regular JSON file of at most 128 KiB".into(),
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| invalid(e.to_string()))?;
        if bytes.len() > 131072 {
            return Err(invalid("JSON file exceeds 128 KiB".into()));
        }
        let config: Self = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        if self.hosts.is_empty() || self.hosts.len() > 32 {
            return Err(StoreError::Invalid(
                "register 1..=32 workspace hosts".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut projects = BTreeSet::new();
        let mut profiles = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        for host in &self.hosts {
            host.validate()?;
            if !ids.insert(&host.id)
                || !names.insert(&host.name)
                || !hashes.insert(&host.token_sha256)
            {
                return Err(StoreError::Invalid(
                    "workspace host identities, names and tokens must be distinct".into(),
                ));
            }
            for project in &host.projects {
                if !projects.insert(&project.project_id)
                    || !profiles.insert(&project.profile_revision)
                {
                    return Err(StoreError::Invalid(
                        "workspace project/profile ownership must not overlap".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl WorkspaceHost {
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if !bounded_text(&self.id, 200, true)
            || self
                .id
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
            || !bounded_text(&self.name, 160, true)
            || self.token_sha256.len() != 64
            || !self
                .token_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.projects.is_empty()
            || self.projects.len() > 100
        {
            return Err(StoreError::Invalid(
                "invalid trusted workspace host identity, token or project bounds".into(),
            ));
        }
        let mut projects = BTreeSet::new();
        let mut profiles = BTreeSet::new();
        for project in &self.projects {
            if !bounded_text(&project.project_id, 200, true)
                || !bounded_text(&project.profile_revision, 200, true)
                || project.profile_revision.chars().any(char::is_whitespace)
                || !projects.insert(&project.project_id)
                || !profiles.insert(&project.profile_revision)
                || project.permitted_actions.is_empty()
                || project.permitted_actions.len() > 32
                || project
                    .permitted_actions
                    .iter()
                    .any(|s| !bounded_text(s, 200, true))
                || project
                    .permitted_actions
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != project.permitted_actions.len()
            {
                return Err(StoreError::Invalid(
                    "invalid or overlapping workspace project/profile registration".into(),
                ));
            }
            validate_limits(&project.limits)?;
        }
        Ok(())
    }
}

impl WorkspaceHostProject {
    pub fn capability_profile(&self, host_name: &str) -> ManagedCapabilityProfile {
        ManagedCapabilityProfile {
            capability: "workspace".into(),
            revision: self.profile_revision.clone(),
            available: true,
            effects: vec!["workspace_execution".into(), "store_local_result".into()],
            destination: format!("workspace:{}", self.project_id),
            max_attempts: 1,
            max_output_chars: 16384,
            workspace_policy: Some(WorkspacePolicy {
                host_name: host_name.into(),
                project_id: self.project_id.clone(),
                permitted_actions: self.permitted_actions.clone(),
                limits: self.limits.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config() -> WorkspaceHostConfig {
        WorkspaceHostConfig {
            hosts: vec![WorkspaceHost {
                id: "nostromo".into(),
                name: "Nostromo".into(),
                token_sha256: "a".repeat(64),
                projects: vec![WorkspaceHostProject {
                    project_id: "project-id".into(),
                    profile_revision: "workspace-v1/project-id".into(),
                    permitted_actions: vec!["ordinary_code_delivery".into()],
                    limits: WorkspaceLimits {
                        max_seconds: 3600,
                        max_turns: 10,
                        max_tokens: 100000,
                    },
                }],
            }],
        }
    }

    #[test]
    fn trusted_configuration_load_is_bounded_canonical_and_regular() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("hosts.json");
        std::fs::write(&path, serde_json::to_vec(&config()).unwrap()).unwrap();
        assert!(WorkspaceHostConfig::load(&path).is_ok());
        let alias = temp.path().join("alias.json");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(WorkspaceHostConfig::load(&alias).is_err());
        std::fs::write(&path, vec![b' '; 131073]).unwrap();
        assert!(WorkspaceHostConfig::load(&path).is_err());
        std::fs::write(&path, br#"{"hosts":[],"extra":true}"#).unwrap();
        assert!(WorkspaceHostConfig::load(&path).is_err());
    }

    #[test]
    fn overlapping_ownership_invalid_tokens_and_unbounded_runtime_fail_closed() {
        let mut duplicate = config();
        let mut other = duplicate.hosts[0].clone();
        other.id = "another".into();
        other.name = "Another".into();
        other.token_sha256 = "b".repeat(64);
        duplicate.hosts.push(other);
        assert!(duplicate.validate().is_err());
        let mut invalid = config();
        invalid.hosts[0].token_sha256 = "unchecked label".into();
        assert!(invalid.validate().is_err());
        let mut invalid = config();
        invalid.hosts[0].projects[0].limits.max_tokens = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = config();
        invalid.hosts[0].projects[0]
            .permitted_actions
            .push("ordinary_code_delivery".into());
        assert!(invalid.validate().is_err());
    }
}
