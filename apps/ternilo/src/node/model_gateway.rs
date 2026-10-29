use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
};
use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_local::{LocalApplication, ServerModelGateway};
use ternilo_protocol::{
    HarnessError, ModelRequest, ModelResponse, NodeModelRequest, RunModelBinding, SessionId,
};

#[derive(serde::Deserialize)]
struct WebAccessFailure {
    error: HarnessError,
}

pub(super) struct NodeModelGateway {
    application: Weak<LocalApplication>,
    endpoint: reqwest::Url,
    token: String,
    client: reqwest::Client,
}

impl NodeModelGateway {
    pub(super) fn new(
        application: &Arc<LocalApplication>,
        gateway: &str,
        token: &str,
    ) -> Result<Self, HarnessError> {
        let mut endpoint = reqwest::Url::parse(gateway)
            .map_err(|_| HarnessError::invalid("invalid Server gateway URL"))?;
        let scheme = if endpoint.scheme() == "wss" {
            "https"
        } else {
            "http"
        };
        endpoint
            .set_scheme(scheme)
            .map_err(|()| HarnessError::invalid("invalid Server model scheme"))?;
        endpoint.set_path("/internal/node/v1/model");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                HarnessError::execution(format!("build Server model client: {error}"))
            })?;
        Ok(Self {
            application: Arc::downgrade(application),
            endpoint,
            token: token.to_owned(),
            client,
        })
    }

    async fn request(
        &self,
        session: &str,
        binding: RunModelBinding,
        request: ModelRequest,
        output: &dyn ModelOutput,
    ) -> Result<ModelResponse, HarnessError> {
        let application = self
            .application
            .upgrade()
            .ok_or_else(|| HarnessError::cancelled("Ternilo is stopping"))?;
        let run_id = request.run_id.clone();
        let origin = application.model_input_origin(session, &run_id).await?;
        let body = NodeModelRequest {
            session_id: SessionId::new(session),
            origin_session_id: origin.session_id,
            run_id,
            provenance: origin.provenance,
            schedule_origins: origin.schedule_origins,
            request_id: super::random_hex_128(),
            binding,
            request,
        };
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| {
                HarnessError::unavailable("Cannot reach the Server account model gateway")
            })?;
        ternilo_builtins::read_model_gateway_response(response, output).await
    }
}

impl ServerModelGateway for NodeModelGateway {
    fn web_access<'a>(
        &'a self,
        session_id: &'a str,
        binding: RunModelBinding,
        credential: Option<(
            ternilo_protocol::RunId,
            ternilo_protocol::WebCapability,
            ternilo_protocol::ProviderWebAccess,
        )>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ternilo_protocol::ProviderWebAccessReply, HarnessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let (invocation, capability, expected) =
                if let Some((run_id, capability, expected)) = credential {
                    let app = self
                        .application
                        .upgrade()
                        .ok_or_else(|| HarnessError::cancelled("Ternilo is stopping"))?;
                    let origin = app.model_input_origin(session_id, &run_id).await?;
                    (
                        Some(ternilo_protocol::NodeRunContext {
                            session_id: SessionId::new(session_id),
                            origin_session_id: origin.session_id,
                            run_id,
                            provenance: origin.provenance,
                            schedule_origins: origin.schedule_origins,
                            binding: binding.clone(),
                        }),
                        Some(capability),
                        Some(Box::new(expected)),
                    )
                } else {
                    (None, None, None)
                };
            let body = ternilo_protocol::NodeWebAccessRequest {
                session_id: SessionId::new(session_id),
                binding,
                invocation,
                credential: capability,
                expected,
            };
            let mut endpoint = self.endpoint.clone();
            endpoint.set_path("/internal/node/v1/model/web-access");
            let response = self
                .client
                .post(endpoint)
                .bearer_auth(&self.token)
                .json(&body)
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await
                .map_err(|_| {
                    HarnessError::unavailable("cannot reach Server for Provider web settings")
                })?;
            if !response.status().is_success() {
                let status = response.status();
                let failure = response.json::<WebAccessFailure>().await.map_err(|_| {
                    HarnessError::unavailable(format!(
                        "Server web gateway returned HTTP {status} without a valid error response"
                    ))
                })?;
                return Err(failure.error);
            }
            response
                .json()
                .await
                .map_err(|_| HarnessError::execution("invalid Server web settings response"))
        })
    }

    fn complete<'a>(
        &'a self,
        session_id: &'a str,
        binding: RunModelBinding,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(HarnessError::cancelled("account model request was cancelled")),
                result = self.request(session_id, binding, request, output.as_ref()) => result,
            }
        })
    }
}
