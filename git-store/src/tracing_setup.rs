//! Tracing initialisation: console/JSON log output + optional OTLP
//! span export when the janitor config supplies a collector address.

use thiserror::Error;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// Errors from [`init`].
#[derive(Debug, Error)]
pub enum TracingInitError {
    #[error("install tracing subscriber: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),

    #[error(transparent)]
    ExporterBuild(#[from] janitor::otlp::ExporterBuildError),
}

/// Guard returned by [`init`]. Keeps the OTel tracer provider alive
/// and flushes buffered spans on drop.
pub struct TracingGuard {
    _otlp: Option<janitor::otlp::TracerGuard>,
}

/// Initialise the tracing subscriber. When `collector_endpoint` is
/// Some, also export spans via OTLP (HTTP/protobuf) to that collector
/// under the given `service_name`.
///
/// `debug` enables `debug` level across the service; `json_format`
/// switches console output to JSON for Google Cloud Logging.
pub fn init(
    service_name: &str,
    debug: bool,
    json_format: bool,
    collector_endpoint: Option<&str>,
) -> Result<TracingGuard, TracingInitError> {
    let level = if debug { "debug" } else { "info" };
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{level},hyper=warn,h2=warn")));

    let fmt_layer: Box<dyn Layer<_> + Send + Sync> = if json_format {
        Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .with_target(true)
                .with_current_span(true)
                .with_span_list(true),
        )
    } else {
        Box::new(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false),
        )
    };

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer);

    let guard = if let Some(endpoint) = collector_endpoint {
        let (layer, otlp_guard) = janitor::otlp::layer(service_name, endpoint)?;
        registry.with(layer).try_init()?;
        TracingGuard {
            _otlp: Some(otlp_guard),
        }
    } else {
        registry.try_init()?;
        TracingGuard { _otlp: None }
    };

    tracing::info!(
        service = service_name,
        otlp_endpoint = collector_endpoint.unwrap_or("<disabled>"),
        "tracing initialised"
    );
    Ok(guard)
}
