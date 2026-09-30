use std::sync::Arc;
use ternilo_local::{LocalApplication, LocalServerBinding};
use ternilo_protocol::HarnessError;
use ternilo_transport::{ExecutorId, NodeCleanupSnapshot};

pub(super) struct CleanupClient {
    endpoint: reqwest::Url,
    client: reqwest::Client,
    token: String,
    executor_id: ExecutorId,
}

pub(crate) fn server_binding(
    gateway: &str,
    node_id: &str,
) -> Result<LocalServerBinding, HarnessError> {
    let mut endpoint = reqwest::Url::parse(gateway)
        .map_err(|_| HarnessError::invalid("invalid Server gateway URL"))?;
    let scheme = if endpoint.scheme() == "wss" {
        "https"
    } else {
        "http"
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|()| HarnessError::invalid("invalid Server gateway scheme"))?;
    endpoint.set_path("");
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(LocalServerBinding {
        server_url: endpoint.to_string(),
        node_id: node_id.to_owned(),
    })
}

impl CleanupClient {
    pub(super) fn new(
        gateway: &str,
        token: &str,
        executor_id: &ExecutorId,
    ) -> Result<Self, HarnessError> {
        let binding = server_binding(gateway, executor_id.as_str())?;
        let mut endpoint = reqwest::Url::parse(&binding.server_url).expect("validated Server URL");
        endpoint.set_path("/api/v1/executors/cleanup");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                HarnessError::execution(format!("create Node cleanup client: {error}"))
            })?;
        Ok(Self {
            endpoint,
            client,
            token: token.to_owned(),
            executor_id: executor_id.clone(),
        })
    }

    pub(super) async fn synchronize(
        &self,
        app: &Arc<LocalApplication>,
    ) -> Result<bool, HarnessError> {
        let mut synchronization = self.endpoint.clone();
        synchronization.set_path("/api/v1/executors/cleanup/sync");
        let response = self
            .client
            .post(synchronization)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"storage_instance_id":app.node_storage_instance_id().await}))
            .send()
            .await
            .map_err(|_| HarnessError::unavailable("cannot reach Server Node cleanup channel"))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|body| {
                    body.get("error")?
                        .get("message")?
                        .as_str()
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "Server could not synchronize Node authorization".to_owned());
            return Err(HarnessError::unavailable(format!(
                "Node authorization synchronization returned HTTP {status}: {message}"
            )));
        }
        let snapshot: NodeCleanupSnapshot = response
            .json()
            .await
            .map_err(|_| HarnessError::invalid("invalid Server Node cleanup snapshot"))?;
        if snapshot.executor_id != self.executor_id {
            return Err(HarnessError::policy(
                "Server cleanup belongs to another Node",
            ));
        }
        let receipts = app.synchronize_account_authorizations(&snapshot).await?;
        for receipt in receipts {
            let response = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(&self.token)
                .json(&receipt)
                .send()
                .await
                .map_err(|_| HarnessError::unavailable("cannot send Node cleanup receipt"))?;
            if !response.status().is_success() {
                return Err(HarnessError::unavailable(
                    "Server rejected the Node cleanup receipt",
                ));
            }
        }
        Ok(snapshot.connection_allowed)
    }

    pub(super) async fn pump(self, app: Arc<LocalApplication>) -> Result<(), HarnessError> {
        let mut previous_failure = None;
        loop {
            match self.synchronize(&app).await {
                Ok(_) => previous_failure = None,
                Err(error) => {
                    app.disconnect_account_authorizations().await;
                    if previous_failure.as_ref() != Some(&error.message) {
                        eprintln!("Node account synchronization: {error}");
                        previous_failure = Some(error.message);
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
}
