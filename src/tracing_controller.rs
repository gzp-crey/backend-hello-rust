use crate::config::SERVICE_NAME;
use anyhow::Error as AnyError;
use axum::{routing::put, Extension, Router};
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};
use tracing::{instrument::WithSubscriber, log, Dispatch, Level, Subscriber};
use tracing_subscriber::{
    filter::EnvFilter,
    layer::SubscriberExt,
    registry::LookupSpan,
    reload::{self, Handle},
    Layer,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(tag = "type")]
pub enum Telemetry {
    /// Disable telemetry
    None,

    /// Enable Jaeger telemetry (https://www.jaegertracing.io)
    Jaeger,

    /// Enbale Zipkin telemetry (https://zipkin.io/)
    Zipkin,

    /// Appinsight telemetry
    AppInsight {
        //endpoint: String,
        instrumentation_key: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    allow_reconfigure: bool,
    telemetry: Telemetry,
}

trait DynHandle: Send + Sync {
    fn reconfigure(&self, config: String) -> Result<(), String>;
}

impl<L, S> DynHandle for Handle<L, S>
where
    L: 'static + Layer<S> + From<EnvFilter> + Send + Sync,
    S: Subscriber,
{
    fn reconfigure(&self, mut new_config: String) -> Result<(), String> {
        new_config.retain(|c| !c.is_whitespace());
        let new_filter = new_config.parse::<EnvFilter>().map_err(|e| format!("{}", e))?;
        self.reload(new_filter).map_err(|e| format!("{}", e))
    }
}

#[tracing::instrument(skip(state))]
async fn configure_log(Extension(state): Extension<Arc<State>>, format: String) -> Result<(), String> {
    log::trace!("format={}", format);
    if let Some(reload_handle) = &state.reload_handle {
        reload_handle.reconfigure(format)
    } else {
        Err("Trace reconfigure is not enabled".into())
    }
}

struct State {
    reload_handle: Option<Box<dyn DynHandle>>,
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State").finish()
    }
}

pub struct Service {
    reload_handle: Option<Box<dyn DynHandle>>,
}

impl Service {
    fn finalize_logger<L>(&mut self, _config: &Config, log: L) -> Result<(), AnyError>
    where
        L: Into<Dispatch>,
    {
        tracing::dispatcher::set_global_default(log.into())?;
        Ok(())
    }

    fn intsall_telemetry<L>(&mut self, config: &Config, log: L) -> Result<(), AnyError>
    where
        L: for<'a> LookupSpan<'a> + Subscriber + WithSubscriber + Send + Sync,
    {
        match &config.telemetry {
            Telemetry::Jaeger => {
                let tracer = opentelemetry_jaeger::new_agent_pipeline()
                    .with_service_name(SERVICE_NAME)
                    .install_batch(opentelemetry::runtime::Tokio)?;
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracked_inactivity(true)
                    .with_tracer(tracer);
                self.finalize_logger(config, log.with(telemetry))
            }
            Telemetry::Zipkin => {
                let tracer = opentelemetry_zipkin::new_pipeline()
                    .with_service_name(SERVICE_NAME)
                    .install_batch(opentelemetry::runtime::Tokio)?;
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracked_inactivity(true)
                    .with_tracer(tracer);
                self.finalize_logger(config, log.with(telemetry))
            }

            Telemetry::AppInsight {
                /*endpoint,*/ instrumentation_key,
            } => {
                let tracer = opentelemetry_application_insights::new_pipeline(instrumentation_key.clone())
                    //.with_endpoint(endpoint).unwrap()
                    .with_service_name(SERVICE_NAME)
                    .with_client(reqwest::Client::new())
                    .install_batch(opentelemetry::runtime::Tokio);
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracked_inactivity(true)
                    .with_tracer(tracer);
                self.finalize_logger(config, log.with(telemetry))
            }
            Telemetry::None => Ok(()), //self.finalize_logger(config, log),
        }
    }

    fn install_logger<L>(&mut self, config: &Config, log: L) -> Result<(), AnyError>
    where
        L: for<'a> LookupSpan<'a> + Subscriber + WithSubscriber + Send + Sync,
    {
        let fmt = tracing_subscriber::fmt::Layer::new();

        if config.allow_reconfigure {
            let env_filter = EnvFilter::from_default_env().add_directive(Level::WARN.into());
            let (env_filter, reload_handle) = reload::Layer::new(env_filter);
            self.reload_handle = Some(Box::new(reload_handle));
            self.intsall_telemetry(config, log.with(fmt).with(env_filter))
        } else {
            self.intsall_telemetry(config, log.with(fmt))
        }
    }

    pub fn into_router(self) -> Router {
        let mut router = Router::new();
        // todo: consider adding it conditionally 'if self.reload_handle.is_some()'
        router = router.route("/filter", put(configure_log));

        let state = State {
            reload_handle: self.reload_handle,
        };
        let state = Arc::new(state);
        router = router.layer(Extension(state));

        router
    }
}

pub async fn service(config: &Config) -> Result<Service, AnyError> {
    let mut service = Service { reload_handle: None };
    service.install_logger(config, tracing_subscriber::registry())?;
    Ok(service)
}
