use std::net::IpAddr;

use salvo_core::Request;
use ternilo_control::ControlUser;

use crate::platform::{
    http::{ApiError, now_ms},
    state::AppState,
};

pub(crate) async fn record(
    state: &AppState,
    request: &Request,
    actor: &ControlUser,
    token: &str,
) -> Result<(), ApiError> {
    let ip = request_ip(state, request);
    let agent = request
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok());
    state
        .store
        .record_browser_session_activity(actor, token, agent, ip, now_ms()?)
        .await?;
    Ok(())
}

pub(super) fn request_ip(state: &AppState, request: &Request) -> Option<IpAddr> {
    client_ip(
        request.remote_addr().ip(),
        request
            .headers()
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
        &state.security.trusted_proxy_ips,
    )
}

fn client_ip(peer: Option<IpAddr>, forwarded: Option<&str>, trusted: &[IpAddr]) -> Option<IpAddr> {
    let peer = peer?;
    if !trusted.contains(&peer) {
        return Some(peer);
    }
    let Some(chain) = forwarded.and_then(|value| {
        value
            .split(',')
            .map(|item| item.trim().parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>()
            .ok()
    }) else {
        return Some(peer);
    };
    let mut address = peer;
    for previous in chain.into_iter().rev() {
        if !trusted.contains(&address) {
            break;
        }
        address = previous;
    }
    Some(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_addresses_require_a_trusted_peer_and_stop_at_the_first_untrusted_hop() {
        let proxy = "127.0.0.1".parse().unwrap();
        let client = "192.0.2.15".parse().unwrap();
        assert_eq!(
            client_ip(Some(client), Some("198.51.100.5"), &[proxy]),
            Some(client)
        );
        assert_eq!(
            client_ip(Some(proxy), Some("198.51.100.5, 192.0.2.15"), &[proxy]),
            Some(client)
        );
        assert_eq!(
            client_ip(Some(proxy), Some("not-an-ip"), &[proxy]),
            Some(proxy)
        );
        assert_eq!(client_ip(None, Some("192.0.2.15"), &[proxy]), None);
        assert_eq!(
            client_ip(Some(proxy), Some("2001:db8::1, 127.0.0.1"), &[proxy]),
            Some("2001:db8::1".parse().unwrap())
        );
    }
}
