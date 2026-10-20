use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{Walk, WalkBuilder, overrides::OverrideBuilder};
use ternilo_protocol::{
    FileListRequest, FileListResult, FileSearchMatch, FileSearchRequest, FileSearchResult,
    HarnessError,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) async fn list_files(
    root: PathBuf,
    request: FileListRequest,
) -> Result<FileListResult, HarnessError> {
    if request.pattern.trim().is_empty() {
        return Err(HarnessError::invalid("file glob must not be empty"));
    }
    let walk = walker(&root, Some(&request.pattern))?;
    let limit = request.limit.clamp(1, 2_000) as usize;
    run(move |control| {
        let mut files = Vec::new();
        let mut diagnostics = Diagnostics::default();
        let mut bytes = 0;
        let mut truncated = false;
        for entry in walk {
            control
                .check()
                .map_err(|error| HarnessError::execution(error.to_string()))?;
            let Some(path) = file_entry(entry, &mut diagnostics) else {
                continue;
            };
            let path = relative_path(&root, &path);
            if files.len() == limit || bytes + path.len() > MAX_BYTES {
                truncated = true;
                break;
            }
            bytes += path.len();
            files.push(path);
        }
        Ok(FileListResult {
            files,
            warnings: diagnostics.messages,
            truncated,
        })
    })
    .await
}

pub(super) async fn search_text(
    root: PathBuf,
    request: FileSearchRequest,
) -> Result<FileSearchResult, HarnessError> {
    if request.pattern.trim().is_empty() {
        return Err(HarnessError::invalid("search pattern must not be empty"));
    }
    let matcher = RegexMatcher::new_line_matcher(&request.pattern)
        .map_err(|error| HarnessError::invalid(format!("invalid search pattern: {error}")))?;
    let walk = walker(&root, request.file_glob.as_deref())?;
    let limit = request.limit.clamp(1, 1_000) as usize;
    run(move |control| {
        let mut result = FileSearchResult {
            matches: Vec::new(),
            warnings: Vec::new(),
            truncated: false,
        };
        let mut diagnostics = Diagnostics::default();
        let mut bytes = 0;
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .heap_limit(Some(MAX_BYTES))
            .binary_detection(BinaryDetection::quit(b'\0'))
            .build();
        for entry in walk {
            control
                .check()
                .map_err(|error| HarnessError::execution(error.to_string()))?;
            let Some(path) = file_entry(entry, &mut diagnostics) else {
                continue;
            };
            let display = relative_path(&root, &path);
            let search = File::open(&path).and_then(|file| {
                searcher.search_reader(
                    &matcher,
                    CheckedReader {
                        file,
                        control: &control,
                    },
                    MatchSink {
                        path: &display,
                        matcher: &matcher,
                        result: &mut result,
                        bytes: &mut bytes,
                        limit,
                        control: &control,
                    },
                )
            });
            control
                .check()
                .map_err(|error| HarnessError::execution(error.to_string()))?;
            if let Err(error) = search {
                diagnostics.push(&format!("{display}: {error}"));
            }
            if result.truncated {
                break;
            }
        }
        result.warnings = diagnostics.messages;
        Ok(result)
    })
    .await
}

fn walker(root: &Path, glob: Option<&str>) -> Result<Walk, HarnessError> {
    let mut overrides = OverrideBuilder::new(root);
    if let Some(glob) = glob {
        overrides
            .add(glob)
            .map_err(|error| HarnessError::invalid(format!("invalid file glob: {error}")))?;
    }
    let overrides = overrides
        .build()
        .map_err(|error| HarnessError::invalid(format!("invalid file glob: {error}")))?;
    Ok(WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .overrides(overrides)
        .build())
}

fn file_entry(
    entry: Result<ignore::DirEntry, ignore::Error>,
    diagnostics: &mut Diagnostics,
) -> Option<PathBuf> {
    match entry {
        Ok(entry) => {
            if let Some(error) = entry.error() {
                diagnostics.push(&error.to_string());
            }
            entry
                .file_type()
                .filter(std::fs::FileType::is_file)
                .map(|_| entry.into_path())
        }
        Err(error) => {
            diagnostics.push(&error.to_string());
            None
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

#[derive(Default)]
struct Diagnostics {
    messages: Vec<String>,
}

impl Diagnostics {
    fn push(&mut self, message: &str) {
        if self.messages.len() < 20 {
            self.messages.push(message.chars().take(1_000).collect());
        } else if self.messages.len() == 20 {
            self.messages
                .push("Additional search diagnostics were omitted.".to_owned());
        }
    }
}

struct MatchSink<'a> {
    path: &'a str,
    matcher: &'a RegexMatcher,
    result: &'a mut FileSearchResult,
    bytes: &'a mut usize,
    limit: usize,
    control: &'a Control,
}

impl Sink for MatchSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, matched: &SinkMatch<'_>) -> io::Result<bool> {
        self.control.check()?;
        let preview = String::from_utf8_lossy(matched.bytes());
        let preview = preview.trim_end_matches(['\r', '\n']);
        let size = self.path.len() + preview.len();
        if self.result.matches.len() == self.limit || *self.bytes + size > MAX_BYTES {
            self.result.truncated = true;
            return Ok(false);
        }
        let first = self
            .matcher
            .find(matched.bytes())
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("search match has no position"))?;
        self.result.matches.push(FileSearchMatch {
            path: self.path.to_owned(),
            line: matched
                .line_number()
                .ok_or_else(|| io::Error::other("search match has no line number"))?,
            column: first.start() as u64 + 1,
            preview: preview.to_owned(),
        });
        *self.bytes += size;
        Ok(true)
    }
}

