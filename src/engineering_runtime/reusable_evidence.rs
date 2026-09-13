//! Compact adapter-owned bindings; original evidence never becomes a new command.
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationBinding {
    execution_id: String,
    outcome_id: String,
    contract_revision: u64,
    contract_digest: String,
    package_id: Option<String>,
    package_digest: String,
    profile_digest: String,
    environment_digest: String,
    item_id: String,
    source_digest: String,
    evidence: EngineeringCriterionEvidence,
}

impl EngineeringRuntime {
    pub(super) fn evidence_context(&self) -> RuntimeResult<Value> {
        let mut child = Command::new("python3")
            .arg(self.profile.broker.with_file_name("evidence_context.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("context stdin missing")?
            .write_all(&serde_json::to_vec(&self.profile)?)?;
        let output = child.wait_with_output()?;
        if !output.status.success() || output.stdout.len() as u64 > MAX_FILE {
            return Err("current evidence context unavailable or exceeds bound".into());
        }
        let mut value: Value = serde_json::from_slice(&output.stdout)?;
        if value["source"].get("unavailable").is_some() {
            return Err(format!(
                "current source unavailable: {}",
                value["source"]["unavailable"]
            )
            .into());
        }
        // The private profile fixes worker restrictions, tools and model settings;
        // bind mutable adapter/executable bytes as well as environment settings.
        let mut identities = vec![];
        for path in [
            &self.profile.broker,
            &self.profile.codex,
            &self.profile.bwrap,
        ] {
            identities.push(sha(&bounded_read(path, 512 * 1024 * 1024)?));
        }
        value["environment"] = json!(sha(&serde_json::to_vec(&json!({
            "environment": value["source"]["environment_identity"], "executables":identities,
            "profile":self.profile,
        }))?));
        Ok(value)
    }

    pub(super) fn bind_validation(
        &self,
        state: &EngineeringOutcomeSnapshot,
        execution: &EngineeringExecution,
        key: &str,
        item_id: &str,
        evidence: &EngineeringCriterionEvidence,
        source: &Value,
    ) -> RuntimeResult<()> {
        // Legacy commands keep their original registration semantics; absence
        // of a captured environment must never manufacture a reusable binding.
        if source.get("environment_identity").is_none() {
            return Ok(());
        }
        let package = state
            .packages
            .iter()
            .find(|p| Some(&p.id) == execution.package_id.as_ref());
        let context = self.evidence_context()?;
        if context["source"] != *source {
            return Err("source or inputs changed since the validation command".into());
        }
        let binding = ValidationBinding {
            execution_id: execution.id.clone(),
            outcome_id: state.id.clone(),
            contract_revision: state.contract_revision,
            contract_digest: sha(&serde_json::to_vec(state.contract())?),
            package_id: execution.package_id.clone(),
            package_digest: sha(&serde_json::to_vec(&package.map(|p| &p.input))?),
            profile_digest: execution.instructions.profile_digest.clone(),
            environment_digest: context["environment"]
                .as_str()
                .ok_or("environment missing")?
                .into(),
            item_id: item_id.into(),
            source_digest: self.blob(&serde_json::to_vec(source)?)?,
            evidence: evidence.clone(),
        };
        let binding_digest = self.blob(&serde_json::to_vec(&binding)?)?;
        atomic(
            &self
                .profile
                .broker_root
                .join(&execution.id)
                .join("receipts")
                .join(format!("binding-{key}.json")),
            &json!({"binding_digest":binding_digest}),
        )
    }

    fn applicable_binding(
        &self,
        binding: &ValidationBinding,
        state: &EngineeringOutcomeSnapshot,
        consumer: &EngineeringExecution,
        context: &Value,
    ) -> RuntimeResult<()> {
        let original = state
            .executions
            .iter()
            .find(|e| e.id == binding.execution_id)
            .ok_or("original execution is outside this outcome")?;
        let package = state
            .packages
            .iter()
            .find(|p| Some(&p.id) == binding.package_id.as_ref())
            .ok_or("original package unavailable")?;
        if binding.outcome_id != state.id
            || binding.contract_revision != state.contract_revision
            || original.contract_revision != binding.contract_revision
            || binding.contract_digest != sha(&serde_json::to_vec(state.contract())?)
            || original.package_id != binding.package_id
            || (consumer.role == EngineeringRole::Worker
                && consumer.package_id != binding.package_id)
            || binding.package_digest != sha(&serde_json::to_vec(&Some(&package.input))?)
            || package.cancellation_requested
            || package.superseded_by.is_some()
            || !package
                .input
                .criteria
                .contains(&binding.evidence.criterion_id)
        {
            return Err(
                "evidence contract, package, inputs or criterion is no longer applicable".into(),
            );
        }
        if binding.profile_digest != original.instructions.profile_digest
            || binding.profile_digest != state.contract().worker.profile_digest
            || context["environment"] != binding.environment_digest
        {
            return Err("evidence worker profile or environment changed".into());
        }
        let source: Value = serde_json::from_slice(&self.evidence(&binding.source_digest)?)?;
        if context["source"] != source {
            return Err("evidence source or relevant inputs changed".into());
        }
        for input in &package.input.inputs {
            self.inspect(input)?;
        }
        self.inspect(&binding.evidence.artefact)?;
        self.evidence(&binding.evidence.command_digest)?;
        self.evidence(&binding.evidence.output_digest)?;
        // These bytes are retained outside the mutable workspace by registration;
        // the original receipt must still exist and match, never caller text.
        let receipts = self
            .profile
            .broker_root
            .join(&binding.execution_id)
            .join("receipts");
        let mut found = false;
        for entry in fs::read_dir(receipts)?.take(4096) {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("validation-"))
            {
                let evidence: EngineeringCriterionEvidence = read_json(&path)?;
                found |= evidence == binding.evidence;
            }
        }
        if !found {
            return Err("original registered validation receipt missing or corrupt".into());
        }
        Ok(())
    }

    pub(super) fn discover_evidence(
        &self,
        state: &EngineeringOutcomeSnapshot,
        consumer: &EngineeringExecution,
    ) -> RuntimeResult<Value> {
        let context = self.evidence_context()?;
        let mut validations = vec![];
        let mut rejected = vec![];
        let mut visited = 0;
        for original in &state.executions {
            let receipts = self.profile.broker_root.join(&original.id).join("receipts");
            if !receipts.is_dir() {
                continue;
            }
            for entry in fs::read_dir(receipts)? {
                visited += 1;
                if visited > 4096 {
                    return Err("evidence discovery exceeds 4096 receipt bound".into());
                }
                let path = entry?.path();
                if !path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("binding-"))
                {
                    continue;
                }
                let result = (|| -> RuntimeResult<ValidationBinding> {
                    let index: Value = read_json(&path)?;
                    let binding: ValidationBinding = serde_json::from_slice(
                        &self.evidence(
                            index["binding_digest"]
                                .as_str()
                                .ok_or("binding digest missing")?,
                        )?,
                    )?;
                    if binding.execution_id != original.id {
                        return Err("original execution attribution mismatch".into());
                    }
                    self.applicable_binding(&binding, state, consumer, &context)?;
                    Ok(binding)
                })();
                match result {
                    Ok(binding) => validations.push(json!({"original_execution_id":binding.execution_id,
                        "item_id":binding.item_id,"package_id":binding.package_id,"contract_revision":binding.contract_revision,
                        "source_digest":binding.source_digest,"environment_digest":binding.environment_digest,"evidence":binding.evidence})),
                    Err(error) => rejected.push(json!({"original_execution_id":original.id,"reference":path.file_name(),"reason":error.to_string()})),
                }
                if validations.len() + rejected.len() > 64 {
                    return Err("evidence discovery exceeds 64 reference bound".into());
                }
            }
        }
        // Reviews retain their original thread/report provenance. Discover only
        // registrations whose parent execution belongs to this contract/outcome.
        let mut reviews = vec![];
        for original in state
            .executions
            .iter()
            .filter(|e| e.contract_revision == state.contract_revision)
        {
            let receipts = self.profile.broker_root.join(&original.id).join("receipts");
            if !receipts.is_dir() {
                continue;
            }
            for entry in fs::read_dir(receipts)?.take(4096) {
                let path = entry?.path();
                if !path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("review-provenance-"))
                {
                    continue;
                }
                let provenance: Value = read_json(&path)?;
                let Some(digest) = provenance["report_digest"].as_str() else {
                    continue;
                };
                let registered = self
                    .profile
                    .broker_root
                    .join("reviews")
                    .join(format!("{digest}.json"));
                if !registered.is_file() {
                    continue;
                }
                let review: EngineeringReviewEvidence = read_json(&registered)?;
                let check = self.verify_review(&review, consumer).and_then(|()| {
                    self.evidence(
                        provenance["provenance_digest"]
                            .as_str()
                            .ok_or("review provenance missing")?,
                    )?;
                    Ok(())
                });
                match check {
                    Ok(()) => reviews.push(json!({"original_execution_id":original.id,"review":review,"provenance":provenance})),
                    Err(error) => rejected.push(json!({"original_execution_id":original.id,"reason":error.to_string()})),
                }
                if reviews.len() + rejected.len() > 64 {
                    return Err("review discovery exceeds 64 reference bound".into());
                }
            }
        }
        Ok(
            json!({"validations":validations,"reviews":reviews,"rejected":rejected,
            "legacy_validation_policy":"Legacy receipts remain usable by their original submission; absent applicability bindings cannot be reused by another execution."}),
        )
    }

    pub(super) fn verify_submission_scoped(
        &self,
        input: &EngineeringSubmissionInput,
        state: &EngineeringOutcomeSnapshot,
        execution: &EngineeringExecution,
    ) -> RuntimeResult<()> {
        for artefact in &input.artefacts {
            self.inspect(artefact)?;
        }
        for evidence in &input.evidence {
            if !input.artefacts.contains(&evidence.artefact) {
                return Err("validation source not in submission".into());
            }
            self.inspect(&evidence.artefact)?;
            self.evidence(&evidence.command_digest)?;
            self.evidence(&evidence.output_digest)?;
        }
        if input.evidence.is_empty() {
            return Ok(());
        }
        let discovered = self.discover_evidence(state, execution)?;
        for evidence in &input.evidence {
            if discovered["validations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["evidence"] == json!(evidence))
            {
                continue;
            }
            // Compatibility for existing in-flight journals: only original
            // unbound receipts, never transplant them to a replacement worker.
            let receipts = self
                .profile
                .broker_root
                .join(&execution.id)
                .join("receipts");
            let mut legacy = false;
            for entry in fs::read_dir(receipts)?.take(4096) {
                let path = entry?.path();
                if let Some(key) = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_prefix("validation-"))
                {
                    if !path.with_file_name(format!("binding-{key}")).exists() {
                        let original: EngineeringCriterionEvidence = read_json(&path)?;
                        legacy |= &original == evidence;
                    }
                }
            }
            if !legacy {
                return Err(format!(
                    "validation is inapplicable to this execution: {}",
                    discovered["rejected"]
                )
                .into());
            }
        }
        Ok(())
    }
}
