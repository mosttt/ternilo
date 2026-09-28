use super::*;
use std::{sync::mpsc, thread, time::Duration};

fn spinning_extension(fuel: u64) -> (tempfile::TempDir, Arc<ExtensionRegistry>) {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[46; 32]);
    let mut policy = ExtensionHostPolicy::default();
    policy.maximum_wasm_component_limits.fuel = fuel;
    let registry = ExtensionRegistry::open(directory.path().join("extensions"), policy).unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = wasm_manifest();
    if let ExtensionRuntime::WasmComponent { limits, .. } = &mut manifest.runtime {
        limits.fuel = fuel;
    }
    let component = WASM_COMPONENT.replacen(
        "          (local.get $handler-ptr)",
        r"          (local.get $handler-ptr)
          (i32.load8_u)
          (i32.const 115)
          (i32.eq)
          (if (then (loop $spin (br $spin))))
          (local.get $handler-ptr)",
        1,
    );
    registry
        .install(
            ExtensionInstallRequest {
                bundle: sign_bundle(
                    manifest,
                    ExtensionPayload::Base64(STANDARD.encode(component)),
                    &key,
                )
                .unwrap(),
                granted_capabilities: BTreeSet::new(),
            },
            2,
        )
        .unwrap();
    (directory, registry)
}

fn invoke(
    extension: &ResolvedExtension,
    handler: &str,
    cancellation: RunCancellation,
) -> Result<ternilo_protocol::ToolOutput, HarnessError> {
    crate::runtime::invoke_extension(
        &extension.installed,
        &extension.compiled,
        handler,
        &object_schema(),
        &ToolExecutionContext {
            cancellation,
            ..execution_context(handler)
        },
        json!({}),
        json!({}),
    )
}

#[test]
fn cancelling_one_wasm_invocation_stops_its_guest_without_stopping_other_stores() {
    let (_directory, registry) = spinning_extension(10_000_000_000);
    let extension = registry
        .resolve("dev.ternilo.wasm-fixture", "1.0.0")
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let start = |label: &'static str| {
        let cancellation = RunCancellation::new();
        let signal = cancellation.clone();
        let extension = extension.clone();
        let barrier = barrier.clone();
        let (sender, result) = mpsc::channel();
        let thread = thread::spawn(move || {
            barrier.wait();
            sender.send(invoke(&extension, label, signal)).unwrap();
        });
        (cancellation, result, thread)
    };
    let (first, first_result, first_thread) = start("spin-first");
    let (second, second_result, second_thread) = start("spin-second");
    barrier.wait();
    assert!(matches!(
        first_result.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    first.cancel();
    let error = first_result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
    first_thread.join().unwrap();
    assert!(matches!(
        second_result.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    second.cancel();
    let error = second_result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
    second_thread.join().unwrap();
    let output = invoke(&extension, "alpha", RunCancellation::new()).unwrap();
    assert_eq!(output.content, "alpha");
}

#[test]
fn wasm_fuel_limits_and_pre_cancelled_inputs_still_apply() {
    let (_directory, registry) = spinning_extension(10_000);
    let extension = registry
        .resolve("dev.ternilo.wasm-fixture", "1.0.0")
        .unwrap();
    let error = invoke(&extension, "spin", RunCancellation::new()).unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Execution);
    let cancellation = RunCancellation::new();
    cancellation.cancel();
    let error = invoke(&extension, "alpha", cancellation).unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
}
