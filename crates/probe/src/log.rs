//! Dual sink: a human-readable line on stdout plus one JSON object per line in a file.
//! Time is QueryPerformanceCounter-based so records from the spy DLL (which
//! stamp their own QPC) land on the same timeline.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use serde_json::{json, Value};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetLocalTime;

pub struct Logger {
    start_qpc: i64,
    freq: f64,
    file: Option<BufWriter<File>>,
    pub path: Option<PathBuf>,
}

pub fn stamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

pub fn qpc() -> i64 {
    let mut q = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut q) };
    q
}

impl Logger {
    pub fn new(prefix: &str, to_file: bool) -> io::Result<Self> {
        let (file, path) = if to_file {
            fs::create_dir_all("logs")?;
            let p = PathBuf::from("logs").join(format!("{prefix}-{}.jsonl", stamp()));
            (Some(BufWriter::new(File::create(&p)?)), Some(p))
        } else {
            (None, None)
        };
        let mut f = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        Ok(Self {
            start_qpc: qpc(),
            freq: f.max(1) as f64,
            file,
            path,
        })
    }

    pub fn secs_at(&self, q: i64) -> f64 {
        (q - self.start_qpc) as f64 / self.freq
    }

    pub fn emit(&mut self, kind: &str, human: &str, rec: Value) {
        self.emit_at(qpc(), kind, human, rec)
    }

    pub fn emit_at(&mut self, q: i64, kind: &str, human: &str, mut rec: Value) {
        let t = self.secs_at(q);
        println!("[{t:>9.3}] {human}");
        if let Some(f) = self.file.as_mut() {
            rec["t"] = json!((t * 1000.0).round() / 1000.0);
            rec["kind"] = json!(kind);
            let _ = writeln!(f, "{rec}");
        }
    }

    /// Plain stdout text that doesn't go to the file.
    pub fn note(&self, s: &str) {
        println!("{s}");
    }

    pub fn flush(&mut self) {
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
    }
}
