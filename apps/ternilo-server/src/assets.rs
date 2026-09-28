use salvo_core::{
    http::header,
    prelude::{Response, Router, Text, handler},
};

const INDEX_HTML: &str = include_str!("../../../web/dist/index.html");
const APP_CSS: &str = include_str!("../../../web/dist/assets/app.css");
const APP_JS: &str = include_str!("../../../web/dist/assets/app.js");
const WEB_MANIFEST: &str = include_str!("../../../web/public/manifest.webmanifest");
const SERVICE_WORKER_JS: &str = include_str!("../../../web/service-worker.js");
const PWA_VERSION: &str = include_str!("../../../web/dist/pwa-version.txt");
const ICON_SVG: &str = include_str!("../../../web/public/assets/icon.svg");
const ICON_192_PNG: &[u8] = include_bytes!("../../../web/public/assets/icon-192.png");
const ICON_512_PNG: &[u8] = include_bytes!("../../../web/public/assets/icon-512.png");
const ICON_MASKABLE_512_PNG: &[u8] =
    include_bytes!("../../../web/public/assets/icon-maskable-512.png");
const OFFLINE_BOOT_JS: &str = "window.__TERNILO_BOOT__ = { offline: true };";

pub(crate) fn router() -> Router {
    Router::new()
        .get(index)
        .push(Router::with_path("offline.html").get(offline_shell))
        .push(Router::with_path("manifest.webmanifest").get(web_manifest))
        .push(Router::with_path("service-worker.js").get(service_worker))
        .push(Router::with_path("assets/offline-boot.js").get(offline_boot))
        .push(Router::with_path("assets/app.css").get(css))
        .push(Router::with_path("assets/app.js").get(javascript))
        .push(Router::with_path("assets/icon.svg").get(icon))
        .push(Router::with_path("assets/icon-192.png").get(icon_192))
        .push(Router::with_path("assets/icon-512.png").get(icon_512))
        .push(Router::with_path("assets/icon-maskable-512.png").get(icon_maskable_512))
}

#[handler]
pub(crate) fn index(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("valid header"),
    );
    response.render(Text::Html(INDEX_HTML.replace(
        "__TERNILO_BOOT_TAG__",
        "<script src=\"/assets/boot.js\"></script>",
    )));
}

#[handler]
fn offline_shell(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("valid header"),
    );
    response.render(Text::Html(INDEX_HTML.replace(
        "__TERNILO_BOOT_TAG__",
        "<script src=\"/assets/offline-boot.js\"></script>",
    )));
}

#[handler]
fn offline_boot(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("valid header"),
    );
    response.render(Text::Js(OFFLINE_BOOT_JS));
}

#[handler]
fn css() -> Text<&'static str> {
    Text::Css(APP_CSS)
}

#[handler]
fn javascript() -> Text<&'static str> {
    Text::Js(APP_JS)
}

#[handler]
fn web_manifest(response: &mut Response) {
    response.render(Text::Plain(WEB_MANIFEST));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/manifest+json; charset=utf-8"
            .parse()
            .expect("valid header"),
    );
}

#[handler]
fn service_worker(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("valid header"),
    );
    response.render(Text::Js(
        SERVICE_WORKER_JS.replace("__TERNILO_ASSET_VERSION__", PWA_VERSION.trim()),
    ));
    response.headers_mut().insert(
        header::HeaderName::from_static("service-worker-allowed"),
        "/".parse().expect("valid header"),
    );
}

#[handler]
fn icon(response: &mut Response) {
    response.render(Text::Plain(ICON_SVG));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "image/svg+xml; charset=utf-8"
            .parse()
            .expect("valid header"),
    );
}

fn render_png(bytes: &'static [u8], response: &mut Response) {
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "image/png".parse().expect("valid header"),
    );
    response.write_body(bytes).expect("write embedded PNG");
}

#[handler]
fn icon_192(response: &mut Response) {
    render_png(ICON_192_PNG, response);
}

#[handler]
fn icon_512(response: &mut Response) {
    render_png(ICON_512_PNG, response);
}

#[handler]
fn icon_maskable_512(response: &mut Response) {
    render_png(ICON_MASKABLE_512_PNG, response);
}

#[cfg(test)]
mod tests {
    use super::*;
    use salvo_core::{
        http::StatusCode,
        routing::PathState,
        test::{ResponseExt, TestClient},
    };

    #[tokio::test]
    async fn shared_shell_preserves_offline_boot_and_service_worker_contracts() {
        let mut index_response = TestClient::get("http://server.test/").send(router()).await;
        assert_eq!(index_response.status_code, Some(StatusCode::OK));
        assert_eq!(index_response.headers()[header::CACHE_CONTROL], "no-store");
        let html = index_response.take_string().await.unwrap();
        assert!(html.contains("/assets/boot.js"));
        assert!(!html.contains("__TERNILO_BOOT_TAG__"));

        let mut offline = TestClient::get("http://server.test/offline.html")
            .send(router())
            .await;
        assert_eq!(offline.headers()[header::CACHE_CONTROL], "no-cache");
        assert!(
            offline
                .take_string()
                .await
                .unwrap()
                .contains("/assets/offline-boot.js")
        );

        let mut worker = TestClient::get("http://server.test/service-worker.js")
            .send(router())
            .await;
        assert_eq!(worker.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(worker.headers()["service-worker-allowed"], "/");
        assert!(
            !worker
                .take_string()
                .await
                .unwrap()
                .contains("__TERNILO_ASSET_VERSION__")
        );
    }

    #[tokio::test]
    async fn shared_assets_do_not_capture_mode_specific_routes() {
        for url in [
            "http://server.test/api/v1/state",
            "http://server.test/assets/boot.js",
            "http://server.test/auth/callback",
            "http://server.test/executor/connect",
        ] {
            let mut request = TestClient::get(url).build();
            let mut path = PathState::from_owned_path(request.uri().path().to_owned());
            assert!(router().detect(&mut request, &mut path).await.is_none());
        }
    }
}
