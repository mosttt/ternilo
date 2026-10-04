use std::{path::PathBuf, sync::Arc};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use salvo_core::{
    http::{StatusCode, header},
    prelude::{Depot, FlowCtrl, Json, Request, Response, Router, Service, Text, handler},
};
use salvo_extra::{affix_state, size_limiter::max_size};
use serde::{Deserialize, Serialize};
use ternilo_control::NativeRegistration;
use ternilo_protocol::HarnessError;
use tokio::sync::{Mutex, OnceCell};

use crate::{
    config::{ServeOptions, ServerConfig, accepts_setup_token, configuration_path, digest_token},
    platform::{Runtime, http::ApiError},
};

struct Bootstrap {
    config_path: PathBuf,
    options: ServeOptions,
    base: ServerConfig,
    token_hash: String,
    initialization: Mutex<()>,
    runtime: OnceCell<Runtime>,
}

pub(crate) fn new_setup_token() -> String {
    format!(
        "ter_b_{}",
        URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
    )
}

pub(crate) async fn execute(options: ServeOptions) -> Result<(), HarnessError> {
    let config_path = configuration_path(options.config_dir.as_deref())?;
    if config_path.exists() {
        let explicit_key = options.setup_token.is_some();
        let mut config = options.load()?;
        if !explicit_key {
            config.setup_token_hash = None;
        }
        return crate::platform::execute(config).await;
    }
    let token = options.setup_token.clone().unwrap_or_else(new_setup_token);
    if token.is_empty() {
        return Err(HarnessError::invalid(
            "initialization Key must not be empty",
        ));
    }
    let base = options.clone().apply(ServerConfig::defaults(
        &config_path,
        "sqlite::memory:".to_owned(),
        STANDARD.encode(rand::random::<[u8; 32]>()),
    ))?;
    let listen = base.listen;
    let state = Arc::new(Bootstrap {
        config_path,
        options,
        base,
        token_hash: digest_token(&token),
        initialization: Mutex::new(()),
        runtime: OnceCell::new(),
    });
    println!("Initialization Key: {token}");
    println!(
        "Open this Server's actual browser address and enter the Key to choose a database and create its owner."
    );
    println!("Keep this Key private. It expires when setup completes.");
    let router = Router::new()
        .hoop(affix_state::inject(Arc::clone(&state)))
        .push(Router::with_path("{**path}").goal(dispatch))
        .goal(dispatch);
    let shutdown_state = Arc::clone(&state);
    crate::http::serve(listen, router, "setup", async move {
        if let Some(runtime) = shutdown_state.runtime.get() {
            runtime.shutdown().await;
        }
    })
    .await
}

#[handler]
async fn dispatch(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    let state = depot
        .get_typed::<Arc<Bootstrap>>()
        .expect("bootstrap state");
    let service = if let Some(runtime) = state.runtime.get() {
        Service::new(Arc::clone(&runtime.router))
    } else {
        Service::new(setup_router(Arc::clone(state)))
    };
    let handler = service.hyper_handler(
        request.local_addr().clone(),
        request.remote_addr().clone(),
        request.scheme().clone(),
        None,
        control.conn().clone(),
        None,
    );
    *response = handler
        .handle(std::mem::replace(request, Request::new()))
        .await;
}

fn setup_router(state: Arc<Bootstrap>) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .hoop(setup_headers)
        .hoop(max_size(16 * 1024))
        .push(crate::assets::router())
        .push(Router::with_path("assets/boot.js").get(boot))
        .push(Router::with_path("readyz").get(ready))
        .push(Router::with_path("livez").get(ready))
        .push(Router::with_path("api/v1/setup").post(initialize))
}

#[derive(Serialize)]
struct Boot {
    setup: bool,
    setup_database_preset: bool,
    setup_public_url: Option<String>,
}

#[handler]
fn boot(depot: &mut Depot, response: &mut Response) {
    let state = depot
        .get_typed::<Arc<Bootstrap>>()
        .expect("bootstrap state");
    let payload = Boot {
        setup: true,
        setup_database_preset: state.options.database_url.is_some(),
        setup_public_url: state.base.public_url.clone(),
    };
    response.render(Text::Js(format!(
        "window.__TERNILO_BOOT__ = {};",
        serde_json::to_string(&payload).expect("serializable boot")
    )));
}

