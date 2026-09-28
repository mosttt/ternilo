#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};

#[test]
fn sigterm_runs_local_web_graceful_shutdown() {
    let data_dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ternilo"))
        .args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            data_dir.path().to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ready = String::new();
    stdout.read_line(&mut ready).unwrap();
    assert!(
        ready.contains("Ternilo local web:"),
        "unexpected local web startup output: {ready:?}"
    );

    thread::sleep(Duration::from_millis(50));
    kill(
        Pid::from_raw(child.id().try_into().unwrap()),
        Signal::SIGTERM,
    )
    .unwrap();

    let status = wait_for_exit(&mut child).unwrap_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        panic!("local web did not exit after SIGTERM")
    });
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "local web exited with {status}: {stderr}");
}

fn wait_for_exit(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}
