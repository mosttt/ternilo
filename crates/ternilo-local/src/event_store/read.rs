use super::EventLogTail;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use ternilo_protocol::{HarnessError, SessionEvent};

pub(super) async fn load(path: PathBuf, start_seq: u64) -> Result<EventLogTail, HarnessError> {
    tokio::task::spawn_blocking(move || read(&path, start_seq))
        .await
        .map_err(|error| HarnessError::execution(format!("read session history task: {error}")))?
}

fn read(path: &Path, start_seq: u64) -> Result<EventLogTail, HarnessError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(EventLogTail {
                events: Vec::new(),
                total_records: 0,
            });
        }
        Err(error) => {
            return Err(HarnessError::execution(format!(
                "open session log {}: {error}",
                path.display()
            )));
        }
    };
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    let mut events = Vec::new();
    let mut total_records = 0_u64;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(|error| {
            HarnessError::execution(format!("read session log {}: {error}", path.display()))
        })? == 0
        {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(HarnessError::execution(format!(
                "session log {} contains an empty record",
                path.display()
            )));
        }
        let seq = total_records;
        total_records = total_records
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("session log sequence exceeds u64"))?;
        if seq < start_seq {
            continue;
        }
        let event: SessionEvent = serde_json::from_slice(&line).map_err(|error| {
            HarnessError::execution(format!("parse session log {}: {error}", path.display()))
        })?;
        if event.seq != seq {
            return Err(HarnessError::execution(format!(
                "session log {} sequence mismatch: expected {seq}, found {}",
                path.display(),
                event.seq
            )));
        }
        events.push(event);
    }
    Ok(EventLogTail {
        events,
        total_records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{RunId, SessionEventKind};

    #[tokio::test]
    async fn long_history_suffix_preserves_record_count_and_sequence_validation() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("events.jsonl");
        let events: Vec<_> = (0..20_000)
            .map(|seq| SessionEvent {
                seq,
                occurred_at_ms: seq,
                run_id: RunId::new("history"),
                kind: SessionEventKind::AssistantReasoningDelta {
                    step: 1,
                    delta: format!("reason-{seq}"),
                },
            })
            .collect();
        let bytes = events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(&file, &bytes).await.unwrap();
        let tail = load(file.clone(), 19_990).await.unwrap();
        assert_eq!(tail.total_records, 20_000);
        assert_eq!(tail.events, events[19_990..]);
        assert!(load(file.clone(), 20_000).await.unwrap().events.is_empty());
        let mut broken = events.last().unwrap().clone();
        broken.seq = 20_001;
        tokio::fs::write(
            &file,
            format!("{bytes}\n{}\n", serde_json::to_string(&broken).unwrap()),
        )
        .await
        .unwrap();
        assert!(load(file, 20_000).await.is_err());
    }
}
