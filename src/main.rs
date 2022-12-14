use anyhow::{anyhow, Error as AnyError};
use axum::Router;
use azure_identity::AzureCliCredential;
use opentelemetry::global;
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    runtime::{Handle as RtHandle, Runtime},
    signal,
};
use tower_http::cors::CorsLayer;
use tracing::{log, Dispatch};

mod config;
mod hello_controller;
mod tracing_controller;

async fn shutdown_signal() {
    signal::ctrl_c().await.expect("expect tokio signal ctrl-c");
    log::warn!("Signal shutdown");
}

async fn async_main(rt_handle: RtHandle) -> Result<(), AnyError> {
    log::warn!("Finding azure credentials...");
    let credential = Arc::new(AzureCliCredential::new());

    let (config, tracing_service) = {
        // initialize a pre-init logger
        let preinit_log = {
            let _ = tracing_log::LogTracer::init();
            let preinit_logger = tracing_subscriber::fmt()
                .with_env_filter("info,sqlx=warn")
                .compact()
                .finish();
            Dispatch::new(preinit_logger)
        };
        let _preinit_log_guard = tracing::dispatcher::set_default(&preinit_log);

        log::trace!("init-trace - ok");
        log::debug!("init-debug - ok");
        log::info!("init-info  - ok");
        log::warn!("init-warn  - ok");
        log::error!("init-error - ok");

        let config = config::Config::new(&rt_handle, &credential)?;
        let tracing_service = tracing_controller::service(&config.tracing).await?;
        log::info!("preinit completed");
        (config, tracing_service)
    };

    log::trace!("Creating services...");
    log::trace!("trace - ok");
    log::debug!("debug - ok");
    log::info!("info  - ok");
    log::warn!("warn  - ok");
    tracing::warn!("warn  - ok(tracing)");
    log::error!("error - ok");

    let cors = CorsLayer::permissive();
    let tracing_layer = axum_tracing_opentelemetry::opentelemetry_tracing_layer();

    let hello_service = hello_controller::service(&config).await?;

    let app = Router::new()
        .nest("/tracing", tracing_service.into_router())
        .nest("/hello", hello_service.into_router())
        .layer(cors)
        .layer(tracing_layer);

    let addr = SocketAddr::from(([0, 0, 0, 0], 80));
    log::warn!("Starting service on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| anyhow!(e))?;

    log::info!("Bye.");
    global::shutdown_tracer_provider();
    Ok(())
}

pub fn main() {
    let rt = Runtime::new().unwrap();

    let handle = rt.handle();
    if let Err(err) = handle.block_on(async_main(handle.clone())) {
        println!("[ERROR] {}", err);
        if let Some(cause) = err.source() {
            println!();
            println!("Caused by:");
            let mut cause = Some(cause);
            let mut i = 0;
            while let Some(e) = cause {
                println!("   {}: {}", i, e);
                cause = e.source();
                i += 1;
            }
        }
        panic!();
    }
}
