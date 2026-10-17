//! Streamed progress log shared by the install stages.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::runner::Stream;

pub(crate) struct Report<'a> {
    pub(crate) log: Option<PathBuf>,
    sink: &'a mut dyn FnMut(Stream, &str),
}

impl<'a> Report<'a> {
    pub(crate) fn new(log: Option<PathBuf>, sink: &'a mut dyn FnMut(Stream, &str)) -> Self {
        Self { log, sink }
    }

    pub(crate) fn line(&mut self, stream: Stream, text: impl AsRef<str>) {
        let text = text.as_ref();
        if let Some(path) = &self.log {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{text}");
            }
        }
        (self.sink)(stream, text);
    }

    pub(crate) fn info(&mut self, text: impl AsRef<str>) {
        self.line(Stream::Stdout, text);
    }

    pub(crate) fn warn(&mut self, text: impl AsRef<str>) {
        self.line(Stream::Stderr, format!("warning: {}", text.as_ref()));
    }
}
