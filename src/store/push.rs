//! Device selection and receipts are fenced Store operations, never network calls.
use super::*;
use crate::notifications::push::{
    PushIntent, PushSubscription, push_device_id, validate_subscription,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bokkie_operator_api::{
    ManagedCapabilityProfile, ManagedPushDelivery, PushDevice, PushDisableRequest,
    PushReceiptRequest, PushRegisterRequest, PushSetup, ServiceIdentity,
};
use hmac::{Hmac, Mac};
use subtle::ConstantTimeEq;

fn invalid(text: &str) -> StoreError {
    StoreError::Invalid(text.into())
}
fn conflict(text: &str) -> StoreError {
    StoreError::Conflict(text.into())
}
fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn receipt_token(subscription: &PushSubscription, id: &str) -> Result<String, StoreError> {
    let key = URL_SAFE_NO_PAD
        .decode(&subscription.auth)
        .map_err(|_| invalid("Invalid saved push authentication key"))?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&key).map_err(|_| invalid("Invalid saved push key"))?;
    mac.update(b"bokkie-push-receipt-v1:");
    mac.update(id.as_bytes());
    Ok(format!("{:x}", mac.finalize().into_bytes()))
}
fn subscription(conn: &Connection, id: &str) -> Result<PushSubscription, StoreError> {
    conn.query_row(
        "SELECT id,label,endpoint,p256dh,auth,vapid_public_key FROM push_devices WHERE id=?1",
        [id],
        |r| {
            Ok(PushSubscription {
                id: r.get(0)?,
                label: r.get(1)?,
                endpoint: r.get(2)?,
                p256dh: r.get(3)?,
                auth: r.get(4)?,
                vapid_public_key: r.get(5)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound("Saved notification device is unavailable".into()))
}
pub(super) fn intent(conn: &Connection, id: &str) -> Result<Option<PushIntent>, StoreError> {
    let data: Option<(String, i64)> = conn
        .query_row(
            "SELECT device_id,expires_at FROM push_deliveries WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    data.map(|(device, expires_at)| {
        let subscription = subscription(conn, &device)?;
        let receipt_token = receipt_token(&subscription, id)?;
        Ok(PushIntent {
            subscription,
            receipt_token,
            expires_at,
        })
    })
    .transpose()
}
pub(super) fn save_intent(
    tx: &Transaction<'_>,
    id: &str,
    definition: &bokkie_operator_api::ManagedTaskDefinition,
    now: i64,
) -> Result<(), StoreError> {
    let device = push_device_id(&definition.profile_revision)
        .ok_or_else(|| invalid("Unknown push profile"))?;
    let subscription = subscription(tx, device)?;
    if definition.destination != format!("Bokkie on {}", subscription.label) {
        return Err(conflict(
            "The reminder destination differs from its reviewed device",
        ));
    }
    let ttl: i64 = tx.query_row(
        "SELECT ttl_seconds FROM push_devices WHERE id=?1",
        [device],
        |r| r.get(0),
    )?;
    let expires = now
        .checked_add(ttl)
        .ok_or_else(|| invalid("Invalid push expiry"))?;
    let token = receipt_token(&subscription, id)?;
    tx.execute(
        "INSERT INTO push_deliveries(id,device_id,expires_at,receipt_hash) VALUES(?1,?2,?3,?4)",
        params![id, device, expires, hash(token.as_bytes())],
    )?;
    Ok(())
}
pub(super) fn validate_destination(
    conn: &Connection,
    definition: &bokkie_operator_api::ManagedTaskDefinition,
) -> Result<(), StoreError> {
    let Some(id) = push_device_id(&definition.profile_revision) else {
        return Ok(());
    };
    let active:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM push_configuration c JOIN push_devices d ON d.id=c.device_id WHERE c.active=1 AND c.device_id=?1 AND ?2='Bokkie on '||d.label)",params![id,definition.destination],|r|r.get(0))?;
    if !active {
        return Err(conflict(
            "The reviewed notification device changed or was disabled; obtain a fresh task review",
        ));
    }
    Ok(())
}
pub(super) fn projection(
    conn: &Connection,
    id: &str,
) -> Result<Option<ManagedPushDelivery>, StoreError> {
    conn.query_row("SELECT d.label,p.expires_at,p.device_status,p.device_reported_at,p.accepted_ttl_seconds FROM push_deliveries p JOIN push_devices d ON d.id=p.device_id WHERE p.id=?1",[id],|r|Ok(ManagedPushDelivery {subscription_label:r.get(0)?,expires_at:r.get(1)?,device_status:r.get(2)?,device_reported_at:r.get(3)?,accepted_ttl_seconds:r.get(4)?})).optional().map_err(Into::into)
}
fn setup(
    conn: &Connection,
    service: ServiceIdentity,
    key: Option<String>,
    ttl_seconds: u32,
) -> Result<PushSetup, StoreError> {
    let (configuration_revision, id, active): (i64, Option<String>, bool) = conn.query_row(
        "SELECT revision,device_id,active FROM push_configuration WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let device = id
        .map(|id| {
            subscription(conn, &id).map(|s| PushDevice {
                id: s.id,
                label: s.label,
                active,
            })
        })
        .transpose()?;
    Ok(PushSetup {
        service,
        configured: key.is_some(),
        vapid_public_key: key,
        configuration_revision,
        device,
        ttl_seconds,
    })
}
fn replay(
    tx: &Transaction<'_>,
    command: &str,
    request: &str,
) -> Result<Option<PushSetup>, StoreError> {
    if Uuid::parse_str(command).is_err() {
        return Err(invalid("Notification command requires a UUID"));
    }
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash,result_json FROM push_commands WHERE id=?1",
            [command],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    previous
        .map(|(saved, json)| {
            if saved != hash(request.as_bytes()) {
                return Err(conflict(
                    "Notification command identity was reused with different input",
                ));
            }
            serde_json::from_str(&json).map_err(|_| invalid("Invalid saved notification receipt"))
        })
        .transpose()
}
fn record(
    tx: &Transaction<'_>,
    command: &str,
    request: &str,
    result: &PushSetup,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO push_commands(id,request_hash,result_json) VALUES(?1,?2,?3)",
        params![
            command,
            hash(request.as_bytes()),
            serde_json::to_string(result)
                .map_err(|_| invalid("Cannot encode notification receipt"))?
        ],
    )?;
    Ok(())
}
impl Store {
    pub(crate) fn push_retention(&self, profile: &str) -> Result<Option<u32>, StoreError> {
        let Some(id) = push_device_id(profile) else {
            return Ok(None);
        };
        self.connection
            .query_row(
                "SELECT ttl_seconds FROM push_devices WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn validate_push_key(&self, key: &str) -> Result<(), StoreError> {
        let saved:Option<String>=self.connection.query_row("SELECT d.vapid_public_key FROM push_configuration c JOIN push_devices d ON d.id=c.device_id WHERE c.active=1",[],|r|r.get(0)).optional()?;
        if saved.is_some_and(|saved| saved != key) {
            return Err(conflict(
                "The selected device is bound to another VAPID key; restore that key or disable the device before rotating it",
            ));
        }
        Ok(())
    }
    pub fn push_setup(
        &self,
        service: ServiceIdentity,
        key: Option<String>,
        ttl_seconds: u32,
    ) -> Result<PushSetup, StoreError> {
        let tx = self.connection.unchecked_transaction()?;
        let view = setup(&tx, service, key, ttl_seconds)?;
        tx.commit()?;
        Ok(view)
    }
    pub fn push_profile(&self, key: &str) -> Result<Option<ManagedCapabilityProfile>, StoreError> {
        let device: Option<String> = self
            .connection
            .query_row(
                "SELECT device_id FROM push_configuration WHERE singleton=1 AND active=1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        device
            .map(|id| {
                let subscription = subscription(&self.connection, &id)?;
                Ok((subscription.vapid_public_key == key)
                    .then(|| ManagedCapabilityProfile::web_push(&id, &subscription.label)))
            })
            .transpose()
            .map(Option::flatten)
    }
    pub fn register_push(
        &mut self,
        request: &PushRegisterRequest,
        service: ServiceIdentity,
        key: &str,
        ttl_seconds: u32,
        now: i64,
    ) -> Result<PushSetup, StoreError> {
        if request.label.trim().is_empty()
            || request.label != request.label.trim()
            || request.label.chars().count() > 80
            || request.label.chars().any(char::is_control)
        {
            return Err(invalid("Give this device a name within 80 characters"));
        }
        validate_subscription(&request.endpoint, &request.keys.p256dh, &request.keys.auth)
            .map_err(|_| invalid("Invalid or unsupported push subscription"))?;
        let raw = serde_json::to_string(&("register", request, key, ttl_seconds))
            .map_err(|_| invalid("Invalid notification registration"))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(mut result) = replay(&tx, &request.command_id, &raw)? {
            result.service = service;
            return Ok(result);
        }
        let before = setup(&tx, service.clone(), Some(key.into()), ttl_seconds)?;
        if before.configuration_revision != request.configuration_revision {
            return Err(conflict(
                "Notification setup changed; refresh and review it again",
            ));
        }
        if before.device.is_some_and(|d| d.active) {
            return Err(conflict(
                "A notification device is already selected; explicitly disable it before enrolling another",
            ));
        }
        let id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO push_devices(id,label,endpoint,p256dh,auth,vapid_public_key,ttl_seconds,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id,request.label,request.endpoint,request.keys.p256dh,request.keys.auth,key,ttl_seconds,now])?;
        tx.execute("UPDATE push_configuration SET revision=revision+1,device_id=?1,active=1 WHERE singleton=1",[&id])?;
        let result = setup(&tx, service, Some(key.into()), ttl_seconds)?;
        record(&tx, &request.command_id, &raw, &result)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn disable_push(
        &mut self,
        request: &PushDisableRequest,
        service: ServiceIdentity,
        key: Option<String>,
        ttl_seconds: u32,
    ) -> Result<PushSetup, StoreError> {
        let raw = serde_json::to_string(&("disable", request))
            .map_err(|_| invalid("Invalid notification command"))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(mut result) = replay(&tx, &request.command_id, &raw)? {
            result.service = service;
            return Ok(result);
        }
        let before = setup(&tx, service.clone(), key.clone(), ttl_seconds)?;
        if before.configuration_revision != request.configuration_revision
            || !before.device.is_some_and(|d| d.active)
        {
            return Err(conflict(
                "Notification setup changed; refresh and review it again",
            ));
        }
        tx.execute(
            "UPDATE push_configuration SET revision=revision+1,active=0 WHERE singleton=1",
            [],
        )?;
        let result = setup(&tx, service, key, ttl_seconds)?;
        record(&tx, &request.command_id, &raw, &result)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn record_push_receipt(
        &mut self,
        request: &PushReceiptRequest,
        now: i64,
    ) -> Result<(), StoreError> {
        if !matches!(request.state.as_str(), "displayed" | "opened" | "expired")
            || request.receipt_token.len() != 64
            || !request
                .receipt_token
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(invalid("Invalid device receipt"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (expected, status, expiry): (String, Option<String>, i64) = tx
            .query_row(
                "SELECT receipt_hash,device_status,expires_at FROM push_deliveries WHERE id=?1",
                [&request.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound("Notification receipt is unavailable".into()))?;
        if !bool::from(
            expected
                .as_bytes()
                .ct_eq(hash(request.receipt_token.as_bytes()).as_bytes()),
        ) {
            return Err(invalid("Invalid device receipt proof"));
        }
        if request.state == "expired" && now < expiry {
            return Err(invalid("The notification has not expired"));
        }
        let rank = |s: &str| match s {
            "opened" => 3,
            "displayed" => 2,
            "expired" => 1,
            _ => 0,
        };
        if rank(&request.state) > rank(status.as_deref().unwrap_or("")) {
            tx.execute(
                "UPDATE push_deliveries SET device_status=?2,device_reported_at=?3 WHERE id=?1",
                params![request.id, request.state, now],
            )?;
            let obligation = require_obligation(&tx, &request.id)?;
            append_event(
                &tx,
                &request.id,
                obligation.occurrence,
                "push_device_reported",
                now,
                Some(obligation.state),
                obligation.state,
                json!({"state":request.state,"human_reading_proved":false}),
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{NotificationOutcome, push::PushConfig};
    use bokkie_operator_api::{ManagedTaskDefinition, ManagedTrigger, PushKeys};
    fn identity() -> ServiceIdentity {
        ServiceIdentity {
            build: "fixture".into(),
            api_contract_version: 1,
            schema_version: 15,
            process_id: 1,
            session_id: "session".into(),
        }
    }
    fn config() -> PushConfig {
        PushConfig {
            vapid_private_key: URL_SAFE_NO_PAD.encode([7; 32]),
            subject: "https://bokkie.example.org".into(),
            timeout_ms: 1000,
            ttl_seconds: 3600,
        }
    }
    fn enrol(store: &mut Store, label: &str) -> PushSetup {
        let key = config().public_key().unwrap();
        let revision = store
            .push_setup(identity(), Some(key.clone()), 3600)
            .unwrap()
            .configuration_revision;
        let r = PushRegisterRequest {
            command_id: Uuid::new_v4().to_string(),
            configuration_revision: revision,
            label: label.into(),
            endpoint: format!(
                "https://fcm.googleapis.com/fcm/send/synthetic-{}",
                label.replace(' ', "-")
            ),
            keys: PushKeys {
                p256dh: key.clone(),
                auth: URL_SAFE_NO_PAD.encode([9; 16]),
            },
        };
        let first = store
            .register_push(&r, identity(), &key, 3600, 100)
            .unwrap();
        assert_eq!(
            store
                .register_push(&r, identity(), &key, 3600, 100)
                .unwrap(),
            first
        );
        let mut changed = r.clone();
        changed.label = "Another".into();
        assert!(
            store
                .register_push(&changed, identity(), &key, 3600, 100)
                .is_err()
        );
        first
    }
    fn create_task(store: &mut Store, recurring: bool) -> String {
        let profile = store
            .push_profile(&config().public_key().unwrap())
            .unwrap()
            .unwrap();
        let mut definition = ManagedTaskDefinition::reminder(
            "Priorities",
            "Review today's priorities",
            &profile.destination,
        );
        definition.profile_revision = profile.revision.clone();
        definition.max_output_chars = 2000;
        if recurring {
            definition.trigger = ManagedTrigger::Recurring {
                cron: "* * * * *".into(),
                timezone: "Australia/Adelaide".into(),
            };
        }
        let id = store
            .managed_create(&Uuid::new_v4().to_string(), &definition, 100)
            .unwrap()
            .task_id;
        let preview = store
            .managed_preview(&id, "session", &[profile.clone()], 100)
            .unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        let command = Uuid::new_v4().to_string();
        let receipt = store
            .managed_activate(&command, &preview, "session", &[profile.clone()], 100)
            .unwrap();
        assert_eq!(
            store
                .managed_activate(&command, &preview, "session", &[profile], 100)
                .unwrap(),
            receipt
        );
        id
    }
    #[test]
    fn one_device_exact_commands_and_immutable_generations_preserve_history() {
        let mut store = Store::open_in_memory().unwrap();
        let first = enrol(&mut store, "Phone");
        let key = config().public_key().unwrap();
        let other = PushRegisterRequest {
            command_id: Uuid::new_v4().to_string(),
            configuration_revision: first.configuration_revision,
            label: "Laptop".into(),
            endpoint: "https://fcm.googleapis.com/fcm/send/synthetic-other".into(),
            keys: PushKeys {
                p256dh: key.clone(),
                auth: URL_SAFE_NO_PAD.encode([9; 16]),
            },
        };
        assert!(
            store
                .register_push(&other, identity(), &key, 3600, 100)
                .is_err()
        );
        let disabled = PushDisableRequest {
            command_id: Uuid::new_v4().to_string(),
            configuration_revision: first.configuration_revision,
        };
        let receipt = store
            .disable_push(&disabled, identity(), Some(key.clone()), 3600)
            .unwrap();
        assert_eq!(
            store
                .disable_push(&disabled, identity(), Some(key.clone()), 3600)
                .unwrap(),
            receipt
        );
        assert!(store.push_profile(&key).unwrap().is_none());
        let second = enrol(&mut store, "Laptop");
        assert_ne!(first.device.unwrap().id, second.device.unwrap().id);
        assert!(
            store
                .connection
                .execute("UPDATE push_devices SET label='Changed'", [])
                .is_err()
        );
        assert!(
            store
                .connection
                .execute("DELETE FROM push_devices", [])
                .is_err()
        );
        assert!(
            store
                .disable_push(
                    &PushDisableRequest {
                        command_id: Uuid::new_v4().to_string(),
                        ..disabled
                    },
                    identity(),
                    Some(key),
                    3600
                )
                .is_err()
        );
    }
    #[test]
    fn admitted_work_and_uncertain_delivery_survive_restart_device_change_and_receipt_replay() {
        let temp = tempfile::TempDir::new().unwrap();
        let db = temp.path().join("push.sqlite");
        let mut store = Store::open(&db).unwrap();
        let first = enrol(&mut store, "Phone");
        let task = create_task(&mut store, true);
        let claim = store
            .claim_due_reminders(120, 30, 1)
            .unwrap()
            .pop()
            .unwrap();
        store
            .disable_push(
                &PushDisableRequest {
                    command_id: Uuid::new_v4().to_string(),
                    configuration_revision: first.configuration_revision,
                },
                identity(),
                Some(config().public_key().unwrap()),
                3600,
            )
            .unwrap();
        enrol(&mut store, "Laptop");
        store
            .complete_managed_note(&claim, "Review today's priorities", 120)
            .unwrap();
        let run = store
            .managed_detail(&task)
            .unwrap()
            .runs
            .into_iter()
            .find(|r| r.obligation_id == claim.obligation_id)
            .unwrap();
        let delivery = run.delivery.unwrap();
        let saved = store.notification_intent(&delivery.id).unwrap();
        let encoded = crate::notifications::push::payload(&saved).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(body["id"], delivery.id);
        assert_eq!(body["task_id"], task);
        assert_eq!(saved.push.as_ref().unwrap().subscription.label, "Phone");
        assert_eq!(saved.push.as_ref().unwrap().expires_at, 3720);
        let send = store
            .claim_due_notifications(120, 5, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&send, 120).unwrap();
        drop(store);
        let mut store = Store::open(&db).unwrap();
        store.claim_due_notifications(126, 5, 1).unwrap();
        assert_eq!(
            store.notification_delivery(&delivery.id).unwrap().status,
            "uncertain"
        );
        assert!(
            store
                .claim_due_notifications(1000, 5, 1)
                .unwrap()
                .is_empty()
        );
        let receipt = PushReceiptRequest {
            id: delivery.id.clone(),
            receipt_token: saved.push.unwrap().receipt_token,
            state: "opened".into(),
        };
        store.record_push_receipt(&receipt, 127).unwrap();
        store.record_push_receipt(&receipt, 128).unwrap();
        let mut late = receipt.clone();
        late.state = "displayed".into();
        store.record_push_receipt(&late, 129).unwrap();
        let after = store.notification_delivery(&delivery.id).unwrap();
        assert_eq!(after.status, "uncertain");
        assert_eq!(after.push.unwrap().device_status.as_deref(), Some("opened"));
        late.receipt_token = "0".repeat(64);
        assert!(store.record_push_receipt(&late, 130).is_err());
        let recovery = bokkie_operator_api::NotificationRecoveryRequest {
            precondition: after.recovery.unwrap(),
            action: bokkie_operator_api::NotificationRecovery::MarkReconciled,
            note: Some("Device reported opened".into()),
        };
        store
            .recover_notification_if_current(&delivery.id, &recovery, 131)
            .unwrap();
        assert!(
            store
                .recover_notification_if_current(&delivery.id, &recovery, 132)
                .is_err()
        );
        assert!(
            store
                .claim_due_notifications(4000, 5, 1)
                .unwrap()
                .is_empty()
        );
        let p = store
            .push_profile(&config().public_key().unwrap())
            .unwrap()
            .unwrap();
        store
            .managed_prepare_reminder_destination(&task, &[p.clone()], 133)
            .unwrap();
        let review = store.managed_preview(&task, "session", &[p], 133).unwrap();
        assert_eq!(review.definition.destination, "Bokkie on Laptop");
        assert_eq!(
            store
                .managed_detail(&task)
                .unwrap()
                .active
                .unwrap()
                .definition
                .destination,
            "Bokkie on Phone"
        );
    }
    #[test]
    fn acceptance_device_evidence_expiry_and_subscription_rejection_are_distinct() {
        let mut store = Store::open_in_memory().unwrap();
        enrol(&mut store, "Phone");
        let task = create_task(&mut store, false);
        assert!(crate::managed::run_one_reminder(&mut store, 100).unwrap());
        let delivery = store
            .managed_detail(&task)
            .unwrap()
            .runs
            .remove(0)
            .delivery
            .unwrap();
        let intent = store.notification_intent(&delivery.id).unwrap();
        let claim = store
            .claim_due_notifications(100, 5, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&claim, 100).unwrap();
        store
            .complete_notification_send(
                &claim,
                NotificationOutcome::PushAccepted {
                    detail: "Push service accepted the reminder".into(),
                    ttl_seconds: 1800,
                },
                101,
            )
            .unwrap();
        let accepted = store.notification_delivery(&delivery.id).unwrap();
        assert_eq!(accepted.status, "accepted_by_push_service");
        assert!(accepted.push.unwrap().device_status.is_none());
        let r = PushReceiptRequest {
            id: delivery.id.clone(),
            receipt_token: intent.push.unwrap().receipt_token,
            state: "expired".into(),
        };
        assert!(store.record_push_receipt(&r, 102).is_err());
        store.record_push_receipt(&r, 3701).unwrap();
        assert_eq!(
            store
                .notification_delivery(&delivery.id)
                .unwrap()
                .push
                .unwrap()
                .device_status
                .as_deref(),
            Some("expired")
        );
        assert!(
            store
                .claim_due_notifications(4000, 5, 1)
                .unwrap()
                .is_empty()
        );
        let other = create_task(&mut store, false);
        assert!(crate::managed::run_one_reminder(&mut store, 100).unwrap());
        let d = store
            .managed_detail(&other)
            .unwrap()
            .runs
            .remove(0)
            .delivery
            .unwrap();
        let c = store
            .claim_due_notifications(100, 5, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&c, 100).unwrap();
        store
            .complete_notification_send(
                &c,
                NotificationOutcome::SubscriptionExpired {
                    detail: "Subscription expired; enrol again".into(),
                },
                101,
            )
            .unwrap();
        assert_eq!(
            store.notification_delivery(&d.id).unwrap().status,
            "needs_attention"
        );
        assert!(
            store
                .push_profile(&config().public_key().unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            store.managed_detail(&other).unwrap().runs[0]
                .result
                .is_some()
        );
    }
    #[test]
    fn encoded_payload_limits_are_checked_before_activation() {
        let mut store = Store::open_in_memory().unwrap();
        enrol(&mut store, "Phone");
        let profile = store
            .push_profile(&config().public_key().unwrap())
            .unwrap()
            .unwrap();
        let mut d =
            ManagedTaskDefinition::reminder("Oversized", "🦘".repeat(1000), &profile.destination);
        d.profile_revision = profile.revision.clone();
        d.max_output_chars = 2000;
        let id = store
            .managed_create("payload-draft", &d, 100)
            .unwrap()
            .task_id;
        let review = store
            .managed_preview(&id, "session", &[profile.clone()], 100)
            .unwrap();
        assert!(
            review
                .blockers
                .iter()
                .any(|b| b.contains("device's delivery limit"))
        );
        assert!(
            store
                .managed_activate("no-oversize", &review, "session", &[profile], 100)
                .is_err()
        );
    }

    #[test]
    fn expired_push_is_resolved_without_extending_or_resending_its_saved_intent() {
        let mut store = Store::open_in_memory().unwrap();
        enrol(&mut store, "Phone");
        let task = create_task(&mut store, false);
        crate::managed::run_one_reminder(&mut store, 100).unwrap();
        let d = store
            .managed_detail(&task)
            .unwrap()
            .runs
            .remove(0)
            .delivery
            .unwrap();
        let c = store
            .claim_due_notifications(3701, 5, 1)
            .unwrap()
            .pop()
            .unwrap();
        store.begin_notification_send(&c, 3701).unwrap();
        store
            .complete_notification_send(
                &c,
                NotificationOutcome::Rejected {
                    retryable: false,
                    detail: "Push delivery expired".into(),
                },
                3702,
            )
            .unwrap();
        let failed = store.notification_delivery(&d.id).unwrap();
        assert_eq!(failed.status, "needs_attention");
        let request = bokkie_operator_api::NotificationRecoveryRequest {
            precondition: failed.recovery.unwrap(),
            action: bokkie_operator_api::NotificationRecovery::RetryAcknowledgingDuplicateRisk,
            note: None,
        };
        assert!(
            store
                .recover_notification_if_current(&d.id, &request, 3703)
                .is_err()
        );
        let resolve = bokkie_operator_api::NotificationRecoveryRequest {
            action: bokkie_operator_api::NotificationRecovery::MarkReconciled,
            ..request
        };
        store
            .recover_notification_if_current(&d.id, &resolve, 3703)
            .unwrap();
        assert_eq!(
            store.notification_delivery(&d.id).unwrap().status,
            "reconciled"
        );
        assert!(
            store
                .claim_due_notifications(4000, 5, 1)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn push_edits_pause_resume_and_review_replay_keep_one_outstanding_schedule() {
        let mut store = Store::open_in_memory().unwrap();
        enrol(&mut store, "Phone");
        let task = create_task(&mut store, true);
        let profile = store
            .push_profile(&config().public_key().unwrap())
            .unwrap()
            .unwrap();
        let original = store.managed_detail(&task).unwrap();
        let mut revised = original.active.unwrap().definition;
        revised.trigger = ManagedTrigger::Recurring {
            cron: "0 10 * * MON-FRI".into(),
            timezone: "Australia/Adelaide".into(),
        };
        store
            .managed_revise(
                "weekday-change",
                &task,
                original.configuration_revision,
                &revised,
                101,
            )
            .unwrap();
        let review = store
            .managed_preview(&task, "session", &[profile.clone()], 101)
            .unwrap();
        let receipt = store
            .managed_activate(
                "confirm-change",
                &review,
                "session",
                &[profile.clone()],
                101,
            )
            .unwrap();
        assert_eq!(
            store
                .managed_activate(
                    "confirm-change",
                    &review,
                    "session",
                    &[profile.clone()],
                    101
                )
                .unwrap(),
            receipt
        );
        store
            .managed_pause("pause-push", &task, receipt.configuration_revision, 102)
            .unwrap();
        assert!(store.claim_due_reminders(200000, 30, 1).unwrap().is_empty());
        let rev = store.managed_detail(&task).unwrap().configuration_revision;
        let resumed = store
            .managed_resume("resume-push", &task, rev, &[profile.clone()], 200000)
            .unwrap();
        assert_eq!(
            store
                .managed_resume("resume-push", &task, rev, &[profile], 200000)
                .unwrap(),
            resumed
        );
        let detail = store.managed_detail(&task).unwrap();
        assert!(detail.next_wake_at.unwrap() > 200000);
        assert_eq!(
            detail
                .runs
                .iter()
                .filter(|r| !matches!(r.state.as_str(), "completed" | "cancelled"))
                .count(),
            1
        );
        assert_eq!(
            store.managed_catalogue("", None, 100).unwrap().items.len(),
            1
        );
    }
    #[test]
    fn device_fence_is_checked_in_the_task_transaction_even_with_a_stale_profile_snapshot() {
        let mut store = Store::open_in_memory().unwrap();
        let setup = enrol(&mut store, "Phone");
        let key = config().public_key().unwrap();
        let profile = store.push_profile(&key).unwrap().unwrap();
        let mut definition = ManagedTaskDefinition::reminder(
            "Pending review",
            "Review priorities",
            &profile.destination,
        );
        definition.profile_revision = profile.revision.clone();
        definition.max_output_chars = 2000;
        definition.trigger = ManagedTrigger::Recurring {
            cron: "0 9 * * MON-FRI".into(),
            timezone: "Australia/Adelaide".into(),
        };
        let id = store
            .managed_create("race-draft", &definition, 100)
            .unwrap()
            .task_id;
        let review = store
            .managed_preview(&id, "session", &[profile.clone()], 100)
            .unwrap();
        store
            .disable_push(
                &PushDisableRequest {
                    command_id: Uuid::new_v4().to_string(),
                    configuration_revision: setup.configuration_revision,
                },
                identity(),
                Some(key.clone()),
                3600,
            )
            .unwrap();
        assert!(
            store
                .managed_activate("stale-device-snapshot", &review, "session", &[profile], 101)
                .is_err()
        );
        assert!(store.managed_detail(&id).unwrap().runs.is_empty());
        let setup = enrol(&mut store, "New phone");
        let profile = store.push_profile(&key).unwrap().unwrap();
        store
            .managed_prepare_reminder_destination(&id, &[profile.clone()], 102)
            .unwrap();
        let review = store
            .managed_preview(&id, "session", &[profile.clone()], 102)
            .unwrap();
        let activated = store
            .managed_activate("fresh-device", &review, "session", &[profile.clone()], 102)
            .unwrap();
        store
            .managed_pause("race-pause", &id, activated.configuration_revision, 103)
            .unwrap();
        let paused = store.managed_detail(&id).unwrap().configuration_revision;
        store
            .disable_push(
                &PushDisableRequest {
                    command_id: Uuid::new_v4().to_string(),
                    configuration_revision: setup.configuration_revision,
                },
                identity(),
                Some(key),
                3600,
            )
            .unwrap();
        assert!(
            store
                .managed_resume("stale-resume-snapshot", &id, paused, &[profile], 104)
                .is_err()
        );
        assert_eq!(
            store.managed_detail(&id).unwrap().status,
            bokkie_operator_api::ManagedTaskStatus::Paused
        );
    }
}
