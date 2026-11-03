//! 审计日志的写盘线程：记录按到达顺序追加到文件，超过大小上限时轮转。
//!
//! 写盘不在调用方（异步线程）上做，也不在锁下做：调用方只把一行交给通道。
//! [`LogWriter::flush`] 等之前交出的行都写完；丢弃时也会等写完再退出。

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

enum Msg {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

pub(super) struct LogWriter {
    tx: Option<mpsc::Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
}

struct LogFile {
    path: PathBuf,
    max_bytes: u64,
    file: File,
    len: u64,
}

impl LogWriter {
    pub(super) fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata()?.len();
        let mut log = LogFile {
            path: path.to_path_buf(),
            max_bytes,
            file,
            len,
        };
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("pagesift-audit".into())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        Msg::Line(line) => log.append(&line),
                        Msg::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    /// 交出一行（已带换行）。
    pub(super) fn write(&self, line: String) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Line(line));
        }
    }

    /// 等之前交出的行都写到文件里。
    pub(super) fn flush(&self) {
        let (done, wait) = mpsc::sync_channel(1);
        if let Some(tx) = &self.tx
            && tx.send(Msg::Flush(done)).is_ok()
        {
            let _ = wait.recv();
        }
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        // 关掉通道，写盘线程写完剩下的行后退出
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl LogFile {
    fn append(&mut self, line: &str) {
        if self.len > 0
            && self.len + line.len() as u64 > self.max_bytes
            && let Err(e) = self.rotate()
        {
            tracing::warn!("audit log rotation failed: {e}");
        }
        // 整行一次写入；文件以追加方式打开，每次写都落在文件末尾
        match self.file.write_all(line.as_bytes()) {
            Ok(()) => self.len += line.len() as u64,
            Err(e) => tracing::warn!("audit log write failed: {e}"),
        }
    }

    fn rotate(&mut self) -> io::Result<()> {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        std::fs::rename(&self.path, &old)?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.len = 0;
        Ok(())
    }
}
