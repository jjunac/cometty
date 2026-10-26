//! Session restoration glue: snapshot the open tabs into
//! [`crate::session`], respawn them at startup, and decide at exit whether
//! the next launch restores or starts fresh.
//!
//! Exit rule: tabs still open (Cmd+Q, window close button) are saved; no
//! tab left (every shell exited or was closed) removes the snapshot. The
//! final decision runs from `ApplicationHandler::exiting`, the only hook
//! macOS reaches on Cmd+Q (AppKit terminates the process right after it).
//! Scrollback is captured only there: debounced runtime snapshots stay
//! tabs-only crash insurance (and drop the previous quit's dumps).

use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

use crate::config::Config;
use crate::session::{self, Session, TabState, WindowState};

use super::App;
use super::persistence::{SessionJob, SessionSaver};

/// Per-process session ownership. Inert (no path) unless this instance
/// holds the session lock, so tests and second instances never touch the
/// user's snapshot.
#[derive(Default)]
pub struct SessionState {
    path: Option<PathBuf>,
    _lock: Option<File>,
    /// Loaded at startup, consumed when the first window opens.
    restored: Option<Session>,
    /// Last windowed (non-fullscreen) geometry: a fullscreen quit restores
    /// the window it came from, not a screen-sized one.
    window: Option<WindowState>,
    saver: SessionSaver,
}

impl SessionState {
    /// Take the session lock and, when `[session] restore` asks for it,
    /// load the previous snapshot.
    pub fn open(config: &Config) -> Self {
        let Some(path) = session::default_path() else {
            return Self::default();
        };
        let Some(lock) = session::acquire_lock(&path) else {
            log::info!("another cometty instance owns the session; starting fresh");
            return Self::default();
        };
        let restored = if config.session.restores_tabs() {
            session::load(&path)
        } else {
            None
        };
        Self {
            path: Some(path),
            _lock: Some(lock),
            window: restored.as_ref().and_then(|s| s.window.clone()),
            restored,
            saver: SessionSaver::default(),
        }
    }

    /// The snapshot to restore in the first window, if any (once).
    pub fn take_restored(&mut self) -> Option<Session> {
        self.restored.take()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.saver.deadline()
    }
}

impl App {
    /// Something a snapshot records changed (tabs, active tab, a shell's
    /// cwd, window geometry, the restore setting): schedule a debounced
    /// write so a crash still restores recent state.
    pub(crate) fn session_changed(&mut self) {
        if self.session.path.is_some() {
            self.session.saver.changed(Instant::now());
        }
    }

    /// Spawn the first window's tabs: the restored snapshot when there is
    /// one (tabs that fail to spawn are skipped), else a single default
    /// tab. Each restored tab replays its scrollback dump (when enabled)
    /// and the banner before the shell's first output is drained. Returns
    /// false when no tab could be spawned.
    pub(crate) fn spawn_initial_tabs(
        &mut self,
        cols: usize,
        rows: usize,
        restored: Option<Session>,
    ) -> bool {
        if let Some(restored) = restored {
            let replay = self.config.session.restores_scrollback();
            for saved in &restored.tabs {
                if !self.spawn_tab(cols, rows, saved.cwd.as_deref()) {
                    continue;
                }
                let dump = match (&self.session.path, &saved.scrollback) {
                    (Some(path), Some(name)) if replay => session::load_scrollback(path, name),
                    _ => None,
                };
                if let Some(tab) = self.tabs.last_mut() {
                    if let Some(dump) = &dump {
                        tab.terminal.replay(dump);
                    }
                    let banner = restored.banner(tab.terminal.cols(), dump.is_some());
                    tab.terminal.feed(banner.as_bytes());
                }
            }
            if !self.tabs.is_empty() {
                log::info!("restored {} tabs", self.tabs.len());
                self.switch_tab(restored.active.min(self.tabs.len() - 1));
                return true;
            }
            log::warn!("no restored tab could be spawned; opening a default tab");
        }
        self.spawn_tab(cols, rows, None)
    }

    pub(crate) fn service_session_saves(&mut self, now: Instant) {
        if let Some(e) = self.session.saver.take_error() {
            log::warn!("failed to save session: {e}");
        }
        if self.session.saver.due(now, false) {
            self.submit_session_job(false);
        }
    }

    /// Final exit decision; joins the writer so the file is settled before
    /// the process ends.
    pub(crate) fn finish_session(&mut self) {
        self.submit_session_job(self.config.session.restores_scrollback());
        if let Some(e) = self.session.saver.finish() {
            log::warn!("failed to save session: {e}");
        }
    }

    fn submit_session_job(&mut self, scrollback: bool) {
        let Some(path) = self.session.path.clone() else {
            return;
        };
        let job = if !self.config.session.restores_tabs() {
            SessionJob::Remove
        } else if !self.tabs.is_empty() {
            let (snapshot, dumps) = self.session_snapshot(scrollback);
            SessionJob::Write(snapshot, dumps)
        } else if self.spawned_any {
            SessionJob::Remove
        } else {
            // Startup failed before any tab existed: keep the old snapshot.
            return;
        };
        self.session.saver.submit(path, job, || {});
    }

