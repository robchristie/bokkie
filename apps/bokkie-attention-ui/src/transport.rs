use std::{sync::mpsc::Sender, time::Duration};

use bokkie_operator_api::{ActionPrecondition, ObligationTopic, OperatorSnapshot};
use eframe::egui;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::model::LifecycleAction;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionRequest {
    pub action: LifecycleAction,
    pub obligation_id: String,
    /// Retained for compatibility with the legacy goal-level route shape.
    pub fingerprint: Option<String>,
    pub proposal_instance_id: Option<String>,
    pub precondition: ActionPrecondition,
    pub actor: String,
    pub note: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiRequest {
    Snapshot,
    Topic {
        obligation_id: String,
        generation: u64,
    },
    Act(Box<ActionRequest>),
}

#[derive(Debug)]
pub enum ApiPayload {
    Snapshot(OperatorSnapshot),
    Topic(ObligationTopic),
    ActionAccepted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiFailure {
    Conflict(String),
    Other(String),
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(message) | Self::Other(message) => formatter.write_str(message),
        }
    }
}

#[derive(Debug)]
pub struct ApiMessage {
    pub request: ApiRequest,
    pub result: Result<ApiPayload, ApiFailure>,
}

pub struct Transport {
    #[cfg(not(target_arch = "wasm32"))]
    base: String,
}

impl Transport {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(base: &str) -> Result<Self, String> {
        let parsed = url::Url::parse(base).map_err(|error| format!("invalid API base: {error}"))?;
        let is_loopback = match parsed.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(_)) | None => false,
        };
        if parsed.scheme() != "http" || !is_loopback {
            return Err("API base must use http on a literal loopback address".to_owned());
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("API base must not contain credentials, a query, or a fragment".to_owned());
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn new() -> Self {
        Self {}
    }

    pub fn send(&self, request: ApiRequest, sender: Sender<ApiMessage>, context: egui::Context) {
        let endpoint = self.endpoint(&request);
        let mut http = match &request {
            ApiRequest::Snapshot | ApiRequest::Topic { .. } => ehttp::Request::get(endpoint),
            ApiRequest::Act(action) => {
                let body = action_body(action);
                let mut request = ehttp::Request::post(endpoint, body);
                request.headers.insert("Content-Type", "application/json");
                request
            }
        };
        http.timeout = Some(Duration::from_secs(5));
        http.headers.insert("Accept", "application/json");
        ehttp::fetch(http, move |response| {
            let result = response
                .map_err(|error| ApiFailure::Other(error.to_string()))
                .and_then(|response| decode(&request, response));
            let _ = sender.send(ApiMessage { request, result });
            context.request_repaint();
        });
    }

    fn endpoint(&self, request: &ApiRequest) -> String {
        let path = match request {
            ApiRequest::Snapshot => "/operator/snapshot".to_owned(),
            ApiRequest::Topic { obligation_id, .. } => format!(
                "/operator/obligations/{}/topic",
                encode_path_segment(obligation_id)
            ),
            ApiRequest::Act(action) => action_endpoint(action),
        };
        #[cfg(not(target_arch = "wasm32"))]
        return format!("{}{path}", self.base);
        #[cfg(target_arch = "wasm32")]
        path
    }
}

fn action_body(action: &ActionRequest) -> Vec<u8> {
    serde_json::to_vec(&ActionBody {
        precondition: &action.precondition,
        actor: &action.actor,
        note: (!action.note.trim().is_empty()).then_some(action.note.trim()),
    })
    .expect("action body is serialisable")
}

#[cfg(target_arch = "wasm32")]
impl Default for Transport {
    fn default() -> Self {
        Self::new()
    }
}

fn action_endpoint(request: &ActionRequest) -> String {
    match request.action {
        LifecycleAction::Approve => format!(
            "/operator/obligations/{}/approve",
            encode_path_segment(&request.obligation_id)
        ),
        LifecycleAction::Reject => format!(
            "/operator/obligations/{}/reject",
            encode_path_segment(&request.obligation_id)
        ),
        LifecycleAction::Retry => format!(
            "/operator/obligations/{}/retry",
            encode_path_segment(&request.obligation_id)
        ),
        LifecycleAction::Cancel => format!(
            "/operator/obligations/{}/cancel",
            encode_path_segment(&request.obligation_id)
        ),
        LifecycleAction::ApproveGardenerProposal => format!(
            "/operator/gardener/proposal-instances/{}/approve",
            encode_path_segment(request.proposal_instance_id.as_deref().unwrap_or_default())
        ),
        LifecycleAction::RejectGardenerProposal => format!(
            "/operator/gardener/proposal-instances/{}/reject",
            encode_path_segment(request.proposal_instance_id.as_deref().unwrap_or_default())
        ),
    }
}

