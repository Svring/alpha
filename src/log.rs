//! Structured logging: date-based folders, per-run files, tee to stdout + file.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Context, Result};
use chrono::Utc;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Returns logs/YYYY-MM-DD/ for the current date. Same day = same folder.
pub fn daily_log_dir(base: &str) -> PathBuf {
    let date = Utc::now().format("%Y-%m-%d");
    PathBuf::from(base).join(date.to_string())
}

/// Returns the path for this run: {command}_{HH-MM-SS}.log
pub fn run_log_path(base: &str, command: &str) -> PathBuf {
    let dir = daily_log_dir(base);
    let time = Utc::now().format("%H-%M-%S");
    dir.join(format!("{command}_{time}.log"))
}

/// Tee writer: writes to both stdout and a file.
struct TeeWriter {
    file: Arc<Mutex<File>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stdout().write_all(buf);
        self.file.lock().map_err(|_| io::ErrorKind::Other)?.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()?;
        self.file.lock().map_err(|_| io::ErrorKind::Other)?.flush()?;
        Ok(())
    }
}

/// MakeWriter that produces TeeWriter for tracing.
struct TeeMakeWriter {
    file: Arc<Mutex<File>>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            file: Arc::clone(&self.file),
        }
    }
}

/// Guard that holds the log file and can write a final summary.
pub struct LogGuard {
    file: Arc<Mutex<File>>,
    start: Instant,
    command: String,
}

impl LogGuard {
    /// Write the run header and summary footer.
    pub fn finish(&self, summary: &RunSummary) -> Result<()> {
        let elapsed = self.start.elapsed();
        let mut f = self.file.lock().map_err(|_| anyhow::anyhow!("lock failed"))?;
        writeln!(f)?;
        writeln!(f, "---")?;
        writeln!(f, "command: {}", self.command)?;
        writeln!(f, "execution_time: {:.2}s", elapsed.as_secs_f64())?;
        if let Some(n) = summary.expressions_simulated {
            writeln!(f, "expressions_simulated: {}", n)?;
        }
        if let Some(ref ids) = summary.alphas_submitable {
            if !ids.is_empty() {
                writeln!(f, "alphas_submitable: {}", ids.join(", "))?;
            }
        }
        if let Some(ref ids) = summary.alphas_submitted {
            if !ids.is_empty() {
                writeln!(f, "alphas_submitted: {}", ids.join(", "))?;
            }
        }
        writeln!(f, "---")?;
        f.flush()?;
        Ok(())
    }
}

/// Summary of a workflow run for logging.
#[derive(Default)]
pub struct RunSummary {
    pub expressions_simulated: Option<usize>,
    pub alphas_submitable: Option<Vec<String>>,
    pub alphas_submitted: Option<Vec<String>>,
}

/// Initialize logging: create date-based dir, open run log file, tee to stdout.
/// Returns a guard to call `finish()` with the run summary.
pub fn init(command: &str, logs_base: &str) -> Result<LogGuard> {
    let path = run_log_path(logs_base, command);
    fs::create_dir_all(path.parent().unwrap())
        .with_context(|| format!("create log dir {}", path.parent().unwrap().display()))?;
    let mut file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open log file {}", path.display()))?;

    let header = format!(
        "=== {} started at {} ===\n",
        command,
        Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );
    io::stdout().write_all(header.as_bytes())?;
    file.write_all(header.as_bytes())?;
    file.flush()?;

    let file = Arc::new(Mutex::new(file));
    let make_writer = TeeMakeWriter {
        file: Arc::clone(&file),
    };

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(fmt::layer().with_writer(make_writer).with_ansi(false).with_target(false))
        .init();

    Ok(LogGuard {
        file,
        start: Instant::now(),
        command: command.to_string(),
    })
}
