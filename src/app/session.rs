//! Session restoration glue: snapshot the open tabs into
//! [`crate::session`], respawn them at startup, and decide at exit whether
//! the next launch restores or starts fresh.
//!
//! Exit rule: tabs still open (Cmd+Q, window close button) are saved; no
//! tab left (every shell exited or was closed) removes the snapshot. The
//! final decision runs from `ApplicationHandler::exiting`, the only hook
//! macOS reaches on Cmd+Q (AppKit terminates the process right after it).

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
    /// tab. Returns false when no tab could be spawned.
    pub(crate) fn spawn_initial_tabs(
        &mut self,
        cols: usize,
        rows: usize,
        restored: Option<Session>,
    ) -> bool {
        if let Some(restored) = restored {
            let banner = restored.banner();
            for tab in &restored.tabs {
                if self.spawn_tab(cols, rows, tab.cwd.as_deref())
                    && let Some(tab) = self.tabs.last_mut()
                {
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
            self.submit_session_job();
        }
    }

    /// Final exit decision; joins the writer so the file is settled before
    /// the process ends.
    pub(crate) fn finish_session(&mut self) {
        self.submit_session_job();
        if let Some(e) = self.session.saver.finish() {
            log::warn!("failed to save session: {e}");
        }
    }

    fn submit_session_job(&mut self) {
        let Some(path) = self.session.path.clone() else {
            return;
        };
        let job = if !self.config.session.restores_tabs() {
            SessionJob::Remove
        } else if !self.tabs.is_empty() {
            SessionJob::Write(self.session_snapshot())
        } else if self.spawned_any {
            SessionJob::Remove
        } else {
            // Startup failed before any tab existed: keep the old snapshot.
            return;
        };
        self.session.saver.submit(path, job, || {});
    }

    fn session_snapshot(&mut self) -> Session {
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
        let tabs = self
            .tabs
            .iter()
            .map(|t| TabState {
                cwd: t.shell_cwd().map(str::to_string),
            })
            .collect();
        Session::new(self.active, self.session.window.clone(), tabs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
