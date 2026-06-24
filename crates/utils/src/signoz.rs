use std::env;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use tracing_subscriber::{EnvFilter, Layer};

use crate::perf_trace;

pub struct SignozTracing<S>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    pub layer: Box<dyn Layer<S> + Send + Sync>,
    pub provider: SdkTracerProvider,
}

pub fn init_layer<S>(
    default_service_name: &'static str,
    filter_directives: &str,
) -> Option<SignozTracing<S>>
where
    S: tracing::Subscriber
        + for<'span> tracing_subscriber::registry::LookupSpan<'span>
        + Send
        + Sync,
{
    if !perf_trace::enabled() || !otlp_endpoint_configured() {
        return None;
    }

    let exporter = match SpanExporter::builder().with_http().build() {
        Ok(exporter) => exporter,
        Err(error) => {
            eprintln!(
                "Failed to initialize SigNoz OTLP span exporter: {error}. \
                 Performance traces will remain local."
            );
            return None;
        }
    };
    let env_filter = match EnvFilter::try_new(filter_directives) {
        Ok(filter) => filter,
        Err(error) => {
            eprintln!(
                "Failed to initialize SigNoz tracing filter: {error}. \
                 Performance traces will remain local."
            );
            return None;
        }
    };

    let service_name = service_name(default_service_name);
    let provider = SdkTracerProvider::builder()
        .with_resource(Resource::builder().with_service_name(service_name).build())
        .with_batch_exporter(exporter)
        .build();

    opentelemetry::global::set_tracer_provider(provider.clone());

    let tracer = provider.tracer(default_service_name);
    let layer = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_filter(env_filter)
        .boxed();

    Some(SignozTracing { layer, provider })
}

pub fn enabled() -> bool {
    perf_trace::enabled() && otlp_endpoint_configured()
}

fn otlp_endpoint_configured() -> bool {
    env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .or_else(|_| env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .ok()
        .is_some_and(|endpoint| !endpoint.trim().is_empty())
}

fn service_name(default_service_name: &'static str) -> String {
    service_name_from(env::var("OTEL_SERVICE_NAME").ok(), default_service_name)
}

fn service_name_from(
    configured_name: Option<String>,
    default_service_name: &'static str,
) -> String {
    configured_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| default_service_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::service_name_from;

    #[test]
    fn service_name_uses_default_when_env_is_not_supplied() {
        assert_eq!(
            service_name_from(None, "vibe-kanban-test"),
            "vibe-kanban-test"
        );
        assert_eq!(
            service_name_from(Some("  ".to_string()), "vibe-kanban-test"),
            "vibe-kanban-test"
        );
    }

    #[test]
    fn service_name_uses_configured_name() {
        assert_eq!(
            service_name_from(Some("custom-service".to_string()), "vibe-kanban-test"),
            "custom-service"
        );
    }
}
