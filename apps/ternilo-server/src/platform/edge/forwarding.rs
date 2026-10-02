use std::{collections::BTreeMap, net::IpAddr, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use salvo_core::prelude::{Depot, Json, Request, Router, handler};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use ternilo_protocol::{HarnessError, InputAuthor, TenantId};
use ternilo_transport::{ExecutorCommandBody, ExecutorId};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::{EdgeGateway, now_ms, random_hex_128};
use crate::{
    gateway_journal::{GatewayLease, PeerRoute, RouteKey},
    platform::{
        http::{ApiError, invalid_request},
        state::app_state,
    },
};

const PEER_PATH: &str = "/api/v1/internal/node/call";
const SIGNATURE_HEADER: &str = "x-ternilo-peer-signature";
const AUTH_WINDOW_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECENT_CALLS: usize = 8_192;

pub(crate) fn validate_cluster_origin(value: &str) -> Result<reqwest::Url, HarnessError> {
    let url =
        reqwest::Url::parse(value).map_err(|_| HarnessError::invalid("cluster URL is invalid"))?;
    let loopback = url
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(HarnessError::invalid(
            "cluster URL must be an HTTPS origin, or HTTP on a literal loopback address",
        ));
    }
    Ok(url)
}

pub(super) struct ClusterForwarder {
    pub(super) origin: String,
    key: Zeroizing<Vec<u8>>,
    client: reqwest::Client,
    recent: Mutex<BTreeMap<String, u64>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForwardRequest {
    pub(super) request_id: String,
    pub(super) target_instance_id: String,
    pub(super) tenant_id: TenantId,
    pub(super) executor_id: ExecutorId,
    pub(super) fencing_token: u64,
    pub(super) issued_at_ms: u64,
    pub(super) timeout_ms: u64,
    pub(super) body: ExecutorCommandBody,
    pub(super) author: Option<InputAuthor>,
    pub(super) authorization: Option<ternilo_transport::NodeInputAuthorization>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardReply {
    request_id: String,
    result: Result<Value, HarnessError>,
}

impl ClusterForwarder {
    pub(super) fn new(origin: &str, master_key: &str) -> Result<Self, HarnessError> {
        let origin = validate_cluster_origin(origin)?.to_string();
        let key = Zeroizing::new(
            STANDARD
                .decode(master_key.trim())
                .map_err(|_| HarnessError::invalid("invalid cluster master key"))?,
        );
        if key.len() != 32 {
            return Err(HarnessError::invalid("invalid cluster master key length"));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| HarnessError::execution("create Server peer client"))?;
        Ok(Self {
            origin,
            key,
            client,
            recent: Mutex::new(BTreeMap::new()),
        })
    }

    fn mac(&self, domain: &[u8], body: &[u8]) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts a 32-byte key");
        mac.update(domain);
        mac.update(body);
        mac
    }

    pub(super) fn sign(&self, body: &[u8]) -> String {
        self.sign_domain(
            b"ternilo-server-peer-v1\0POST\0/api/v1/internal/node/call\0",
            body,
        )
    }

    pub(super) fn authenticate(&self, signature: &str, body: &[u8]) -> Result<(), HarnessError> {
        self.authenticate_domain(
            b"ternilo-server-peer-v1\0POST\0/api/v1/internal/node/call\0",
            signature,
            body,
        )
    }

    pub(super) fn sign_domain(&self, domain: &[u8], body: &[u8]) -> String {
        STANDARD.encode(self.mac(domain, body).finalize().into_bytes())
    }

    pub(super) fn authenticate_domain(
        &self,
        domain: &[u8],
        signature: &str,
        body: &[u8],
    ) -> Result<(), HarnessError> {
        let signature = STANDARD
            .decode(signature)
            .map_err(|_| HarnessError::policy("invalid Server peer authentication"))?;
        self.mac(domain, body)
            .verify_slice(&signature)
            .map_err(|_| HarnessError::policy("invalid Server peer authentication"))
    }

    async fn admit(
        &self,
        input: &ForwardRequest,
        instance_id: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        if input.timeout_ms == 0 || input.timeout_ms > MAX_TIMEOUT_MS {
            return Err(HarnessError::policy("invalid Server peer request timeout"));
        }
        self.admit_identity(
            &input.request_id,
            &input.target_instance_id,
            input.issued_at_ms,
            instance_id,
            now,
        )
        .await
    }

    pub(super) async fn admit_identity(
        &self,
        request_id: &str,
        target: &str,
        issued_at_ms: u64,
        instance_id: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        if target != instance_id
            || issued_at_ms.abs_diff(now) > AUTH_WINDOW_MS
            || request_id.len() != 32
            || !request_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(HarnessError::policy(
                "Server peer target, timestamp or request identity is invalid",
            ));
        }
        let mut recent = self.recent.lock().await;
        recent.retain(|_, expires| *expires > now);
        if recent.contains_key(request_id) {
            return Err(HarnessError::conflict(
                "Server peer call was already accepted; it cannot be replayed",
            ));
        }
        if recent.len() >= MAX_RECENT_CALLS {
            return Err(HarnessError::unavailable("Server peer admission is busy"));
        }
        recent.insert(
            request_id.to_owned(),
            issued_at_ms.saturating_add(AUTH_WINDOW_MS + 1),
        );
        Ok(())
    }

