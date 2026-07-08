use std::path::Path;
use std::sync::OnceLock;

use portable_pty::{CommandBuilder, PtySize};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{default_shell, Pty, Writer};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{default_shell, Pty, Writer};

/// Identity of the terminal a process runs inside; inherited values are wrong.
/// Bit of a hack, but fixed a lot of issues I faced during testing.
const SCRUB_TERMINAL_IDENTITY: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "GHOSTTY_",
    "KITTY_",
    "WEZTERM_",
    "ALACRITTY_",
    "VTE_VERSION",
    "KONSOLE_",
    "ITERM_",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "WINDOWID",
    "TMUX",
    "TMUX_",
    "STY",
];

/// Activated dev-shell state; the new shell re-activates from its own cwd.
const SCRUB_DEV_SHELL_STATE: &[&str] = &[
    "DEVENV_",
    "DIRENV_",
    "IN_NIX_SHELL",
    "VIRTUAL_ENV",
    "VIRTUAL_ENV_",
    "CONDA_",
];

fn scrubbed(key: &str) -> bool {
    let hit = |list: &[&str]| {
        list.iter().any(|m| {
            if m.ends_with('_') {
                key.starts_with(m)
            } else {
                key == *m
            }
        })
    };
    hit(SCRUB_TERMINAL_IDENTITY) || hit(SCRUB_DEV_SHELL_STATE)
}

pub struct Options<'a> {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub shell: Option<&'a str>,
    pub cwd: &'a Path,
    pub login: bool,
}

pub enum Drained {
    Data,
    Empty,
    Eof,
}

/// Per-frame byte cap; a flooding child must not stall the frame loop.
const DRAIN_BUDGET: usize = 4 * 1024 * 1024;

fn command(shell: &str, opts: &Options) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    if opts.login && cfg!(unix) {
        cmd.arg("-l");
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // Start as fresh as a new terminal window.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(scrubbed) {
            cmd.env_remove(&key);
        }
    }
    cmd.cwd(opts.cwd);
    cmd
}

fn size(cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: cols.saturating_mul(cell_w),
        pixel_height: rows.saturating_mul(cell_h),
    }
}

#[derive(Clone)]
pub struct ShellProfile {
    pub name: String,
    pub path: String,
}

pub fn get_available_shells() -> &'static Vec<ShellProfile> {
    static SHELLS: OnceLock<Vec<ShellProfile>> = OnceLock::new();
    SHELLS.get_or_init(|| {
        #[cfg(windows)]
        {
            crate::pty::windows::get_available_shells()
        }
        #[cfg(unix)]
        {
            crate::pty::unix::get_available_shells()
        }
    })
}