struct Control {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl Control {
    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "workspace search stopped or timed out after 10000 ms",
            ));
        }
        Ok(())
    }
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct CheckedReader<'a> {
    file: File,
    control: &'a Control,
}
impl Read for CheckedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.control.check()?;
        self.file.read(buf)
    }
}

async fn run<T: Send + 'static>(
    search: impl FnOnce(Control) -> Result<T, HarnessError> + Send + 'static,
) -> Result<T, HarnessError> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancelled.clone());
    let control = Control {
        cancelled,
        deadline: Instant::now() + TIMEOUT,
    };
    tokio::time::timeout(
        TIMEOUT,
        tokio::task::spawn_blocking(move || search(control)),
    )
    .await
    .map_err(|_| HarnessError::execution("workspace search timed out after 10000 ms"))?
    .map_err(|error| HarnessError::execution(format!("workspace search failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pattern: &str, file_glob: Option<&str>, limit: u32) -> FileSearchRequest {
        FileSearchRequest {
            pattern: pattern.into(),
            file_glob: file_glob.map(str::to_owned),
            limit,
        }
    }

    #[tokio::test]
    async fn native_search_preserves_globs_ignore_rules_hidden_files_and_positions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "needle\n").unwrap();
        std::fs::write(root.join(".hidden.txt"), "needle\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "first\r\n中文 needle needle\r\n").unwrap();
        std::fs::write(root.join("binary.dat"), b"\0needle\n").unwrap();
        let result = search_text(root.into(), query("needle", None, 100))
            .await
            .unwrap();
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert!(!result.truncated);
        assert_eq!(result.matches.len(), 2);
        let matched = result
            .matches
            .iter()
            .find(|m| m.path == "src/main.rs")
            .unwrap();
        assert_eq!(
            (matched.line, matched.column, matched.preview.as_str()),
            (2, 8, "中文 needle needle")
        );
        assert!(result.matches.iter().any(|m| m.path == ".hidden.txt"));
        let result = search_text(root.into(), query("needle", Some("ignored.txt"), 100))
            .await
            .unwrap();
        assert_eq!(result.matches[0].path, "ignored.txt");
        let files = list_files(
            root.into(),
            FileListRequest {
                pattern: "**/*.rs".into(),
                limit: 100,
            },
        )
        .await
        .unwrap();
        assert_eq!(files.files, vec!["src/main.rs"]);
        assert!(files.warnings.is_empty());
    }

    #[tokio::test]
    async fn native_search_handles_bom_no_matches_invalid_queries_and_limits() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let utf16 = [0xff, 0xfe]
            .into_iter()
            .chain(
                "你好 needle\r\nnext needle\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect::<Vec<_>>();
        std::fs::write(root.join("utf16.txt"), utf16).unwrap();
        let result = search_text(root.into(), query("needle", None, 1))
            .await
            .unwrap();
        assert_eq!(result.matches.len(), 1);
        assert!(result.truncated);
        assert_eq!(result.matches[0].preview, "你好 needle");
        let result = search_text(root.into(), query("needle", None, 2))
            .await
            .unwrap();
        assert_eq!(result.matches.len(), 2);
        assert!(!result.truncated);
        assert!(
            search_text(root.into(), query("absent", None, 10))
                .await
                .unwrap()
                .matches
                .is_empty()
        );
        for request in [
            query("[", None, 10),
            query("needle", Some("{broken"), 10),
            query(" ", None, 10),
        ] {
            assert!(search_text(root.into(), request).await.is_err());
        }
        assert!(
            list_files(
                root.into(),
                FileListRequest {
                    pattern: "{broken".into(),
                    limit: 10
                }
            )
            .await
            .is_err()
        );
        std::fs::write(root.join("second.txt"), "x").unwrap();
        let files = list_files(
            root.into(),
            FileListRequest {
                pattern: "*.txt".into(),
                limit: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(files.files.len(), 1);
        assert!(files.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_search_keeps_readable_files_and_does_not_follow_external_links() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("colon:line\nname.txt"), "needle\n").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "needle\n").unwrap();
        symlink(outside.path(), root.join("external")).unwrap();
        symlink(outside.path().join("secret.txt"), root.join("link.txt")).unwrap();
        let private = root.join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(private.join("hidden.txt"), "needle\n").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o0)).unwrap();
        let denied = std::fs::read_dir(&private).is_err();
        let matches = search_text(root.into(), query("needle", None, 10)).await;
        let files = list_files(
            root.into(),
            FileListRequest {
                pattern: "*.txt".into(),
                limit: 10,
            },
        )
        .await;
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let matches = matches.unwrap();
        let files = files.unwrap();
        assert!(
            matches
                .matches
                .iter()
                .any(|m| m.path == "colon:line\nname.txt")
        );
        assert!(files.files.iter().any(|p| p == "colon:line\nname.txt"));
        assert!(
            !matches
                .matches
                .iter()
                .any(|m| m.path.contains("external") || m.path == "link.txt")
        );
        assert!(
            !files
                .files
                .iter()
                .any(|p| p.contains("external") || p == "link.txt")
        );
        assert_eq!(!matches.warnings.is_empty(), denied);
        assert_eq!(!files.warnings.is_empty(), denied);
    }

    #[tokio::test]
    async fn native_search_bounds_long_lines_and_stops_cancelled_workers() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("long.txt"), "x".repeat(MAX_BYTES + 1)).unwrap();
        let result = search_text(directory.path().into(), query("x", None, 10))
            .await
            .unwrap();
        assert!(result.matches.is_empty());
        assert!(!result.warnings.is_empty());

        let (started, start) = tokio::sync::oneshot::channel();
        let (finished, finish) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(move |control| {
            let _ = started.send(());
            while control.check().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = finished.send(());
            Ok(())
        }));
        start.await.unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(2), finish)
            .await
            .unwrap()
            .unwrap();
    }
}
