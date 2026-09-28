#![forbid(unsafe_code)]

#[cfg(windows)]
mod windows {
    use std::{
        collections::HashMap,
        ffi::{OsStr, OsString},
        io::{Read, Write},
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use clap::{Parser, ValueEnum};
    use zagens_windows_sandbox::{
        PlanInput, SpawnStdio, WindowsSandboxMode, plan_exec, read_handle_loop, spawn,
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    #[derive(Clone, Copy, Debug, ValueEnum)]
    enum Mode {
        ReadOnly,
        WorkspaceWrite,
    }

    #[derive(Debug, Parser)]
    #[command(name = "ternilo-sandbox-windows")]
    struct Cli {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long, value_enum)]
        mode: Mode,
        #[arg(last = true, required = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    }

    pub fn run() -> Result<i32> {
        let cli = Cli::parse();
        let workspace = cli.workspace.canonicalize()?;
        if !workspace.is_dir() {
            return Err(format!("workspace is not a directory: {}", workspace.display()).into());
        }
        let (program, arguments) = cli
            .command
            .split_first()
            .ok_or("sandbox command must not be empty")?;
        let mut plan = plan_exec(PlanInput {
            program: os_string(program),
            args: arguments.iter().map(|value| os_string(value)).collect(),
            cwd: workspace.clone(),
            env: child_environment(),
            writable_roots: match cli.mode {
                Mode::ReadOnly => Vec::new(),
                Mode::WorkspaceWrite => vec![workspace],
            },
            protected_write_paths: Vec::new(),
            network_allowed: true,
            mode: WindowsSandboxMode::Unelevated,
            private_desktop: false,
            tty: false,
        })?;
        plan.env.retain(|name, _| {
            let name = name.to_ascii_uppercase();
            !name.starts_with("DEEPSEEK_")
                && !name.starts_with("ZAGENS_")
                && !name.starts_with("TERNILO_")
        });

        let mut process = spawn(
            &plan,
            SpawnStdio {
                capture_stdout: true,
                capture_stderr: true,
                stdin_open: true,
                stdin_data: None,
            },
        )?;
        let (stdout_handle, stderr_handle) = process.detach_output_readers();
        let stdout = read_handle_loop(stdout_handle, |chunk| {
            let mut output = std::io::stdout().lock();
            let _ = output.write_all(chunk);
            let _ = output.flush();
        });
        let stderr = read_handle_loop(stderr_handle, |chunk| {
            let mut output = std::io::stderr().lock();
            let _ = output.write_all(chunk);
            let _ = output.flush();
        });

        let process = Arc::new(Mutex::new(process));
        let stdin_process = Arc::clone(&process);
        std::thread::spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut chunk = [0_u8; 8192];
            loop {
                let read = match input.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let Ok(mut process) = stdin_process.lock() else {
                    break;
                };
                if process.write_stdin(&chunk[..read]).is_err() {
                    break;
                }
            }
            if let Ok(mut process) = stdin_process.lock() {
                process.close_stdin();
            }
        });

        let exit_code = loop {
            let status = process
                .lock()
                .map_err(|_| "sandbox process lock poisoned")?
                .try_wait()?;
            if let Some(exit_code) = status {
                break exit_code;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if let Ok(mut process) = process.lock() {
            process.finalize_conpty_after_exit();
            process.close_stdin();
        }
        let _ = stdout.join();
        let _ = stderr.join();
        Ok(i32::try_from(exit_code).unwrap_or(1))
    }

    fn child_environment() -> HashMap<String, String> {
        std::env::vars_os()
            .filter_map(|(name, value)| {
                let name = name.into_string().ok()?;
                if sensitive_environment_name(OsStr::new(&name)) {
                    return None;
                }
                Some((name, value.into_string().ok()?))
            })
            .collect()
    }

    fn sensitive_environment_name(name: &OsStr) -> bool {
        let name = name.to_string_lossy().to_ascii_uppercase();
        name.starts_with("TERNILO_")
            || name.starts_with("ZAGENS_")
            || name.starts_with("DEEPSEEK_")
            || ["KEY", "SECRET", "TOKEN", "PASSWORD", "CREDENTIAL"]
                .iter()
                .any(|marker| name.contains(marker))
    }

    fn os_string(value: &OsStr) -> String {
        value.to_string_lossy().into_owned()
    }
}

#[cfg(windows)]
fn main() {
    match windows::run() {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(error) => {
            eprintln!("ternilo Windows sandbox failed: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ternilo-sandbox-windows is only available on Windows");
    std::process::exit(1);
}
