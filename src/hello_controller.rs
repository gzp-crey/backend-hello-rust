use crate::config::Config;
use anyhow::Error as AnyError;
use axum::{routing::get, Extension, Json, Router};
use std::sync::Arc;

struct State {
    config: Config,
}

//#[tracing::instrument]
async fn get_configuration(Extension(state): Extension<Arc<State>>) -> Json<Config> {
    Json(state.config.clone())
}

pub struct Service {
    config: Config,
}

impl Service {
    pub fn into_router(self) -> Router {
        let mut router = Router::new();
        router = router.route("/config", get(get_configuration));

        let state = State { config: self.config };
        let state = Arc::new(state);
        router = router.layer(Extension(state));

        router
    }
}

pub async fn service(config: &Config) -> Result<Service, AnyError> {
    let service = Service { config: config.clone() };
    Ok(service)
}
