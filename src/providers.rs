//! One-attempt, byte-preserving HTTP transports. Live provider conformance is a
//! separate acceptance gate; this module never refreshes or discovers credentials.
use std::{net::IpAddr, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;

use crate::{domain::*, ports::*};

const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct SyntheticTransport;

impl SyntheticTransport {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Transport for SyntheticTransport {
    async fn send(
        &self,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse, TransportError> {
        check_protocol(&request)?;
        let data = match request.protocol {
            Protocol::Responses => Bytes::from_static(b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_synthetic\",\"status\":\"in_progress\"}}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_synthetic\",\"status\":\"completed\",\"output\":[]}}\n\n"),
            Protocol::Messages => Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_synthetic\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":0}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
        };
        Ok(UpstreamResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            stream: Box::pin(futures_util::stream::iter(vec![Ok(
                StreamEvent::Terminal {
                    bytes: data,
                    outcome: Settlement::Succeeded,
                },
            )])),
        })
    }
}

/// Full inference URLs, not client-supplied base URLs. The loopback exception is
/// explicit and restricted to literal loopback IPs, avoiding DNS rebinding.
pub struct Endpoint {
    pub product: Product,
    pub url: String,
}

pub struct HttpTransport {
    client: Client,
    endpoints: Vec<(Product, Url)>,
}

impl HttpTransport {
    pub fn new(endpoints: Vec<Endpoint>, allow_loopback_http: bool) -> Result<Self> {
        let mut parsed = Vec::new();
        for endpoint in endpoints {
            let url = Url::parse(&endpoint.url).map_err(|_| invalid_endpoint())?;
            let loopback = url
                .host_str()
                .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            if !(url.scheme() == "https"
                || (allow_loopback_http && url.scheme() == "http" && loopback))
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || parsed
                    .iter()
                    .any(|(product, _)| *product == endpoint.product)
            {
                return Err(invalid_endpoint());
            }
            parsed.push((endpoint.product, url));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(90))
            .timeout(Duration::from_secs(15 * 60))
            .user_agent(concat!("poolparty/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| invalid_endpoint())?;
        Ok(Self {
            client,
            endpoints: parsed,
        })
    }
}

fn invalid_endpoint() -> Error {
    Error::new(
        ErrorCode::InvalidInput,
        "invalid provider endpoint configuration",
    )
}
fn failure(certainty: DispatchCertainty, code: ErrorCode) -> TransportError {
    TransportError { certainty, code }
}
fn uncertain() -> TransportError {
    failure(DispatchCertainty::Unknown, ErrorCode::UpstreamUnavailable)
}
fn check_protocol(request: &UpstreamRequest) -> std::result::Result<(), TransportError> {
    if request.protocol != request.prepared.account.product.protocol() {
        return Err(failure(
            DispatchCertainty::NotDispatched,
            ErrorCode::Unsupported,
        ));
    }
    Ok(())
}

fn secret_header(value: &str) -> std::result::Result<HeaderValue, TransportError> {
    if value.is_empty() {
        return Err(failure(
            DispatchCertainty::NotDispatched,
            ErrorCode::CredentialUnavailable,
        ));
    }
    let mut header = HeaderValue::from_str(value).map_err(|_| {
        failure(
            DispatchCertainty::NotDispatched,
            ErrorCode::CredentialUnavailable,
        )
    })?;
    header.set_sensitive(true);
    Ok(header)
}

fn authentication(request: &UpstreamRequest) -> std::result::Result<HeaderMap, TransportError> {
    let mut headers = HeaderMap::new();
    let credential_error = || {
        failure(
            DispatchCertainty::NotDispatched,
            ErrorCode::CredentialUnavailable,
        )
    };
    if request.protocol == Protocol::Responses {
        let auth: Value =
            serde_json::from_str(request.secret.expose()).map_err(|_| credential_error())?;
        let tokens = auth.get("tokens").ok_or_else(credential_error)?;
        let access = tokens
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(credential_error)?;
        let account = tokens
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(credential_error)?;
        headers.insert("authorization", secret_header(&format!("Bearer {access}"))?);
        headers.insert("chatgpt-account-id", secret_header(account)?);
        headers.insert("originator", HeaderValue::from_static("poolparty"));
    } else {
        if request.secret.expose().is_empty() {
            return Err(credential_error());
        }
        headers.insert(
            "authorization",
            secret_header(&format!("Bearer {}", request.secret.expose()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    }
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    Ok(headers)
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(
        &self,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse, TransportError> {
        check_protocol(&request)?;
        let url = self
            .endpoints
            .iter()
            .find(|(product, _)| *product == request.prepared.account.product)
            .map(|(_, url)| url.clone())
            .ok_or_else(|| failure(DispatchCertainty::NotDispatched, ErrorCode::Unsupported))?;
        let headers = authentication(&request)?;
        // Build failures are provably local. Every send/read failure is conservative.
        let outgoing = self
            .client
            .post(url)
            .headers(headers)
            .body(request.body)
            .build()
            .map_err(|_| failure(DispatchCertainty::NotDispatched, ErrorCode::InvalidInput))?;
        let response = self
            .client
            .execute(outgoing)
            .await
            .map_err(|_| uncertain())?;
        let status = response.status().as_u16();
        let headers = ["content-type", "retry-after", "x-request-id", "request-id"]
            .into_iter()
            .filter_map(|name| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .filter(|v| v.len() <= 1024)
                    .map(|v| (name.into(), v.into()))
            })
            .collect();
        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            });
        let protocol = request.protocol;
        let stream = async_stream::stream! {
            let mut body = response.bytes_stream();
            if !(200..300).contains(&status) {
                let mut total = 0usize;
                while let Some(chunk) = body.next().await {
                    let chunk = match chunk { Ok(chunk) => chunk, Err(_) => { yield Err(uncertain()); return; } };
                    total = total.saturating_add(chunk.len());
                    if total > MAX_ERROR_BYTES { yield Err(uncertain()); return; }
                    yield Ok(StreamEvent::Data(chunk));
                }
                if (400..500).contains(&status) && status != 408 {
                    yield Ok(StreamEvent::Finished(Settlement::Rejected));
                } else {
                    yield Err(uncertain());
                }
                return;
            }
            if !is_sse { yield Err(uncertain()); return; }
            let mut parser = TerminalParser::new(protocol);
            while let Some(chunk) = body.next().await {
                let chunk = match chunk { Ok(chunk) => chunk, Err(_) => { yield Err(uncertain()); return; } };
                let outcome = parser.feed(&chunk);
                match outcome {
                    Ok(Some(outcome)) => {
                        yield Ok(StreamEvent::Terminal { bytes: chunk, outcome });
                        return;
                    }
                    Ok(None) => { yield Ok(StreamEvent::Data(chunk)); }
                    Err(()) => { yield Err(uncertain()); return; }
                }
            }
            yield Err(uncertain());
        };
        Ok(UpstreamResponse {
            status,
            headers,
            stream: Box::pin(stream),
        })
    }
}

/// Frame boundaries, not substring searches, establish terminal evidence. Limits
/// apply to a complete event, including comments and ignored fields.
struct TerminalParser {
    protocol: Protocol,
    line: Vec<u8>,
    data: Vec<u8>,
    event: Option<String>,
    frame_bytes: usize,
}

impl TerminalParser {
    fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            line: Vec::new(),
            data: Vec::new(),
            event: None,
            frame_bytes: 0,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> std::result::Result<Option<Settlement>, ()> {
        for byte in chunk {
            self.frame_bytes += 1;
            if self.frame_bytes > MAX_EVENT_BYTES {
                return Err(());
            }
            if *byte != b'\n' {
                self.line.push(*byte);
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            let line = std::mem::take(&mut self.line);
            if line.is_empty() {
                let terminal = self.finish_frame()?;
                self.frame_bytes = 0;
                if terminal.is_some() {
                    return Ok(terminal);
                }
            } else if !line.starts_with(b":") {
                let (name, value) = match line.iter().position(|b| *b == b':') {
                    Some(index) => (
                        &line[..index],
                        line[index + 1..]
                            .strip_prefix(b" ")
                            .unwrap_or(&line[index + 1..]),
                    ),
                    None => (line.as_slice(), &[][..]),
                };
                match name {
                    b"data" => {
                        self.data.extend_from_slice(value);
                        self.data.push(b'\n');
                    }
                    b"event" => {
                        self.event = Some(std::str::from_utf8(value).map_err(|_| ())?.to_owned());
                    }
                    _ => {}
                }
            }
        }
        Ok(None)
    }

    fn finish_frame(&mut self) -> std::result::Result<Option<Settlement>, ()> {
        let event = self.event.take();
        let data = std::mem::take(&mut self.data);
        if data.is_empty() || data == b"[DONE]\n" {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(&data).map_err(|_| ())?;
        if !value.is_object() {
            return Err(());
        }
        let Some(kind) = value.get("type") else {
            return Ok(None);
        };
        let kind = kind.as_str().ok_or(())?;
        if event
            .as_deref()
            .is_some_and(|name| !name.is_empty() && name != kind)
        {
            return Err(());
        }
        if self.protocol == Protocol::Responses {
            let expected = match kind {
                "response.completed" => Some("completed"),
                "response.failed" => Some("failed"),
                "response.incomplete" => Some("incomplete"),
                _ => None,
            };
            if let (Some(expected), Some(response)) = (expected, value.get("response"))
                && (!response.is_object()
                    || response
                        .get("status")
                        .is_some_and(|status| status.as_str() != Some(expected)))
            {
                return Err(());
            }
        }
        let terminal = match (self.protocol, kind) {
            (Protocol::Responses, "response.completed") => Some(Settlement::Succeeded),
            (Protocol::Responses, "response.failed" | "response.incomplete") => {
                Some(Settlement::Rejected)
            }
            (Protocol::Messages, "message_stop") => Some(Settlement::Succeeded),
            (Protocol::Messages, "error") => Some(Settlement::Rejected),
            _ => None,
        };
        Ok(terminal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_boundary_can_split_crlf_and_json() {
        let wire = b"event: response.completed\r\ndata: {\"type\":\"response.completed\",\r\ndata: \"response\":{\"status\":\"completed\"}}\r\n\r\n";
        for split in 0..wire.len() {
            let mut parser = TerminalParser::new(Protocol::Responses);
            assert_eq!(parser.feed(&wire[..split]), Ok(None));
            assert_eq!(parser.feed(&wire[split..]), Ok(Some(Settlement::Succeeded)));
        }
        let mut parser = TerminalParser::new(Protocol::Responses);
        for (index, byte) in wire.iter().enumerate() {
            assert_eq!(
                parser.feed(&[*byte]),
                if index + 1 == wire.len() {
                    Ok(Some(Settlement::Succeeded))
                } else {
                    Ok(None)
                }
            );
        }
    }

    #[test]
    fn comments_reset_bounds_per_frame_and_never_create_a_terminal() {
        let mut parser = TerminalParser::new(Protocol::Messages);
        for _ in 0..MAX_EVENT_BYTES {
            assert_eq!(parser.feed(b": message_stop\n\n"), Ok(None));
        }
        assert!(parser.line.is_empty());
        assert!(parser.data.is_empty());
        assert_eq!(parser.frame_bytes, 0);
    }
}
