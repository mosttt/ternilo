use super::{
    Arc, Duration, EXECUTOR_PROTOCOL_VERSION, ExecutorId, HarnessError, LocalApplication, now_ms,
    server_binding,
};
use ternilo_transport::{
    ClientUpdateCommand, MAINTENANCE_POLL_MS, MAINTENANCE_PROTOCOL_VERSION, MaintenanceCommand,
    MaintenanceHello, MaintenanceReply, MaintenanceRequest, MaintenanceResponse,
};

pub(super) async fn run(
    gateway: &str,
    token: &str,
    executor: &ExecutorId,
    nonce: &str,
    updater: Arc<crate::updates::RemoteUpdater>,
    application: Arc<LocalApplication>,
    gate: tokio::sync::watch::Sender<bool>,
) -> Result<(), HarnessError> {
    let binding = server_binding(gateway, executor.as_str())?;
    let endpoint = format!(
        "{}/api/v1/executors/maintenance/v1",
        binding.server_url.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| HarnessError::execution("create maintenance client"))?;
    let cleanup = super::cleanup::CleanupClient::new(gateway, token, executor)?;
    let mut introduced = false;
    let mut reply = None;
    let mut retry = Duration::from_secs(1);
    loop {
        let info = updater.info().await?;
        let body = if introduced {
            MaintenanceRequest::Poll {
                instance_nonce: nonce.to_owned(),
                info,
                reply: reply.clone(),
            }
        } else {
            MaintenanceRequest::Hello {
                hello: MaintenanceHello {
                    protocol_version: MAINTENANCE_PROTOCOL_VERSION,
                    executor_id: executor.clone(),
                    instance_nonce: nonce.to_owned(),
                    execution_protocol_version: EXECUTOR_PROTOCOL_VERSION,
                    info,
                },
            }
        };
        let result = async {
            // Bind fresh credentials to this data directory before any authenticated use.
            if !introduced && !cleanup.synchronize(&application).await? {
                return Err(HarnessError::unavailable(
                    "Node credential is restricted to the account cleanup channel",
                ));
            }
            exchange(&client, &endpoint, token, &body).await
        }
        .await;
        match result {
            Ok(response) => {
                response.scope.validate()?;
                if introduced {
                    reply = None;
                } else {
                    application.set_directory_account_owner(response.scope.user_id.clone())?;
                    crate::service::LocalServiceConfig::persist_connection(
                        application.data_dir(),
                        gateway,
                        token,
                        executor.as_str(),
                    )
                    .await?;
                    updater.connected(&response.scope).await;
                    println!(
                        "Ternilo maintenance connected to Server {}",
                        response.server_version
                    );
                    if response.execution_protocol_version != EXECUTOR_PROTOCOL_VERSION {
                        eprintln!(
                            "Execution is unavailable: client protocol {EXECUTOR_PROTOCOL_VERSION}, Server protocol {}. Remote updates remain available; update this computer to the Server's matching release.",
                            response.execution_protocol_version
                        );
                    }
                }
                introduced = true;
                retry = Duration::from_secs(1);
                gate.send_replace(response.execution_protocol_version == EXECUTOR_PROTOCOL_VERSION);
                if let Some(command) = response.command {
                    reply = Some(execute(&updater, command).await?);
                    continue;
                }
                tokio::select! { ()=updater.changed()=>{}, ()=tokio::time::sleep(Duration::from_millis(response.poll_after_ms.clamp(500,MAINTENANCE_POLL_MS)))=>{} }
            }
            Err(error) => {
                gate.send_replace(false);
                updater.disconnected();
                introduced = false;
                eprintln!("maintenance connection failed: {error}");
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(Duration::from_secs(30));
            }
        }
    }
}
async fn exchange(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    body: &MaintenanceRequest,
) -> Result<MaintenanceResponse, HarnessError> {
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|_| HarnessError::unavailable("cannot reach the Server maintenance channel"))?;
    if !response.status().is_success() {
        return Err(HarnessError::unavailable(format!(
            "Server maintenance channel returned HTTP {}; check the computer credential and update Server and Ternilo if maintenance is not supported",
            response.status().as_u16()
        )));
    }
    let response: MaintenanceResponse = response
        .json()
        .await
        .map_err(|_| HarnessError::invalid("invalid Server maintenance response"))?;
    if response.protocol_version != MAINTENANCE_PROTOCOL_VERSION {
        return Err(HarnessError::invalid(
            "maintenance protocol mismatch; manually update Server and Ternilo",
        ));
    }
    Ok(response)
}

async fn execute(
    updater: &Arc<crate::updates::RemoteUpdater>,
    command: MaintenanceCommand,
) -> Result<MaintenanceReply, HarnessError> {
    let result = if command.expires_at_ms <= now_ms()? {
        Err(HarnessError::unavailable("maintenance command expired"))
    } else {
        match command.command {
            ClientUpdateCommand::Status => updater.info().await,
            ClientUpdateCommand::Start { request } => updater.start(request).await,
            ClientUpdateCommand::Cancel { id } => updater.cancel(&id).await,
        }
    };
    Ok(match result {
        Ok(info) => MaintenanceReply {
            id: command.id,
            info: Some(info),
            error: None,
        },
        Err(error) => MaintenanceReply {
            id: command.id,
            info: None,
            error: Some(error.to_string()),
        },
    })
}
