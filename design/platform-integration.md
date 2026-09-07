# Shared observability and portal integration

Status: proposed application contract, grounded in an inspection of the target
platform's infrastructure-as-code. This is not live deployment verification.
Private source references and actual configuration values stay with the operator.

## Reuse shared platform services

Poolparty integrates with the existing observability, identity, ingress, storage,
and application portal services. Its runtime does not deploy a second telemetry
stack. The application UI owns current account and session operations; Grafana
owns historical telemetry and operational alerts.

| Concern | Public application contract | Private integration responsibility |
| --- | --- | --- |
| Logs | Structured JSON on stdout/stderr, stable application label | Existing pod-log collector, parsing, retention, Loki queries |
| Traces | OpenTelemetry spans exported over OTLP | Collector endpoint, network admission, Tempo/Grafana configuration |
| Metrics | OTLP export by default; optional Prometheus endpoint | Existing metrics ingestion or an explicit authorized scrape job |
| Dashboards | Portable dashboard definitions when metrics exist | File-provider registration, data-source mapping, folder and alert provisioning |
| Portal | Standard Homepage ingress annotations | Real URL/group/icon choices and matching pod selector |
| Access | Application auth and documented network dependencies | Platform namespace admission, ingress/TLS, identity-provider registrations |

Do not assume a `ServiceMonitor`, dashboard ConfigMap label, or
`prometheus.io/scrape` annotation is consumed by an installed controller. The
inspected metrics pipeline has explicit scrape jobs, and dashboard provisioning
uses an explicit file inventory. Use OTLP or add the required platform-owned
configuration through its normal infrastructure workflow.

## Telemetry contract

Wire Rust `tracing` instrumentation into OpenTelemetry with configurable OTLP
exporters. Start with OTLP/HTTP protobuf; allow gRPC when required by an
installation. A synthetic environment contract:

```text
OTEL_SERVICE_NAME=poolparty
OTEL_RESOURCE_ATTRIBUTES=service.namespace=poolparty,service.version=dev,deployment.environment.name=example
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector.telemetry.svc.cluster.local:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

These are intended application inputs, not already implemented settings. The
initializer must explicitly honor the supported variables and tested Rust SDK
behavior. Signal-specific HTTP endpoints include `/v1/traces` or `/v1/metrics`;
the common endpoint is a base URL. Follow the
[OTLP exporter configuration](https://opentelemetry.io/docs/languages/sdk-configuration/otlp-exporter/)
and [Rust SDK guidance](https://opentelemetry.io/docs/languages/rust/), pinning the
implementation versions when introduced. Inject any exporter authentication
separately; never place secrets in resource attributes.

Structured logs carry severity, event name, timestamp, request ID, and trace/span
IDs when present. Set the pod label `app: poolparty` alongside the standard
application labels so a collector that derives service identity from `app` can
correlate the workload. Kubernetes workload metadata comes from collector
enrichment or explicitly configured resource attributes.

Use the existing pod-log collection path by default. Do not also export identical
logs through the application's OTLP exporter. CRI framing and Kubernetes metadata
enrichment alone do not parse the inner JSON or create Grafana trace links. Verify
JSON field extraction and trace-ID linking; any collector/derived-field changes
belong to the private platform configuration.

Span boundaries should include admission, binding lookup/create, upstream attempt,
stream lifetime, usage collection, and credential refresh. Keep incoming trace
context separate from authentication. Never record prompt text, response content,
tool arguments/results, authorization headers, OAuth payloads, or secret values.
Do not forward internal baggage to an external provider by default.

Use bounded labels for metrics: protocol, provider, route template, outcome,
reason code, and a controlled pool class where needed. Binding/session/request
IDs, raw URLs, emails, and credential/account identifiers are not metric labels.
Per-account diagnosis remains in the authenticated control API and restricted
structured events; any per-account timeseries needs a separately reviewed bounded
label contract. Raw URL paths containing binding IDs must be normalized.

Initial signal families should cover request rate/errors, admission latency,
time to first event, stream duration and cancellation, active claims, binding
conflicts, collection freshness/failures, credential refresh outcomes, database
latency, and telemetry export drops. Attribute stream failures independently of
the initial HTTP status, which may already be 200.

Export asynchronously with bounded queues and timeouts. Collector failure cannot
block allocation or keep inference streams open. Expose exporter health locally
and make shutdown flushing bounded. Persist authoritative allocation/account state
in Poolparty's database; the observability stack is not its state store.

## Grafana and alerts

Ship generic dashboards only after the corresponding metric names and units are
stable. The private integration registers them in the platform's existing
declarative file-provider inventory, maps data sources, and provisions alerts and
notification routing. No second Grafana, ad hoc UI-only import, or implicit
ConfigMap sidecar discovery is assumed.

Useful alerts include no eligible accounts for a required pool, stale quota
collection, repeated refresh failure, admission saturation, sustained upstream
errors, database pressure, and missing application telemetry. Distinguish normal
quota exhaustion from a broken service. Retention, routing recipients, and actual
dashboard URLs are private deployment policy, not application constants.

## Homepage portal registration

Add annotations to the user-facing Kubernetes **Ingress**, not just the Service
or Deployment. Homepage's
[automatic discovery](https://gethomepage.dev/configs/kubernetes/)
supports the standard annotation namespace. Synthetic overlay fragment:

```yaml
metadata:
  name: poolparty-ui
  annotations:
    gethomepage.dev/enabled: "true"
    gethomepage.dev/name: Poolparty
    gethomepage.dev/group: Applications
    gethomepage.dev/icon: mdi-pool
    gethomepage.dev/description: Provider accounts, usage, and session routing
    gethomepage.dev/pod-selector: app=poolparty
```

Choose the real group and hostname in the private overlay. Ensure the selector
matches the daemon pods in the Ingress namespace. Annotate only the canonical UI
Ingress when the application has multiple ingress paths, avoiding duplicate tiles.
A tile and its pod-status indicator provide discovery and workload visibility;
they do not establish provider capacity or authorize application access.

Portal visibility is not necessarily per-application authorization. Keep tile
metadata suitable for everyone who can access the portal, omit account details,
and preserve Poolparty's own auth when following a link directly. The initial
tile needs no Poolparty API credential or custom usage widget.

## Network admission and acceptance

A new namespace may be denied by existing platform ingress policies even when
its own egress policy allows the connection. The private integration must arrange
both sides of required paths: consumers to Poolparty, ingress to Poolparty,
Poolparty to telemetry and identity endpoints, DNS, and upstream HTTPS. Use the
platform's established namespace admission or narrowly scoped rules. DNS resolution
and a Service object alone are not proof of reachability.

Before calling the integration complete, verify a portal tile and matching pod
status, a redacted synthetic log in Loki, a correlated trace in Tempo, a metric in
the existing metrics backend, and a provisioned dashboard. Verify both allowed
and denied network/auth paths from the actual consumer and workload namespaces.
Also test that collector unavailability does not disrupt inference. All live
checks belong to deployment validation; this design update changes no platform.