    /// The snapshot, plus each tab's VT dump when `scrollback` asks for it.
    /// Dumps are encoded here (the grids live on this thread); the worker
    /// only writes them.
    fn session_snapshot(&mut self, scrollback: bool) -> (Session, Vec<(String, Vec<u8>)>) {
        if let Some(w) = self.window.as_ref() {
            let size = w.inner_size();
            if !super::is_fullscreen_like(w, size.width, size.height) {
                let logical = size.to_logical::<f64>(w.scale_factor());
                self.session.window = Some(WindowState {
                    width: logical.width,
                    height: logical.height,
                    position: w.outer_position().ok().map(|p| (p.x, p.y)),
                });
            }
        }
        let mut dumps = Vec::new();
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for (i, t) in self.tabs.iter().enumerate() {
            let mut state = TabState {
                cwd: t.shell_cwd().map(str::to_string),
                scrollback: None,
            };
            let dump = if scrollback {
                t.terminal.dump_scrollback()
            } else {
                Vec::new()
            };
            if dump.len() > session::MAX_SCROLLBACK_BYTES {
                log::warn!("tab {i} scrollback is {} bytes; not saved", dump.len());
            } else if !dump.is_empty() {
                let name = session::scrollback_name(i);
                state.scrollback = Some(name.clone());
                dumps.push((name, dump));
            }
            tabs.push(state);
        }
        let snapshot = Session::new(self.active, self.session.window.clone(), tabs);
        (snapshot, dumps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn app_at(path: &std::path::Path) -> App {
        let mut app = App::new(
            None,
            crate::theme::Theme::default(),
            Config::default(),
            crate::app::AppStartup {
                config_path_override: None,
                cli_theme: None,
                config_corrupt: false,
            },
            std::sync::Arc::new(std::sync::Mutex::new(crate::logbuf::LogBuffer::new(10))),
        );
        app.session.path = Some(path.to_path_buf());
        app
    }

    fn add_tab(app: &mut App, cwd: &str) {
        let (pty, _output, _input) = crate::pty::test_support::session();
        let terminal = crate::term::Terminal::new(20, 2, crate::theme::Theme::default());
        let mut tab = crate::app::Tab::new(app.next_tab_id, terminal, pty);
        tab.spawn_cwd = Some(cwd.to_string());
        app.next_tab_id += 1;
        app.tabs.push(tab);
        app.spawned_any = true;
    }

    #[test]
    fn exit_saves_open_tabs_and_forgets_closed_sessions() {
        let dir = std::env::temp_dir().join(format!("cometty-app-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        let cwd = dir.to_str().unwrap();

        // Quit with tabs open: saved in order, with the active tab.
        let mut app = app_at(&path);
        add_tab(&mut app, cwd);
        add_tab(&mut app, "/");
        app.active = 1;
        app.finish_session();
        let saved = session::load(&path).unwrap();
        assert_eq!(saved.active, 1);
        let cwds: Vec<_> = saved.tabs.iter().map(|t| t.cwd.as_deref()).collect();
        assert_eq!(cwds, [Some(cwd), Some("/")]);

        // Startup failure before any tab: the old snapshot survives.
        app_at(&path).finish_session();
        assert!(session::load(&path).is_some());

        // Every tab closed: next launch starts fresh.
        app.tabs.clear();
        app.finish_session();
        assert!(!path.exists());

        // Restore turned off: forgotten even with tabs open.
        let mut app = app_at(&path);
        add_tab(&mut app, cwd);
        app.finish_session();
        assert!(path.exists());
        app.config.session.restore = "off".to_string();
        app.finish_session();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_saves_scrollback_only_when_enabled() {
        let dir = std::env::temp_dir().join(format!("cometty-app-sb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        let cwd = dir.to_str().unwrap();

        let mut app = app_at(&path);
        add_tab(&mut app, cwd);
        add_tab(&mut app, cwd);
        let tab = &mut app.tabs[0].terminal;
        tab.feed(b"first\r\nsecond\r\nthird\r\n");
        let expected: Vec<_> = (0..3)
            .map(|i| tab.grid().global_line_cells(i).unwrap())
            .collect();

        // Debounced runtime snapshots never carry dumps.
        app.session_changed();
        app.service_session_saves(Instant::now() + Duration::from_secs(5));
        assert_eq!(app.session.saver.finish(), None);
        assert_eq!(session::load(&path).unwrap().tabs.len(), 2);
        assert!(!session::scrollback_dir(&path).exists());

        // Default quit: the written tab's dump replays its rows; the empty
        // tab references nothing.
        app.finish_session();
        let saved = session::load(&path).unwrap();
        assert_eq!(saved.tabs[1].scrollback, None);
        let name = saved.tabs[0]
            .scrollback
            .as_deref()
            .expect("dump referenced");
        let dump = session::load_scrollback(&path, name).unwrap();
        let mut replayed = crate::term::Terminal::new(20, 2, crate::theme::Theme::default());
        replayed.replay(&dump);
        let got: Vec<_> = (0..3)
            .map(|i| replayed.grid().global_line_cells(i).unwrap())
            .collect();
        assert_eq!(got, expected);

        // `tabs` keeps the tabs but no contents.
        app.config.session.restore = "tabs".to_string();
        app.finish_session();
        assert!(session::load(&path).unwrap().tabs[0].scrollback.is_none());
        assert!(!session::scrollback_dir(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
