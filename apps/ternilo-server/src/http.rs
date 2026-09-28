use std::{future::Future, net::SocketAddr, time::Duration};

use salvo_core::{
    conn::tcp::TcpAcceptor,
    prelude::{Router, Server},
};
use ternilo_protocol::HarnessError;

pub(crate) async fn serve(
    listen: SocketAddr,
    router: Router,
    mode: &str,
    on_shutdown: impl Future<Output = ()>,
) -> Result<(), HarnessError> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| HarnessError::execution(format!("bind {listen}: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| HarnessError::execution(format!("read server address: {error}")))?;
    println!("Ternilo server ({mode}) listening on http://{address}");
    println!("Use an external HTTPS gateway for public access and preserve WebSocket upgrades.");
    let acceptor = TcpAcceptor::try_from(listener)
        .map_err(|error| HarnessError::execution(format!("create server acceptor: {error}")))?;
    let server = Server::new(acceptor);
    let handle = server.handle();
    let serving = server.try_serve(router);
    tokio::pin!(serving);
    let result = tokio::select! {
        result = &mut serving => result,
        () = shutdown_signal() => {
            handle.stop_graceful(Some(Duration::from_secs(10)));
            on_shutdown.await;
            serving.await
        }
    };
    result.map_err(|error| HarnessError::execution(format!("{mode} server: {error}")))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.expect("install Ctrl-C handler"),
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("install Ctrl-C handler");
}
