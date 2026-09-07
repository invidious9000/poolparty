use std::{
    collections::BTreeSet,
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
};
use bytes::Bytes;
use futures_util::StreamExt;
use poolparty::{
    domain::*,
    ports::*,
    providers::{Endpoint, HttpTransport, SyntheticTransport},
};
use tokio::{sync::Mutex, task::JoinHandle};

type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

struct Origin {
    url: String,
    calls: Arc<AtomicUsize>,
    seen: Seen,
    streams_dropped: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

struct StreamLifetime(Arc<AtomicUsize>);

impl Drop for StreamLifetime {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn origin(status: StatusCode, chunks: Vec<Bytes>, hold_open: bool) -> Origin {
    origin_with_content_type(
        status,
        chunks,
        hold_open,
        Some("text/event-stream; charset=utf-8"),
    )
    .await
}

async fn origin_with_content_type(
    status: StatusCode,
    chunks: Vec<Bytes>,
    hold_open: bool,
    content_type: Option<&'static str>,
) -> Origin {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let streams_dropped = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/inference",
        post({
            let calls = calls.clone();
            let seen = seen.clone();
            let streams_dropped = streams_dropped.clone();
            move |request: Request| {
                let calls = calls.clone();
                let seen = seen.clone();
                let chunks = chunks.clone();
                let streams_dropped = streams_dropped.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let (parts, body) = request.into_parts();
                    let body = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                    seen.lock().await.push((parts.headers, body));
                    let output = async_stream::stream! {
                        let _lifetime = StreamLifetime(streams_dropped);
                        for chunk in chunks {
                            yield Ok::<Bytes, Infallible>(chunk);
                            tokio::task::yield_now().await;
                        }
                        if hold_open { std::future::pending::<()>().await; }
                    };
                    let mut response = Response::builder().status(status);
                    if let Some(content_type) = content_type {
                        response = response.header("content-type", content_type);
                    }
                    response
                        .header("x-request-id", "synthetic-request")
                        .header("set-cookie", "private=synthetic")
                        .header("location", "/inference")
                        .body(Body::from_stream(output))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Origin {
        url: format!("http://{address}/inference"),
        calls,
        seen,
        streams_dropped,
        task,
    }
}

fn request(product: Product) -> UpstreamRequest {
    let credential = CredentialRef {
        id: CredentialId::new("credential-a").unwrap(),
        generation: 1,
    };
    let account = Account {
        id: AccountId::new("account-a").unwrap(),
        product,
        quota_owner: QuotaOwnerId::new("owner-a").unwrap(),
        pools: BTreeSet::from([PoolId::new("pool-a").unwrap()]),
        credential: credential.clone(),
        models: BTreeSet::from(["synthetic-model".into()]),
        enabled: true,
    };
    let binding = Binding {
        id: BindingId::new("binding-a").unwrap(),
        principal: PrincipalId::new("caller-a").unwrap(),
        intent: CreateBinding {
            session: ClientSessionId::new("session-a").unwrap(),
            pool: PoolId::new("pool-a").unwrap(),
            product,
            model: "synthetic-model".into(),
            account: Some(account.id.clone()),
            effort: None,
        },
        account: account.id.clone(),
        created_at: 0,
        closed_at: None,
    };
    let attempt = Attempt {
        id: AttemptId::new("attempt-a").unwrap(),
        binding: binding.id.clone(),
        quota_owner: account.quota_owner.clone(),
        operation: None,
        request_fingerprint: "synthetic".into(),
        credential,
        state: AttemptState::Dispatching,
        created_at: 0,
        updated_at: 0,
    };
    let secret = if product == Product::CodexSubscription {
        r#"{"tokens":{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","account_id":"synthetic-account"}}"#
    } else {
        "synthetic-api-key"
    };
    UpstreamRequest {
        prepared: PreparedAttempt {
            account,
            binding,
            attempt,
        },
        protocol: product.protocol(),
        body: Bytes::from_static(
            br#"{ "model":"synthetic-model", "stream":true, "opaque":{"tool":"untouched"} }"#,
        ),
        secret: SecretValue::new(secret.into()),
    }
}

fn transport(origin: &Origin, product: Product) -> HttpTransport {
    HttpTransport::new(
        vec![Endpoint {
            product,
            url: origin.url.clone(),
        }],
        true,
    )
    .unwrap()
}

async fn collect(
    mut response: UpstreamResponse,
) -> (Vec<u8>, Vec<Settlement>, Vec<TransportError>) {
    let mut data = Vec::new();
    let mut terminal = Vec::new();
    let mut errors = Vec::new();
    while let Some(event) = response.stream.next().await {
        match event {
            Ok(StreamEvent::Data(bytes)) => data.extend_from_slice(&bytes),
            Ok(StreamEvent::Terminal { bytes, outcome }) => {
                data.extend_from_slice(&bytes);
                terminal.push(outcome);
            }
            Ok(StreamEvent::Finished(outcome)) => terminal.push(outcome),
            Err(error) => errors.push(error),
        }
    }
    (data, terminal, errors)
}

#[tokio::test]
async fn synthetic_protocols_finish_deterministically() {
    for product in [Product::CodexSubscription, Product::KimiCoding] {
        let (bytes, terminal, errors) = collect(
            SyntheticTransport::new()
                .send(request(product))
                .await
                .unwrap(),
        )
        .await;
        assert!(!bytes.is_empty());
        assert_eq!(terminal, [Settlement::Succeeded]);
        assert!(errors.is_empty());
    }
}

#[tokio::test]
async fn final_bytes_and_terminal_proof_arrive_in_one_event() {
    let wire = b"data: {\"type\":\"message_stop\"}\n\n";
    let server = origin(StatusCode::OK, vec![Bytes::from_static(wire)], true).await;
    let mut response = transport(&server, Product::KimiCoding)
        .send(request(Product::KimiCoding))
        .await
        .unwrap();
    let mut delivered = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), response.stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match event {
            StreamEvent::Data(bytes) => {
                delivered.extend_from_slice(&bytes);
                assert_ne!(
                    delivered, wire,
                    "completion was exposed without settlement proof"
                );
            }
            StreamEvent::Terminal { bytes, outcome } => {
                delivered.extend_from_slice(&bytes);
                assert_eq!(delivered, wire);
                assert_eq!(outcome, Settlement::Succeeded);
                break;
            }
            _ => panic!("terminal bytes must carry settlement proof"),
        }
    }
    assert!(response.stream.next().await.is_none());

    let mut response = SyntheticTransport::new()
        .send(request(Product::KimiCoding))
        .await
        .unwrap();
    assert!(matches!(
        response.stream.next().await.unwrap().unwrap(),
        StreamEvent::Terminal {
            outcome: Settlement::Succeeded,
            ..
        }
    ));
    assert!(response.stream.next().await.is_none());
}

#[tokio::test]
async fn responses_preserve_bytes_and_stop_at_native_terminal_without_eof() {
    let wire = concat!(
        "event: response.created\r\ndata: {\"type\":\"response.created\"}\r\n\r\n",
        "event: response.reasoning_summary_text.delta\r\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"message_stop response.completed\"}\r\n\r\n",
        "event: response.function_call_arguments.delta\r\ndata: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"arg\\\":\"}\r\n\r\n",
        "event: response.completed\r\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\r\n\r\n"
    );
    let chunks = wire
        .as_bytes()
        .chunks(3)
        .map(Bytes::copy_from_slice)
        .collect();
    let server = origin(StatusCode::OK, chunks, true).await;
    let req = request(Product::CodexSubscription);
    let original_body = req.body.clone();
    let response = transport(&server, Product::CodexSubscription)
        .send(req)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "x-request-id" && value == "synthetic-request")
    );
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "set-cookie" || name == "location")
    );
    let (bytes, terminal, errors) = tokio::time::timeout(Duration::from_secs(3), collect(response))
        .await
        .unwrap();
    assert_eq!(bytes, wire.as_bytes());
    assert_eq!(terminal, [Settlement::Succeeded]);
    assert!(errors.is_empty());
    let seen = server.seen.lock().await;
    let (headers, body) = &seen[0];
    assert_eq!(body, &original_body);
    assert_eq!(headers["authorization"], "Bearer synthetic-access");
    assert_eq!(headers["chatgpt-account-id"], "synthetic-account");
    assert_eq!(headers["originator"], "poolparty");
    assert_eq!(headers["content-type"], "application/json");
    assert!(headers.get("x-api-key").is_none());
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn messages_preserve_tools_signatures_and_usage_with_product_auth() {
    let wire = concat!(
        ": heartbeat\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"signature_delta\",\"signature\":\"opaque\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":12,\"output_tokens\":4}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    for product in [
        Product::KimiCoding,
        Product::GlmCoding,
        Product::DeepseekPayg,
    ] {
        let server = origin(
            StatusCode::OK,
            vec![Bytes::from_static(wire.as_bytes())],
            true,
        )
        .await;
        let response = transport(&server, product)
            .send(request(product))
            .await
            .unwrap();
        let (bytes, terminal, errors) =
            tokio::time::timeout(Duration::from_secs(3), collect(response))
                .await
                .unwrap();
        assert_eq!(bytes, wire.as_bytes());
        assert_eq!(terminal, [Settlement::Succeeded]);
        assert!(errors.is_empty());
        let seen = server.seen.lock().await;
        assert_eq!(seen[0].0["authorization"], "Bearer synthetic-api-key");
        assert_eq!(seen[0].0["anthropic-version"], "2023-06-01");
        assert!(seen[0].0.get("chatgpt-account-id").is_none());
    }
}

