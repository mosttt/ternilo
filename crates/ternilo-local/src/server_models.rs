use linorun_core::CallContext;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
};
use ternilo_kernel::{ModelGatewayProvider, ModelOutput, RunCancellation};
use ternilo_protocol::{HarnessError, ModelRequest, ModelResponse, RunModelBinding};

#[derive(Debug)]
pub struct ModelInputOrigin {
    pub session_id: ternilo_protocol::SessionId,
    pub provenance: Option<ternilo_protocol::InputProvenance>,
    pub schedule_origins: Vec<ternilo_protocol::ScheduleModelOrigin>,
}

pub trait ServerModelGateway: Send + Sync {
    fn complete<'a>(
        &'a self,
        session_id: &'a str,
        binding: RunModelBinding,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>>;
}

#[derive(Default)]
pub(crate) struct ServerModelGateways(RwLock<Option<Arc<dyn ServerModelGateway>>>);

impl ServerModelGateways {
    pub(crate) fn installed(&self) -> bool {
        self.0.read().expect("Server model gateway lock").is_some()
    }
    pub(crate) fn install(&self, gateway: Arc<dyn ServerModelGateway>) {
        *self.0.write().expect("Server model gateway lock") = Some(gateway);
    }
    pub(crate) fn for_session(
        self: &Arc<Self>,
        session_id: String,
    ) -> Arc<dyn ModelGatewayProvider> {
        Arc::new(SessionGateway {
            gateways: Arc::clone(self),
            session_id,
        })
    }
}

struct SessionGateway {
    gateways: Arc<ServerModelGateways>,
    session_id: String,
}

impl ModelGatewayProvider for SessionGateway {
    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        binding: RunModelBinding,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let gateway = self
                .gateways
                .0
                .read()
                .expect("Server model gateway lock")
                .clone()
                .ok_or_else(|| {
                    HarnessError::unavailable(
                        "Connect this computer to its Server to use an account Provider",
                    )
                })?;
            gateway
                .complete(&self.session_id, binding, request, output, cancellation)
                .await
        })
    }
}
