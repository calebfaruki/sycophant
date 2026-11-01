//! Ctrl-C and SIGTERM cleanup: restore terminal echo, remove the private
//! temp files a command holds, run the command's undo list in reverse, then
//! exit 130. The handler runs on its own thread, so it may take locks and
//! spawn `stty`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

use crate::commands::common::warn;

static ECHO_OFF: AtomicBool = AtomicBool::new(false);
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static FILES: Tracked = Tracked::new();
pub(crate) static UNDOS: UndoList = UndoList::new();
static INSTALL: Once = Once::new();

/// Paths to remove if the command is interrupted.
struct Tracked(Mutex<Vec<PathBuf>>);

impl Tracked {
    const fn new() -> Self {
        Tracked(Mutex::new(Vec::new()))
    }

    fn track(&self, path: PathBuf) {
        if let Ok(mut files) = self.0.lock() {
            files.push(path);
        }
    }

    fn forget(&self, path: &Path) {
        if let Ok(mut files) = self.0.lock() {
            files.retain(|f| f != path);
        }
    }

    /// Remove every tracked path; one already gone, or never created, is not
    /// an error.
    fn remove_all(&self) {
        if let Ok(files) = self.0.lock() {
            for f in files.iter() {
                let _ = std::fs::remove_file(f);
            }
        }
    }
}

struct Entry {
    label: String,
    manual: String,
    undo: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

/// A change runs while holding the list, so a drain never starts mid-change
/// and each undo runs once, whether the error path or the handler drains.
pub(crate) struct UndoList(Mutex<Vec<Entry>>);

pub(crate) struct Held<'a>(MutexGuard<'a, Vec<Entry>>);

impl UndoList {
    pub(crate) const fn new() -> Self {
        UndoList(Mutex::new(Vec::new()))
    }

    pub(crate) fn hold(&self) -> Held<'_> {
        install();
        Held(self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn drain(&self) {
        let mut held = self.hold();
        for entry in std::mem::take(&mut *held.0).into_iter().rev() {
            if let Err(e) = (entry.undo)() {
                warn(&format!(
                    "could not undo {}: {e}\n    undo it by hand: {}",
                    entry.label, entry.manual
                ));
            }
        }
    }

    pub(crate) fn clear(&self) {
        self.hold().0.clear();
    }
}

impl Held<'_> {
    pub(crate) fn record(
        &mut self,
        label: &str,
        manual: &str,
        undo: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        self.0.push(Entry {
            label: label.to_string(),
            manual: manual.to_string(),
            undo: Box::new(undo),
        });
    }
}

fn install() {
    INSTALL.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            if ECHO_OFF.load(Ordering::SeqCst) {
                set_echo(true);
                eprintln!();
            }
            INTERRUPTED.store(true, Ordering::SeqCst);
            FILES.remove_all();
            UNDOS.drain();
            std::process::exit(130);
        });
    });
}

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

fn set_echo(on: bool) {
    let _ = Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .stdin(Stdio::inherit())
        .status();
}

/// Turn terminal echo off until `echo_on`; an interrupt turns it back on.
pub(crate) fn echo_off() {
    install();
    ECHO_OFF.store(true, Ordering::SeqCst);
    set_echo(false);
}

pub(crate) fn echo_on() {
    set_echo(true);
    ECHO_OFF.store(false, Ordering::SeqCst);
}

/// Remove `path` if the command is interrupted before `forget` is called.
/// Called before the file is created, so no window leaves it untracked.
pub(crate) fn track(path: PathBuf) {
    install();
    FILES.track(path);
}

pub(crate) fn forget(path: &Path) {
    FILES.forget(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn file(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("syco-interrupt-{}-{name}", std::process::id()));
        std::fs::write(&p, "x").unwrap();
        p
    }

    #[test]
    fn removes_tracked_files_only() {
        let t = Tracked::new();
        let (kept, gone, forgotten) = (file("kept"), file("gone"), file("forgotten"));
        t.track(gone.clone());
        t.track(forgotten.clone());
        t.track(std::env::temp_dir().join("syco-interrupt-never-created"));
        t.forget(&forgotten);
        t.remove_all();
        assert!(!gone.exists());
        assert!(kept.exists());
        assert!(forgotten.exists());
        for p in [kept, forgotten] {
            std::fs::remove_file(p).unwrap();
        }
    }

    fn logging_undo(
        log: &Arc<Mutex<Vec<&'static str>>>,
        name: &'static str,
        result: Result<(), String>,
    ) -> impl FnOnce() -> Result<(), String> + Send + 'static {
        let log = log.clone();
        move || {
            log.lock().unwrap().push(name);
            result
        }
    }

    #[test]
    fn drain_runs_each_undo_once_in_reverse_and_a_second_drain_runs_none() {
        let list = UndoList::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        for name in ["first", "second", "third"] {
            list.hold().record(
                name,
                &format!("undo {name}"),
                logging_undo(&log, name, Ok(())),
            );
        }
        list.drain();
        assert_eq!(*log.lock().unwrap(), ["third", "second", "first"]);
        list.drain();
        assert_eq!(*log.lock().unwrap(), ["third", "second", "first"]);
    }

    #[test]
    fn drain_runs_every_undo_past_a_failure() {
        let list = UndoList::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        list.hold()
            .record("first", "undo first", logging_undo(&log, "first", Ok(())));
        list.hold().record(
            "second",
            "undo second",
            logging_undo(&log, "second", Err("second broke".into())),
        );
        list.hold()
            .record("third", "undo third", logging_undo(&log, "third", Ok(())));
        list.drain();
        assert_eq!(*log.lock().unwrap(), ["third", "second", "first"]);
    }
}
