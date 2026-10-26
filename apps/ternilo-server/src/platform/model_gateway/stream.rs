use std::time::Duration;

use salvo_core::{http::header, prelude::Response};
use ternilo_protocol::ModelGatewayFrame;
use tokio::sync::mpsc;

use crate::platform::http::{ApiError, invalid_request};

pub(in crate::platform) fn stream_frames(
    response: &mut Response,
    request_id: &str,
    receiver: mpsc::Receiver<ModelGatewayFrame>,
) -> Result<(), ApiError> {
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-ndjson".parse().expect("static MIME type"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static cache policy"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        "no".parse().expect("static proxy policy"),
    );
    response
        .headers_mut()
        .insert("x-request-id", request_id.parse().map_err(invalid_request)?);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    response.stream(futures_util::stream::unfold(
        (receiver, heartbeat),
        |(mut receiver, mut heartbeat)| async move {
            let data = tokio::select! {
                biased;
                frame = receiver.recv() => serde_json::to_vec(&frame?).map(|mut bytes| {
                    bytes.push(b'\n');
                    bytes
                }),
                // NDJSON whitespace keeps the transport alive without producing model output.
                _ = heartbeat.tick() => Ok(vec![b'\n']),
            };
            Some((data, (receiver, heartbeat)))
        },
    ));
    Ok(())
}
