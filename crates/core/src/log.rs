//! Logs to stdout, and also to hourly files in `LOG_DIR` when set, so the MCP server can read them.
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Keep the returned guard alive in `main`, or the last lines are not flushed to the file.
pub fn init(name: &str, filter: EnvFilter) -> Option<WorkerGuard> {
    let file = std::env::var("LOG_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .and_then(|dir| {
            Builder::new()
                .rotation(Rotation::HOURLY)
                .filename_prefix(name)
                .filename_suffix("log")
                .max_log_files(24)
                .build(&dir)
                .map_err(|e| eprintln!("file logging off, cannot write to {dir}: {e}"))
                .ok()
                .map(|f| (f, dir))
        })
        .map(|(f, dir)| {
            // A restart appends to this hour's file, so its size so far counts (a crash loop).
            let now = chrono::Utc::now();
            let path = format!("{dir}/{name}.{}.log", now.format("%Y-%m-%d-%H"));
            let size = std::fs::metadata(path).map_or(0, |m| m.len());
            let mut c = Capped::new(f, HOUR_CAP);
            (c.hour, c.written) = (Capped::<()>::hour_now(), size);
            tracing_appender::non_blocking(c)
        });
    let (layer, guard) = match file {
        Some((w, g)) => (Some(fmt::layer().with_ansi(false).with_writer(w)), Some(g)),
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(layer)
        .init();
    guard
}

/// Bytes one hourly file may take: 24 files of this stay well under the host's disk, so a log
/// storm in one server cannot fill the volume Postgres lives on.
const HOUR_CAP: u64 = 100 * 1024 * 1024;

/// Writes until `cap` bytes went out this hour, then drops lines (one notice) until the next hour.
struct Capped<W> {
    inner: W,
    cap: u64,
    hour: u64,
    written: u64,
}

impl<W> Capped<W> {
    fn new(inner: W, cap: u64) -> Self {
        Self {
            inner,
            cap,
            hour: 0,
            written: 0,
        }
    }

    fn hour_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() / 3600)
    }
}

impl<W: std::io::Write> std::io::Write for Capped<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_at(buf, Self::hour_now())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: std::io::Write> Capped<W> {
    fn write_at(&mut self, buf: &[u8], hour: u64) -> std::io::Result<usize> {
        if hour != self.hour {
            (self.hour, self.written) = (hour, 0);
        }
        if self.written >= self.cap {
            return Ok(buf.len());
        }
        self.written += buf.len() as u64;
        if self.written >= self.cap {
            self.inner
                .write_all(b"log file full for this hour: lines are dropped until the next hour (stdout still has them)\n")?;
            return Ok(buf.len());
        }
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }
}

#[cfg(test)]
mod tests {
    use super::Capped;

    #[test]
    fn a_full_hour_drops_lines_until_the_next() {
        let mut c = Capped::new(Vec::new(), 10);
        c.write_at(b"12345\n", 1).unwrap();
        c.write_at(b"678901\n", 1).unwrap();
        c.write_at(b"x\n", 1).unwrap();
        c.write_at(b"next\n", 2).unwrap();
        let out = String::from_utf8(c.inner).unwrap();
        assert_eq!(out.lines().count(), 3, "{out}");
        assert!(out.starts_with("12345\nlog file full"));
        assert!(out.ends_with("next\n"));
    }
}
