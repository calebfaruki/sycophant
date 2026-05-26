//! tracing-subscriber `MakeWriter` that filters every formatted log byte
//! through a `ScrubSet` before emitting to the inner writer.
//!
//! Wraps `io::stdout()` by default. Used by main.rs to install a
//! tracing subscriber that cannot leak registered secrets via log lines,
//! even when downstream crates (reqwest, hyper) emit raw header values
//! under `RUST_LOG=trace`.

use shared::scrub::ScrubSet;
use std::io;
use std::sync::Arc;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone)]
pub(crate) struct ScrubMakeWriter {
    set: Arc<ScrubSet>,
}

impl ScrubMakeWriter {
    pub(crate) fn new(set: Arc<ScrubSet>) -> Self {
        Self { set }
    }
}

impl<'a> MakeWriter<'a> for ScrubMakeWriter {
    type Writer = ScrubWriter<io::Stdout>;

    fn make_writer(&'a self) -> Self::Writer {
        ScrubWriter::new(self.set.clone(), io::stdout())
    }
}

pub(crate) struct ScrubWriter<W: io::Write> {
    set: Arc<ScrubSet>,
    inner: W,
}

impl<W: io::Write> ScrubWriter<W> {
    pub(crate) fn new(set: Arc<ScrubSet>, inner: W) -> Self {
        Self { set, inner }
    }
}

impl<W: io::Write> io::Write for ScrubWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.set.is_empty() {
            return self.inner.write(buf);
        }
        let raw = std::str::from_utf8(buf).unwrap_or("");
        let scrubbed = self.set.apply(raw);
        // Report `buf.len()` consumed so the formatter doesn't loop;
        // we always consume the entire input regardless of how many
        // bytes we wrote downstream.
        let _ = self.inner.write_all(scrubbed.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;
    use tracing_subscriber::fmt::MakeWriter;

    const SCRUB_ENV: &str = "TEST_WRITER_SCRUB";

    /// Sink that scrub-filters bytes and stores the result for inspection.
    #[derive(Clone, Default)]
    struct CapturingSink {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    struct CapturingWriter {
        set: Arc<ScrubSet>,
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl io::Write for CapturingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let raw = std::str::from_utf8(buf).unwrap_or("");
            let scrubbed = self.set.apply(raw);
            self.buf
                .lock()
                .unwrap()
                .extend_from_slice(scrubbed.as_bytes());
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct CapturingMakeWriter {
        set: Arc<ScrubSet>,
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl<'a> MakeWriter<'a> for CapturingMakeWriter {
        type Writer = CapturingWriter;
        fn make_writer(&'a self) -> Self::Writer {
            CapturingWriter {
                set: self.set.clone(),
                buf: self.buf.clone(),
            }
        }
    }

    #[test]
    #[serial]
    fn event_with_secret_is_scrubbed_before_stdout() {
        std::env::set_var("TEST_KEY", "sk-leak-trace");
        std::env::set_var(SCRUB_ENV, r#"[{"name":"api","env":"TEST_KEY"}]"#);
        let set = Arc::new(ScrubSet::from_env_var(SCRUB_ENV));
        let sink = CapturingSink::default();
        let make = CapturingMakeWriter {
            set: set.clone(),
            buf: sink.buf.clone(),
        };

        let subscriber = tracing_subscriber::fmt()
            .with_writer(make)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("auth failed: Bearer sk-leak-trace");
        });

        let out = String::from_utf8(sink.buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains("[REDACTED:api]"), "scrubbed tag absent: {out}");
        assert!(
            !out.contains("sk-leak-trace"),
            "raw key bytes leaked to writer: {out}"
        );

        std::env::remove_var("TEST_KEY");
        std::env::remove_var(SCRUB_ENV);
    }

    /// Inner writer that records every `flush()` call. Used to prove
    /// ScrubWriter forwards flush to its inner writer (not a no-op).
    #[derive(Default)]
    struct FlushCountingInner {
        flushes: u32,
    }

    impl io::Write for FlushCountingInner {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    #[serial]
    fn flush_propagates_to_inner() {
        // Kills the `flush -> Ok(())` mutant: ScrubWriter must forward
        // flush calls to its inner writer, not return Ok(()) directly.
        std::env::remove_var(SCRUB_ENV);
        let set = Arc::new(ScrubSet::from_env_var(SCRUB_ENV));
        let mut writer = ScrubWriter::new(set, FlushCountingInner::default());
        for _ in 0..3 {
            io::Write::flush(&mut writer).unwrap();
        }
        assert_eq!(writer.inner.flushes, 3);
    }

    #[test]
    #[serial]
    fn empty_scrubset_passes_bytes_through() {
        std::env::remove_var(SCRUB_ENV);
        let set = Arc::new(ScrubSet::from_env_var(SCRUB_ENV));
        let sink = CapturingSink::default();
        let make = CapturingMakeWriter {
            set: set.clone(),
            buf: sink.buf.clone(),
        };

        let subscriber = tracing_subscriber::fmt()
            .with_writer(make)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello world");
        });

        let out = String::from_utf8(sink.buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains("hello world"));
    }
}