#[handler]
fn ready() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "setup_required"}))
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DatabaseSelection {
    Sqlite,
    Postgres {
        url: String,
        migration_url: Option<String>,
    },
    Preset,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    setup_token: String,
    database: DatabaseSelection,
    public_url: Option<String>,
    username: String,
    email: String,
    password: String,
}

#[handler]
async fn initialize(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = request
        .parse_json::<SetupRequest>()
        .await
        .map_err(|_| HarnessError::invalid("invalid setup request"))?;
    let state = Arc::clone(
        depot
            .get_typed::<Arc<Bootstrap>>()
            .expect("bootstrap state"),
    );
    if !accepts_setup_token(Some(&state.token_hash), &body.setup_token) {
        return Err(ApiError::unauthorized(HarnessError::policy(
            "initialization Key is invalid",
        )));
    }
    let owner = NativeRegistration {
        username: body.username,
        email: body.email,
        password: body.password,
    };
    owner.validate()?;
    let _initialization = state.initialization.lock().await;
    if state.runtime.get().is_some() || state.config_path.exists() {
        return Err(HarnessError::conflict("server is already configured").into());
    }
    let mut config = state.base.clone();
    match (state.options.database_url.as_ref(), body.database) {
        (Some(_), DatabaseSelection::Preset) => {}
        (Some(_), _) => {
            return Err(HarnessError::invalid("database is preset by startup options").into());
        }
        (None, DatabaseSelection::Sqlite) => {
            config.database_url = crate::setup::default_database_url(&state.config_path)?;
        }
        (None, DatabaseSelection::Postgres { url, migration_url }) => {
            if !url.starts_with("postgres://") && !url.starts_with("postgresql://") {
                return Err(HarnessError::invalid(
                    "PostgreSQL connection must use postgres:// or postgresql://",
                )
                .into());
            }
            config.database_url = url;
            if state.options.migration_database_url.is_none() {
                config.migration_database_url = migration_url.filter(|value| !value.is_empty());
            }
        }
        (None, DatabaseSelection::Preset) => {
            return Err(HarnessError::invalid("choose a database").into());
        }
    }
    if config.database_url.contains(":memory:") {
        return Err(HarnessError::invalid("setup requires a persistent database").into());
    }
    if state.options.public_url.is_none() {
        config.public_url = body.public_url.filter(|value| !value.is_empty());
    }
    config.setup_token_hash = Some(state.token_hash.clone());
    config.validate()?;
    // Build every schema and the complete application before saving its configuration.
    let runtime = crate::platform::prepare(config.clone()).await.map_err(|_| {
        HarnessError::execution("database connection or schema initialization failed; check its address, credentials and schema permissions")
    })?;
    if runtime.store.instance_settings().await?.is_some() {
        return Err(HarnessError::conflict(
            "this database already has an owner; use its existing configuration",
        )
        .into());
    }
    config.write_new(&state.config_path)?;
    // Save the master key before creating any account. A failed owner transaction can
    // be retried through the protected account setup page using this configuration.
    let owner = runtime
        .store
        .initialize_owner(&owner, crate::setup::now_ms()?)
        .await;
    let _ = state.runtime.set(runtime);
    owner.map_err(|_| {
        HarnessError::execution("configuration was saved; reload this page to retry owner setup")
    })?;
    println!(
        "Server setup complete. Configuration: {}",
        state.config_path.display()
    );
    Ok(Json(serde_json::json!({"configured": true})))
}

#[handler]
async fn setup_headers(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    control.call_next(request, depot, response).await;
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (
            header::CONTENT_SECURITY_POLICY,
            crate::platform::web::BASE_SECURITY_POLICY,
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ] {
        response
            .headers_mut()
            .insert(name, value.parse().expect("static header"));
    }
    if response.status_code.is_none() {
        response.status_code(StatusCode::OK);
    }
}
