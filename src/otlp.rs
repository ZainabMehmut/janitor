//! Export of tracing spans to a collector over OTLP.
//!
//! This replaces the aiozipkin tracing the Python services set up from
//! `config.zipkin_address`. Modern zipkin servers accept OTLP natively,
//! so a URL like `http://zipkin.local:9411/v1/traces` or an
//! OTLP-collector URL both work.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{SdkTracerProvider, Tracer};
use opentelemetry_sdk::Resource;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::registry::LookupSpan;

/// Failure to set up the OTLP exporter.
#[derive(Debug)]
pub struct ExporterBuildError {
    endpoint: String,
    source: opentelemetry_otlp::ExporterBuildError,
}

impl std::fmt::Display for ExporterBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "build OTLP exporter for {}: {}",
            self.endpoint, self.source
        )
    }
}

impl std::error::Error for ExporterBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Keeps the tracer provider alive and flushes buffered spans on drop,
/// so a short-lived process doesn't lose its last batch.
pub struct TracerGuard {
    provider: SdkTracerProvider,
}

impl Drop for TracerGuard {
    fn drop(&mut self) {
        if let Err(e) = self.provider.shutdown() {
            eprintln!("tracing: OTLP provider shutdown failed: {}", e);
        }
    }
}

/// Build a tracer provider that exports spans for `service_name` to
/// `endpoint` over OTLP (HTTP/protobuf).
///
/// Like aiozipkin.create in the Python services, 10% of traces are
/// sampled.
pub fn build_provider(
    service_name: &str,
    endpoint: &str,
) -> Result<SdkTracerProvider, ExporterBuildError> {
    use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig};
    use opentelemetry_sdk::trace::Sampler;

    let exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint)
        .build()
        .map_err(|source| ExporterBuildError {
            endpoint: endpoint.to_string(),
            source,
        })?;

    Ok(SdkTracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            0.1,
        ))))
        .with_resource(
            Resource::builder_empty()
                .with_service_name(service_name.to_string())
                .build(),
        )
        .with_batch_exporter(exporter)
        .build())
}

/// Create a `tracing` layer that exports spans to `endpoint`, and
/// install its provider as the global tracer provider.
///
/// The returned guard must be kept alive for as long as spans should
/// be exported.
pub fn layer<S>(
    service_name: &str,
    endpoint: &str,
) -> Result<(OpenTelemetryLayer<S, Tracer>, TracerGuard), ExporterBuildError>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    let provider = build_provider(service_name, endpoint)?;
    let tracer = provider.tracer(service_name.to_string());
    opentelemetry::global::set_tracer_provider(provider.clone());
    Ok((
        tracing_opentelemetry::layer().with_tracer(tracer),
        TracerGuard { provider },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_provider_accepts_http_endpoint() {
        let provider = build_provider("test-service", "http://localhost:4318/v1/traces")
            .expect("build provider for a well-formed endpoint URL");
        provider.shutdown().expect("shutdown provider");
    }

    #[test]
    fn build_provider_rejects_bad_endpoint() {
        let err = build_provider("svc", "not a url").unwrap_err();
        assert_eq!(err.endpoint, "not a url");
    }
}
