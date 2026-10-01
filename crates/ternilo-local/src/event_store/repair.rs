use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use ternilo_protocol::{HarnessError, SessionEvent, SessionId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLogRepairAction {
    None,
    AppendNewline,
    DiscardIncompleteTail,
}

#[derive(Debug, Serialize)]
pub struct SessionLogRepairReport {
    pub session_id: SessionId,
    pub action: SessionLogRepairAction,
    pub applied: bool,
    pub valid_records: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub backup_path: Option<PathBuf>,
}

/// Inspect or repair one local transcript while exclusively owning the data directory.
pub async fn repair_session_log(
    data_dir: &Path,
    session_id: &SessionId,
    apply: bool,
) -> Result<SessionLogRepairReport, HarnessError> {
    session_id.validate()?;
    let path = data_dir
        .join("data/sessions")
        .join(format!("{}.jsonl", super::encode_id(session_id.as_str())));
    if !path.is_file() {
        return Err(HarnessError::invalid("session log does not exist"));
    }
    let guard = crate::application::acquire_data_lock(data_dir)?;
    let session_id = session_id.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        repair(&path, session_id, apply)
    })
    .await
    .map_err(|error| HarnessError::execution(format!("session log maintenance task: {error}")))?
}

fn repair(
    path: &Path,
    session_id: SessionId,
    apply: bool,
) -> Result<SessionLogRepairReport, HarnessError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(apply)
        .open(path)
        .map_err(io_error)?;
    let mut report = inspect(&file, session_id)?;
    if !apply || report.action == SessionLogRepairAction::None {
        return Ok(report);
    }
    let backup_path =
        path.with_extension(format!("jsonl.repair-{:032x}.bak", rand::random::<u128>()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut backup = options.open(&backup_path).map_err(io_error)?;
    file.rewind().map_err(io_error)?;
    std::io::copy(&mut file, &mut backup).map_err(io_error)?;
    backup.sync_all().map_err(io_error)?;
    // Make the backup durable before changing the only canonical transcript.
    #[cfg(unix)]
    File::open(path.parent().expect("session log has a parent"))
        .and_then(|parent| parent.sync_all())
        .map_err(io_error)?;
    match report.action {
        SessionLogRepairAction::DiscardIncompleteTail => {
            file.set_len(report.bytes_after).map_err(io_error)?;
        }
        SessionLogRepairAction::AppendNewline => {
            file.seek(SeekFrom::End(0)).map_err(io_error)?;
            file.write_all(b"\n").map_err(io_error)?;
        }
        SessionLogRepairAction::None => unreachable!(),
    }
    file.sync_all().map_err(io_error)?;
    report.applied = true;
    report.backup_path = Some(backup_path);
    Ok(report)
}

fn inspect(file: &File, session_id: SessionId) -> Result<SessionLogRepairReport, HarnessError> {
    let bytes_before = file.metadata().map_err(io_error)?.len();
    let mut report = SessionLogRepairReport {
        session_id,
        action: SessionLogRepairAction::None,
        applied: false,
        valid_records: 0,
        bytes_before,
        bytes_after: bytes_before,
        backup_path: None,
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut offset = 0;
    loop {
        line.clear();
        let length = reader.read_until(b'\n', &mut line).map_err(io_error)?;
        if length == 0 {
            return Ok(report);
        }
        let terminated = line.last() == Some(&b'\n');
        match serde_json::from_slice::<SessionEvent>(&line) {
            Ok(event) => {
                if event.seq != report.valid_records {
                    return Err(HarnessError::invalid(format!(
                        "session log sequence mismatch at record {}: found {}; no repair was applied",
                        report.valid_records, event.seq
                    )));
                }
                report.valid_records += 1;
                if !terminated {
                    report.action = SessionLogRepairAction::AppendNewline;
                    report.bytes_after = bytes_before
                        .checked_add(1)
                        .ok_or_else(|| HarnessError::invalid("session log is too large"))?;
                    return Ok(report);
                }
            }
            Err(error) if !terminated && error.is_eof() => {
                report.action = SessionLogRepairAction::DiscardIncompleteTail;
                report.bytes_after = offset;
                return Ok(report);
            }
            Err(_) => {
                return Err(HarnessError::invalid(format!(
                    "session log contains a damaged complete record at byte {offset}; no repair was applied"
                )));
            }
        }
        offset += length as u64;
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used directly as a map_err adapter."
)]
fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::execution(format!("session log maintenance: {error}"))
}

#[cfg(test)]
mod tests;