#[tokio::test]
async fn glm_preserves_thinking_tool_fragments_and_complete_continuation_bodies() {
    let wire = concat!(
        "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"synthetic-message\",\"content\":[]}}\r\n\r\n",
        "event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\r\n\r\n",
        "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"preserve response.completed and message_stop as text\"}}\r\n\r\n",
        "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"synthetic-signature\"}}\r\n\r\n",
        "event: content_block_stop\r\ndata: {\"type\":\"content_block_stop\",\"index\":0}\r\n\r\n",
        "event: content_block_start\r\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-a\",\"name\":\"read_fixture\",\"input\":{}}}\r\n\r\n",
        "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\r\n\r\n",
        "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"fixture.txt\\\"}\"}}\r\n\r\n",
        "event: content_block_stop\r\ndata: {\"type\":\"content_block_stop\",\"index\":1}\r\n\r\n",
        "event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":23,\"cache_read_input_tokens\":7}}\r\n\r\n",
        "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n"
    );
    let server = origin(
        StatusCode::OK,
        wire.as_bytes()
            .chunks(7)
            .map(Bytes::copy_from_slice)
            .collect(),
        true,
    )
    .await;
    let adapter = transport(&server, Product::GlmCoding);
    let initial = serde_json::json!({
        "model":"synthetic-model", "stream":true, "thinking":{"type":"adaptive"},
        "output_config":{"effort":"high"}, "max_tokens":256,
        "messages":[{"role":"user","content":"Read the synthetic fixture"}],
        "tools":[{"name":"read_fixture","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}]
    });
    let mut continuation = initial.clone();
    continuation["messages"] = serde_json::json!([
        initial["messages"][0],
        {"role":"assistant","content":[
            {"type":"thinking","thinking":"preserve response.completed and message_stop as text","signature":"synthetic-signature"},
            {"type":"tool_use","id":"tool-a","name":"read_fixture","input":{"path":"fixture.txt"}}
        ]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-a","content":"synthetic result"}]}
    ]);
    for body in [initial, continuation] {
        let original = Bytes::from(serde_json::to_vec(&body).unwrap());
        let mut req = request(Product::GlmCoding);
        req.body = original.clone();
        let response = adapter.send(req).await.unwrap();
        let (bytes, terminal, errors) =
            tokio::time::timeout(Duration::from_secs(3), collect(response))
                .await
                .unwrap();
        assert_eq!(bytes, wire.as_bytes());
        assert_eq!(terminal, [Settlement::Succeeded]);
        assert!(errors.is_empty());
        let seen = server.seen.lock().await;
        assert_eq!(seen.last().unwrap().1, original);
        assert_eq!(
            seen.last().unwrap().0["authorization"],
            "Bearer synthetic-api-key"
        );
        assert_eq!(seen.last().unwrap().0["anthropic-version"], "2023-06-01");
    }
    assert_eq!(server.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn glm_partial_messages_and_json_application_errors_remain_uncertain() {
    for (wire, content_type) in [
        (
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "text/event-stream",
        ),
        ("data: {\"type\":\"message_stop\"}\n", "text/event-stream"),
        ("data: [DONE]\n\n", "text/event-stream"),
        (
            "event: message_stop\ndata: {\"type\":\"message_delta\"}\n\n",
            "text/event-stream",
        ),
        (
            "{\"error\":{\"code\":\"1308\",\"message\":\"synthetic rejection\"}}",
            "application/json",
        ),
    ] {
        let server = origin_with_content_type(
            StatusCode::OK,
            vec![Bytes::copy_from_slice(wire.as_bytes())],
            false,
            Some(content_type),
        )
        .await;
        let (_, terminal, errors) = collect(
            transport(&server, Product::GlmCoding)
                .send(request(Product::GlmCoding))
                .await
                .unwrap(),
        )
        .await;
        assert!(terminal.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].certainty, DispatchCertainty::Unknown);
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn glm_error_codes_preserve_distinct_native_bodies_without_retry() {
    // Vendor documents HTTP429 for several different causes; it is not itself a
    // quota observation: https://docs.z.ai/api-reference/api-code
    for (status, code) in [
        (StatusCode::UNAUTHORIZED, "1003"),
        (StatusCode::FORBIDDEN, "1220"),
        (StatusCode::TOO_MANY_REQUESTS, "1113"),
        (StatusCode::TOO_MANY_REQUESTS, "1302"),
        (StatusCode::TOO_MANY_REQUESTS, "1308"),
        (StatusCode::TOO_MANY_REQUESTS, "1309"),
        (StatusCode::TOO_MANY_REQUESTS, "1310"),
        (StatusCode::TOO_MANY_REQUESTS, "1311"),
    ] {
        let body = serde_json::to_vec(
            &serde_json::json!({"error":{"code":code,"message":"synthetic rejection"}}),
        )
        .unwrap();
        let server = origin_with_content_type(
            status,
            vec![Bytes::copy_from_slice(&body)],
            false,
            Some("application/json"),
        )
        .await;
        let response = transport(&server, Product::GlmCoding)
            .send(request(Product::GlmCoding))
            .await
            .unwrap();
        assert_eq!(response.status, status.as_u16());
        let (bytes, terminal, errors) = collect(response).await;
        assert_eq!(bytes, body);
        assert_eq!(terminal, [Settlement::Rejected]);
        assert!(errors.is_empty());
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn glm_stream_cancellation_closes_the_origin_without_retry_or_terminal() {
    let server = origin(
        StatusCode::OK,
        vec![Bytes::from_static(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"synthetic in-progress reasoning\"}}\n\n")],
        true,
    ).await;
    let mut response = transport(&server, Product::GlmCoding)
        .send(request(Product::GlmCoding))
        .await
        .unwrap();
    assert!(matches!(
        response.stream.next().await.unwrap().unwrap(),
        StreamEvent::Data(_)
    ));
    assert_eq!(server.streams_dropped.load(Ordering::SeqCst), 0);
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.streams_dropped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(server.streams_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn eof_wrong_protocol_and_text_markers_never_prove_completion() {
    for (product, wire) in [
        (
            Product::CodexSubscription,
            "data: {\"type\":\"response.created\"}\n\n",
        ),
        (
            Product::CodexSubscription,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"response.completed\"}\n\n",
        ),
        (
            Product::CodexSubscription,
            "data: {\"type\":\"message_stop\"}\n\n",
        ),
        (Product::CodexSubscription, "data: [DONE]\n\n"),
        (
            Product::CodexSubscription,
            "data: {\"type\":\"response.completed\"}\n",
        ),
        (
            Product::KimiCoding,
            "data: {\"type\":\"response.completed\"}\n\n",
        ),
    ] {
        let server = origin(
            StatusCode::OK,
            vec![Bytes::from_static(wire.as_bytes())],
            false,
        )
        .await;
        let (_, terminal, errors) = collect(
            transport(&server, product)
                .send(request(product))
                .await
                .unwrap(),
        )
        .await;
        assert!(terminal.is_empty(), "{wire}");
        assert_eq!(errors.len(), 1, "{wire}");
        assert_eq!(errors[0].certainty, DispatchCertainty::Unknown);
    }
}

#[tokio::test]
async fn malformed_mismatched_and_oversized_frames_are_uncertain() {
    for wire in [
        b"event: response.completed\ndata: not-json\n\n".to_vec(),
        b"event: response.completed\ndata: {\"type\":\"response.created\"}\n\n".to_vec(),
        b"data: {\"type\":\"response.completed\"\n\n".to_vec(),
        b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"in_progress\"}}\n\n"
            .to_vec(),
        b"data: {\"type\":\"response.completed\",\"response\":null}\n\n".to_vec(),
        vec![b'x'; 256 * 1024 + 1],
    ] {
        let server = origin(StatusCode::OK, vec![Bytes::from(wire)], false).await;
        let (_, terminal, errors) = collect(
            transport(&server, Product::CodexSubscription)
                .send(request(Product::CodexSubscription))
                .await
                .unwrap(),
        )
        .await;
        assert!(terminal.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].certainty, DispatchCertainty::Unknown);
    }
}

#[tokio::test]
async fn native_failures_settle_without_waiting_for_socket_close() {
    for (product, kind) in [
        (Product::CodexSubscription, "response.failed"),
        (Product::CodexSubscription, "response.incomplete"),
        (Product::KimiCoding, "error"),
        (Product::GlmCoding, "error"),
    ] {
        let wire = format!("event: {kind}\ndata: {{\"type\":\"{kind}\"}}\n\n");
        let server = origin(StatusCode::OK, vec![Bytes::from(wire)], true).await;
        let (_, terminal, errors) = tokio::time::timeout(
            Duration::from_secs(3),
            collect(
                transport(&server, product)
                    .send(request(product))
                    .await
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(terminal, [Settlement::Rejected]);
        assert!(errors.is_empty());
    }
}

#[tokio::test]
async fn redirects_and_server_errors_never_retry_and_only_clear_rejections_release() {
    for (status, expected) in [
        (StatusCode::TEMPORARY_REDIRECT, Settlement::Uncertain),
        (StatusCode::INTERNAL_SERVER_ERROR, Settlement::Uncertain),
        (StatusCode::SERVICE_UNAVAILABLE, Settlement::Uncertain),
        (StatusCode::REQUEST_TIMEOUT, Settlement::Uncertain),
        (StatusCode::TOO_MANY_REQUESTS, Settlement::Rejected),
        (StatusCode::UNAUTHORIZED, Settlement::Rejected),
    ] {
        let server = origin(status, vec![Bytes::from_static(b"synthetic-error")], false).await;
        let response = transport(&server, Product::KimiCoding)
            .send(request(Product::KimiCoding))
            .await
            .unwrap();
        assert_eq!(response.status, status.as_u16());
        let (body, terminal, errors) = collect(response).await;
        assert_eq!(body, b"synthetic-error");
        if expected == Settlement::Uncertain {
            assert!(terminal.is_empty());
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].certainty, DispatchCertainty::Unknown);
        } else {
            assert_eq!(terminal, [expected]);
            assert!(errors.is_empty());
        }
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn valid_opaque_extensions_are_forwarded_without_terminal_inference() {
    let wire = b"event: vendor.extension\ndata: {\"custom\":\"opaque\"}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    let server = origin(StatusCode::OK, vec![Bytes::from_static(wire)], false).await;
    let (body, terminal, errors) = collect(
        transport(&server, Product::KimiCoding)
            .send(request(Product::KimiCoding))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body, wire);
    assert_eq!(terminal, [Settlement::Succeeded]);
    assert!(errors.is_empty());
}

#[tokio::test]
async fn oversized_rejection_body_does_not_release_capacity() {
    let server = origin(
        StatusCode::TOO_MANY_REQUESTS,
        vec![Bytes::from(vec![b'x'; 64 * 1024 + 1])],
        false,
    )
    .await;
    let (body, terminal, errors) = collect(
        transport(&server, Product::KimiCoding)
            .send(request(Product::KimiCoding))
            .await
            .unwrap(),
    )
    .await;
    assert!(body.len() <= 64 * 1024);
    assert!(terminal.is_empty());
    assert_eq!(errors[0].certainty, DispatchCertainty::Unknown);
}

#[tokio::test]
async fn local_credential_protocol_and_configuration_failures_do_not_dispatch() {
    let server = origin(StatusCode::OK, Vec::new(), false).await;
    let adapter = transport(&server, Product::CodexSubscription);
    let mut invalid = request(Product::CodexSubscription);
    invalid.secret = SecretValue::new("malformed-secret".into());
    let error = match adapter.send(invalid).await {
        Err(error) => error,
        Ok(_) => panic!("accepted invalid credential"),
    };
    assert_eq!(error.certainty, DispatchCertainty::NotDispatched);
    let mut invalid = request(Product::CodexSubscription);
    invalid.protocol = Protocol::Messages;
    let error = match adapter.send(invalid).await {
        Err(error) => error,
        Ok(_) => panic!("accepted wrong protocol"),
    };
    assert_eq!(error.certainty, DispatchCertainty::NotDispatched);
    let error = match adapter.send(request(Product::KimiCoding)).await {
        Err(error) => error,
        Ok(_) => panic!("accepted unconfigured product"),
    };
    assert_eq!(error.certainty, DispatchCertainty::NotDispatched);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    for url in [
        "http://example.com/inference",
        "http://localhost/inference",
        "https://user:password@example.com/inference",
        "https://example.com/inference?secret=value",
        "file:///tmp/inference",
    ] {
        assert!(
            HttpTransport::new(
                vec![Endpoint {
                    product: Product::KimiCoding,
                    url: url.into()
                }],
                true
            )
            .is_err()
        );
    }
    assert!(
        HttpTransport::new(
            vec![Endpoint {
                product: Product::KimiCoding,
                url: server.url.clone()
            }],
            false
        )
        .is_err()
    );
}

#[tokio::test]
async fn codex_missing_content_type_accepts_only_proven_sse_completion() {
    let wire = Bytes::from_static(b"event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n");
    let origin = origin_with_content_type(StatusCode::OK, vec![wire.clone()], true, None).await;
    let response = transport(&origin, Product::CodexSubscription)
        .send(request(Product::CodexSubscription))
        .await
        .unwrap();
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value == "text/event-stream")
    );
    let (actual, terminal, errors) =
        tokio::time::timeout(Duration::from_secs(3), collect(response))
            .await
            .unwrap();
    assert_eq!(actual, wire);
    assert_eq!(terminal, [Settlement::Succeeded]);
    assert!(errors.is_empty());
    assert_eq!(origin.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_header_exception_never_accepts_eof_or_explicit_wrong_mime() {
    for (product, mime, wire) in [
        (
            Product::CodexSubscription,
            None,
            b"data: {\"type\":\"response.created\"}\n\n".as_slice(),
        ),
        (
            Product::CodexSubscription,
            None,
            b"<html>synthetic non-stream body</html>".as_slice(),
        ),
        (
            Product::CodexSubscription,
            Some("application/json"),
            b"data: {\"type\":\"response.completed\"}\n\n".as_slice(),
        ),
        (
            Product::GlmCoding,
            None,
            b"data: {\"type\":\"message_stop\"}\n\n".as_slice(),
        ),
    ] {
        let origin = origin_with_content_type(
            StatusCode::OK,
            vec![Bytes::copy_from_slice(wire)],
            false,
            mime,
        )
        .await;
        let (_, terminal, errors) = collect(
            transport(&origin, product)
                .send(request(product))
                .await
                .unwrap(),
        )
        .await;
        assert!(terminal.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].certainty, DispatchCertainty::Unknown);
        assert_eq!(origin.calls.load(Ordering::SeqCst), 1);
    }
}
