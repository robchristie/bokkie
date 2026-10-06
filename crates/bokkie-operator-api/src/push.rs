//! Public device configuration. Subscription endpoints and keys stay private.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PushDevice {
    pub id: String,
    pub label: String,
    pub active: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PushSetup {
    pub service: crate::ServiceIdentity,
    pub configured: bool,
    pub vapid_public_key: Option<String>,
    pub configuration_revision: i64,
    pub device: Option<PushDevice>,
    pub ttl_seconds: u32,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushRegisterRequest {
    pub command_id: String,
    pub configuration_revision: i64,
    pub label: String,
    pub endpoint: String,
    pub keys: PushKeys,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushDisableRequest {
    pub command_id: String,
    pub configuration_revision: i64,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushReceiptRequest {
    pub id: String,
    pub receipt_token: String,
    pub state: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedPushDelivery {
    pub subscription_label: String,
    pub expires_at: i64,
    pub device_status: Option<String>,
    pub device_reported_at: Option<i64>,
    pub accepted_ttl_seconds: Option<u32>,
}
