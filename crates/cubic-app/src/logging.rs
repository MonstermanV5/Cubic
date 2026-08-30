use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

use time::OffsetDateTime;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::{format::Writer, time::FormatTime};

const RETAINED_LOGS: usize = 5;
const BUFFERED_LOG_LINES: usize = 8_192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogLevel {
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn configured() -> Self {
        std::env::var("CUBIC_LOG_LEVEL")
            .ok()
            .map_or(Self::Info, |value| Self::resolve(&value))
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "info" | "INFO" => Some(Self::Info),
            "debug" | "DEBUG" => Some(Self::Debug),
            "trace" | "TRACE" => Some(Self::Trace),
            _ => None,
        }
    }

    fn resolve(value: &str) -> Self {
        Self::parse(value).unwrap_or(Self::Info)
    }

    const fn filter(self) -> LevelFilter {
        match self {
            Self::Info => LevelFilter::INFO,
            Self::Debug => LevelFilter::DEBUG,
            Self::Trace => LevelFilter::TRACE,
        }
    }
}

#[derive(Debug)]
pub(crate) enum LoggingError {
    NoPlatformDataDirectory,
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Subscriber(String),
}

impl fmt::Display for LoggingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlatformDataDirectory => {
                formatter.write_str("platform data directory is unavailable")
            }
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Subscriber(message) => write!(formatter, "tracing subscriber: {message}"),
        }
    }
}

pub(crate) fn initialize() -> Result<tracing_appender::non_blocking::WorkerGuard, LoggingError> {
    let root =
        cubic_platform::persistent_data_directory().ok_or(LoggingError::NoPlatformDataDirectory)?;
    let file = prepare_log_file(&root.join("logs"))?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(BUFFERED_LOG_LINES)
        .lossy(true)
        .finish(TeeWriter { file });
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_timer(SystemLocalTime)
        .with_thread_names(true)
        .with_target(true)
        .with_ansi(false)
        .with_max_level(LogLevel::configured().filter())
        .try_init()
        .map_err(|error| LoggingError::Subscriber(error.to_string()))?;
    Ok(guard)
}

pub(crate) fn initialize_stderr_only() {
    let _ = tracing_subscriber::fmt()
        .with_timer(SystemLocalTime)
        .with_thread_names(true)
        .with_target(true)
        .with_ansi(false)
        .with_max_level(LogLevel::configured().filter())
        .try_init();
}

fn prepare_log_file(logs: &Path) -> Result<File, LoggingError> {
    fs::create_dir_all(logs).map_err(|source| LoggingError::Io {
        operation: "create log directory",
        source,
    })?;
    rotate_logs(logs)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(logs.join("latest.log"))
        .map_err(|source| LoggingError::Io {
            operation: "create latest.log",
            source,
        })
}

fn rotate_logs(logs: &Path) -> Result<(), LoggingError> {
    let previous = logs.join("previous");
    fs::create_dir_all(&previous).map_err(|source| LoggingError::Io {
        operation: "create previous-log directory",
        source,
    })?;
    let oldest = previous.join(archive_name(RETAINED_LOGS));
    if oldest.exists() {
        fs::remove_file(&oldest).map_err(|source| LoggingError::Io {
            operation: "remove oldest retained log",
            source,
        })?;
    }
    for index in (1..RETAINED_LOGS).rev() {
        let from = previous.join(archive_name(index));
        if from.exists() {
            fs::rename(&from, previous.join(archive_name(index + 1))).map_err(|source| {
                LoggingError::Io {
                    operation: "rotate retained log",
                    source,
                }
            })?;
        }
    }
    let latest = logs.join("latest.log");
    if latest.exists() {
        fs::rename(&latest, previous.join(archive_name(1))).map_err(|source| LoggingError::Io {
            operation: "archive previous latest.log",
            source,
        })?;
    }
    Ok(())
}

fn archive_name(index: usize) -> String {
    format!("previous-{index}.log")
}

struct TeeWriter {
    file: File,
}

impl Write for TeeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write_all(buffer)?;
        io::stderr().write_all(buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        io::stderr().flush()
    }
}

struct SystemLocalTime;

impl FormatTime for SystemLocalTime {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        // `now_local` asks the operating system for its current timezone rules,
        // including daylight-saving transitions. UTC is a safe formatting
        // fallback only on platforms where the local offset cannot be obtained.
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        write_wall_clock(writer, now)
    }
}

fn write_wall_clock(writer: &mut impl fmt::Write, time: OffsetDateTime) -> fmt::Result {
    write!(
        writer,
        "[{:02}:{:02}:{:02}]",
        time.hour(),
        time.minute(),
        time.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_levels_accept_supported_case_forms_and_fall_back_safely() {
        assert_eq!(LogLevel::parse("info"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("DEBUG"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("trace"), Some(LogLevel::Trace));
        assert_eq!(LogLevel::parse("TRACE"), Some(LogLevel::Trace));
        assert_eq!(LogLevel::resolve("unsupported"), LogLevel::Info);
        assert_eq!(LogLevel::Info.filter(), LevelFilter::INFO);
        assert_eq!(LogLevel::Debug.filter(), LevelFilter::DEBUG);
        assert_eq!(LogLevel::Trace.filter(), LevelFilter::TRACE);
    }

    #[test]
    fn wall_clock_format_uses_the_supplied_local_offset_and_stays_compact() {
        let utc = OffsetDateTime::from_unix_timestamp(55_738).unwrap();
        let local = utc.to_offset(time::UtcOffset::from_hms(1, 0, 0).unwrap());
        let mut rendered = String::new();
        write_wall_clock(&mut rendered, local).unwrap();
        assert_eq!(rendered, "[16:28:58]");
    }

    #[test]
    fn latest_is_created_and_previous_logs_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let logs = root.path().join("logs");
        for launch in 0..8 {
            let mut file = prepare_log_file(&logs).unwrap();
            writeln!(file, "launch {launch}").unwrap();
        }
        assert!(logs.join("latest.log").is_file());
        for index in 1..=RETAINED_LOGS {
            assert!(logs.join("previous").join(archive_name(index)).is_file());
        }
        assert!(
            !logs
                .join("previous")
                .join(archive_name(RETAINED_LOGS + 1))
                .exists()
        );
    }

    #[test]
    fn unusable_destination_returns_an_error() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("not-a-directory");
        fs::write(&blocker, b"file").unwrap();
        assert!(prepare_log_file(&blocker).is_err());
    }
}
