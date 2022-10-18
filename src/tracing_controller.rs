use crate::config::SERVICE_NAME;
use anyhow::Error as AnyError;
use axum::{routing::put, Extension, Json, Router};
use opentelemetry::{
    runtime::Tokio as OTTokio,
    sdk::{trace as otsdk, Resource},
    trace::Tracer,
};
use opentelemetry_semantic_conventions::resource as otconv;
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};
use tracing::{instrument::WithSubscriber, log, Dispatch, Level, Subscriber};
use tracing_opentelemetry::PreSampledTracer;
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

    /// Dump trace to the standard output
    StdOut,

    /// Enable Jaeger telemetry (https://www.jaegertracing.io)
    Jaeger,

    /// Enbale Zipkin telemetry (https://zipkin.io/)
    Zipkin,

    /// Appinsight telemetry
    AppInsight { instrumentation_key: String },
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

#[derive(Debug, Serialize, Deserialize)]
pub struct TraceConfigRequest {
    filter: String,
}

async fn reconfigure(
    Extension(state): Extension<Arc<State>>,
    Json(format): Json<TraceConfigRequest>,
) -> Result<(), String> {
    log::trace!("config: {:#?}", format);
    if let Some(reload_handle) = &state.reload_handle {
        reload_handle.reconfigure(format.filter)
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
    fn set_global_logger<L>(&mut self, tracing_pipeline: L) -> Result<(), AnyError>
    where
        L: Into<Dispatch>,
    {
        tracing::dispatcher::set_global_default(tracing_pipeline.into())?;
        Ok(())
    }

    fn intsall_telemetry_with_tracer<L, T>(
        &mut self,
        _config: &Config,
        tracing_pipeline: L,
        tracer: T,
    ) -> Result<(), AnyError>
    where
        L: for<'a> LookupSpan<'a> + Subscriber + WithSubscriber + Send + Sync,
        T: 'static + Tracer + PreSampledTracer + Send + Sync,
    {
        let telemetry = tracing_opentelemetry::layer()
            .with_tracked_inactivity(true)
            .with_tracer(tracer);
        let tracing_pipeline = tracing_pipeline.with(telemetry);
        self.set_global_logger(tracing_pipeline)?;
        Ok(())
    }

    fn intsall_telemetry<L>(&mut self, config: &Config, tracing_pipeline: L) -> Result<(), AnyError>
    where
        L: for<'a> LookupSpan<'a> + Subscriber + WithSubscriber + Send + Sync,
    {
        let resource = Resource::new(vec![otconv::SERVICE_NAME.string(SERVICE_NAME)]);

        match &config.telemetry {
            Telemetry::StdOut => {
                let tracer = opentelemetry::sdk::export::trace::stdout::PipelineBuilder::default()
                    .with_trace_config(
                        otsdk::config()
                            .with_resource(resource)
                            .with_sampler(otsdk::Sampler::AlwaysOn),
                    )
                    .install_simple();
                self.intsall_telemetry_with_tracer(config, tracing_pipeline, tracer)
            }
            Telemetry::Jaeger => {
                let tracer = opentelemetry_jaeger::new_agent_pipeline()
                    .with_trace_config(otsdk::config().with_resource(resource))
                    .with_service_name(SERVICE_NAME)
                    .install_batch(OTTokio)?;
                self.intsall_telemetry_with_tracer(config, tracing_pipeline, tracer)
            }
            Telemetry::Zipkin => {
                let tracer = opentelemetry_zipkin::new_pipeline()
                    .with_trace_config(otsdk::config().with_resource(resource))
                    .with_service_name(SERVICE_NAME)
                    .install_batch(OTTokio)?;
                self.intsall_telemetry_with_tracer(config, tracing_pipeline, tracer)
            }

            Telemetry::AppInsight { instrumentation_key } => {
                let tracer = opentelemetry_application_insights::new_pipeline(instrumentation_key.clone())
                    .with_trace_config(otsdk::config().with_resource(resource))
                    .with_service_name(SERVICE_NAME)
                    .with_client(reqwest::Client::new())
                    .install_batch(OTTokio);
                self.intsall_telemetry_with_tracer(config, tracing_pipeline, tracer)
            }
            Telemetry::None => Ok(()), //self.finalize_logger(config, log),
        }
    }

    fn install_logger<L>(&mut self, config: &Config, tracing_pipeline: L) -> Result<(), AnyError>
    where
        L: for<'a> LookupSpan<'a> + Subscriber + WithSubscriber + Send + Sync,
    {
        if config.allow_reconfigure {
            let env_filter = EnvFilter::from_default_env().add_directive(Level::INFO.into());
            let (env_filter, reload_handle) = reload::Layer::new(env_filter);
            self.reload_handle = Some(Box::new(reload_handle));
            let tracing_pipeline = tracing_pipeline.with(env_filter);

            let fmt = tracing_subscriber::fmt::Layer::new();
            let tracing_pipeline = tracing_pipeline.with(fmt);

            self.intsall_telemetry(config, tracing_pipeline)?;
        } else {
            let env_filter = EnvFilter::from_default_env().add_directive(Level::INFO.into());
            let tracing_pipeline = tracing_pipeline.with(env_filter);

            let fmt = tracing_subscriber::fmt::Layer::new();
            let tracing_pipeline = tracing_pipeline.with(fmt);

            self.intsall_telemetry(config, tracing_pipeline)?;
        }

        Ok(())
    }

    pub fn into_router(self) -> Router {
        let mut router = Router::new();
        // todo: consider adding it conditionally 'if self.reload_handle.is_some()'
        router = router.route("/config", put(reconfigure));

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
