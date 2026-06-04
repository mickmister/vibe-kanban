use std::sync::OnceLock;

use sentry_tracing::{EventFilter, SentryLayer};
use tracing::Level;

use crate::perf_trace;

static INIT_GUARD: OnceLock<sentry::ClientInitGuard> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
pub enum SentrySource {
    Backend,
    Desktop,
    Mcp,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SentryPerfMode {
    SourceDefault,
    Backend,
}

impl SentryPerfMode {
    fn enables_backend_perf(self, source: SentrySource) -> bool {
        matches!(source, SentrySource::Backend) || matches!(self, Self::Backend)
    }
}

impl SentrySource {
    fn tag(self) -> &'static str {
        match self {
            SentrySource::Backend => "backend",
            SentrySource::Desktop => "desktop",
            SentrySource::Mcp => "mcp",
            SentrySource::Remote => "remote",
        }
    }

    fn dsn(self) -> Option<String> {
        let value = match self {
            SentrySource::Remote => option_env!("SENTRY_DSN_REMOTE")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("SENTRY_DSN_REMOTE").ok()),
            _ => option_env!("SENTRY_DSN")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("SENTRY_DSN").ok()),
        };
        value.filter(|s| !s.is_empty())
    }
}

fn environment() -> &'static str {
    if cfg!(debug_assertions) {
        "dev"
    } else {
        "production"
    }
}

pub fn init_once(source: SentrySource) {
    init_once_with_perf_mode(source, SentryPerfMode::SourceDefault);
}

pub fn init_once_with_perf_mode(source: SentrySource, perf_mode: SentryPerfMode) {
    let Some(dsn) = source.dsn() else {
        return;
    };

    INIT_GUARD.get_or_init(|| {
        let trace_config = if perf_mode.enables_backend_perf(source) && perf_trace::enabled() {
            trace_sample_rate_config()
        } else {
            TraceSampleRateConfig::Unset
        };
        if let TraceSampleRateConfig::Invalid { name, value } = &trace_config {
            eprintln!(
                "Ignoring invalid {name}={value:?}; expected a Sentry trace \
                 sample rate between 0.0 and 1.0"
            );
        }
        let traces_sample_rate = trace_config.rate();
        sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                environment: Some(environment().into()),
                traces_sample_rate,
                ..Default::default()
            },
        ))
    });

    sentry::configure_scope(|scope| {
        scope.set_tag("source", source.tag());
    });
}

pub fn configure_user_scope(user_id: &str, username: Option<&str>, email: Option<&str>) {
    let mut sentry_user = sentry::User {
        id: Some(user_id.to_string()),
        ..Default::default()
    };

    if let Some(username) = username {
        sentry_user.username = Some(username.to_string());
    }

    if let Some(email) = email {
        sentry_user.email = Some(email.to_string());
    }

    sentry::configure_scope(|scope| {
        scope.set_user(Some(sentry_user));
    });
}

pub fn sentry_layer<S>(source: SentrySource) -> SentryLayer<S>
where
    S: tracing::Subscriber,
    S: for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    sentry_layer_with_perf_mode(source, SentryPerfMode::SourceDefault)
}

pub fn sentry_layer_with_perf_mode<S>(
    source: SentrySource,
    perf_mode: SentryPerfMode,
) -> SentryLayer<S>
where
    S: tracing::Subscriber,
    S: for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let include_perf_trace_spans =
        perf_mode.enables_backend_perf(source)
            && perf_trace::enabled()
            && trace_sample_rate_config().rate() > 0.0;

    SentryLayer::default()
        .span_filter(move |meta| {
            matches!(*meta.level(), Level::DEBUG | Level::INFO | Level::WARN | Level::ERROR)
                || (include_perf_trace_spans && matches!(*meta.level(), Level::TRACE))
        })
        .event_filter(|meta| match *meta.level() {
            Level::ERROR => EventFilter::Event,
            Level::DEBUG | Level::INFO | Level::WARN => EventFilter::Breadcrumb,
            Level::TRACE => EventFilter::Ignore,
        })
}

#[derive(Debug, PartialEq)]
enum TraceSampleRateConfig {
    Unset,
    Valid(f32),
    Invalid { name: &'static str, value: String },
}

impl TraceSampleRateConfig {
    fn rate(&self) -> f32 {
        match self {
            Self::Valid(rate) => *rate,
            Self::Unset | Self::Invalid { .. } => 0.0,
        }
    }
}

fn trace_sample_rate_config() -> TraceSampleRateConfig {
    trace_sample_rate_config_from(first_env_var([
        "VK_SENTRY_TRACES_SAMPLE_RATE",
        "SENTRY_TRACES_SAMPLE_RATE",
    ]))
}

fn trace_sample_rate_config_from(
    configured_rate: Option<(&'static str, String)>,
) -> TraceSampleRateConfig {
    let Some((name, value)) = configured_rate else {
        return TraceSampleRateConfig::Unset;
    };

    match value.trim().parse::<f32>() {
        Ok(value) if (0.0..=1.0).contains(&value) => TraceSampleRateConfig::Valid(value),
        _ => TraceSampleRateConfig::Invalid { name, value },
    }
}

fn first_env_var<const N: usize>(names: [&'static str; N]) -> Option<(&'static str, String)> {
    names.into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (name, value))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        SentryPerfMode, SentrySource, TraceSampleRateConfig, trace_sample_rate_config_from,
    };

    #[test]
    fn traces_sample_rate_is_disabled_by_default() {
        assert_eq!(trace_sample_rate_config_from(None), TraceSampleRateConfig::Unset);
    }

    #[test]
    fn traces_sample_rate_accepts_configured_value() {
        assert_eq!(
            trace_sample_rate_config_from(Some(("VK_SENTRY_TRACES_SAMPLE_RATE", "0.25".into()))),
            TraceSampleRateConfig::Valid(0.25)
        );
    }

    #[test]
    fn traces_sample_rate_rejects_invalid_configured_value() {
        assert_eq!(
            trace_sample_rate_config_from(Some(("VK_SENTRY_TRACES_SAMPLE_RATE", "2".into()))),
            TraceSampleRateConfig::Invalid {
                name: "VK_SENTRY_TRACES_SAMPLE_RATE",
                value: "2".into(),
            }
        );
        assert_eq!(
            trace_sample_rate_config_from(Some(("SENTRY_TRACES_SAMPLE_RATE", "oops".into()))),
            TraceSampleRateConfig::Invalid {
                name: "SENTRY_TRACES_SAMPLE_RATE",
                value: "oops".into(),
            }
        );
    }

    #[test]
    fn source_default_perf_mode_only_enables_backend_source() {
        assert!(SentryPerfMode::SourceDefault.enables_backend_perf(SentrySource::Backend));
        assert!(!SentryPerfMode::SourceDefault.enables_backend_perf(SentrySource::Desktop));
        assert!(!SentryPerfMode::SourceDefault.enables_backend_perf(SentrySource::Mcp));
        assert!(!SentryPerfMode::SourceDefault.enables_backend_perf(SentrySource::Remote));
    }

    #[test]
    fn backend_perf_mode_enables_embedded_backend_processes() {
        assert!(SentryPerfMode::Backend.enables_backend_perf(SentrySource::Backend));
        assert!(SentryPerfMode::Backend.enables_backend_perf(SentrySource::Desktop));
    }
}
