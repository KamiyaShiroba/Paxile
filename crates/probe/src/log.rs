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
            let (f, p) = (0u64..)
                .find_map(|n| {
                    let p = PathBuf::from("logs").join(format!(
                        "{prefix}-{}-{}-{n}.jsonl",
                        stamp(),
                        std::process::id()
                    ));
                    match File::options().write(true).create_new(true).open(&p) {
                        Ok(f) => Some(Ok((f, p))),
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
                        Err(e) => Some(Err(e)),
                    }
                })
                .expect("unbounded filename iterator")?;
            (Some(BufWriter::new(f)), Some(p))
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
            if let Err(e) = writeln!(f, "{rec}").and_then(|_| f.flush()) {
                eprintln!("paxile-probe: diagnostic write/flush failed: {e}");
            }
        }
    }

    /// Plain stdout text that doesn't go to the file.
    pub fn note(&self, s: &str) {
        println!("{s}");
    }
    pub fn flush(&mut self) {
        if let Some(f) = self.file.as_mut() {
            if let Err(e) = f.flush() {
                eprintln!("paxile-probe: diagnostic flush failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_record_is_visible_before_logger_drop_and_names_are_unique() {
        let mut first = Logger::new("probe-regression", true).unwrap();
        let second = Logger::new("probe-regression", true).unwrap();
        assert_ne!(first.path, second.path);
        assert!(first
            .path
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .contains(&format!("-{}-", std::process::id())));
        first.emit_at(123, "backend", "safe", json!({"qpc":123}));
        let text = fs::read_to_string(first.path.as_ref().unwrap()).unwrap();
        let record: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(record["qpc"], 123);
        let second_path = second.path.clone().unwrap();
        drop(second);
        fs::remove_file(second_path).unwrap();
        let path = first.path.clone().unwrap();
        drop(first);
        fs::remove_file(path).unwrap();
    }
}
