use super::{Depot, Response, Text, app_state, handler, header, json};

const INDEX_HTML: &str = include_str!("../../../../web/dist/index.html");
const APP_CSS: &str = include_str!("../../../../web/dist/assets/app.css");
const APP_JS: &str = include_str!("../../../../web/dist/assets/app.js");
const WEB_MANIFEST: &str = include_str!("../../../../web/public/manifest.webmanifest");
const SERVICE_WORKER_JS: &str = include_str!("../../../../web/service-worker.js");
const PWA_VERSION: &str = include_str!("../../../../web/dist/pwa-version.txt");
const ICON_SVG: &str = include_str!("../../../../web/public/assets/icon.svg");
const ICON_192_PNG: &[u8] = include_bytes!("../../../../web/public/assets/icon-192.png");
const ICON_512_PNG: &[u8] = include_bytes!("../../../../web/public/assets/icon-512.png");
const ICON_MASKABLE_512_PNG: &[u8] =
    include_bytes!("../../../../web/public/assets/icon-maskable-512.png");
const OFFLINE_BOOT_JS: &str = "window.__TERNILO_BOOT__ = { offline: true };";

#[handler]
pub(super) fn index(depot: &mut Depot, response: &mut Response) {
    let state = app_state(depot);
    let home = ternilo_local::home_directory()
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let boot = json!({
        "apiToken": state.shared.api_token,
        "remote": false,
        "home": home,
        "openConfig": true
    });
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("valid header"),
    );
    response.render(Text::Html(INDEX_HTML.replace(
        "__TERNILO_BOOT_TAG__",
        &format!("<script>window.__TERNILO_BOOT__ = {boot};</script>"),
    )));
}

#[handler]
pub(super) fn offline_shell(response: &mut Response) {
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
pub(super) fn offline_boot(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("valid header"),
    );
    response.render(Text::Js(OFFLINE_BOOT_JS));
}

#[handler]
pub(super) fn css() -> Text<&'static str> {
    Text::Css(APP_CSS)
}

#[handler]
pub(super) fn javascript() -> Text<&'static str> {
    Text::Js(APP_JS)
}

#[handler]
pub(super) fn web_manifest(response: &mut Response) {
    response.render(Text::Plain(WEB_MANIFEST));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/manifest+json; charset=utf-8"
            .parse()
            .expect("valid header"),
    );
}

#[handler]
pub(super) fn service_worker(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("valid header"),
    );
    response.render(Text::Js(
        SERVICE_WORKER_JS.replace("__TERNILO_ASSET_VERSION__", PWA_VERSION.trim()),
    ));
}

#[handler]
pub(super) fn icon(response: &mut Response) {
    response.render(Text::Plain(ICON_SVG));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "image/svg+xml; charset=utf-8"
            .parse()
            .expect("valid header"),
    );
}

pub(super) fn render_png(bytes: &'static [u8], response: &mut Response) {
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "image/png".parse().expect("valid header"),
    );
    response.write_body(bytes).expect("write embedded PNG");
}

#[handler]
pub(super) fn icon_192(response: &mut Response) {
    render_png(ICON_192_PNG, response);
}

#[handler]
pub(super) fn icon_512(response: &mut Response) {
    render_png(ICON_512_PNG, response);
}

#[handler]
pub(super) fn icon_maskable_512(response: &mut Response) {
    render_png(ICON_MASKABLE_512_PNG, response);
}
