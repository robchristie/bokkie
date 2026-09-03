use std::{sync::mpsc::Sender, time::Duration};

use eframe::egui;
use serde::de::DeserializeOwned;

use crate::model::{AuditEventReadModel, ObligationReadModel};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiRequest {
    List,
    Events { obligation_id: String },
    Cancel { obligation_id: String },
}

#[derive(Debug)]
pub enum ApiPayload {
    Obligations(Vec<ObligationReadModel>),
    Events(Vec<AuditEventReadModel>),
    Cancelled(ObligationReadModel),
}

#[derive(Debug)]
pub struct ApiMessage {
    pub request: ApiRequest,
    pub result: Result<ApiPayload, String>,
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
            ApiRequest::List | ApiRequest::Events { .. } => ehttp::Request::get(endpoint),
            ApiRequest::Cancel { .. } => ehttp::Request::post(endpoint, Vec::new()),
        };
        http.timeout = Some(Duration::from_secs(5));
        http.headers.insert("Accept", "application/json");
        ehttp::fetch(http, move |response| {
            let result = response
                .map_err(|error| error.to_string())
                .and_then(|response| decode(&request, response));
            let _ = sender.send(ApiMessage { request, result });
            context.request_repaint();
        });
    }

    fn endpoint(&self, request: &ApiRequest) -> String {
        let path = match request {
            ApiRequest::List => "/obligations".to_owned(),
            ApiRequest::Events { obligation_id } => {
                format!("/obligations/{}/events", encode_path_segment(obligation_id))
            }
            ApiRequest::Cancel { obligation_id } => {
                format!("/obligations/{}/cancel", encode_path_segment(obligation_id))
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        return format!("{}{path}", self.base);
        #[cfg(target_arch = "wasm32")]
        path
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for Transport {
    fn default() -> Self {
        Self::new()
    }
}

fn decode(request: &ApiRequest, response: ehttp::Response) -> Result<ApiPayload, String> {
    if !response.ok {
        return Err(format!(
            "request failed with HTTP {} {}",
            response.status, response.status_text
        ));
    }
    match request {
        ApiRequest::List => decode_json(&response).map(ApiPayload::Obligations),
        ApiRequest::Events { .. } => decode_json(&response).map(ApiPayload::Events),
        ApiRequest::Cancel { .. } => decode_json(&response).map(ApiPayload::Cancelled),
    }
}

fn decode_json<T: DeserializeOwned>(response: &ehttp::Response) -> Result<T, String> {
    response
        .json()
        .map_err(|error| format!("invalid API response: {error}"))
}

fn encode_path_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn native_endpoint_retains_the_validated_loopback_base() {
        let transport = Transport::new("http://127.0.0.1:7744/").unwrap();
        assert_eq!(
            transport.endpoint(&ApiRequest::List),
            "http://127.0.0.1:7744/obligations"
        );
    }
}
