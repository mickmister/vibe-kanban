use std::sync::OnceLock;

use sentry_tracing::{EventFilter, SentryLayer};
use tracing::Level;

static INIT_GUARD: OnceLock<sentry::ClientInitGuard> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
pub enum SentrySource {
    Backend,
    Desktop,
    Mcp,
    Remote,
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
    let Some(dsn) = source.dsn() else {
        return;
    };

    INIT_GUARD.get_or_init(|| {
        let traces_sample_rate = traces_sample_rate().unwrap_or(0.0);
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

pub fn sentry_layer<S>() -> SentryLayer<S>
where
    S: tracing::Subscriber,
    S: for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let include_perf_trace_spans =
        perf_tracing_enabled() && traces_sample_rate().unwrap_or(0.0) > 0.0;

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

fn traces_sample_rate() -> Option<f32> {
    traces_sample_rate_from(
        perf_tracing_enabled(),
        first_env_var(["VK_SENTRY_TRACES_SAMPLE_RATE", "SENTRY_TRACES_SAMPLE_RATE"]),
    )
}

fn traces_sample_rate_from(
    perf_tracing_enabled: bool,
    configured_rate: Option<String>,
) -> Option<f32> {
    configured_rate
        .and_then(|value| value.trim().parse::<f32>().ok())
        .map(|value| value.clamp(0.0, 1.0))
        .or_else(|| perf_tracing_enabled.then_some(1.0))
}

fn perf_tracing_enabled() -> bool {
    env_flag("VK_PERF_TRACING")
}

fn first_env_var<const N: usize>(names: [&str; N]) -> Option<String> {
    names
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::traces_sample_rate_from;

    #[test]
    fn traces_sample_rate_is_disabled_by_default() {
        assert_eq!(traces_sample_rate_from(false, None), None);
    }

    #[test]
    fn traces_sample_rate_defaults_to_one_for_perf_tracing() {
        assert_eq!(traces_sample_rate_from(true, None), Some(1.0));
    }

    #[test]
    fn traces_sample_rate_clamps_configured_value() {
        assert_eq!(traces_sample_rate_from(true, Some("2".into())), Some(1.0));
        assert_eq!(
            traces_sample_rate_from(true, Some("-1".into())),
            Some(0.0)
        );
        assert_eq!(
            traces_sample_rate_from(false, Some("0.25".into())),
            Some(0.25)
        );
    }
}