fn decode(request: &ApiRequest, response: ehttp::Response) -> Result<ApiPayload, ApiFailure> {
    if !response.ok {
        let message = serde_json::from_slice::<ErrorEnvelope>(&response.bytes)
            .map(|body| body.error.message)
            .unwrap_or_else(|_| {
                format!(
                    "request failed with HTTP {} {}",
                    response.status, response.status_text
                )
            });
        return if response.status == 409 {
            Err(ApiFailure::Conflict(message))
        } else {
            Err(ApiFailure::Other(message))
        };
    }
    match request {
        ApiRequest::Snapshot => decode_json(&response).map(ApiPayload::Snapshot),
        ApiRequest::Topic { .. } => decode_json(&response).map(ApiPayload::Topic),
        ApiRequest::Act(_) => Ok(ApiPayload::ActionAccepted),
    }
}

fn decode_json<T: DeserializeOwned>(response: &ehttp::Response) -> Result<T, ApiFailure> {
    response
        .json()
        .map_err(|error| ApiFailure::Other(format!("invalid API response: {error}")))
}

fn encode_path_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[derive(Serialize)]
struct ActionBody<'a> {
    precondition: &'a ActionPrecondition,
    actor: &'a str,
    note: Option<&'a str>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(action: LifecycleAction) -> ApiRequest {
        ApiRequest::Act(Box::new(ActionRequest {
            action,
            obligation_id: "obligation/1".to_owned(),
            fingerprint: Some("abc def".to_owned()),
            proposal_instance_id: Some("instance/2".to_owned()),
            precondition: ActionPrecondition {
                obligation_id: "obligation/1".to_owned(),
                occurrence: 1,
                state_revision: 7,
                gardener_fingerprint: action.is_gardener().then(|| "abc def".to_owned()),
                gardener_proposal_instance_id: action
                    .is_gardener()
                    .then(|| "instance/2".to_owned()),
                gardener_source_commit: action.is_gardener().then(|| "c".repeat(40)),
                gardener_source_observation_id: action.is_gardener().then_some(9),
                gardener_source_inspection_id: action
                    .is_gardener()
                    .then(|| "inspection/2".to_owned()),
                gardener_generation: action.is_gardener().then_some(2),
            },
            actor: "operator".to_owned(),
            note: String::new(),
        }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_transport_accepts_only_literal_http_loopback_bases() {
        assert!(Transport::new("http://127.0.0.1:7744").is_ok());
        assert!(Transport::new("http://[::1]:7744").is_ok());
        assert!(Transport::new("http://localhost:7744").is_err());
        assert!(Transport::new("https://127.0.0.1:7744").is_err());
        assert!(Transport::new("http://192.0.2.1:7744").is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn endpoints_use_operator_reads_and_exact_lifecycle_paths() {
        let transport = Transport::new("http://127.0.0.1:7744/").unwrap();
        assert_eq!(
            transport.endpoint(&ApiRequest::Snapshot),
            "http://127.0.0.1:7744/operator/snapshot"
        );
        assert_eq!(
            transport.endpoint(&ApiRequest::Topic {
                obligation_id: "obligation/1".to_owned(),
                generation: 7,
            }),
            "http://127.0.0.1:7744/operator/obligations/obligation%2F1/topic"
        );
        let expected_paths = [
            "/operator/obligations/obligation%2F1/approve",
            "/operator/obligations/obligation%2F1/reject",
            "/operator/obligations/obligation%2F1/retry",
            "/operator/obligations/obligation%2F1/cancel",
            "/operator/gardener/proposal-instances/instance%2F2/approve",
            "/operator/gardener/proposal-instances/instance%2F2/reject",
        ];
        for (lifecycle_action, expected_path) in
            LifecycleAction::ALL.into_iter().zip(expected_paths)
        {
            assert_eq!(
                transport.endpoint(&action(lifecycle_action)),
                format!("http://127.0.0.1:7744{expected_path}")
            );
        }
    }

    #[test]
    fn transition_conflict_is_classified_separately_from_transport_failure() {
        let response = ehttp::Response {
            url: "http://127.0.0.1:7744/operator/obligations/id/approve".to_owned(),
            ok: false,
            status: 409,
            status_text: "Conflict".to_owned(),
            headers: ehttp::Headers::default(),
            bytes: br#"{"error":{"code":"transition_conflict","message":"occurrence changed"}}"#
                .to_vec(),
        };
        assert!(matches!(
            decode(&action(LifecycleAction::Approve), response),
            Err(ApiFailure::Conflict(message)) if message == "occurrence changed"
        ));
    }

    #[test]
    fn every_lifecycle_action_body_carries_the_reviewed_precondition() {
        for lifecycle_action in LifecycleAction::ALL {
            let request = match action(lifecycle_action) {
                ApiRequest::Act(request) => request,
                _ => unreachable!(),
            };
            let body: serde_json::Value = serde_json::from_slice(&action_body(&request)).unwrap();
            assert_eq!(body["precondition"]["obligation_id"], "obligation/1");
            assert_eq!(body["precondition"]["occurrence"], 1);
            assert_eq!(body["precondition"]["state_revision"], 7);
            assert_eq!(
                body["precondition"]["gardener_fingerprint"].is_string(),
                lifecycle_action.is_gardener()
            );
        }
    }
}
