use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use ternilo_protocol::{HarnessError, SessionEvent, SessionEventPage, SessionHistoryQuery};

pub(super) async fn load(
    path: PathBuf,
    query: SessionHistoryQuery,
) -> Result<SessionEventPage, HarnessError> {
    query.validate()?;
    tokio::task::spawn_blocking(move || read(&path, query))
        .await
        .map_err(|error| HarnessError::execution(format!("read history page task: {error}")))?
}

fn read(path: &Path, query: SessionHistoryQuery) -> Result<SessionEventPage, HarnessError> {
    if query.before_seq == Some(0) {
        return Ok(SessionEventPage::new(Vec::new()));
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SessionEventPage::new(Vec::new()));
        }
        Err(error) => return Err(io_error(&error)),
    };
    // Capture the file length once. Appends after this snapshot are delivered by Live.
    let mut position = file.metadata().map_err(|error| io_error(&error))?.len();
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut reversed_line = Vec::new();
    let mut events = Vec::new();
    let mut newer_seq = None;
    let mut first_byte = true;
    while position > 0 {
        let size = usize::try_from(position.min(chunk.len() as u64)).unwrap();
        position -= size as u64;
        file.seek(SeekFrom::Start(position))
            .map_err(|error| io_error(&error))?;
        file.read_exact(&mut chunk[..size])
            .map_err(|error| io_error(&error))?;
        for byte in chunk[..size].iter().rev().copied() {
            if first_byte {
                first_byte = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\n' {
                read_record(&mut reversed_line, &mut newer_seq, query, &mut events)?;
                if events.len() == query.limit as usize {
                    events.reverse();
                    return Ok(SessionEventPage::new(events));
                }
            } else {
                reversed_line.push(byte);
            }
        }
    }
    if !first_byte {
        read_record(&mut reversed_line, &mut newer_seq, query, &mut events)?;
        if newer_seq != Some(0) {
            return Err(HarnessError::execution(
                "session history does not start at sequence 0",
            ));
        }
    }
    events.reverse();
    Ok(SessionEventPage::new(events))
}

fn read_record(
    line: &mut Vec<u8>,
    newer_seq: &mut Option<u64>,
    query: SessionHistoryQuery,
    events: &mut Vec<SessionEvent>,
) -> Result<(), HarnessError> {
    line.reverse();
    let event: SessionEvent = serde_json::from_slice(line)
        .map_err(|error| HarnessError::execution(format!("parse session history page: {error}")))?;
    line.clear();
    if newer_seq.is_some_and(|newer| event.seq.checked_add(1) != Some(newer)) {
        return Err(HarnessError::execution(
            "session history has a non-contiguous sequence",
        ));
    }
    *newer_seq = Some(event.seq);
    if query.before_seq.is_none_or(|before| event.seq < before) {
        events.push(event);
    }
    Ok(())
}

fn io_error(error: &std::io::Error) -> HarnessError {
    HarnessError::execution(format!("read session history page: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{RunId, SessionEventKind};

    #[tokio::test]
    async fn backwards_pages_preserve_large_records_and_stable_cursors_during_appends() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("events.jsonl");
        let events: Vec<_> = (0..407)
            .map(|seq| SessionEvent {
                seq,
                occurred_at_ms: seq,
                run_id: RunId::new("history"),
                kind: SessionEventKind::AssistantReasoningDelta {
                    step: 1,
                    delta: if seq == 205 {
                        "分页🙂".repeat(20_000)
                    } else {
                        format!("reason-{seq}")
                    },
                },
            })
            .collect();
        let bytes = events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for ending in ["", "\n", "\r\n"] {
            std::fs::write(&file, format!("{bytes}{ending}")).unwrap();
            let latest = load(file.clone(), SessionHistoryQuery::default())
                .await
                .unwrap();
            assert_eq!(latest.events, events[207..]);
            assert_eq!(latest.next_before_seq, Some(207));
            let appended = SessionEvent {
                seq: 407,
                ..events[0].clone()
            };
            std::fs::write(
                &file,
                format!("{bytes}\n{}\n", serde_json::to_string(&appended).unwrap()),
            )
            .unwrap();
            let older = load(
                file.clone(),
                SessionHistoryQuery {
                    before_seq: latest.next_before_seq,
                    limit: 200,
                },
            )
            .await
            .unwrap();
            assert_eq!(older.events, events[7..207]);
            assert_eq!(older.next_before_seq, Some(7));
            let first = load(
                file.clone(),
                SessionHistoryQuery {
                    before_seq: older.next_before_seq,
                    limit: 200,
                },
            )
            .await
            .unwrap();
            assert_eq!(first.events, events[..7]);
            assert_eq!(first.next_before_seq, None);
            assert!(
                load(
                    file.clone(),
                    SessionHistoryQuery {
                        before_seq: Some(0),
                        limit: 200
                    }
                )
                .await
                .unwrap()
                .events
                .is_empty()
            );
        }
        std::fs::write(&file, format!("{bytes}\n\n")).unwrap();
        assert!(
            load(file.clone(), SessionHistoryQuery::default())
                .await
                .is_err()
        );
        let broken = SessionEvent {
            seq: 500,
            ..events[0].clone()
        };
        std::fs::write(
            &file,
            format!("{bytes}\n{}\n", serde_json::to_string(&broken).unwrap()),
        )
        .unwrap();
        assert!(load(file, SessionHistoryQuery::default()).await.is_err());
    }
}
