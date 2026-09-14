// Included in the runtime fixture module: no model or external service.
#[test]
fn replacement_reuses_bound_evidence_and_checks_coverage_before_submission() {
    for (segmented, github) in [(false, false), (true, false), (true, true)] {
        let mut f = Fixture::with_package_input(github, Some("starting input"));
        fs::write(f.runtime.profile.workspace.join("reader.txt"), "page").unwrap();
        fs::write(f.runtime.profile.workspace.join("input.txt"), "fixed input").unwrap();
        let artefact = if github {
            for args in [vec!["add", "."], vec!["-c", "commit.gpgsign=false", "commit", "-m", "Implement reader"]] {
                assert!(Command::new("git").args(args).current_dir(&f.runtime.profile.workspace).output().unwrap().status.success());
            }
            let revision = |name: &str| String::from_utf8(Command::new("git").args(["rev-parse", name]).current_dir(&f.runtime.profile.workspace).output().unwrap().stdout).unwrap().trim().to_owned();
            EngineeringArtefact::Git { repository: f.runtime.profile.workspace.to_string_lossy().into(), commit: revision("HEAD"), tree: revision("HEAD^{tree}") }
        } else { f.runtime.file("reader.txt").unwrap().0 };
        let context = f.runtime.evidence_context().unwrap();
        let source = &context["source"];
        // The package's immutable starting input has legitimately been edited
        // before the successful validation. Reuse binds the validated source.
        let mut log = vec![Event { sequence:1, kind:"item/completed".into(),
            value:json!({"threadId":"root","turnId":"turn","item":{"id":"canonical","type":"commandExecution","command":"tools/check.sh","aggregatedOutput":"PASS","exitCode":0}}) }];
        for phase in ["item/started", "item/completed"] {
            let mut value = json!({"phase":phase,"thread_id":"root","turn_id":"turn","item_id":"canonical","source":source});
            if segmented {
                let bytes = serde_json::to_vec(source).unwrap();
                let digest = sha(&bytes);
                let blobs = f.directory(&f.worker).join("journal-blobs");
                fs::create_dir_all(&blobs).unwrap();
                fs::write(blobs.join(&digest), &bytes).unwrap();
                value.as_object_mut().unwrap().remove("source");
                value["source_ref"] = json!({"sha256":digest,"bytes":bytes.len(),"encoding":"json"});
            }
            log.push(Event {sequence:log.len() as u64+1,kind:"command_source".into(),value});
        }
        let directory = f.directory(&f.worker);
        let evidence: EngineeringCriterionEvidence = serde_json::from_value(f.runtime.dynamic(
            &mut f.store,&directory,&f.worker,"registered",
            &json!({"tool":"bokkie_validation","arguments":{"item_id":"canonical","artefact":artefact,"criterion_id":"reader"}}),&log,105).unwrap()).unwrap();
        let report = json!({"artefacts":[artefact],"verdict":"pass","findings":[]}).to_string();
        let review: EngineeringReviewEvidence = serde_json::from_value(f.runtime.dynamic(
            &mut f.store,&directory,&f.worker,"review",&json!({"tool":"bokkie_review","arguments":{"reviewer_thread_id":"child-thread","reviewer_turn_id":"child-turn"}}),
            &child_review_log(&report),105).unwrap()).unwrap();
        f.event(&f.worker,1,"boundary_reaped",json!({"boundary":"fixture:namespace"}));
        let state = snapshot(&f.store,&f.id).unwrap();
        f.runtime.reconcile(&mut f.store,&state,&f.worker,106).unwrap();
        let claims = f.store.claim_due_engineering(EngineeringRole::Worker,107,600,1).unwrap();
        assert_eq!(claims.len(),1);
        let state = snapshot(&f.store,&f.id).unwrap();
        let replacement = state.executions.iter().find(|e|e.id == claims[0].execution_id).unwrap().clone();
        assert_ne!(replacement.id,f.worker.id);
        f.prepare(&replacement);
        let found = f.runtime.discover_evidence(&state,&replacement).unwrap();
        assert_eq!(found["validations"][0]["original_execution_id"],f.worker.id);
        assert_eq!(found["validations"][0]["item_id"],"canonical");
        assert_eq!(found["reviews"][0]["review"],json!(review));
        let mut submission = EngineeringSubmissionInput {artefacts:vec![artefact.clone()],evidence:vec![evidence],limitations:String::new()};
        f.runtime.verify_submission_scoped(&submission,&state,&replacement).unwrap();
        let directory = f.directory(&replacement);
        let check = |submission:&EngineeringSubmissionInput,review:&EngineeringReviewEvidence|json!({"expected":state.precondition(),"command":{"kind":"submit_result","input":submission},"preflight_only":true,"review":review});
        let result = f.runtime.command(&mut f.store,&directory,&replacement,"advisory",check(&submission,&review),108).unwrap();
        assert_eq!(result["ready"],true);
        assert!(!directory.join("submission.json").exists());
        assert!(!directory.join("cancel.json").exists());
        let (input,_) = f.runtime.file("input.txt").unwrap();
        submission.artefacts.push(input);
        let error = f.runtime.command(&mut f.store,&directory,&replacement,"gap",check(&submission,&review),108).unwrap_err();
        assert!(error.to_string().contains("exact submitted artefacts"),"{error}");
        assert!(snapshot(&f.store,&f.id).unwrap().submissions.is_empty());
        // Reading old evidence does not restore the original execution's claim.
        let error = f.store.engineering_command(actor(&f.worker),EngineeringCommandEnvelope {
            command_id:"stale-submit".into(),expected:Some(state.precondition()),command:EngineeringCommand::SubmitResult(submission.clone())},108).unwrap_err();
        assert!(matches!(error,StoreError::Fenced));
        let mut changed = state.clone();
        changed.contract_revision += 1;
        // Avoid calling contract() for a nonexistent version: add a real-shaped revision.
        let mut contract = changed.contracts.last().unwrap().clone();
        contract.revision = changed.contract_revision;
        changed.contracts.push(contract);
        assert!(f.runtime.discover_evidence(&changed,&replacement).unwrap()["validations"].as_array().unwrap().is_empty());
        let mut changed = state.clone();
        changed.packages[0].input.instructions.push_str(" changed input policy");
        assert!(f.runtime.discover_evidence(&changed,&replacement).unwrap()["validations"].as_array().unwrap().is_empty());
        fs::write(f.runtime.profile.workspace.join("input.txt"),"changed").unwrap();
        let found = f.runtime.discover_evidence(&state,&replacement).unwrap();
        assert!(found["validations"].as_array().unwrap().is_empty());
        assert!(found["rejected"].to_string().contains("source or relevant inputs changed"));
        fs::write(f.runtime.profile.workspace.join("input.txt"),"fixed input").unwrap();
        // Starting-input provenance is still required, even though its bytes
        // differ legitimately from the validated source.
        let EngineeringArtefact::File { sha256: input_digest, .. } = &state.packages[0].input.inputs[0] else { panic!("file input") };
        let input_blob = f.runtime.profile.broker_root.join("blobs").join(input_digest);
        let original = fs::read(&input_blob).unwrap();
        fs::write(&input_blob, b"corrupt").unwrap();
        let found = f.runtime.discover_evidence(&state,&replacement).unwrap();
        assert!(found["validations"].as_array().unwrap().is_empty());
        assert!(found["rejected"].to_string().contains("digest mismatch"));
        fs::write(&input_blob, &original).unwrap();
        let mut bad_length = state.clone();
        if let EngineeringArtefact::File { bytes, .. } = &mut bad_length.packages[0].input.inputs[0] { *bytes += 1; }
        // A changed declared input also changes the immutable package binding.
        assert!(f.runtime.discover_evidence(&bad_length,&replacement).unwrap()["validations"].as_array().unwrap().is_empty());
        let index:Value = read_json(&f.directory(&f.worker).join("receipts/binding-registered.json")).unwrap();
        fs::write(f.runtime.profile.broker_root.join("blobs").join(index["binding_digest"].as_str().unwrap()),b"corrupt").unwrap();
        let found = f.runtime.discover_evidence(&state,&replacement).unwrap();
        assert!(found["validations"].as_array().unwrap().is_empty());
        assert!(!found["rejected"].as_array().unwrap().is_empty());
    }
}

