use std::cell::RefCell;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::rc::Rc;

use portable_pty::{Child, ExitStatus, MasterPty, native_pty_system};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;

use super::{Drained, Options};

/// Matches the Windows channel bound; a query-flooding child that never
/// reads its input cannot grow the pending buffer without limit.
const PENDING_CAP: usize = 4 * 1024 * 1024;

/// Cloneable write handle to the pty master. The fd is non-blocking, so
/// remainders the kernel refuses (large pastes) are buffered rather than
/// dropped; the frame loop flushes them.
#[derive(Clone)]
pub struct Writer {
    fd: Rc<OwnedFd>,
    pending: Rc<RefCell<Vec<u8>>>,
}

impl Writer {
    pub fn write(&self, data: &[u8]) {
        let mut pending = self.pending.borrow_mut();
        let buffer = |pending: &mut Vec<u8>, data: &[u8]| {
            let room = PENDING_CAP.saturating_sub(pending.len());
            pending.extend_from_slice(&data[..data.len().min(room)]);
        };
        if !pending.is_empty() {
            buffer(&mut pending, data);
            return;
        }
        let mut remaining = data;
        while !remaining.is_empty() {
            match rustix::io::write(&self.fd, remaining) {
                Ok(n) => remaining = &remaining[n..],
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => {
                    buffer(&mut pending, remaining);
                    return;
                }
                Err(_) => return,
            }
        }
    }

    pub fn flush(&self) {
        let mut pending = self.pending.borrow_mut();
        while !pending.is_empty() {
            match rustix::io::write(&self.fd, &pending) {
                Ok(n) => {
                    pending.drain(..n);
                }
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => return,
                Err(_) => {
                    pending.clear();
                    return;
                }
            }
        }
    }
}

pub struct Pty {
    master: Box<dyn MasterPty>,
    child: Box<dyn Child + Send + Sync>,
    writer: Writer,
}

impl Pty {
    /// Spawn a shell in a new pty; the master fd is non-blocking so the
    /// frame loop can drain it.
    pub fn spawn(opts: Options) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(super::size(opts.cols, opts.rows, opts.cell_w, opts.cell_h))
            .map_err(io::Error::other)?;

        let shell = opts
            .shell
            .map(str::to_owned)
            .or_else(|| std::env::var("SHELL").ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let cmd = super::command(&shell, &opts);

        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        drop(pair.slave);

        let raw = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("pty master has no fd"))?;
        let fd = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;
        fcntl_setfl(&fd, fcntl_getfl(&fd)? | OFlags::NONBLOCK)?;

        Ok(Self {
            master: pair.master,
            child,
            writer: Writer {
                fd: Rc::new(fd),
                pending: Rc::new(RefCell::new(Vec::new())),
            },
        })
    }

    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }

    /// Read what is buffered, up to the frame budget, handing chunks to `sink`.
    pub fn drain(&mut self, mut sink: impl FnMut(&[u8])) -> Drained {
        let mut buf = [0u8; 65536];
        let mut got_data = false;
        let mut total = 0;
        loop {
            if total >= super::DRAIN_BUDGET {
                return Drained::Data;
            }
            match rustix::io::read(&*self.writer.fd, &mut buf) {
                Ok(0) => return Drained::Eof,
                Ok(n) => {
                    got_data = true;
                    total += n;
                    sink(&buf[..n]);
                }
                Err(Errno::AGAIN) => break,
                Err(Errno::INTR) => continue,
                // Linux reports EIO when the slave side closes.
                Err(_) => return Drained::Eof,
            }
        }
        if got_data {
            Drained::Data
        } else {
            Drained::Empty
        }
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_w: u16, cell_h: u16) {
        let _ = self.master.resize(super::size(cols, rows, cell_w, cell_h));
    }

    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Name of the terminal's current foreground process, used for titles.
    pub fn foreground_process_name(&self) -> Option<String> {
        foreground_process_name(self.child.process_id()?)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// OS-specific lookups live below this line; consumers must handle None.

/// Linux: /proc/<shell>/stat field tpgid, then its comm.
#[cfg(target_os = "linux")]
fn foreground_process_name(shell_pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{shell_pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let tpgid: i64 = after_comm.split_whitespace().nth(5)?.parse().ok()?;
    if tpgid <= 0 {
        return None;
    }
    let comm = std::fs::read_to_string(format!("/proc/{tpgid}/comm")).ok()?;
    let name = comm.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(not(target_os = "linux"))]
fn foreground_process_name(_shell_pid: u32) -> Option<String> {
    None
}
