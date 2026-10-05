use salvo_core::{
    http::{HeaderValue, Method, StatusCode, header},
    prelude::{Depot, FlowCtrl, Json, Request, Response, Router, Scribe, handler},
};
use salvo_extra::size_limiter::max_size;
use serde::{Deserialize, Serialize};
use ternilo_control::{
    AccountLoginMethods, AccountStatus, ControlUser, IdentitySession, InstanceMode,
    InstanceSettings, NativeRegistration, NativeSessionGrant, PlatformRole, RegistrationSettings,
    TenantSummary,
};
use ternilo_protocol::{HarnessError, TenantId};

use super::{
    auth::{self, authentication_error},
    http::{ApiError, bearer_token, invalid_request, now_ms},
    state::{AppState, actor, app_state},
};

const MAX_IDENTITY_BODY_BYTES: u64 = 16 * 1024;

mod email;
mod mfa;
pub(super) mod session_details;
mod sessions;

pub(crate) fn router() -> Router {
    Router::with_path("api/v1")
        .hoop(no_store)
        .hoop(max_size(MAX_IDENTITY_BODY_BYTES))
        .push(
            Router::with_path("auth")
                .push(Router::with_path("setup").post(setup))
                .push(Router::with_path("login").post(login))
                .push(Router::with_path("password-recovery").post(email::request_recovery))
                .push(Router::with_path("password-reset").post(email::reset_password))
                .push(Router::with_path("register").post(register))
                .push(Router::with_path("oidc/register").post(register_oidc))
                .push(Router::with_path("logout").post(logout))
                .push(Router::with_path("invitations/accept").post(accept_invitation))
                .push(
                    Router::with_path("oidc-link")
                        .hoop(native_binding_auth)
                        .hoop(auth::user_auth)
                        .get(get_oidc_link)
                        .post(link_oidc),
                ),
        )
        .push(
            Router::new()
                .hoop(auth::user_auth)
                .push(Router::with_path("auth/session").get(get_session))
                .push(Router::with_path("auth/password").post(change_password))
                .push(sessions::router())
                .push(mfa::router())
                .push(Router::with_path("auth/email").get(email::status))
                .push(Router::with_path("auth/email/send").post(email::send_verification))
                .push(Router::with_path("auth/email/verify").post(email::verify))
                .push(Router::with_path("invitations/accept").post(join_invitation)),
        )
}

#[handler]
async fn native_binding_auth(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    if request.method() == Method::POST {
        let result = bearer_token(request).and_then(|token| {
            if token.starts_with("ter_a_") {
                Ok(())
            } else {
                Err(HarnessError::policy(
                    "sign in with your username and password before linking OIDC",
                )
                .into())
            }
        });
        if let Err(error) = result {
            error.render(response);
            control.skip_rest();
            return;
        }
    }
    control.call_next(request, depot, response).await;
}

#[handler]
pub(super) async fn no_store(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    control.call_next(request, depot, response).await;
}

#[derive(Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Public login capabilities are independent flags, not mutually exclusive states."
)]
pub(crate) struct AuthConfig {
    initialized: bool,
    mode: InstanceMode,
    native_enabled: bool,
    oidc_enabled: bool,
    email_enabled: bool,
    registration: RegistrationSettings,
    oidc_providers: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turnstile: Option<serde_json::Value>,
}