#[test]
fn delivery_receipt_rejects_boolean_only_and_mismatched_ci_and_cleanup() {
    let f = Fixture::with_github(true);
    let head = "a".repeat(40);
    let merge = "b".repeat(40);
    let tree = "c".repeat(40);
    let ci = |revision: &str| json!({"state":"success","head":revision,
        "run":{"id":1,"url":"https://github.com/robchristie/pagefold/actions/runs/1","attempt":2,"head":revision,"status":"completed","conclusion":"success"},
        "jobs":[{"id":3,"url":"https://github.com/robchristie/pagefold/actions/runs/1/job/3","run_id":1,"attempt":2,"head":revision,"name":"Fresh-checkout verification","status":"completed","conclusion":"success"}]});
    let receipt = json!({"repo":"robchristie/pagefold","base":"main","branch":"codex/fixture","pr":2,
        "head":head,"head_tree":tree,"merge_commit":merge,"merge_tree":tree,"merged":true,"tree_equal":true,
        "post_merge_verified":true,"pre_merge_ci":ci(&head),"post_merge_ci":ci(&merge)});
    f.runtime.validate_delivery_receipt(&receipt).unwrap();
    assert!(f.runtime.validate_delivery_receipt(&json!({"post_merge_verified":true})).is_err());
    for pointer in ["/repo","/merge_tree","/post_merge_ci/head","/post_merge_ci/run/head",
        "/pre_merge_ci/jobs/0/head","/pre_merge_ci/jobs/0/attempt","/post_merge_ci/run/url"] {
        let mut invalid = receipt.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!("mismatch");
        assert!(f.runtime.validate_delivery_receipt(&invalid).is_err(),"{pointer}");
    }
    let mut state = snapshot(&f.store,&f.id).unwrap();
    state.delivery_operations.push(EngineeringDeliveryOperation {id:"merge".into(),execution_id:f.supervisor.id.clone(),
        contract_revision:state.contract_revision,operation:"merge".into(),arguments_json:"{}".into(),
        evidence_digest:Some(f.runtime.blob(&serde_json::to_vec(&receipt).unwrap()).unwrap()),post_merge_verified:true});
    let args = json!({"pr":2,"head":head,"tree":tree,"merge_commit":merge});
    assert!(f.runtime.validate_cleanup_scope(&state,&args).unwrap_err().to_string().contains("execution"));
    for execution in &mut state.executions { execution.cessation_verified = true; }
    f.runtime.validate_cleanup_scope(&state,&args).unwrap();
    let mut wrong = args.clone();
    wrong["head"] = json!(merge);
    assert!(f.runtime.validate_cleanup_scope(&state,&wrong).is_err());
}

