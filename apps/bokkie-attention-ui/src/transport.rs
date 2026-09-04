use std::{sync::mpsc::Sender, time::Duration};

use bokkie_operator_api::{
    API_CONTRACT_VERSION, ActionPrecondition, BOKKIE_BUILD_ID, ObligationTopic,
    OperatorObligationProjection, OperatorSnapshot, ProjectionChangePage, SUPPORTED_SCHEMA_VERSION,
    ServiceIdentity, SessionBootstrap,
};
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
    Bootstrap,
    SnapshotPage {
        generation: u64,
        cursor: Option<String>,
        watermark: Option<i64>,
    },
    TopicPage {
        obligation_id: String,
        generation: u64,
        cursor: Option<String>,
        watermark: Option<i64>,
    },
    Changes {
        generation: u64,
        after: i64,
        through: Option<i64>,
    },
    Obligation {
        obligation_id: String,
        generation: u64,
    },
    Act(Box<ActionRequest>),
}

#[derive(Debug)]
pub enum ApiPayload {
    Bootstrap(ApiSession),
    SnapshotPage(OperatorSnapshot),
    TopicPage(ObligationTopic),
    Changes(ProjectionChangePage),
    Obligation(Box<OperatorObligationProjection>),
    ActionAccepted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiFailure {
    Conflict(String),
    ProjectionGap(String),
    InvalidCursor(String),
    SessionChanged(String),
    Other(String),
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(message)
            | Self::ProjectionGap(message)
            | Self::InvalidCursor(message)
            | Self::SessionChanged(message)
            | Self::Other(message) => formatter.write_str(message),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ApiSession {
    service: ServiceIdentity,
    mutation_token: String,
}

impl std::fmt::Debug for ApiSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiSession")
            .field("service", &self.service)
            .field("mutation_token", &"[REDACTED]")
            .finish()
    }
}