    pub(super) async fn forward(
        &self,
        peer: PeerRoute,
        route: RouteKey,
        body: ExecutorCommandBody,
        author: Option<InputAuthor>,
        timeout: Duration,
        authorization: Option<ternilo_transport::NodeInputAuthorization>,
    ) -> Result<Value, HarnessError> {
        let origin = validate_cluster_origin(&peer.endpoint)?;
        let input = ForwardRequest {
            request_id: random_hex_128(),
            target_instance_id: peer.lease.owner_id,
            tenant_id: route.tenant_id,
            executor_id: route.executor_id,
            fencing_token: peer.lease.fencing_token,
            issued_at_ms: now_ms()?,
            timeout_ms: u64::try_from(timeout.as_millis())
                .unwrap_or(MAX_TIMEOUT_MS)
                .min(MAX_TIMEOUT_MS),
            body,
            author,
            authorization,
        };
        let bytes = serde_json::to_vec(&input)
            .map_err(|_| HarnessError::execution("encode Server peer request"))?;
        let signature = self.sign(&bytes);
        let mut response = self
            .client
            .post(origin.join(PEER_PATH).expect("fixed peer path"))
            .header(SIGNATURE_HEADER, signature)
            .header("Content-Type", "application/json")
            .body(bytes)
            .timeout(timeout.saturating_add(Duration::from_secs(5)))
            .send()
            .await
            .map_err(|_| uncertain_forward())?;
        if !response.status().is_success() {
            return Err(uncertain_forward());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| uncertain_forward())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_REPLY_BYTES {
                return Err(HarnessError::unavailable(
                    "Server peer reply exceeds the Node message limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let reply: ForwardReply =
            serde_json::from_slice(&bytes).map_err(|_| uncertain_forward())?;
        if reply.request_id != input.request_id {
            return Err(uncertain_forward());
        }
        reply.result
    }
}

fn uncertain_forward() -> HarnessError {
    HarnessError::unavailable(
        "Server peer request did not return a verified result; execution may already have started, so it was not retried",
    )
}

impl EdgeGateway {
    pub(super) async fn receive_forwarded(
        &self,
        input: ForwardRequest,
    ) -> Result<Value, HarnessError> {
        let cluster = self
            .cluster
            .as_ref()
            .ok_or_else(|| HarnessError::policy("Server peer forwarding is disabled"))?;
        let now = now_ms()?;
        let route = RouteKey::new(input.tenant_id.clone(), input.executor_id.clone());
        let lease = GatewayLease {
            owner_id: input.target_instance_id.clone(),
            fencing_token: input.fencing_token,
        };
        self.journal.check_lease(&route, &lease, now).await?;
        let connected = self.connected(&route).await?;
        if connected.lease.fencing_token != lease.fencing_token {
            return Err(HarnessError::conflict(
                "Node connection changed before Server peer dispatch",
            ));
        }
        validate_body(&input.body, &connected.hello.capabilities)?;
        if super::commands::creates_input(&input.body) && input.authorization.is_none() {
            return Err(HarnessError::policy(
                "Server peer input requires its original admission authority",
            ));
        }
        cluster.admit(&input, &self.instance_id, now).await?;
        let remaining = input
            .issued_at_ms
            .saturating_add(input.timeout_ms)
            .saturating_sub(now);
        if remaining == 0 {
            return Err(HarnessError::unavailable(
                "Server peer request expired before dispatch",
            ));
        }
        self.call_body(
            route,
            input.body,
            Duration::from_millis(remaining),
            input.author,
            input.authorization,
        )
        .await
    }
}

pub(super) fn validate_body(
    body: &ExecutorCommandBody,
    capabilities: &std::collections::BTreeSet<ternilo_transport::ExecutorCapability>,
) -> Result<(), HarnessError> {
    match body {
        ExecutorCommandBody::Application { request } => {
            request.validate()?;
            request.validate_executor_capabilities(capabilities)
        }
        ExecutorCommandBody::CancelRun { session_id, run_id } => {
            session_id.validate()?;
            run_id.validate()?;
            if capabilities.contains(&ternilo_transport::ExecutorCapability::RunCancellation) {
                Ok(())
            } else {
                Err(HarnessError::policy(
                    "Node does not support run cancellation",
                ))
            }
        }
        ExecutorCommandBody::CloudRun { .. } => Err(HarnessError::policy(
            "managed runs do not use Node peer routing",
        )),
    }
}

pub(crate) fn router() -> Router {
    Router::new()
        .push(Router::with_path("internal/node/call").post(forwarded_call))
        .push(super::model_forwarding::peer::router())
}

#[handler]
async fn forwarded_call(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ForwardReply>, ApiError> {
    let edge = &app_state(depot).edge;
    let cluster = edge.cluster.as_ref().ok_or_else(|| {
        ApiError::unauthorized(HarnessError::policy("Server peer forwarding is disabled"))
    })?;
    let signature = request
        .headers()
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::unauthorized(HarnessError::policy(
                "Server peer authentication is required",
            ))
        })?
        .to_owned();
    let bytes = request.payload().await.map_err(invalid_request)?;
    cluster
        .authenticate(&signature, bytes)
        .map_err(ApiError::unauthorized)?;
    let input: ForwardRequest = serde_json::from_slice(bytes).map_err(invalid_request)?;
    let request_id = input.request_id.clone();
    let result = edge.receive_forwarded(input).await;
    Ok(Json(ForwardReply { request_id, result }))
}