#[handler]
pub(crate) async fn auth_config(
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<AuthConfig>, ApiError> {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let state = app_state(depot);
    let instance = state.store.instance_settings().await?;
    let runtime = state.security.current(&state.store).await?;
    let oidc_providers = runtime.settings.oidc_providers.iter().filter_map(|settings| {
        let provider = runtime.providers.get(&settings.id)?;
        Some(serde_json::json!({"id":settings.id,"name":provider.name,"config":provider.web.public_config()}))
    }).collect::<Vec<_>>();
    Ok(Json(AuthConfig {
        initialized: instance.is_some(),
        mode: instance.map_or(InstanceMode::SingleUser, |value| value.mode),
        native_enabled: true,
        oidc_enabled: !oidc_providers.is_empty(),
        email_enabled: runtime.mailer.is_some(),
        registration: state.store.registration_settings().await?,
        oidc_providers,
        turnstile: runtime
            .settings
            .turnstile
            .as_ref()
            .map(|settings| serde_json::json!({"site_key": settings.site_key})),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    email: String,
    setup_token: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
    mfa_code: Option<String>,
    turnstile_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRequest {
    email: String,
    username: String,
    password: String,
    turnstile_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcLinkRequest {
    access_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptInvitationRequest {
    email: String,
    token: String,
    username: String,
    password: String,
    turnstile_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinInvitationRequest {
    token: String,
}

#[derive(Serialize)]
pub(crate) struct BrowserInstance {
    #[serde(flatten)]
    settings: InstanceSettings,
    managed_execution_enabled: bool,
}

#[derive(Serialize)]
struct BrowserSession {
    email: Option<String>,
    user: ControlUser,
    instance: BrowserInstance,
    is_instance_owner: bool,
    platform_role: PlatformRole,
    personal_tenant_id: TenantId,
    personal_project_id: String,
    expires_at_ms: Option<u64>,
}

#[derive(Serialize)]
struct BrowserSessionGrant {
    access_token: String,
    #[serde(flatten)]
    session: BrowserSession,
}

#[derive(Serialize)]
struct RegistrationResult {
    status: AccountStatus,
    user_id: ternilo_protocol::UserId,
    session: Option<BrowserSessionGrant>,
}

pub(super) fn browser_instance(state: &AppState, settings: InstanceSettings) -> BrowserInstance {
    BrowserInstance {
        settings,
        managed_execution_enabled: state.managed_execution_enabled,
    }
}

fn browser_session(state: &AppState, session: IdentitySession) -> BrowserSession {
    BrowserSession {
        email: session.email,
        user: session.user,
        instance: browser_instance(state, session.instance),
        is_instance_owner: session.is_instance_owner,
        platform_role: session.platform_role,
        personal_tenant_id: session.personal_tenant_id,
        personal_project_id: session.personal_project_id,
        expires_at_ms: session.expires_at_ms,
    }
}

async fn browser_grant(
    state: &AppState,
    request: &Request,
    grant: NativeSessionGrant,
) -> Result<BrowserSessionGrant, ApiError> {
    session_details::record(state, request, &grant.session.user, &grant.access_token).await?;
    Ok(BrowserSessionGrant {
        access_token: grant.access_token,
        session: browser_session(state, grant.session),
    })
}

#[handler]
async fn setup(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<BrowserSessionGrant>, ApiError> {
    let body = request
        .parse_json::<SetupRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    if state.store.instance_settings().await?.is_some() {
        return Err(HarnessError::conflict("server is already initialized").into());
    }
    if !crate::config::accepts_setup_token(state.setup_token_hash.as_deref(), &body.setup_token) {
        return Err(ApiError::unauthorized(HarnessError::policy(
            "setup token is invalid",
        )));
    }
    let registration = NativeRegistration {
        email: body.email,
        username: body.username,
        password: body.password,
    };
    let grant = state
        .store
        .initialize_owner(&registration, now_ms()?)
        .await?;
    Ok(Json(browser_grant(state, request, grant).await?))
}

#[handler]
async fn login(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<BrowserSessionGrant>, ApiError> {
    let body = request
        .parse_json::<LoginRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .security
        .current(&state.store)
        .await?
        .verify_turnstile(body.turnstile_token.as_deref(), "login")
        .await?;
    let credentials = state
        .store
        .verify_native_credentials(&body.username, &body.password)
        .await
        .map_err(authentication_error)?;
    let credentials = state
        .store
        .verify_native_mfa(credentials, body.mfa_code.as_deref(), now_ms()?)
        .await?;
    let grant = state
        .store
        .create_native_browser_session(credentials, now_ms()?)
        .await?;
    Ok(Json(browser_grant(state, request, grant).await?))
}

#[handler]
async fn register(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<RegistrationResult>, ApiError> {
    let body = request
        .parse_json::<RegisterRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .security
        .current(&state.store)
        .await?
        .verify_turnstile(body.turnstile_token.as_deref(), "register")
        .await?;
    let registration = state
        .store
        .register_native(
            &NativeRegistration {
                email: body.email,
                username: body.username,
                password: body.password,
            },
            now_ms()?,
        )
        .await?;
    response.status_code(StatusCode::CREATED);
    Ok(Json(RegistrationResult {
        status: registration.status,
        user_id: registration.user_id,
        session: match registration.session {
            Some(grant) => Some(browser_grant(state, request, grant).await?),
            None => None,
        },
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcRegistrationRequest {
    invitation_token: Option<String>,
    email: Option<String>,
    username: String,
    turnstile_token: Option<String>,
}

#[handler]
async fn register_oidc(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<ternilo_control::OidcRegistrationOutcome>, ApiError> {
    let state = app_state(depot);
    let token = bearer_token(request)?;
    let runtime = state.security.current(&state.store).await?;
    let (principal, _) = auth::authenticate_oidc_identity(state, token)
        .await
        .map_err(authentication_error)?;
    let body = request
        .parse_json::<OidcRegistrationRequest>()
        .await
        .map_err(invalid_request)?;
    runtime
        .verify_turnstile(body.turnstile_token.as_deref(), "register")
        .await?;
    let registration = state
        .store
        .register_oidc(
            &principal,
            &body.username,
            principal
                .email
                .as_deref()
                .filter(|email| !email.trim().is_empty())
                .or(body.email.as_deref())
                .unwrap_or(""),
            body.invitation_token.as_deref(),
            now_ms()?,
        )
        .await?;
    response.status_code(StatusCode::CREATED);
    Ok(Json(registration))
}

#[handler]
async fn logout(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), ApiError> {
    let token = bearer_token(request)?;
    let state = app_state(depot);
    // A paused account must still be able to discard its native session.
    if token.starts_with("ter_a_") {
        state.store.logout_native_session(token, now_ms()?).await?;
    } else if token.starts_with("ter_o_") {
        state.store.revoke_oidc_session(token).await?;
    } else {
        auth::authenticate_token(state, token)
            .await
            .map_err(authentication_error)?;
    }
    response.status_code(StatusCode::NO_CONTENT);
    Ok(())
}

#[handler]
fn get_session(depot: &mut Depot) -> Json<BrowserSession> {
    let session = depot
        .get_typed::<IdentitySession>()
        .expect("user authentication middleware must run first")
        .clone();
    Json(browser_session(app_state(depot), session))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordChangeRequest {
    current_password: String,
    new_password: String,
}

#[handler]
async fn change_password(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_control::NativePasswordReset>, ApiError> {
    let body = request
        .parse_json::<PasswordChangeRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let result = state
        .store
        .change_native_password(
            actor(depot),
            &body.current_password,
            &body.new_password,
            now_ms()?,
        )
        .await?;
    state
        .cloud_events
        .reauthenticate_user(&actor(depot).user_id);
    Ok(Json(result))
}

#[handler]
async fn get_oidc_link(depot: &mut Depot) -> Result<Json<AccountLoginMethods>, ApiError> {
    Ok(Json(
        app_state(depot)
            .store
            .account_login_methods(actor(depot))
            .await?,
    ))
}

#[handler]
async fn link_oidc(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AccountLoginMethods>, ApiError> {
    let state = app_state(depot);
    let body = request
        .parse_json::<OidcLinkRequest>()
        .await
        .map_err(invalid_request)?;
    let (principal, _) = auth::authenticate_oidc_identity(state, &body.access_token)
        .await
        .map_err(authentication_error)?;
    let result = state
        .store
        .link_native_oidc(actor(depot), &principal, now_ms()?)
        .await;
    if body.access_token.starts_with("ter_o_") {
        state.store.revoke_oidc_session(&body.access_token).await?;
    }
    Ok(Json(result?))
}

#[handler]
async fn join_invitation(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<TenantSummary>, ApiError> {
    let body = request
        .parse_json::<JoinInvitationRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .join_invitation(actor(depot), &body.token, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn accept_invitation(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<BrowserSessionGrant>, ApiError> {
    let body = request
        .parse_json::<AcceptInvitationRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .security
        .current(&state.store)
        .await?
        .verify_turnstile(body.turnstile_token.as_deref(), "invitation")
        .await?;
    let registration = NativeRegistration {
        email: body.email,
        username: body.username,
        password: body.password,
    };
    let state = app_state(depot);
    let grant = state
        .store
        .accept_user_invitation(&body.token, &registration, now_ms()?)
        .await?;
    Ok(Json(browser_grant(state, request, grant).await?))
}