impl ApiSession {
    pub(crate) fn from_bootstrap(bootstrap: SessionBootstrap) -> Result<Self, ApiFailure> {
        validate_compatibility(&bootstrap.service)?;
        if bootstrap.mutation_token.len() != 64
            || !bootstrap
                .mutation_token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ApiFailure::Other(
                "Bokkie returned an invalid mutation session".to_owned(),
            ));
        }
        Ok(Self {
            service: bootstrap.service,
            mutation_token: bootstrap.mutation_token,
        })
    }

    fn matches(&self, service: &ServiceIdentity) -> bool {
        self.service == *service
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

    pub fn send(
        &self,
        request: ApiRequest,
        session: Option<&ApiSession>,
        sender: Sender<ApiMessage>,
        context: egui::Context,
    ) {
        let mut http = match self.http_request(&request, session) {
            Ok(http) => http,
            Err(error) => {
                let _ = sender.send(ApiMessage {
                    request,
                    result: Err(error),
                });
                context.request_repaint();
                return;
            }
        };
        http.timeout = Some(Duration::from_secs(5));
        http.headers.insert("Accept", "application/json");
        let expected_session = session.cloned();
        ehttp::fetch(http, move |response| {
            let result = response
                .map_err(|error| ApiFailure::Other(error.to_string()))
                .and_then(|response| decode(&request, response, expected_session.as_ref()));
            let _ = sender.send(ApiMessage { request, result });
            context.request_repaint();
        });
    }

    fn http_request(
        &self,
        request: &ApiRequest,
        session: Option<&ApiSession>,
    ) -> Result<ehttp::Request, ApiFailure> {
        let endpoint = self.endpoint(request);
        match request {
            ApiRequest::Bootstrap
            | ApiRequest::SnapshotPage { .. }
            | ApiRequest::TopicPage { .. }
            | ApiRequest::Changes { .. }
            | ApiRequest::Obligation { .. } => Ok(ehttp::Request::get(endpoint)),
            ApiRequest::Act(action) => {
                let session = session.ok_or_else(|| {
                    ApiFailure::SessionChanged(
                        "A current Bokkie mutation session is required".to_owned(),
                    )
                })?;
                let body = action_body(action);
                let mut request = ehttp::Request::new(
                    ehttp::Method::POST,
                    endpoint,
                    &[("Content-Type", "application/json")],
                )
                .with_body(body);
                request
                    .headers
                    .insert("X-Bokkie-Mutation-Token", &session.mutation_token);
                Ok(request)
            }
        }
    }

    fn endpoint(&self, request: &ApiRequest) -> String {
        let path = match request {
            ApiRequest::Bootstrap => "/bootstrap".to_owned(),
            ApiRequest::SnapshotPage {
                cursor, watermark, ..
            } => page_endpoint("/operator/snapshot", cursor.as_deref(), *watermark),
            ApiRequest::TopicPage {
                obligation_id,
                cursor,
                watermark,
                ..
            } => page_endpoint(
                &format!(
                    "/operator/obligations/{}/topic",
                    encode_path_segment(obligation_id)
                ),
                cursor.as_deref(),
                *watermark,
            ),
            ApiRequest::Changes { after, through, .. } => {
                let mut path = format!("/operator/changes?after={after}");
                if let Some(through) = through {
                    path.push_str(&format!("&through={through}"));
                }
                path
            }
            ApiRequest::Obligation { obligation_id, .. } => format!(
                "/operator/obligations/{}",
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

fn page_endpoint(path: &str, cursor: Option<&str>, watermark: Option<i64>) -> String {
    let mut parameters = Vec::new();
    if let Some(watermark) = watermark {
        parameters.push(format!("watermark={watermark}"));
    }
    if let Some(cursor) = cursor {
        parameters.push(format!("cursor={}", encode_query_value(cursor)));
    }
    if parameters.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", parameters.join("&"))
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

fn decode(
    request: &ApiRequest,
    response: ehttp::Response,
    expected_session: Option<&ApiSession>,
) -> Result<ApiPayload, ApiFailure> {
    if !response.ok {
        let (code, message) = serde_json::from_slice::<ErrorEnvelope>(&response.bytes)
            .map(|body| (Some(body.error.code), body.error.message))
            .unwrap_or_else(|_| {
                (
                    None,
                    format!(
                        "request failed with HTTP {} {}",
                        response.status, response.status_text
                    ),
                )
            });
        return match (response.status, code.as_deref()) {
            (409, Some("projection_gap")) => Err(ApiFailure::ProjectionGap(message)),
            (_, Some("invalid_request")) if request.is_projection_read() => {
                Err(ApiFailure::InvalidCursor(message))
            }
            (409, _) => Err(ApiFailure::Conflict(message)),
            (403, Some("mutation_token_required" | "mutation_token_invalid")) => {
                Err(ApiFailure::SessionChanged(message))
            }
            _ => Err(ApiFailure::Other(message)),
        };
    }
    match request {
        ApiRequest::Bootstrap => decode_json::<SessionBootstrap>(&response)
            .and_then(ApiSession::from_bootstrap)
            .map(ApiPayload::Bootstrap),
        ApiRequest::SnapshotPage { .. } => {
            decode_json::<OperatorSnapshot>(&response).and_then(|snapshot| {
                validate_response_identity(
                    snapshot.service.as_ref(),
                    expected_session,
                    "operator snapshot page",
                )?;
                Ok(ApiPayload::SnapshotPage(snapshot))
            })
        }
        ApiRequest::TopicPage { .. } => {
            decode_json::<ObligationTopic>(&response).and_then(|topic| {
                validate_response_identity(
                    topic.service.as_ref(),
                    expected_session,
                    "operator topic page",
                )?;
                Ok(ApiPayload::TopicPage(topic))
            })
        }
        ApiRequest::Changes { .. } => {
            decode_json::<ProjectionChangePage>(&response).and_then(|page| {
                validate_response_identity(
                    Some(&page.service),
                    expected_session,
                    "operator change page",
                )?;
                Ok(ApiPayload::Changes(page))
            })
        }
        ApiRequest::Obligation { .. } => decode_json::<OperatorObligationProjection>(&response)
            .and_then(|projection| {
                validate_response_identity(
                    Some(&projection.service),
                    expected_session,
                    "operator obligation projection",
                )?;
                Ok(ApiPayload::Obligation(Box::new(projection)))
            }),
        ApiRequest::Act(_) => Ok(ApiPayload::ActionAccepted),
    }
}

impl ApiRequest {
    fn is_projection_read(&self) -> bool {
        matches!(
            self,
            Self::SnapshotPage { .. }
                | Self::TopicPage { .. }
                | Self::Changes { .. }
                | Self::Obligation { .. }
        )
    }
}

fn validate_response_identity(
    service: Option<&ServiceIdentity>,
    expected_session: Option<&ApiSession>,
    response_name: &str,
) -> Result<(), ApiFailure> {
    let service = service.ok_or_else(|| {
        ApiFailure::SessionChanged(format!(
            "Bokkie {response_name} omitted its process identity"
        ))
    })?;
    let session = expected_session.ok_or_else(|| {
        ApiFailure::SessionChanged(format!(
            "Bokkie {response_name} arrived without a bootstrap session"
        ))
    })?;
    if !session.matches(service) {
        return Err(ApiFailure::SessionChanged(
            "Bokkie restarted or changed its API session".to_owned(),
        ));
    }
    Ok(())
}

fn decode_json<T: DeserializeOwned>(response: &ehttp::Response) -> Result<T, ApiFailure> {
    response
        .json()
        .map_err(|error| ApiFailure::Other(format!("invalid API response: {error}")))
}

fn encode_path_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn encode_query_value(value: &str) -> String {
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
    code: String,
    message: String,
}

fn validate_compatibility(service: &ServiceIdentity) -> Result<(), ApiFailure> {
    if service.build != BOKKIE_BUILD_ID
        || service.api_contract_version != API_CONTRACT_VERSION
        || service.schema_version != SUPPORTED_SCHEMA_VERSION
    {
        return Err(ApiFailure::Other(format!(
            "Incompatible Bokkie service (build {}, API {}, schema {}); this UI requires build {}, API {}, schema {}",
            service.build,
            service.api_contract_version,
            service.schema_version,
            BOKKIE_BUILD_ID,
            API_CONTRACT_VERSION,
            SUPPORTED_SCHEMA_VERSION
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(session_id: &str) -> ServiceIdentity {
        ServiceIdentity {
            build: BOKKIE_BUILD_ID.to_owned(),
            api_contract_version: API_CONTRACT_VERSION,
            schema_version: SUPPORTED_SCHEMA_VERSION,
            process_id: 42,
            session_id: session_id.to_owned(),
        }
    }

    fn session(session_id: &str, token: &str) -> ApiSession {
        ApiSession::from_bootstrap(SessionBootstrap {
            service: service(session_id),
            mutation_token: token.to_owned(),
        })
        .unwrap()
    }

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
            transport.endpoint(&ApiRequest::Bootstrap),
            "http://127.0.0.1:7744/bootstrap"
        );
        assert_eq!(
            transport.endpoint(&ApiRequest::SnapshotPage {
                generation: 1,
                cursor: None,
                watermark: None,
            }),
            "http://127.0.0.1:7744/operator/snapshot"
        );
        assert_eq!(
            transport.endpoint(&ApiRequest::TopicPage {
                obligation_id: "obligation/1".to_owned(),
                generation: 7,
                cursor: Some("opaque+/=".to_owned()),
                watermark: Some(41),
            }),
            "http://127.0.0.1:7744/operator/obligations/obligation%2F1/topic?watermark=41&cursor=opaque%2B%2F%3D"
        );
        assert_eq!(
            transport.endpoint(&ApiRequest::Changes {
                generation: 8,
                after: 41,
                through: Some(73),
            }),
            "http://127.0.0.1:7744/operator/changes?after=41&through=73"
        );
        assert_eq!(
            transport.endpoint(&ApiRequest::Obligation {
                obligation_id: "obligation/1".to_owned(),
                generation: 8,
            }),
            "http://127.0.0.1:7744/operator/obligations/obligation%2F1"
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
            decode(&action(LifecycleAction::Approve), response, None),
            Err(ApiFailure::Conflict(message)) if message == "occurrence changed"
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn mutations_require_a_memory_only_session_and_submit_its_header() {
        let transport = Transport::new("http://127.0.0.1:7744").unwrap();
        let request = action(LifecycleAction::Cancel);
        assert!(matches!(
            transport.http_request(&request, None),
            Err(ApiFailure::SessionChanged(_))
        ));

        let token = "a".repeat(64);
        let session = session("session-one", &token);
        let http = transport.http_request(&request, Some(&session)).unwrap();
        assert_eq!(
            http.headers.get("X-Bokkie-Mutation-Token"),
            Some(token.as_str())
        );
        assert_eq!(http.headers.get("Content-Type"), Some("application/json"));
        assert!(!format!("{session:?}").contains(&token));
    }

    #[test]
    fn incompatible_bootstrap_and_restarted_snapshot_fail_closed() {
        let mut incompatible = service("session-one");
        incompatible.api_contract_version += 1;
        assert!(matches!(
            ApiSession::from_bootstrap(SessionBootstrap {
                service: incompatible,
                mutation_token: "a".repeat(64),
            }),
            Err(ApiFailure::Other(message)) if message.contains("Incompatible")
        ));

        let old_session = session("session-one", &"a".repeat(64));
        let response = ehttp::Response {
            url: "http://127.0.0.1:7744/operator/snapshot".to_owned(),
            ok: true,
            status: 200,
            status_text: "OK".to_owned(),
            headers: ehttp::Headers::default(),
            bytes: serde_json::to_vec(&OperatorSnapshot {
                captured_at: 100,
                service: Some(service("session-two")),
                next_cursor: None,
                watermark: 17,
                obligations: Vec::new(),
            })
            .unwrap(),
        };
        assert!(matches!(
            decode(
                &ApiRequest::SnapshotPage {
                    generation: 1,
                    cursor: None,
                    watermark: None,
                },
                response,
                Some(&old_session)
            ),
            Err(ApiFailure::SessionChanged(message)) if message.contains("restarted")
        ));
    }

    #[test]
    fn every_projection_response_requires_the_exact_bootstrap_identity() {
        let current = session("session-one", &"a".repeat(64));
        assert!(
            validate_response_identity(
                Some(&service("session-one")),
                Some(&current),
                "projection",
            )
            .is_ok()
        );
        assert!(matches!(
            validate_response_identity(
                Some(&service("session-two")),
                Some(&current),
                "projection",
            ),
            Err(ApiFailure::SessionChanged(message)) if message.contains("restarted")
        ));
        assert!(matches!(
            validate_response_identity(None, Some(&current), "projection"),
            Err(ApiFailure::SessionChanged(message)) if message.contains("omitted")
        ));
        assert!(matches!(
            validate_response_identity(Some(&service("session-one")), None, "projection"),
            Err(ApiFailure::SessionChanged(message)) if message.contains("without a bootstrap")
        ));
    }

    #[test]
    fn projection_gap_and_invalid_cursor_are_recovery_failures() {
        let request = ApiRequest::Changes {
            generation: 1,
            after: 7,
            through: None,
        };
        let response = |status, code: &str, message: &str| ehttp::Response {
            url: "http://127.0.0.1:7744/operator/changes".to_owned(),
            ok: false,
            status,
            status_text: "failure".to_owned(),
            headers: ehttp::Headers::default(),
            bytes: serde_json::to_vec(&serde_json::json!({
                "error": {"code": code, "message": message}
            }))
            .unwrap(),
        };
        assert!(matches!(
            decode(
                &request,
                response(409, "projection_gap", "cursor was pruned"),
                None
            ),
            Err(ApiFailure::ProjectionGap(message)) if message == "cursor was pruned"
        ));
        assert!(matches!(
            decode(
                &request,
                response(400, "invalid_request", "cursor is malformed"),
                None
            ),
            Err(ApiFailure::InvalidCursor(message)) if message == "cursor is malformed"
        ));
    }

    #[test]
    fn stale_token_error_is_classified_as_a_session_change() {
        let response = ehttp::Response {
            url: "http://127.0.0.1:7744/operator/obligations/id/cancel".to_owned(),
            ok: false,
            status: 403,
            status_text: "Forbidden".to_owned(),
            headers: ehttp::Headers::default(),
            bytes: br#"{"error":{"code":"mutation_token_invalid","message":"acquire a current session"}}"#
                .to_vec(),
        };
        assert!(matches!(
            decode(&action(LifecycleAction::Cancel), response, None),
            Err(ApiFailure::SessionChanged(message)) if message == "acquire a current session"
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
