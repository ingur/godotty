use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};

use portable_pty::{Child, ExitStatus, MasterPty, native_pty_system};

use super::{Drained, Options};

/// Cloneable write handle to the ConPTY input pipe. Writes are synchronous
/// and complete fully, so there is no pending buffer.
#[derive(Clone)]
pub struct Writer {
    inner: Rc<RefCell<Box<dyn Write + Send>>>,
}

impl Writer {
    pub fn write(&self, data: &[u8]) {
        let _ = self.inner.borrow_mut().write_all(data);
    }

    pub fn flush(&self) {
        let _ = self.inner.borrow_mut().flush();
    }
}

pub struct Pty {
    master: Option<Box<dyn MasterPty>>,
    child: Box<dyn Child + Send + Sync>,
    writer: Writer,
    rx: Receiver<Vec<u8>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Pty {
    /// Spawn a shell in a new ConPTY. ConPTY requires each pipe serviced on
    /// its own thread, so a blocking reader thread feeds a channel.
    pub fn spawn(opts: Options) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(super::size(opts.cols, opts.rows, opts.cell_w, opts.cell_h))
            .map_err(io::Error::other)?;

        let shell = opts
            .shell
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(default_shell);
        let cmd = super::command(&shell, &opts);

        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        // Closing the slave is what gives the reader EOF when the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        // Bounded: a full channel blocks the reader, so ConPTY backpressures
        // a flooding child instead of buffering its output unboundedly.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let writer = pair.master.take_writer().map_err(io::Error::other)?;

        Ok(Self {
            master: Some(pair.master),
            child,
            writer: Writer {
                inner: Rc::new(RefCell::new(writer)),
            },
            rx,
            reader: Some(reader),
        })
    }

    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }

    /// Hand queued chunks to `sink`, up to the frame budget.
    pub fn drain(&mut self, mut sink: impl FnMut(&[u8])) -> Drained {
        let mut got_data = false;
        let mut total = 0;
        loop {
            if total >= super::DRAIN_BUDGET {
                return Drained::Data;
            }
            match self.rx.try_recv() {
                Ok(chunk) => {
                    got_data = true;
                    total += chunk.len();
                    sink(&chunk);
                }
                Err(TryRecvError::Empty) => {
                    return if got_data {
                        Drained::Data
                    } else {
                        Drained::Empty
                    };
                }
                Err(TryRecvError::Disconnected) => {
                    return if got_data {
                        Drained::Data
                    } else {
                        Drained::Eof
                    };
                }
            }
        }
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_w: u16, cell_h: u16) {
        if let Some(master) = &self.master {
            let _ = master.resize(super::size(cols, rows, cell_w, cell_h));
        }
    }

    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// ConPTY has no foreground-process-group equivalent; titles rely on OSC.
    pub fn foreground_process_name(&self) -> Option<String> {
        None
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Unblock the reader wherever it waits: disconnect the channel
        // (blocked send) and close the ConPTY (blocked read), then join so
        // no thread outlives the reloadable library.
        let (_tx, dead) = std::sync::mpsc::sync_channel(0);
        drop(std::mem::replace(&mut self.rx, dead));
        drop(self.master.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn default_shell() -> String {
    for exe in ["pwsh.exe", "powershell.exe"] {
        if on_path(exe) {
            return exe.into();
        }
    }
    std::env::var("COMSPEC")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "cmd.exe".into())
}

fn on_path(exe: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(exe).is_file())
}