#[test]
fn uncaptured_package_input_must_still_match_after_validation() {
    let mut f = Fixture::with_package_input(true, Some("fixed input"));
    fs::write(f.runtime.profile.workspace.join(".gitignore"), "input.txt\n").unwrap();
    fs::write(f.runtime.profile.workspace.join("reader.txt"), "page").unwrap();
    let (artefact, _) = f.runtime.file("reader.txt").unwrap();
    let context = f.runtime.evidence_context().unwrap();
    let source = &context["source"];
    assert!(source["files"].get("input.txt").is_none());
    let log = vec![
        Event { sequence: 1, kind: "item/completed".into(), value: json!({"threadId":"root","turnId":"turn","item":{"id":"canonical","type":"commandExecution","command":"tools/check.sh","aggregatedOutput":"PASS","exitCode":0}}) },
        Event { sequence: 2, kind: "command_source".into(), value: json!({"phase":"item/started","thread_id":"root","turn_id":"turn","item_id":"canonical","source":source}) },
        Event { sequence: 3, kind: "command_source".into(), value: json!({"phase":"item/completed","thread_id":"root","turn_id":"turn","item_id":"canonical","source":source}) },
    ];
    let directory = f.directory(&f.worker);
    f.runtime.dynamic(&mut f.store, &directory, &f.worker, "registered", &json!({"tool":"bokkie_validation","arguments":{"item_id":"canonical","artefact":artefact,"criterion_id":"reader"}}), &log, 105).unwrap();
    let state = snapshot(&f.store, &f.id).unwrap();
    assert_eq!(f.runtime.discover_evidence(&state, &f.worker).unwrap()["validations"].as_array().unwrap().len(), 1);
    fs::write(f.runtime.profile.workspace.join("input.txt"), "changed input").unwrap();
    assert_eq!(f.runtime.evidence_context().unwrap()["source"], *source);
    let found = f.runtime.discover_evidence(&state, &f.worker).unwrap();
    assert!(found["validations"].as_array().unwrap().is_empty());
    assert!(found["rejected"].to_string().contains("source file no longer matches submission"));
}
