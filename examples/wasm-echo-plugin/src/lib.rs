wit_bindgen::generate!({
    path: "../../crates/ternilo-extension/wit",
    world: "runtime",
});

struct EchoPlugin;

impl Guest for EchoPlugin {
    fn invoke(
        handler: String,
        _context_json: String,
        input_json: String,
    ) -> Result<String, String> {
        let arguments: serde_json::Value =
            serde_json::from_str(&input_json).map_err(|error| error.to_string())?;
        let content = match handler.as_str() {
            "echo" => arguments
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("hello from wasm")
                .to_owned(),
            "read_workspace" => {
                let path = arguments
                    .get("path")
                    .and_then(|value| value.as_str())
                    .ok_or("read_workspace requires a path")?;
                ternilo::extension::host::log(
                    ternilo::extension::host::LogLevel::Debug,
                    "reading a workspace file",
                )?;
                ternilo::extension::host::read_workspace_text(path)?
            }
            _ => return Err(format!("unsupported tool handler {handler:?}")),
        };
        serde_json::to_string(&serde_json::json!({
            "content": content,
            "is_error": false
        }))
        .map_err(|error| error.to_string())
    }
}

export!(EchoPlugin);
