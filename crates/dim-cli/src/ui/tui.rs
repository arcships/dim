//! The interactive interface. The welcome banner is committed once into
//! scrollback; the conversation streams in as whole Markdown blocks (2.4), the
//! unclosed tail and the composer box live at the bottom and repaint by row
//! diff (2.3). The terminal layer (2.2) guarantees the exit contract.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::model::session::{Command, SessionPort, SwitchKind, Update};
use crate::model::view::{RowKind, ViewRow, ViewSnapshot};
use crate::ui::Env;
use crate::ui::approvals_panel::{ApprovalsPanel, status_label};
use crate::ui::composer::commands::{COMMANDS, Context as CommandContext, Needs};
use crate::ui::composer::{Composer, Outcome as ComposerOutcome, history};
use crate::ui::i18n::{self, t, tf};
use crate::ui::language_panel::LanguagePanel;
use crate::ui::models_panel::ModelsPanel;
use crate::ui::prompts::{AskUserPrompt, Effect, ExitConfirm, ExitReason, PermissionPrompt};
use crate::ui::providers_panel::ProvidersPanel;
use crate::ui::render::fullscreen::Document;
use crate::ui::render::fullscreen::MouseOutcome;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::render::panel::clip;
use crate::ui::render::{Frame, FullScreen, Overlay, Screen};
use crate::ui::sessions_panel::SessionsPanel;
use crate::ui::settings_panel::{Setting, SettingsPanel, Values as SettingValues};
use crate::ui::terminal::{InputEvent, KeyCode, Modifiers, Terminal, TerminalError, Wait};
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::theme::{Palette, Theme, detect};
use crate::ui::theme_panel::ThemePanel;
use crate::ui::transcript::block::{Block, Tone, group_rows, opens_group, project_row};
use crate::ui::transcript::blocks::{
    ASSISTANT_MARK, UserStyle, apply_gutter_mark, display_width, gutter_width, render_user,
    truncate_cells,
};
use crate::ui::transcript::commit::CommitState;
use crate::ui::transcript::reveal::Reveal;
use crate::ui::transcript::stream::{StreamPlanner, TextBlock, render_open};
use crate::ui::transcript::tool_group::{Detail, project_group, render_group};

/// The stop reason and the note committed when the user interrupts a run.
pub const INTERRUPT_REASON: &str = "User interrupted the run.";
const COMPOSER_PLACEHOLDER: &str = "Ask anything...";
/// `config.json` key of the tool detail toggle, shared with the TS CLI.
const TOOL_DETAILS_KEY: &str = "tuiToolDetailsExpanded";
/// Frames are coalesced to roughly 60 Hz.
/// A resize waits this long after the last size change before it is applied.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
/// ...but never longer than this from the first change of a drag.
const RESIZE_MAX_DELAY: Duration = Duration::from_millis(500);
/// Committed blocks kept as sources; older ones are forgotten.
const HISTORY_MAX_BLOCKS: usize = 5_000;
/// Rows of history a scrollback rewrite lays out at most.
const REWRITE_MAX_ROWS: usize = 10_000;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// How often the loop looks at session updates while input is idle.
const SESSION_POLL: Duration = Duration::from_millis(40);
/// One blank column of margin before every document row (L's content pad).
const MARGIN: usize = 1;
/// Revision of the transcript the planner is tracking; a session switch bumps it.
const SESSION_REVISION: u64 = 1;
/// How long a Ctrl+C keeps the exit window armed.
const EXIT_ARM: Duration = Duration::from_millis(2500);
/// Where a committed transcript batch goes in the history.
enum Placement {
    New,
    /// The first batch of a streamed block.
    Stream,
    /// More of the streamed block the last `Stream` began.
    Continue,
}

/// `config.json` key of the fullscreen mouse switch (`"on"` / `"off"`).
const MOUSE_KEY: &str = "tuiMouseMode";

/// Where a palette action or a sent message moves the fullscreen view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    Top,
    Bottom,
    /// Pick the newest user message, as Tab does.
    Pick,
}

/// How long a transient notice stays in the status row.
const NOTICE_TTL: Duration = Duration::from_millis(2500);
/// The live region's pulse: one beat per 250 ms.
const PULSE: Duration = Duration::from_millis(250);

pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

pub fn run(host: Box<dyn SessionPort>, surroundings: Env) -> i32 {
    let mut terminal = match Terminal::enter() {
        Ok(terminal) => terminal,
        Err(TerminalError::NotATerminal) => {
            eprintln!("dim needs an interactive terminal; use 'dim exec' for scripts");
            return 2;
        }
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    i18n::set(i18n::saved_locale(&surroundings.home).unwrap_or_else(i18n::system_locale));
    let synchronized = !crate::ui::terminal::in_tmux();
    let notify_terminal = crate::ui::notify::Terminal::from_env();
    let mut screen = Screen::new(terminal.size(), synchronized);
    // The fullscreen interface, while it is the one on screen; the
    // scrollback `screen` waits underneath and is rewritten on the way back.
    let mut full = None;
    if surroundings.fullscreen {
        if terminal.set_fullscreen(true).is_err() {
            return terminal.lost();
        }
        full = Some(FullScreen::new(terminal.size(), synchronized));
    } else if let Some((row, column)) = terminal.cursor_position() {
        screen.set_origin(if column == 0 { row } else { row + 1 });
    }
    // Capability and background are decided once, before the first line is
    // written: committed scrollback cannot be repainted.
    let background = terminal
        .background_report()
        .and_then(detect::parse_background_report);
    let env = detect::env_from_process();
    let detected = Theme::detect(&env, true, background.as_deref());
    let saved = crate::ui::theme_panel::saved_theme_id(&surroundings.home);
    let theme = match &saved {
        Some(id) => Theme::named(
            id,
            detected.mode,
            detected.appearance,
            background.as_deref(),
        ),
        None => detected.clone(),
    };
    let cwd_key = surroundings.workspace.to_string_lossy().into_owned();
    let mut app = App::new(
        host,
        MarkdownRenderer::new(&theme),
        theme.palette.clone(),
        surroundings.home.clone(),
        cwd_key,
    );
    app.fullscreen = full.is_some();
    app.theme_context = Some(ThemeContext {
        mode: detected.mode,
        appearance: detected.appearance,
        background,
        active: saved.unwrap_or_else(|| crate::ui::theme_panel::SYSTEM.to_owned()),
    });
    let mut dirty = true;
    let mut last_frame: Option<Instant> = None;
    let mut resize: Option<PendingResize> = None;
    let code = loop {
        for update in app.host.drain() {
            app.apply(update);
            dirty = true;
        }
        // Text still being revealed draws a frame each interval.
        if app.revealing() {
            dirty = true;
        }
        for event in std::mem::take(&mut app.notifications) {
            if terminal
                .write(&crate::ui::notify::sequence(&notify_terminal, &event))
                .is_err()
            {
                let code = terminal.lost();
                app.host.shutdown();
                return code;
            }
        }
        // The `/settings` mouse switch applies to the screen at once.
        if full.is_some() && terminal.mouse() != app.mouse && terminal.set_mouse(app.mouse).is_err()
        {
            let code = terminal.lost();
            app.host.shutdown();
            return code;
        }
        let now = Instant::now();
        // A resize is acted on once it settles (design §6.4).
        let current = terminal.size();
        let laid_out = match &full {
            // Fullscreen lays the transcript out by width; a new height
            // only repaints, at once.
            Some(full) => {
                if full.size() != current {
                    dirty = true;
                }
                (full.layout_cols(), current.1)
            }
            None => screen.size(),
        };
        if current != laid_out {
            match &mut resize {
                Some(pending) if pending.size == current => {}
                Some(pending) => {
                    pending.size = current;
                    pending.last = now;
                }
                None => {
                    resize = Some(PendingResize {
                        size: current,
                        first: now,
                        last: now,
                    })
                }
            }
        } else {
            resize = None;
        }
        let settled = resize.as_ref().is_some_and(|p| {
            now.duration_since(p.last) >= RESIZE_DEBOUNCE
                || now.duration_since(p.first) >= RESIZE_MAX_DELAY
        });
        let reflow = app.take_reflow_request();
        if let Some(full) = &mut full
            && (settled || reflow)
        {
            // The alternate screen keeps no history to clean up: the
            // transcript is measured again for the new width or palette.
            resize = None;
            let size = terminal.size();
            full.relayout(size.0);
            let frame = app.frame(size);
            let bytes = full.render(&frame, size, &app.palette, &app);
            if terminal.write(&bytes).is_err() || terminal.write_failed() {
                let code = terminal.lost();
                app.host.shutdown();
                return code;
            }
            dirty = false;
            last_frame = Some(Instant::now());
        } else if settled || reflow {
            resize = None;
            let size = terminal.size();
            // Termux resizes the height with every on-screen keyboard toggle;
            // the ordinary frame path keeps the region valid without wiping
            // the history each time.
            let keyboard =
                size.0 == screen.size().0 && std::env::var_os("TERMUX_VERSION").is_some();
            if reflow || !keyboard {
                let rows = app.reflow_history(size.0 as usize);
                let frame = app.frame(size);
                let wanted = screen.wanted_for(size, frame.live.len());
                let mut bytes = screen.rewrite(size, &rows, wanted);
                // The frame's own commits and live rows go out right behind.
                bytes.extend(screen.render(&frame, size));
                if terminal.write(&bytes).is_err() || terminal.write_failed() {
                    let code = terminal.lost();
                    app.host.shutdown();
                    return code;
                }
                dirty = false;
                last_frame = Some(Instant::now());
            } else {
                dirty = true;
            }
        }
        // A frame waits while the last one is still being written, as
        // grok-build's presenter does: a slow terminal gets fewer, current
        // frames instead of a queue of stale ones.
        let due = last_frame.is_none_or(|at| now.duration_since(at) >= FRAME_INTERVAL)
            && !terminal.writing();
        if dirty && due {
            let size = terminal.size();
            let frame = app.frame(size);
            let bytes = match &mut full {
                Some(full) => full.render(&frame, size, &app.palette, &app),
                None => screen.render(&frame, size),
            };
            if terminal.write(&bytes).is_err() || terminal.write_failed() {
                let code = terminal.lost();
                app.host.shutdown();
                return code;
            }
            dirty = false;
            last_frame = Some(Instant::now());
        }
        // A run publishes every frame interval: look as often, so the
        // stream reaches the screen as evenly as it is published.
        let wait = match (dirty, last_frame) {
            (true, Some(at)) => FRAME_INTERVAL.saturating_sub(now.duration_since(at)),
            _ if app.running() => FRAME_INTERVAL,
            _ => SESSION_POLL,
        };
        match terminal.next_event(wait) {
            Wait::Lost => {
                let code = terminal.lost();
                app.host.shutdown();
                return code;
            }
            Wait::Timeout => {
                if terminal.write_failed() {
                    let code = terminal.lost();
                    app.host.shutdown();
                    return code;
                }
                if app.tick() {
                    dirty = true;
                }
            }
            Wait::Event(InputEvent::Mouse(event)) => {
                let Some(full) = &mut full else { continue };
                match full.mouse(event, &app) {
                    MouseOutcome::Ignored => {}
                    MouseOutcome::Redraw => dirty = true,
                    MouseOutcome::Overlay(rows) => {
                        app.scroll_overlay(rows * 3);
                        dirty = true;
                    }
                    MouseOutcome::Copy(text) => {
                        crate::ui::clipboard::copy_local(&text);
                        if terminal
                            .write(&crate::ui::clipboard::osc52(
                                &text,
                                crate::ui::terminal::in_tmux(),
                            ))
                            .is_err()
                        {
                            let code = terminal.lost();
                            app.host.shutdown();
                            return code;
                        }
                        app.set_notice(t("Copied to clipboard"));
                        dirty = true;
                    }
                    MouseOutcome::Open(url) => {
                        // Over SSH the opener would run on the remote host.
                        let local = std::env::var_os("SSH_CONNECTION").is_none()
                            && std::env::var_os("SSH_TTY").is_none();
                        let opened = local && crate::browser::open(&url);
                        app.set_notice(if opened {
                            t("Opened link").to_owned()
                        } else {
                            tf("Link: {url}", &[("url", &url)])
                        });
                        dirty = true;
                    }
                }
            }
            // Amp's message navigation: Tab (or Up while a message is
            // picked) goes to older user messages, Shift+Tab or Down to newer
            // ones and back to the composer past the newest; Esc ends it and
            // `e` takes the message into the composer.
            Wait::Event(InputEvent::Key(key))
                if full.is_some()
                    && app.keyboard_free()
                    && (matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
                        || full.as_ref().is_some_and(FullScreen::focused)) =>
            {
                let Some(full) = &mut full else { continue };
                dirty = true;
                match key.code {
                    KeyCode::Tab | KeyCode::Up if key.mods.0 == 0 => full.focus_older(&app),
                    KeyCode::BackTab | KeyCode::Down => full.focus_newer(&app),
                    KeyCode::Tab => full.focus_newer(&app),
                    KeyCode::Esc => full.clear_focus(),
                    KeyCode::Char('e') if key.mods.0 == 0 => {
                        if let Some(text) = full.focused_text(&app) {
                            app.composer.set_draft(&text);
                        }
                        full.clear_focus();
                        full.follow();
                    }
                    _ => {
                        // Anything else is typing: the pick ends and the key
                        // goes to the composer.
                        full.clear_focus();
                        if let Outcome::Exit(code) = app.handle(InputEvent::Key(key)) {
                            break code;
                        }
                    }
                }
            }
            Wait::Event(InputEvent::Key(key))
                if full.is_some()
                    && app.keyboard_free()
                    && key.mods.0 == 0
                    && (matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
                        || (matches!(key.code, KeyCode::Home | KeyCode::End)
                            && app.composer.is_empty())) =>
            {
                // Home and End scroll while there is no draft to move in.
                if let Some(full) = &mut full {
                    match key.code {
                        KeyCode::Home => full.scroll_to_top(),
                        KeyCode::End => full.follow(),
                        code => full.page(code == KeyCode::PageDown),
                    }
                }
                dirty = true;
            }
            Wait::Event(event) => match app.handle(event) {
                Outcome::Continue => {
                    if let Some(full) = &mut full {
                        match app.jump.take() {
                            Some(Jump::Bottom) => full.follow(),
                            Some(Jump::Top) => full.scroll_to_top(),
                            Some(Jump::Pick) => full.focus_older(&app),
                            None => {}
                        }
                    }
                    dirty = true;
                }
                Outcome::SwitchMode => {
                    // Both ways the history is laid out again by the reflow
                    // above: the alternate screen starts empty, and the
                    // shell's screen is rewritten whole.
                    let leaving = full.take();
                    let entering = leaving.is_none();
                    if let Some(mut leaving) = leaving {
                        let _ = terminal.write(&leaving.leave());
                    } else {
                        let _ = terminal.write(&screen.erase_live());
                    }
                    if terminal.set_fullscreen(entering).is_err() {
                        let code = terminal.lost();
                        app.host.shutdown();
                        return code;
                    }
                    if entering {
                        full = Some(FullScreen::new(terminal.size(), synchronized));
                    } else {
                        screen = Screen::new(terminal.size(), synchronized);
                    }
                    app.fullscreen = entering;
                    app.request_reflow();
                    dirty = true;
                }
                Outcome::Suspend => {
                    if terminal.suspend().is_err() {
                        let code = terminal.lost();
                        app.host.shutdown();
                        return code;
                    }
                    // The shell may have written over the screen while the
                    // job was stopped: lay the whole transcript out again.
                    app.request_reflow();
                    dirty = true;
                }
                Outcome::Exit(code) => break code,
            },
        }
    };
    // A plain exit says how to come back, once the session has a message to
    // come back to: an empty one is not worth resuming. The fullscreen
    // interface hands the shell's screen back first, so the panel lands
    // under what was there before.
    let mut bytes = match &mut full {
        Some(full) => {
            let _ = terminal.write(&full.leave());
            let _ = terminal.set_fullscreen(false);
            Vec::new()
        }
        None => screen.leave(),
    };
    let resume =
        (app.session_active() && !app.session_id.is_empty()).then_some(app.session_id.as_str());
    bytes.extend(format!("\r\n{}\r\n", goodbye_panel(resume).join("\r\n")).into_bytes());
    let _ = terminal.write(&bytes);
    terminal.leave();
    app.host.shutdown();
    code
}

/// What a theme is built against: the terminal's capability, appearance and
/// reported background, and the theme in effect.
struct ThemeContext {
    mode: crate::ui::theme::ColorMode,
    appearance: crate::ui::theme::Appearance,
    background: Option<String>,
    active: String,
}

struct PendingResize {
    size: (u16, u16),
    first: Instant,
    last: Instant,
}

enum Outcome {
    Continue,
    /// `/tui`: switch between the scrollback and fullscreen interfaces.
    SwitchMode,
    /// `/suspend`: give the terminal to the shell until the job continues.
    Suspend,
    Exit(i32),
}

struct App {
    host: Box<dyn SessionPort>,
    composer: Composer,
    cwd_key: String,
    home: PathBuf,
    /// How tool groups are drawn (`/tools`); groups already committed keep
    /// the form they were written in.
    tool_detail: Detail,
    notices: Vec<String>,
    /// Multi-line status blocks to commit (reports), with their tone.
    reports: Vec<(String, Tone)>,
    banner_committed: bool,
    snapshot: Option<ViewSnapshot>,
    /// `provider / model` once the session exists.
    label: Option<String>,
    failure: Option<String>,
    failure_committed: bool,
    planner: StreamPlanner,
    commits: CommitState,
    markdown: MarkdownRenderer,
    palette: Palette,
    /// The modal prompt on top of the composer, if any.
    prompt: Option<Prompt>,
    /// Requests already answered here: the projection keeps listing them
    /// until the daemon's confirmation arrives.
    answered: HashSet<String>,
    notice: Option<(String, Instant)>,
    exit_armed_until: Option<Instant>,
    run_started: Option<Instant>,
    last_pulse: u64,
    /// Every committed block, oldest first, as its source: both interfaces
    /// lay it out again from here (a scrollback rewrite, the fullscreen
    /// virtual list). Bounded by `HISTORY_MAX_BLOCKS`.
    history: std::collections::VecDeque<Block>,
    /// Blocks dropped from the front of `history` so far.
    dropped: usize,
    /// The history id of the block the newest stream batch began; its next
    /// batches rewrite it.
    stream_block: Option<usize>,
    /// How many times a history block was rewritten, and the newest one.
    rewritten: (u64, usize),
    session_id: String,
    /// The default tool-approval mode (wire name).
    approvals: String,
    /// The next approvals reading is the answer to a change: say so.
    approvals_toast: bool,
    theme_context: Option<ThemeContext>,
    /// The palette changed: the visible rows are laid out again.
    reflow_requested: bool,
    /// Rows of the snapshot before this index are earlier history: not
    /// drawn until `/history` asks for them.
    window_floor: usize,
    /// How far `/history` has paged back; never above `window_floor`.
    history_floor: usize,
    /// A session was just switched; the next snapshot decides the window.
    awaiting_window: bool,
    /// What to write once the switched-to history is out.
    pending_boundary: Option<SwitchKind>,
    /// `(messages in the window, messages before it)` of the last switch.
    window_info: (usize, usize),
    /// Rows `[start, end)` queued to be drawn as an earlier-history page.
    history_request: Option<(usize, usize)>,
    /// The fullscreen interface is on screen.
    fullscreen: bool,
    /// Where the fullscreen transcript goes next: the bottom once a message
    /// is sent, or where a palette action asked.
    jump: Option<Jump>,
    /// The `/settings` row a picker was opened from: closing the picker
    /// goes back to it.
    settings_return: Option<Setting>,
    /// Fullscreen reads the mouse (`tuiMouseMode`); off leaves selection
    /// and the wheel to the terminal.
    pub mouse: bool,
    /// Ask the terminal to notify when the run needs the user
    /// (`tuiNotifyOnComplete`).
    notify: bool,
    /// The pace of each streaming answer and the bytes of it shown, by row
    /// id.
    revealed: std::collections::HashMap<String, (Reveal, usize)>,
    /// Notifications owed to the terminal, written by the main loop.
    pub notifications: Vec<crate::ui::notify::Event>,
    /// The permission or question request last notified: a prompt that
    /// stays open notifies once.
    notified_request: Option<String>,
    /// The draft set aside while Ctrl+O/Ctrl+P has the palette open; it
    /// comes back when the palette closes.
    palette_stash: Option<String>,
    /// The workspace's git branch for the banner, read once.
    branch: String,
}

/// Widest the fullscreen command palette is drawn (Amp's).
const OVERLAY_PALETTE_WIDTH: usize = 80;
/// Widest a fullscreen picker is drawn.
const OVERLAY_LIST_WIDTH: usize = 100;
/// Tallest a fullscreen overlay is drawn.
const OVERLAY_MAX_ROWS: usize = 30;
/// Least rows inside the fullscreen composer box.
const FULLSCREEN_COMPOSER_ROWS: usize = 2;

/// Messages a resume shows at first, and per `/history` page.
const HISTORY_WINDOW: usize = 100;
/// Widest a session boundary rule is drawn.
const BOUNDARY_MAX_WIDTH: usize = 60;

enum Prompt {
    Sessions(SessionsPanel),
    Models(ModelsPanel),
    Providers(Box<ProvidersPanel>),
    Approvals(ApprovalsPanel),
    Theme(ThemePanel),
    Language(LanguagePanel),
    Settings(SettingsPanel),
    Exit(ExitConfirm),
    Permission(PermissionPrompt),
    Ask(AskUserPrompt),
}

impl App {
    fn new(
        host: Box<dyn SessionPort>,
        markdown: MarkdownRenderer,
        palette: Palette,
        home: PathBuf,
        cwd_key: String,
    ) -> Self {
        let past = history::load(&home, &cwd_key);
        let expanded = crate::ui::config::read(&home, TOOL_DETAILS_KEY).and_then(|v| v.as_bool());
        let mouse = crate::ui::config::read(&home, MOUSE_KEY).is_none_or(|v| v != "off");
        let notify = crate::ui::config::read(&home, crate::ui::notify::NOTIFY_KEY)
            .is_none_or(|v| v != false);
        Self {
            host,
            composer: Composer::new(past),
            cwd_key,
            tool_detail: if expanded == Some(true) {
                Detail::Expanded
            } else {
                Detail::Collapsed
            },
            home,
            notices: Vec::new(),
            reports: Vec::new(),
            banner_committed: false,
            snapshot: None,
            label: None,
            failure: None,
            failure_committed: false,
            planner: StreamPlanner::new(),
            commits: CommitState::default(),
            markdown,
            palette,
            prompt: None,
            answered: HashSet::new(),
            notice: None,
            exit_armed_until: None,
            run_started: None,
            last_pulse: 0,
            history: Default::default(),
            dropped: 0,
            stream_block: None,
            rewritten: (0, 0),
            session_id: String::new(),
            approvals: "normal".to_owned(),
            approvals_toast: false,
            theme_context: None,
            reflow_requested: false,
            window_floor: 0,
            history_floor: 0,
            awaiting_window: false,
            pending_boundary: None,
            window_info: (0, 0),
            history_request: None,
            fullscreen: false,
            jump: None,
            settings_return: None,
            mouse,
            notify,
            notifications: Vec::new(),
            revealed: std::collections::HashMap::new(),
            notified_request: None,
            palette_stash: None,
            branch: git_branch().unwrap_or_else(|| t("Not available").into()),
        }
    }

    /// The wheel over an overlay: moves the picker's or the palette's
    /// selection by `rows` (+ down).
    fn scroll_overlay(&mut self, rows: isize) {
        let code = if rows > 0 { KeyCode::Down } else { KeyCode::Up };
        for _ in 0..rows.unsigned_abs() {
            let _ = self.handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
                code,
                Modifiers::NONE,
            )));
        }
    }

    /// Repaints with theme `id` (what `system` is for this terminal when it
    /// is not a registry theme); the visible rows follow on the next frame.
    fn paint_theme(&mut self, id: &str) {
        let Some(context) = &mut self.theme_context else {
            return;
        };
        let theme = crate::ui::theme::Theme::named(
            id,
            context.mode,
            context.appearance,
            context.background.as_deref(),
        );
        context.active = id.to_owned();
        self.markdown = MarkdownRenderer::new(&theme);
        self.palette = theme.palette;
        self.reflow_requested = true;
    }

    fn request_reflow(&mut self) {
        self.reflow_requested = true;
    }

    fn take_reflow_request(&mut self) -> bool {
        std::mem::take(&mut self.reflow_requested)
    }

    /// True once the session has a message (what "needs a session" means).
    fn session_active(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| s.rows.iter().any(|r| r.kind == RowKind::UserMessage))
    }

    /// No prompt, panel or palette owns the keys.
    fn keyboard_free(&self) -> bool {
        self.prompt.is_none() && !self.composer.palette_open()
    }

    fn running(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.running)
    }

    /// What the command palette weighs availability against.
    fn context(&self) -> CommandContext {
        CommandContext {
            session: self.session_active(),
            running: self.running(),
            queued: !self.queued().is_empty(),
            fullscreen: self.fullscreen,
        }
    }

    /// Sends the next queued message in as steering (Ctrl+S).
    fn steer(&mut self) {
        if let Some((id, _)) = self.queued().into_iter().next() {
            self.host.send(Command::InputControl {
                input_id: id,
                action: "steer",
            });
        }
    }

    fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now() + NOTICE_TTL));
    }

    fn arm_exit(&mut self) {
        self.exit_armed_until = Some(Instant::now() + EXIT_ARM);
    }

    /// Why leaving now would cut something off, if it would.
    fn busy_reason(&self) -> Option<ExitReason> {
        let snapshot = self.snapshot.as_ref()?;
        if !snapshot.questions.is_empty() {
            return Some(ExitReason::AskUserPending);
        }
        if !snapshot.approvals.is_empty() {
            return Some(ExitReason::PermissionPending);
        }
        snapshot.running.then_some(ExitReason::AgentRunning)
    }

    /// Opens the prompt for the first pending approval or question, and
    /// closes one whose request is gone.
    fn sync_prompt(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let approval = snapshot
            .approvals
            .iter()
            .find(|a| !self.answered.contains(&a.request_id));
        let question = snapshot
            .questions
            .iter()
            .find(|q| !self.answered.contains(&q.request_id));
        match &self.prompt {
            Some(Prompt::Exit(_))
            | Some(Prompt::Sessions(_))
            | Some(Prompt::Models(_))
            | Some(Prompt::Providers(_))
            | Some(Prompt::Approvals(_))
            | Some(Prompt::Theme(_))
            | Some(Prompt::Language(_))
            | Some(Prompt::Settings(_)) => {}
            Some(Prompt::Permission(p))
                if approval.is_some_and(|a| a.request_id == p.request_id) => {}
            Some(Prompt::Ask(p)) if question.is_some_and(|q| q.request_id == p.request_id) => {}
            _ => {
                self.prompt = if let Some(a) = approval {
                    Some(Prompt::Permission(PermissionPrompt::new(
                        a.request_id.clone(),
                        a.branch_id.clone(),
                        a.reason.clone(),
                        a.tool_name.clone(),
                        a.tool_input.clone(),
                    )))
                } else {
                    question.map(|q| {
                        Prompt::Ask(AskUserPrompt::new(
                            q.request_id.clone(),
                            q.branch_id.clone(),
                            q.questions.clone(),
                        ))
                    })
                };
            }
        }
    }

    /// Time-driven changes: the pulse of a running turn, notice and arm
    /// expiry. True when the screen needs a frame.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if let Some((_, until)) = &self.notice
            && now >= *until
        {
            self.notice = None;
            changed = true;
        }
        if self.exit_armed_until.is_some_and(|until| now >= until) {
            self.exit_armed_until = None;
        }
        if self.running() && self.prompt.is_none() {
            let beat = (now
                .duration_since(self.run_started.unwrap_or(now))
                .as_millis()
                / PULSE.as_millis()) as u64;
            if beat != self.last_pulse {
                self.last_pulse = beat;
                changed = true;
            }
        }
        changed
    }

    /// Queued inputs, oldest first: submitted while a turn was already
    /// running, not yet consumed.
    fn queued(&self) -> Vec<(String, String)> {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .queued
                    .iter()
                    .map(|input| (input.id.clone(), input.text.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Ready { provider, model } => {
                self.label = Some(format!("{provider} / {model}"));
            }
            Update::Snapshot(snapshot) => {
                let was_running = self.running();
                self.snapshot = Some(*snapshot);
                if self.running() && !was_running {
                    self.run_started = Some(Instant::now());
                    self.last_pulse = 0;
                }
                if !self.running() {
                    self.run_started = None;
                }
                self.sync_prompt();
                self.queue_notification(was_running);
            }
            Update::FilesChanged(count) => {
                self.notices.push(tf(
                    "files changed  {count}",
                    &[("count", &count.to_string())],
                ));
            }
            Update::Switched { session_id, kind } => self.switch_session(session_id, kind),
            Update::Sessions(items) => {
                if let Some(Prompt::Sessions(panel)) = &mut self.prompt {
                    panel.set_items(items);
                }
            }
            Update::Toast(text) => self.set_notice(text),
            Update::Report { text, warning } => self
                .reports
                .push((text, if warning { Tone::Warning } else { Tone::Muted })),
            Update::Approvals(mode) => match mode {
                Ok(mode) => {
                    if std::mem::take(&mut self.approvals_toast) {
                        let title = crate::ui::approvals_panel::MODES
                            .iter()
                            .find(|(m, ..)| *m == mode)
                            .map_or("", |(_, title, ..)| t(title));
                        let target = if self.session_active() {
                            t("active session")
                        } else {
                            t("new-session draft")
                        };
                        self.set_notice(tf(
                            "Tool approvals: {title} ({target})",
                            &[("title", title), ("target", target)],
                        ));
                    }
                    self.approvals = mode;
                }
                Err(error) => self.notices.push(error),
            },
            Update::Providers(providers) => {
                if let Some(Prompt::Providers(panel)) = &mut self.prompt {
                    panel.set_providers(providers);
                }
            }
            Update::Oauth { attempt, event } => {
                let effect = match &mut self.prompt {
                    Some(Prompt::Providers(panel)) => panel.oauth_event(attempt, event),
                    _ => Effect::None,
                };
                self.apply_effect(effect);
            }
            Update::ProviderResult(result) => {
                if let Some(Prompt::Providers(panel)) = &mut self.prompt {
                    panel.set_result(result);
                }
            }
            Update::Models(models) => {
                if let Some(Prompt::Models(panel)) = &mut self.prompt {
                    panel.set_models(models);
                }
            }
            Update::Reconnecting { attempt } => {
                self.set_notice(if attempt == 1 {
                    t("Connection lost; reconnecting...").to_owned()
                } else {
                    tf(
                        "Reconnecting... (attempt {attempt})",
                        &[("attempt", &attempt.to_string())],
                    )
                });
            }
            Update::Reconnected => {
                self.set_notice(t("Reconnected"));
                self.notices.push(t("reconnected").to_owned());
            }
            Update::Notice(text) => {
                self.notices.push(text);
            }
            Update::Failed(message) => self.failure = Some(message),
        }
    }

    fn handle(&mut self, event: InputEvent) -> Outcome {
        let outcome = self.handle_input(event);
        // A palette opened over a draft gives the draft back when it closes
        // without leaving text of its own (closed, or a command ran).
        if !self.composer.palette_open()
            && let Some(draft) = self.palette_stash.take()
            && self.composer.is_empty()
        {
            self.composer.set_draft(&draft);
        }
        outcome
    }

    fn handle_input(&mut self, event: InputEvent) -> Outcome {
        let key = match event {
            InputEvent::Paste(text) => {
                // A prompt owns the keyboard; a paste only reaches the
                // composer.
                if self.prompt.is_none() {
                    self.composer.paste(&text);
                }
                return Outcome::Continue;
            }
            InputEvent::Key(key) => key,
            // Only the fullscreen surface reads the mouse.
            InputEvent::Mouse(_) => return Outcome::Continue,
        };
        let ctrl = key.mods.contains(Modifiers::CTRL);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c();
        }
        // Priority: exit confirm, runtime prompt, composer.
        if let Some(prompt) = &mut self.prompt {
            let effect = match prompt {
                Prompt::Sessions(p) => p.handle_key(key),
                Prompt::Models(p) => p.handle_key(key),
                Prompt::Providers(p) => p.handle_key(key),
                Prompt::Approvals(p) => p.handle_key(key),
                Prompt::Theme(p) => p.handle_key(key),
                Prompt::Language(p) => p.handle_key(key),
                Prompt::Settings(p) => p.handle_key(key),
                Prompt::Exit(p) => p.handle_key(key),
                Prompt::Permission(p) => p.handle_key(key),
                Prompt::Ask(p) => p.handle_key(key),
            };
            return self.apply_effect(effect);
        }
        match key.code {
            // The command palette, also over a draft (Amp's Ctrl+O; the
            // banner's Ctrl+P).
            KeyCode::Char('o' | 'p') if ctrl && !self.composer.palette_open() => {
                if !self.composer.is_empty() {
                    self.palette_stash = Some(self.composer.text().to_owned());
                }
                self.composer.set_draft("/");
                return Outcome::Continue;
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    return Outcome::Exit(0);
                }
            }
            KeyCode::Char('s') if ctrl => {
                self.steer();
                return Outcome::Continue;
            }
            KeyCode::Esc if !self.composer.palette_open() => {
                if self.running() {
                    self.interrupt();
                } else if !self.composer.is_empty() {
                    self.composer.clear();
                }
                return Outcome::Continue;
            }
            KeyCode::Up
                if key.mods.0 == 0 && self.composer.is_empty() && !self.queued().is_empty() =>
            {
                // Take the newest queued message back for editing.
                if let Some((id, text)) = self.queued().pop() {
                    self.host.send(Command::InputControl {
                        input_id: id,
                        action: "withdraw",
                    });
                    self.composer.set_draft(&text);
                }
                return Outcome::Continue;
            }
            _ => {}
        }
        match self.composer.handle_key(key) {
            ComposerOutcome::Submit(text) => {
                let text = text.trim().to_owned();
                let _ = history::append(&self.home, &self.cwd_key, &text);
                self.composer.push_history(&text);
                self.jump = Some(Jump::Bottom);
                if self.running() {
                    self.set_notice(t("Message queued"));
                }
                self.host.submit(text);
            }
            ComposerOutcome::Command { name, args } => return self.run_command(&name, &args),
            ComposerOutcome::Handled | ComposerOutcome::Pass => {}
        }
        Outcome::Continue
    }

    /// Ctrl+C is staged: in scrollback a busy session is interrupted first
    /// and asks for confirmation the second time; an idle one, and any
    /// session in fullscreen (where Esc interrupts), clears the draft, then
    /// warns, then exits — through the confirmation while busy.
    fn ctrl_c(&mut self) -> Outcome {
        let now = Instant::now();
        let armed = self.exit_armed_until.is_some_and(|until| now < until);
        if self.fullscreen
            && let Some(reason) = self.busy_reason()
            && self.composer.is_empty()
            && armed
        {
            self.prompt = Some(Prompt::Exit(ExitConfirm::new(reason)));
            return Outcome::Continue;
        }
        if let Some(reason) = self.busy_reason().filter(|_| !self.fullscreen) {
            if armed {
                self.prompt = Some(Prompt::Exit(ExitConfirm::new(reason)));
            } else {
                self.interrupt();
                self.arm_exit();
                self.set_notice(t("Interrupt requested; Ctrl+C again to exit"));
            }
            return Outcome::Continue;
        }
        if !self.composer.is_empty() {
            self.composer.clear();
            self.arm_exit();
            self.set_notice(t("Press Ctrl+C again to exit"));
            return Outcome::Continue;
        }
        if armed {
            return Outcome::Exit(130);
        }
        self.arm_exit();
        self.set_notice(t("Press Ctrl+C again to exit"));
        Outcome::Continue
    }

    /// Stops the run and records it at once: the note lands in the transcript
    /// ahead of whatever the interrupted answer still commits.
    fn interrupt(&mut self) {
        self.host.stop();
        self.notices.push(t(INTERRUPT_REASON).to_owned());
    }

    fn apply_effect(&mut self, effect: Effect) -> Outcome {
        match effect {
            Effect::None => {}
            Effect::Close => self.prompt = None,
            Effect::Exit => return Outcome::Exit(130),
            Effect::Approval {
                request_id,
                branch_id,
                allow,
                remember,
                audit,
            } => {
                self.answered.insert(request_id.clone());
                self.host.send(Command::Approval {
                    request_id,
                    branch_id,
                    allow,
                    remember,
                });
                self.notices.push(audit);
                self.prompt = None;
                self.sync_prompt();
            }
            Effect::Answers {
                request_id,
                branch_id,
                answers,
            } => {
                self.answered.insert(request_id.clone());
                self.host.send(Command::AskAnswers {
                    request_id,
                    branch_id,
                    answers,
                });
                self.prompt = None;
                self.sync_prompt();
            }
            Effect::Cancel {
                request_id,
                branch_id,
                reason,
            } => {
                self.answered.insert(request_id.clone());
                self.host.send(Command::AskCancel {
                    request_id,
                    branch_id,
                    reason,
                });
                self.prompt = None;
                self.sync_prompt();
            }
            Effect::Notice(text) => self.set_notice(text),
            Effect::Resume(id) => {
                self.prompt = None;
                self.composer.clear();
                self.host.send(Command::Resume(id));
            }
            Effect::Refresh => self.host.send(Command::ListSessions),
            Effect::LanguagePreview(locale) => {
                i18n::set(locale.unwrap_or_else(i18n::system_locale));
            }
            Effect::LanguageRestore(locale) => {
                self.prompt = None;
                i18n::set(locale.unwrap_or_else(i18n::system_locale));
            }
            Effect::LanguageApply(locale) => {
                self.prompt = None;
                i18n::set(locale.unwrap_or_else(i18n::system_locale));
                let saved = match locale {
                    Some(locale) => {
                        crate::ui::config::write(&self.home, i18n::LOCALE_KEY, locale.code().into())
                    }
                    None => crate::ui::config::remove(&self.home, i18n::LOCALE_KEY),
                };
                self.set_notice(match (saved, locale) {
                    (Some(error), _) => {
                        tf("Could not save the language: {error}", &[("error", &error)])
                    }
                    (None, Some(locale)) => {
                        tf("Language set to {locale}", &[("locale", locale.label())])
                    }
                    (None, None) => t("Language set to system default").to_owned(),
                });
            }
            Effect::Setting(setting) => self.setting(setting),
            Effect::ThemePreview(id) => self.paint_theme(&id),
            Effect::ThemeRestore(id) => {
                self.prompt = None;
                self.paint_theme(&id);
            }
            Effect::ThemeApply(id) => {
                self.prompt = None;
                self.paint_theme(&id);
                if let Some(error) = crate::ui::theme_panel::save_theme_id(&self.home, &id) {
                    self.set_notice(error);
                }
            }
            Effect::SetApprovals(mode) => {
                self.prompt = None;
                self.approvals_toast = true;
                self.host.send(Command::SetApprovals(mode));
            }
            Effect::Provider(action) => self.host.send(Command::Provider(action)),
            Effect::OauthStart(start) => self.host.send(Command::OauthStart(start)),
            Effect::OauthCancel => self.host.send(Command::OauthCancel),
            Effect::OauthBack => {}
            Effect::OauthPaste(url) => self.host.send(Command::OauthPaste(url)),
            Effect::Print(text) => self.reports.push((text, Tone::Muted)),
            Effect::Connected(provider_id) => {
                self.set_notice(tf("Connected {provider}", &[("provider", &provider_id)]));
                self.prompt = Some(Prompt::Models(ModelsPanel::new()));
                self.host.send(Command::ListModels);
            }
            Effect::SelectModel {
                provider_id,
                model_id,
            } => {
                self.prompt = None;
                self.host.send(Command::SelectModel {
                    provider_id,
                    model_id,
                });
            }
        }
        // A picker opened from `/settings` goes back to its row once closed.
        if self.prompt.is_none()
            && let Some(setting) = self.settings_return.take()
        {
            self.open_settings(setting);
        }
        Outcome::Continue
    }

    /// The run needs the user: it just finished, or a permission or question
    /// request appeared (each request once).
    fn queue_notification(&mut self, was_running: bool) {
        use crate::ui::notify::Event;
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let pending = snapshot
            .approvals
            .iter()
            .find(|a| !self.answered.contains(&a.request_id))
            .map(|a| (a.request_id.clone(), Event::Permission(a.tool_name.clone())))
            .or_else(|| {
                snapshot
                    .questions
                    .iter()
                    .find(|q| !self.answered.contains(&q.request_id))
                    .map(|q| (q.request_id.clone(), Event::Question))
            });
        let raised = pending
            .as_ref()
            .filter(|(id, _)| self.notified_request.as_ref() != Some(id))
            .map(|(_, event)| event.clone());
        self.notified_request = pending.map(|(id, _)| id);
        let finished = was_running && !self.running();
        let event = raised.or(finished.then_some(Event::Complete));
        if let (true, Some(event)) = (self.notify, event) {
            self.notifications.push(event);
        }
    }

    /// Opens `/settings` with `select` picked, showing the current values.
    fn open_settings(&mut self, select: Setting) {
        let theme = self
            .theme_context
            .as_ref()
            .map_or("system", |c| c.active.as_str());
        let theme = crate::ui::theme_panel::label_of(theme);
        let language = match i18n::saved_locale(&self.home) {
            Some(locale) => locale.label().to_owned(),
            None => tf(
                "System default ({locale})",
                &[("locale", i18n::system_locale().label())],
            ),
        };
        let values = SettingValues {
            theme: theme.to_owned(),
            language,
            fullscreen_default: crate::ui::config::saved_fullscreen(&self.home),
            tools_expanded: self.tool_detail == Detail::Expanded,
            mouse: self.mouse,
            notify: self.notify,
            approvals: t(status_label(&self.approvals)).to_owned(),
            model: self.label.clone(),
        };
        self.prompt = Some(Prompt::Settings(SettingsPanel::new(&values, select)));
    }

    /// Flips a `/settings` switch, or opens the picker behind a row.
    fn setting(&mut self, setting: Setting) {
        self.settings_return = Some(setting);
        match setting {
            Setting::Theme => {
                self.settings_return = None;
                self.run_command("theme", "");
                self.settings_return = self.prompt.is_some().then_some(setting);
            }
            Setting::Language => {
                self.prompt = Some(Prompt::Language(LanguagePanel::new(i18n::saved_locale(
                    &self.home,
                ))));
            }
            Setting::Approvals => {
                self.prompt = Some(Prompt::Approvals(ApprovalsPanel::new(&self.approvals)));
            }
            Setting::Model => {
                self.prompt = Some(Prompt::Models(ModelsPanel::new()));
                self.host.send(Command::ListModels);
            }
            Setting::StartupForm => {
                let fullscreen = !crate::ui::config::saved_fullscreen(&self.home);
                if let Some(error) = crate::ui::config::save_fullscreen(&self.home, fullscreen) {
                    self.set_notice(error);
                }
                self.settings_return = None;
                self.open_settings(setting);
            }
            Setting::Notify => {
                self.notify = !self.notify;
                let key = crate::ui::notify::NOTIFY_KEY;
                if let Some(error) = crate::ui::config::write(&self.home, key, self.notify.into()) {
                    self.set_notice(error);
                }
                self.settings_return = None;
                self.open_settings(setting);
            }
            Setting::ToolDetails => {
                self.toggle_tool_details();
                self.settings_return = None;
                self.open_settings(setting);
            }
            Setting::Mouse => {
                self.mouse = !self.mouse;
                let value = if self.mouse { "on" } else { "off" };
                if let Some(error) = crate::ui::config::write(&self.home, MOUSE_KEY, value.into()) {
                    self.set_notice(error);
                }
                self.settings_return = None;
                self.open_settings(setting);
            }
        }
    }

    fn toggle_tool_details(&mut self) {
        let (detail, label) = match self.tool_detail {
            Detail::Collapsed => (Detail::Expanded, t("expanded")),
            Detail::Expanded => (Detail::Collapsed, t("collapsed")),
        };
        self.tool_detail = detail;
        let saved = crate::ui::config::write(
            &self.home,
            TOOL_DETAILS_KEY,
            (detail == Detail::Expanded).into(),
        );
        self.notices.push(match saved {
            None => tf("Tool details {state}", &[("state", label)]),
            Some(error) => tf(
                "Tool details {state} (not saved: {error})",
                &[("state", label), ("error", &error)],
            ),
        });
    }

    fn run_command(&mut self, name: &str, _args: &str) -> Outcome {
        // An action that cannot run now says why instead of failing quietly.
        if let Some(command) = COMMANDS.iter().find(|c| c.name == name)
            && !command.available(self.context())
        {
            self.set_notice(match command.needs {
                Needs::Session => tf(
                    "/{name} needs a session — send a message first",
                    &[("name", name)],
                ),
                Needs::Run => t("Nothing is running").to_owned(),
                Needs::Queue => t("No queued message").to_owned(),
                Needs::Fullscreen => t("Only in fullscreen (/tui)").to_owned(),
                Needs::Nothing => String::new(),
            });
            return Outcome::Continue;
        }
        match name {
            "interrupt" => {
                self.interrupt();
                Outcome::Continue
            }
            "steer" => {
                self.steer();
                Outcome::Continue
            }
            "scroll-top" => {
                self.jump = Some(Jump::Top);
                Outcome::Continue
            }
            "scroll-bottom" => {
                self.jump = Some(Jump::Bottom);
                Outcome::Continue
            }
            "pick-message" => {
                self.jump = Some(Jump::Pick);
                Outcome::Continue
            }
            "usage" => {
                self.host.send(Command::Usage);
                Outcome::Continue
            }
            // Ctrl+Z is undo in the composer, so the shell hand-off is a
            // command.
            "suspend" => Outcome::Suspend,
            "exit" => Outcome::Exit(0),
            "tui" => Outcome::SwitchMode,
            "new" => {
                self.host.send(Command::NewSession);
                Outcome::Continue
            }
            "resume" => {
                self.prompt = Some(Prompt::Sessions(SessionsPanel::new()));
                self.host.send(Command::ListSessions);
                Outcome::Continue
            }
            "theme" => {
                if let Some(context) = &self.theme_context {
                    self.prompt = Some(Prompt::Theme(ThemePanel::new(
                        context.mode,
                        context.appearance,
                        &context.active,
                    )));
                }
                Outcome::Continue
            }
            "approvals" => {
                self.prompt = Some(Prompt::Approvals(ApprovalsPanel::new(&self.approvals)));
                Outcome::Continue
            }
            "connect" => {
                self.prompt = Some(Prompt::Providers(Box::new(ProvidersPanel::new())));
                self.host.send(Command::ListProviders);
                Outcome::Continue
            }
            "models" => {
                self.prompt = Some(Prompt::Models(ModelsPanel::new()));
                self.host.send(Command::ListModels);
                Outcome::Continue
            }
            "history" => {
                self.request_history();
                Outcome::Continue
            }
            "tools" => {
                self.toggle_tool_details();
                Outcome::Continue
            }
            "keys" => {
                let rows = crate::ui::keys::report(self.fullscreen, self.mouse);
                self.reports.push((rows.join("\n"), Tone::Muted));
                Outcome::Continue
            }
            "settings" => {
                self.open_settings(Setting::Theme);
                Outcome::Continue
            }
            "language" => {
                self.prompt = Some(Prompt::Language(LanguagePanel::new(i18n::saved_locale(
                    &self.home,
                ))));
                Outcome::Continue
            }
            "reconnect" => {
                self.host.send(Command::Reconnect);
                Outcome::Continue
            }
            other => {
                self.notices
                    .push(tf("/{name} is not yet available", &[("name", other)]));
                Outcome::Continue
            }
        }
    }

    /// Streaming answers still have text the screen has not shown.
    fn revealing(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| {
            s.rows
                .iter()
                .any(|row| self.revealing_row(row) && self.shown(&row.row_id) < row.text.len())
        })
    }

    /// Bytes of the answer `row_id` shown so far.
    fn shown(&self, row_id: &str) -> usize {
        self.revealed.get(row_id).map_or(0, |(_, shown)| *shown)
    }

    /// A streaming answer, or one that settled while its reveal was behind.
    fn revealing_row(&self, row: &ViewRow) -> bool {
        row.kind == RowKind::StreamingAssistant
            || (row.kind == RowKind::AssistantMessage && self.revealed.contains_key(&row.row_id))
    }

    /// Moves each streaming answer's reveal on to now, at the steady pace
    /// of its stream (see [`Reveal`]); an answer that settled drains what is
    /// left. The pace works in bytes, the rate it follows is measured in
    /// them too, and the shown end is moved up to a character boundary.
    fn advance_reveal(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            self.revealed.clear();
            return;
        };
        let now = Instant::now();
        let mut next = std::collections::HashMap::new();
        for row in &snapshot.rows {
            if !self.revealing_row(row) {
                continue;
            }
            let text = row.text.as_str();
            let (mut pace, _) = self
                .revealed
                .remove(&row.row_id)
                .unwrap_or_else(|| (Reveal::new(now), 0));
            let ended = row.kind == RowKind::AssistantMessage;
            let mut end = pace.advance(now, text.len(), ended).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            next.insert(row.row_id.clone(), (pace, end));
        }
        // Answers that settled leave: the stream planner has their text.
        self.revealed = next;
    }

    fn frame(&mut self, size: (u16, u16)) -> Frame {
        self.advance_reveal();
        let cols = size.0 as usize;
        let width = cols.saturating_sub(2 + MARGIN).max(1);
        let mut frame = Frame::default();
        if !self.banner_committed {
            let text = banner(cols, &self.palette, &self.branch);
            self.record(Block::Banner);
            frame.commits.push(text);
            self.banner_committed = true;
        }
        let (commits, sources, tail) = self.transcript(width);
        for (text, (source, placement)) in commits.into_iter().zip(sources) {
            match placement {
                Placement::New => self.record(source),
                Placement::Stream => {
                    self.record(source);
                    self.stream_block = Some(self.dropped + self.history.len() - 1);
                }
                Placement::Continue => self.rewrite_stream_block(source),
            }
            frame.commits.push(text);
        }
        if !self.awaiting_window
            && let Some(kind) = self.pending_boundary.take()
        {
            let label = match kind {
                SwitchKind::New => t("new session").to_owned(),
                SwitchKind::Resumed => {
                    let (count, hidden) = self.window_info;
                    let sid = self.session_id.get(..8).unwrap_or(&self.session_id);
                    let mut label = tf(
                        "resumed {id} · {count} messages",
                        &[("id", sid), ("count", &count.to_string())],
                    );
                    if hidden > 0 {
                        label.push_str(t(" · earlier history: /history"));
                    }
                    label
                }
            };
            let line = boundary_line(&label, width);
            let text = chunk(&[fg(&line, &self.palette.muted)]);
            self.record(Block::Boundary(label));
            frame.commits.push(text);
        }
        for notice in std::mem::take(&mut self.notices) {
            let interrupted = notice == t(INTERRUPT_REASON);
            let tone = if interrupted {
                &self.palette.warning
            } else {
                &self.palette.muted
            };
            let rows = gutter_text("◦", &notice, width, tone);
            let text = chunk(&rows);
            let tone = if interrupted {
                Tone::Warning
            } else {
                Tone::Muted
            };
            self.record(Block::Status { text: notice, tone });
            frame.commits.push(text);
        }
        for (text, tone) in std::mem::take(&mut self.reports) {
            let color = match tone {
                Tone::Muted => &self.palette.muted,
                Tone::Warning => &self.palette.warning,
                Tone::Danger => &self.palette.danger,
            };
            let rows = gutter_text("◦", &text, width, color);
            let chunk_text = chunk(&rows);
            self.record(Block::Status { text, tone });
            frame.commits.push(chunk_text);
        }
        if let Some(message) = self.failure.clone()
            && !self.failure_committed
        {
            self.failure_committed = true;
            let text = pad_block(&[
                String::new(),
                tf("Error: {message}", &[("message", &message)]),
            ]);
            self.record(Block::Failure(message));
            frame.commits.push(text);
        }

        // While a prompt is open the live region is hidden: the prompt owns
        // the screen below the composer, and what it asks about is already
        // in the transcript.
        let mut live = if self.prompt.is_some() {
            Vec::new()
        } else {
            tail
        };
        frame.dock = live.len();
        if self.prompt.is_none() {
            let queued = self.queued();
            if !queued.is_empty() {
                live.extend(queue_strip(&queued, cols, &self.palette));
            }
        }
        // One blank row above the composer, always: the run status lives in
        // the composer's top border rather than on a row of its own, so the
        // composer does not hop when a run starts or ends.
        live.push(String::new());
        let status = (self.prompt.is_none() && self.running()).then(|| {
            const FRAMES: [&str; 4] = ["·", "•", "●", "•"];
            let started = self.run_started.unwrap_or_else(Instant::now);
            let elapsed = started.elapsed();
            let beat = (elapsed.as_millis() / PULSE.as_millis()) as usize;
            let thinking = self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.rows.iter().any(|row| {
                    row.kind == RowKind::Thinking && row.streaming && !row.text.trim().is_empty()
                })
            });
            let label = if thinking {
                "thinking…  {elapsed}"
            } else {
                "working…  {elapsed}"
            };
            format!(
                "{} {}",
                FRAMES[beat % FRAMES.len()],
                tf(label, &[("elapsed", &format_elapsed(elapsed))])
            )
        });
        let inner = cols.saturating_sub(2 * MARGIN + 4).max(1);
        let prompt = "\u{203a} ";
        let prompt_width = 2;
        let text_width = inner.saturating_sub(prompt_width).max(1);
        let p = &self.palette;
        self.composer.set_context(self.context());
        let mut rows: Vec<String> = Vec::new();
        // While a panel or prompt owns the keyboard the composer's mark dims.
        let prompt_color = if self.prompt.is_some() {
            &p.faint
        } else {
            &p.accent
        };
        // Fullscreen draws the palette as Amp does: a box of its own over
        // the transcript, with the query in it and the composer empty below.
        let palette_overlay =
            self.fullscreen && self.prompt.is_none() && self.composer.palette_open();
        let (cursor_row, cursor_col) = if self.composer.is_empty() || palette_overlay {
            rows.push(format!(
                "{}{}",
                fg(prompt, prompt_color),
                fg(t(COMPOSER_PLACEHOLDER), &p.muted)
            ));
            (0, prompt_width)
        } else {
            let (input_rows, cursor) = self.composer.render_rows(text_width as u16, p);
            for (i, row) in input_rows.into_iter().enumerate() {
                let lead = if i == 0 {
                    fg(prompt, prompt_color)
                } else {
                    "  ".to_owned()
                };
                rows.push(format!("{lead}{row}"));
            }
            (cursor.1 as usize, prompt_width + cursor.0 as usize)
        };
        // Fullscreen keeps room for a second line, as Amp's input does.
        if self.fullscreen && rows.len() < FULLSCREEN_COMPOSER_ROWS {
            rows.resize(FULLSCREEN_COMPOSER_ROWS, String::new());
        }
        let box_top = live.len();
        let muted = |label: &str| fg(label, &p.muted);
        live.extend(boxed(
            &rows,
            cols,
            &|edge| fg(edge, &p.border),
            &|_, body| body.to_owned(),
            status
                .as_deref()
                .map(|text| (text, &muted as &dyn Fn(&str) -> String)),
        ));
        // Panels sit under the composer: a runtime prompt, or the command
        // palette while a command is being typed.
        let outer = cols.saturating_sub(2 + MARGIN).max(4);
        let margin = " ".repeat(MARGIN);
        let composer_rows = rows.len() + 2;
        let panel_rows = (size.1 as usize)
            .saturating_sub(composer_rows + 1 + 4 + 1)
            .min(18);
        // Fullscreen centres the palette and the pickers over the
        // transcript, as tall as the viewport allows.
        if self.fullscreen {
            let budget = (size.1 as usize)
                .saturating_sub(composer_rows + 2 + 2)
                .min(OVERLAY_MAX_ROWS);
            let list_width = cols.saturating_sub(4).min(OVERLAY_LIST_WIDTH);
            let list = |rows: Vec<String>| Some(Overlay { rows, cursor: None });
            frame.overlay = match &self.prompt {
                Some(Prompt::Sessions(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Models(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Providers(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Approvals(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Theme(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Language(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Settings(prompt)) => list(prompt.render(list_width, budget, p)),
                None if palette_overlay => {
                    let width = cols.saturating_sub(4).min(OVERLAY_PALETTE_WIDTH);
                    let page = budget.saturating_sub(6).clamp(3, 16);
                    self.composer
                        .palette_overlay(width, page, p)
                        .map(|(rows, query)| Overlay {
                            rows,
                            cursor: Some((2, (4 + query) as u16)),
                        })
                }
                _ => None,
            };
        }
        let panel = match &self.prompt {
            _ if frame.overlay.is_some() => None,
            Some(Prompt::Sessions(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Models(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Providers(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Approvals(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Theme(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Language(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Settings(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Exit(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Permission(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Ask(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            None => self.composer.palette_panel(outer, p),
        };
        if let Some(panel) = panel {
            live.extend(panel.into_iter().map(|row| format!("{margin}{row}")));
        }
        live.push(self.status_line(cols));
        frame.cursor = Some((
            (box_top + 1 + cursor_row) as u16,
            (MARGIN + 2 + cursor_col).min(cols.saturating_sub(1)) as u16,
        ));
        frame.live = live;
        frame
    }

    /// Commits owed this frame and the live rows (unclosed streaming tail,
    /// the reasoning preview and running tools, in transcript order).
    #[allow(clippy::type_complexity)]
    fn transcript(&mut self, width: usize) -> (Vec<String>, Vec<(Block, Placement)>, Vec<String>) {
        let Some(snapshot) = &self.snapshot else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        let gutter = gutter_width(width);
        let content = width.saturating_sub(gutter).max(1);
        let p = &self.palette;

        let is_message = |row: &ViewRow| row.kind.is_message();
        if self.awaiting_window {
            // The first snapshot of a switched-to session: only the newest
            // messages are drawn, the rest wait for `/history`.
            self.awaiting_window = false;
            let messages: Vec<usize> = snapshot
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| is_message(row))
                .map(|(i, _)| i)
                .collect();
            let hidden = messages.len().saturating_sub(HISTORY_WINDOW);
            self.window_floor = if hidden > 0 { messages[hidden] } else { 0 };
            self.history_floor = self.window_floor;
            self.window_info = (messages.len() - hidden, hidden);
        }
        let floor = self.window_floor;

        enum LiveItem {
            Text(String),
            Thinking(String),
            Tool(usize),
        }
        let mut live_text: Vec<TextBlock> = Vec::new();
        let mut live_items: Vec<LiveItem> = Vec::new();
        let mut settled_assistant: Vec<TextBlock> = Vec::new();
        let mut settled_ids: Vec<String> = Vec::new();
        // An answer that settled before the reveal reached its end keeps
        // streaming until it does; rows after it wait, so the history keeps
        // its order.
        let mut held = false;
        for (index, row) in snapshot.rows.iter().enumerate() {
            if index < floor || held {
                continue;
            }
            let text = row.text.as_str();
            match row.kind {
                // The projection flips the same row to `assistant_message` when
                // the model output ends.
                RowKind::StreamingAssistant => {
                    // Only what the reveal reached: the rest follows evenly.
                    let shown = self.shown(&row.row_id);
                    live_text.push(TextBlock {
                        id: &row.row_id,
                        text: &text[..shown.min(text.len())],
                    });
                    live_items.push(LiveItem::Text(row.row_id.clone()));
                }
                // Reasoning shows its newest rows while it runs and commits as
                // one block when it ends.
                // A thought with no text yet (or only whitespace) draws nothing.
                RowKind::Thinking if row.text.trim().is_empty() => {}
                RowKind::Thinking if row.streaming => {
                    live_items.push(LiveItem::Thinking(row.text.clone()));
                }
                // The calls of one request are one group: it stays live until
                // every call has its result (a call waiting on an approval
                // included), then commits whole under its first call's id.
                RowKind::ToolCall if !opens_group(&snapshot.rows, index) => {}
                RowKind::ToolCall
                    if group_rows(&snapshot.rows, index)
                        .iter()
                        .any(|call| call.pending) =>
                {
                    live_items.push(LiveItem::Tool(index));
                }
                RowKind::AssistantMessage => match self.revealed.get(&row.row_id) {
                    Some(&(_, shown)) if shown < text.len() => {
                        live_text.push(TextBlock {
                            id: &row.row_id,
                            text: &text[..shown],
                        });
                        live_items.push(LiveItem::Text(row.row_id.clone()));
                        held = true;
                    }
                    _ => {
                        settled_assistant.push(TextBlock {
                            id: &row.row_id,
                            text,
                        });
                        settled_ids.push(row.row_id.clone());
                    }
                },
                // The interrupt note is written when the key is pressed.
                RowKind::Status if text == INTERRUPT_REASON => {}
                _ => settled_ids.push(row.row_id.clone()),
            }
        }

        let markdown = &self.markdown;
        let text_color = p.text.clone();
        let render = |source: &str, content_width: usize, gutter: usize, first: bool| {
            let rows = markdown.render(source, content_width, gutter);
            if first {
                apply_gutter_mark(rows, ASSISTANT_MARK, gutter, &|mark: &str| {
                    fg(mark, &text_color)
                })
            } else {
                rows
            }
        };
        let mut plan = self.planner.plan(
            SESSION_REVISION,
            &live_text,
            &settled_assistant,
            width,
            &render,
        );
        let mut commits: Vec<String> = Vec::new();
        let mut sources: Vec<(Block, Placement)> = Vec::new();
        let detail = self.tool_detail;
        // A streamed batch: a block of its own, or more rows of the open
        // block it began (no blank row between them).
        let streamed = |batch: crate::ui::transcript::stream::Batch| {
            let placement = if batch.continues {
                Placement::Continue
            } else {
                Placement::Stream
            };
            let text = if batch.continues {
                pad_block(&batch.lines)
            } else {
                chunk(&batch.lines)
            };
            let source = batch.source.map(|s| Block::Assistant {
                text: s.text,
                first: s.first,
            });
            (text, source.map(|source| (source, placement)))
        };
        let draw = |index: usize,
                    finalized: Option<crate::ui::transcript::stream::Batch>|
         -> (String, Option<(Block, Placement)>) {
            match (snapshot.rows[index].kind, finalized) {
                // The planner has laid out this answer already.
                (RowKind::AssistantMessage, Some(batch)) if batch.lines.is_empty() => {
                    (String::new(), None)
                }
                (RowKind::AssistantMessage, Some(batch)) => streamed(batch),
                _ => {
                    let block = project_row(&snapshot.rows, index, detail);
                    let lines = render_block(&block, width, p, markdown);
                    if lines.is_empty() {
                        return (String::new(), None);
                    }
                    (chunk(&lines), Some((block, Placement::New)))
                }
            }
        };
        if let Some((start, end)) = self.history_request.take() {
            // An earlier page goes out first, under its own header.
            let len = snapshot.rows.len();
            let (start, end) = (start.min(len), end.min(len));
            let count = snapshot.rows[start..end]
                .iter()
                .filter(|r| is_message(r))
                .count();
            let header = tf(
                "Earlier history · {id} · {count} messages · /history for the previous page",
                &[
                    ("id", self.session_id.get(..8).unwrap_or(&self.session_id)),
                    ("count", &count.to_string()),
                ],
            );
            commits.push(chunk(&[fg(&truncate_cells(&header, width), &p.muted)]));
            sources.push((Block::Note(header), Placement::New));
            for index in start..end {
                if snapshot.rows[index].kind == RowKind::ToolCall
                    && !opens_group(&snapshot.rows, index)
                {
                    continue;
                }
                if let (text, Some(source)) = draw(index, None) {
                    commits.push(text);
                    sources.push(source);
                }
            }
        }
        let fresh = self.commits.plan(&settled_ids).fresh;
        for id in fresh {
            let Some(index) = snapshot.rows.iter().position(|r| r.row_id == id) else {
                continue;
            };
            // A batch with lines always has its source.
            if let (text, Some(source)) = draw(index, plan.finalized.remove(&id)) {
                commits.push(text);
                sources.push(source);
            }
        }
        for batch in plan.active {
            if let (text, Some(source)) = streamed(batch) {
                commits.push(text);
                sources.push(source);
            }
        }

        // Live rows, laid out exactly as they will commit.
        let pad = " ".repeat(MARGIN);
        let mut tail: Vec<String> = Vec::new();
        for item in &live_items {
            let rows: Vec<String> = match item {
                LiveItem::Text(id) => match self.planner.pending(id) {
                    // An open diagram is drawn once its source is whole; the
                    // live region holds two rows for it meanwhile, so it
                    // neither grows nor shows half a diagram.
                    Some(pending) if pending.diagram => {
                        let lines = pending.text.lines().count().saturating_sub(1);
                        let lead = " ".repeat(gutter);
                        let rows = vec![
                            format!("{lead}{}", fg("┌─ mermaid", &p.faint)),
                            format!(
                                "{lead}{} {}",
                                fg("│", &p.faint),
                                fg(
                                    &tf(
                                        "drawing diagram… {count} lines",
                                        &[("count", &lines.to_string())]
                                    ),
                                    &p.muted
                                )
                            ),
                        ];
                        if pending.first {
                            apply_gutter_mark(rows, ASSISTANT_MARK, gutter, &|mark: &str| {
                                fg(mark, &p.text)
                            })
                        } else {
                            rows
                        }
                    }
                    Some(pending) if !pending.text.trim().is_empty() => {
                        let (first, written) = (pending.first, pending.written);
                        let rows = render_open(pending.text, pending.open_fence, &|text| {
                            render(text, content, gutter, first)
                        });
                        if written > 0 {
                            // The block's first rows are committed already:
                            // the rest follows them without a blank row.
                            tail.extend(
                                rows.into_iter().skip(written).map(|r| format!("{pad}{r}")),
                            );
                            continue;
                        }
                        rows
                    }
                    _ => Vec::new(),
                },
                LiveItem::Thinking(text) => live_thinking(text, width, p),
                LiveItem::Tool(index) => render_group(
                    &project_group(&group_rows(&snapshot.rows, *index), detail),
                    width,
                    p,
                    "·",
                ),
            };
            if rows.is_empty() {
                continue;
            }
            tail.push(String::new());
            tail.extend(rows.into_iter().map(|r| format!("{pad}{r}")));
        }
        (commits, sources, tail)
    }

    fn switch_session(&mut self, session_id: String, kind: SwitchKind) {
        self.session_id = session_id;
        self.planner.reset();
        self.commits = CommitState::default();
        self.answered.clear();
        self.prompt = None;
        self.snapshot = None;
        self.window_floor = 0;
        self.history_floor = 0;
        self.window_info = (0, 0);
        self.history_request = None;
        self.awaiting_window = true;
        self.pending_boundary = Some(kind);
        self.composer.clear();
        // Fullscreen holds the transcript itself: another session starts on
        // a clean view under the banner. Scrollback keeps what the terminal
        // already shows.
        if self.fullscreen {
            self.dropped += self.history.len();
            self.history.clear();
            self.stream_block = None;
            self.record(Block::Banner);
            self.jump = Some(Jump::Bottom);
        }
    }

    /// `/history`: queue the next older page of the loaded session.
    fn request_history(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            self.set_notice(t("No active session"));
            return;
        };
        let floor = self.history_floor.min(snapshot.rows.len());
        let messages: Vec<usize> = snapshot.rows[..floor]
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind.is_message())
            .map(|(i, _)| i)
            .collect();
        if messages.is_empty() {
            self.set_notice(t("No earlier messages."));
            return;
        }
        let start = if messages.len() > HISTORY_WINDOW {
            messages[messages.len() - HISTORY_WINDOW]
        } else {
            0
        };
        self.history_request = Some((start, floor));
        self.history_floor = start;
    }

    /// Remembers a committed block (bounded, newest last).
    fn record(&mut self, source: Block) {
        self.history.push_back(source);
        if self.history.len() > HISTORY_MAX_BLOCKS {
            self.history.pop_front();
            self.dropped += 1;
        }
    }

    /// Puts the whole text of the open streamed block in place of what it
    /// had: the block keeps one history entry however many batches wrote it.
    fn rewrite_stream_block(&mut self, source: Block) {
        let index = self
            .stream_block
            .and_then(|id| id.checked_sub(self.dropped));
        match index.and_then(|index| self.history.get_mut(index)) {
            Some(block) => {
                *block = source;
                self.rewritten = (self.rewritten.0 + 1, self.stream_block.unwrap_or(0));
            }
            None => self.record(source),
        }
    }

    /// Lays one committed chunk out for `cols` columns.
    fn render_source(&self, source: &Block, cols: usize) -> String {
        let width = cols.saturating_sub(2 + MARGIN).max(1);
        match source {
            Block::Banner => banner(cols, &self.palette, &self.branch),
            block => chunk(&render_block(block, width, &self.palette, &self.markdown)),
        }
    }

    /// The history for a scrollback rewrite, laid out for `cols` columns,
    /// one string per row: the newest `REWRITE_MAX_ROWS` rows at most.
    fn reflow_history(&self, cols: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut rows = 0;
        for source in self.history.iter().rev() {
            let chunk: Vec<String> = raw_rows(&self.render_source(source, cols)).collect();
            rows += chunk.len();
            chunks.push(chunk);
            if rows >= REWRITE_MAX_ROWS {
                break;
            }
        }
        let mut out: Vec<String> = chunks.into_iter().rev().flatten().collect();
        let excess = out.len().saturating_sub(REWRITE_MAX_ROWS);
        out.drain(..excess);
        out
    }

    fn status_line(&self, cols: usize) -> String {
        // mode, approvals mode, then the one status slot: a transient notice,
        // else what the session is waiting for.
        let slot = match &self.notice {
            Some((text, _)) => Some(text.clone()),
            None => match &self.snapshot {
                Some(s) if !s.questions.is_empty() => Some(t("answer required").to_owned()),
                Some(s) if !s.approvals.is_empty() => Some(t("permission required").to_owned()),
                Some(s) if s.running => Some(t("esc interrupt").to_owned()),
                _ => None,
            },
        };
        let mut left = format!(
            "{}AGENT  \u{23f5}\u{23f5} {}",
            " ".repeat(MARGIN),
            t(status_label(&self.approvals))
        );
        if let Some(slot) = slot {
            left.push_str("  ");
            left.push_str(&slot);
        }
        let total = cols.saturating_sub(2);
        // The model label goes first when the row is too narrow for both.
        let line = match &self.label {
            Some(label) if width_of(&left) + 2 + width_of(label) <= total => {
                let gap = total - width_of(&left) - width_of(label);
                format!("{left}{}{label}", " ".repeat(gap))
            }
            _ => clip(&left, total).0,
        };
        // The transient notice is the one thing on the row that just
        // happened: it takes the highlight colour, the rest stays muted.
        if let Some((text, _)) = &self.notice
            && let Some(at) = line.find(text.as_str())
        {
            let (before, rest) = line.split_at(at);
            let (notice, after) = rest.split_at(text.len().min(rest.len()));
            return format!(
                "{}{}{}",
                fg(before, &self.palette.muted),
                fg(notice, &self.palette.highlight),
                fg(after, &self.palette.muted)
            );
        }
        fg(&line, &self.palette.muted)
    }
}

impl Document for App {
    fn rewritten(&self) -> (u64, usize) {
        self.rewritten
    }

    fn dropped(&self) -> usize {
        self.dropped
    }

    fn len(&self) -> usize {
        self.history.len()
    }

    fn rows(&self, index: usize, cols: usize) -> Vec<String> {
        raw_rows(&self.render_source(&self.history[index], cols)).collect()
    }

    fn estimate(&self, index: usize, cols: usize) -> usize {
        match &self.history[index] {
            // Markdown is the costly layout: count the wrapped source lines
            // under the blank row that opens the chunk.
            Block::Assistant { text, .. } => {
                let width = cols.saturating_sub(2 + MARGIN).max(1);
                let content = width.saturating_sub(gutter_width(width)).max(1);
                1 + text
                    .lines()
                    .map(|line| display_width(line).div_ceil(content).max(1))
                    .sum::<usize>()
            }
            _ => self.rows(index, cols).len(),
        }
    }

    fn message(&self, index: usize) -> Option<&str> {
        match &self.history[index] {
            Block::User(text) => Some(text),
            _ => None,
        }
    }
}

/// `<1s`, `42s`, `3m07s`.
fn format_elapsed(elapsed: Duration) -> String {
    let ms = elapsed.as_millis();
    if ms < 1000 {
        return "<1s".to_owned();
    }
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }
    format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
}

/// The queue, drawn in a titled border above the composer: the next message
/// to run reads at the top.
fn queue_strip(items: &[(String, String)], cols: usize, p: &Palette) -> Vec<String> {
    use crate::ui::render::panel::bordered;
    use crate::ui::transcript::blocks::truncate_cells;
    const MAX_ROWS: usize = 5;
    let outer = cols.saturating_sub(2 + MARGIN).max(4);
    let inner = outer.saturating_sub(4).max(1);
    let shown = &items[..items.len().min(MAX_ROWS)];
    let hidden = items.len() - shown.len();
    let mut rows: Vec<String> = shown
        .iter()
        .map(|(_, text)| {
            let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
            fg(&truncate_cells(&format!("› {preview}"), inner), &p.accent)
        })
        .collect();
    let hint = if hidden == 0 {
        t("↑ edit · ctrl+s steer now · /queue").to_owned()
    } else {
        tf(
            "+{count} more · ↑ edit · ctrl+s steer now · /queue",
            &[("count", &hidden.to_string())],
        )
    };
    rows.push(fg(&truncate_cells(&hint, inner), &p.muted));
    let title = paint(
        &tf("Queue ({count})", &[("count", &items.len().to_string())]),
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    );
    let margin = " ".repeat(MARGIN);
    bordered(&rows, outer, None, &p.border, Some(&title))
        .into_iter()
        .map(|row| format!("{margin}{row}"))
        .collect()
}

/// Rows of running reasoning under its title and blank row: the newest
/// text, with one `…` row standing for what scrolled away (as Grok draws it).
const THINKING_ROWS: usize = 4;
/// The bar in front of every row of a thought shown whole.
const THINKING_BAR: &str = "\u{258e}"; // ▎

/// Running reasoning: its title, then its newest rows, all behind the bar.
/// The block keeps one height from its first token, so nothing around it
/// moves while the text streams.
fn live_thinking(text: &str, width: usize, p: &Palette) -> Vec<String> {
    let body = thinking_body(text, width);
    let mut rows = vec![
        thinking_title(t("Thinking…"), width, p),
        thinking_row("", width, p),
    ];
    if body.len() > THINKING_ROWS {
        rows.push(thinking_row("…", width, p));
        let newest = &body[body.len() - (THINKING_ROWS - 1)..];
        rows.extend(newest.iter().map(|row| thinking_row(row, width, p)));
    } else {
        rows.extend(body.iter().map(|row| thinking_row(row, width, p)));
    }
    rows.resize_with(2 + THINKING_ROWS, || thinking_row("", width, p));
    rows
}

/// A settled thought: one row with its duration and the start of its last
/// paragraph (the conclusion), or the whole text behind the bar when
/// expanded.
fn settled_thinking(
    text: &str,
    elapsed_ms: Option<u64>,
    expanded: bool,
    width: usize,
    p: &Palette,
) -> Vec<String> {
    use crate::ui::transcript::blocks::{gutter_mark, truncate_cells};
    let label = match elapsed_ms {
        Some(ms) => tf("Thought for {duration}", &[("duration", &duration(ms))]),
        None => t("Thought").to_owned(),
    };
    if expanded {
        let body = thinking_body(text, width);
        return [thinking_title(&label, width, p), thinking_row("", width, p)]
            .into_iter()
            .chain(body.iter().map(|row| thinking_row(row, width, p)))
            .collect();
    }
    let summary = text
        .rsplit("\n\n")
        .find(|paragraph| !paragraph.trim().is_empty())
        .unwrap_or_default()
        .replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let gutter = gutter_width(width);
    let content = width.saturating_sub(gutter).max(1);
    let label = truncate_cells(&label, content);
    let mut row = format!(
        "{}{}",
        fg(&gutter_mark(ASSISTANT_MARK, gutter), &p.accent),
        thinking_label(&label, p)
    );
    let room = content.saturating_sub(display_width(&label) + 3);
    if !summary.is_empty() && room > 0 {
        row.push_str(&fg(" · ", &p.faint));
        row.push_str(&fg(&truncate_cells(&summary, room), &p.faint));
    }
    vec![row]
}

/// The title row of a thought shown whole: `◆ label` behind the bar.
fn thinking_title(label: &str, width: usize, p: &Palette) -> String {
    use crate::ui::transcript::blocks::{gutter_mark, truncate_cells};
    let gutter = gutter_width(width);
    let room = width.saturating_sub(gutter + 2).max(1);
    format!(
        "{}{}{}",
        fg(&gutter_mark(THINKING_BAR, gutter), &p.border),
        fg(&format!("{ASSISTANT_MARK} "), &p.accent),
        thinking_label(&truncate_cells(label, room), p)
    )
}

fn thinking_label(label: &str, p: &Palette) -> String {
    paint(
        label,
        Paint {
            fg: Some(&p.muted),
            italic: true,
            ..Paint::default()
        },
    )
}

/// One row of reasoning text behind the bar.
fn thinking_row(text: &str, width: usize, p: &Palette) -> String {
    use crate::ui::transcript::blocks::gutter_mark;
    let bar = gutter_mark(THINKING_BAR, gutter_width(width));
    format!("{}{}", fg(&bar, &p.border), fg(text, &p.muted))
}

/// Reasoning text as plain prose wrapped to the content width, its
/// paragraphs kept: runs of blank lines fold into one, the edges are trimmed
/// and bold markers dropped.
fn thinking_body(text: &str, width: usize) -> Vec<String> {
    let content = width.saturating_sub(gutter_width(width)).max(1);
    let mut rows: Vec<String> = Vec::new();
    for line in text.trim().lines().map(str::trim_end) {
        if line.is_empty() {
            if rows.last().is_some_and(|last| !last.is_empty()) {
                rows.push(String::new());
            }
        } else {
            rows.extend(wrap_prose(&line.replace("**", ""), content));
        }
    }
    rows
}

/// Punctuation that stays on the row of the text it closes.
const CLOSING: &str = "。，、；：！？）」』】》〉…";

/// Greedy wrap for prose in any script: rows break at spaces and between
/// wide (CJK) characters, closing punctuation stays with what it closes, and
/// a word longer than the row is cut.
fn wrap_prose(line: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    // Unbreakable units, each with whether a space came before it.
    let mut units: Vec<(String, bool)> = Vec::new();
    let mut spaced = false;
    for cluster in line.graphemes(true) {
        if cluster.chars().all(char::is_whitespace) {
            spaced = true;
            continue;
        }
        let narrow = width_of(cluster) <= 1;
        match units.last_mut() {
            Some((unit, _))
                if !spaced
                    && (CLOSING.contains(cluster)
                        || (narrow
                            && unit
                                .graphemes(true)
                                .next_back()
                                .is_some_and(|last| width_of(last) <= 1))) =>
            {
                unit.push_str(cluster)
            }
            _ => units.push((cluster.to_owned(), spaced)),
        }
        spaced = false;
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    for (unit, spaced) in units {
        let used = width_of(&row);
        let gap = usize::from(spaced && used > 0);
        if used > 0 && used + gap + width_of(&unit) > width {
            rows.push(std::mem::take(&mut row));
        } else if gap > 0 {
            row.push(' ');
        }
        if row.is_empty() && width_of(&unit) > width {
            let mut chunks = wrap(&unit, width);
            row = chunks.pop().unwrap_or_default();
            rows.extend(chunks);
        } else {
            row.push_str(&unit);
        }
    }
    rows.push(row);
    rows
}

/// `31.9s`, or `2m 5s` from a minute on.
fn duration(ms: u64) -> String {
    if ms < 59_950 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let seconds = (ms + 500) / 1000;
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

/// A gutter mark and wrapped text in one tone (errors, status notes).
fn gutter_text(mark: &str, text: &str, width: usize, color: &str) -> Vec<String> {
    use crate::ui::transcript::blocks::{gutter_mark, wrap_cells};
    let gutter = gutter_width(width);
    let content = width.saturating_sub(gutter).max(1);
    wrap_cells(text, content)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let lead = if i == 0 {
                gutter_mark(mark, gutter)
            } else {
                " ".repeat(gutter)
            };
            format!(
                "{}{}",
                fg(&lead, color),
                fg(if row.is_empty() { " " } else { &row }, color)
            )
        })
        .collect()
}

/// A committed chunk owns the blank row that separates it from what came
/// before, and every row carries the document pad.
/// A rule centred on `label`, at most [`BOUNDARY_MAX_WIDTH`] wide.
fn boundary_line(label: &str, width: usize) -> String {
    use crate::ui::transcript::blocks::truncate_cells;
    let width = width.clamp(8, BOUNDARY_MAX_WIDTH);
    let text = format!(
        " {} ",
        truncate_cells(label.trim(), width.saturating_sub(4).max(1))
    );
    let remaining = width.saturating_sub(width_of(&text));
    let left = remaining / 2;
    let right = remaining - left;
    truncate_cells(
        &format!("{}{text}{}", "─".repeat(left), "─".repeat(right)),
        width,
    )
}

/// Draws one block as rows at `width` columns, without the spacing row and
/// the document pad that a commit adds.
fn render_block(
    block: &Block,
    width: usize,
    p: &Palette,
    markdown: &MarkdownRenderer,
) -> Vec<String> {
    let gutter = gutter_width(width);
    let content = width.saturating_sub(gutter).max(1);
    match block {
        // The startup banner is drawn at the full width by `banner`.
        Block::Banner => Vec::new(),
        Block::User(text) => render_user(
            text,
            width,
            &UserStyle {
                color: &p.user,
                surface: p.surface.as_deref(),
            },
        ),
        Block::Assistant { text, first } => {
            let rows = markdown.render(text, content, gutter);
            if *first {
                let color = p.text.clone();
                apply_gutter_mark(rows, ASSISTANT_MARK, gutter, &|mark: &str| fg(mark, &color))
            } else {
                rows
            }
        }
        Block::Thinking {
            text,
            elapsed_ms,
            expanded,
        } => settled_thinking(text, *elapsed_ms, *expanded, width, p),
        Block::ToolGroup(group) => render_group(group, width, p, "·"),
        Block::Error(text) => gutter_text("■", text, width, &p.danger),
        Block::Status { text, tone } => {
            let color = match tone {
                Tone::Muted => &p.muted,
                Tone::Warning => &p.warning,
                Tone::Danger => &p.danger,
            };
            gutter_text("◦", text, width, color)
        }
        Block::Failure(message) => vec![tf("Error: {message}", &[("message", message)])],
        Block::Boundary(label) => vec![fg(&boundary_line(label, width), &p.muted)],
        Block::Note(text) => vec![fg(&truncate_cells(text, width), &p.muted)],
    }
}

fn chunk(lines: &[String]) -> String {
    let mut rows = vec![String::new()];
    rows.extend(lines.iter().cloned());
    pad_block(&rows)
}

fn pad_block(rows: &[String]) -> String {
    let pad = " ".repeat(MARGIN);
    rows.iter()
        .map(|row| {
            if row.is_empty() {
                String::new()
            } else {
                format!("{pad}{row}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn banner(cols: usize, p: &Palette, branch: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| t("Not available").into());
    let branch = branch.to_owned();
    // Port of `buildStartupPanel`: the logo beside the greeting when the box
    // is wide enough, the greeting wrapped under it otherwise.
    let body_width = cols.saturating_sub(2 + MARGIN).max(4) - 4;
    let greeting = [
        tf("Welcome to DIM {version}!", &[("version", version)]),
        t("Type / for commands · Ctrl+P for the palette").to_owned(),
    ];
    let header = startup_header(&greeting, body_width);
    let inline = body_width >= 32;
    let label_width = if inline {
        (10 + 2).clamp(8, body_width.saturating_sub(12).max(8))
    } else {
        body_width
    };
    let value_width = body_width.saturating_sub(label_width).max(1);
    let mut info = vec![String::new()];
    for (label, value) in [(t("Workspace"), &cwd), (t("Git branch"), &branch)] {
        if !inline {
            info.push(label.to_owned());
            info.extend(wrap(value, body_width));
            continue;
        }
        for (index, part) in wrap(value, value_width).into_iter().enumerate() {
            let lead = if index == 0 { label } else { "" };
            let pad = " ".repeat(label_width.saturating_sub(width_of(lead)));
            info.push(format!("{lead}{pad}{part}"));
        }
    }
    let rows: Vec<String> = header.iter().chain(info.iter()).cloned().collect();
    let header_rows = header.len();
    let mut text = boxed(
        &rows,
        cols,
        &|edge| fg(edge, &p.accent),
        &|index, body| {
            let color = if index < header_rows {
                &p.accent
            } else {
                &p.text_soft
            };
            fg(body, color)
        },
        None,
    )
    .join("\n");
    text.push('\n');
    text
}

fn git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// The box printed under the transcript on exit, with the command that
/// reopens the session, ready to paste, when there is one worth reopening.
fn goodbye_panel(resume: Option<&str>) -> Vec<String> {
    let mut body = vec![t("Bye!").to_owned()];
    if let Some(session_id) = resume {
        body.extend([
            String::new(),
            t("To resume this session:").to_owned(),
            format!("  dimcode --resume {session_id}"),
        ]);
    }
    let inner = body.iter().map(|line| width_of(line)).max().unwrap_or(0) + 4;
    std::iter::once(format!("╭{}╮", "─".repeat(inner)))
        .chain(
            body.iter()
                .map(|line| format!("│  {line}{}  │", " ".repeat(inner - 4 - width_of(line)))),
        )
        .chain(std::iter::once(format!("╰{}╯", "─".repeat(inner))))
        .collect()
}

/// The rows of a committed chunk (its closing newline ends the last row).
fn raw_rows(text: &str) -> impl Iterator<Item = String> + '_ {
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(str::to_owned)
}

/// Paints plain text with a colour.
type Painter<'a> = &'a dyn Fn(&str) -> String;

/// Rounded box drawing around `rows`, sized to `cols` with the common
/// margin. Rows that are too long are cut to the inner width. `edge` paints
/// the border characters. `status`, plain text painted by `label`, is set
/// into the top border (`╭─ status ───╮`), Amp-style.
fn boxed(
    rows: &[String],
    cols: usize,
    edge: Painter,
    interior: &dyn Fn(usize, &str) -> String,
    status: Option<(&str, Painter)>,
) -> Vec<String> {
    // L's boxes end two columns short of the right edge, margin included.
    let outer = cols.saturating_sub(2 + MARGIN).max(4);
    let inner = outer - 4;
    let margin = " ".repeat(MARGIN);
    let mut out = Vec::with_capacity(rows.len() + 2);
    let top = match status {
        // `╭─ `, the label, then a space and at least one `─` before `╮`.
        Some((text, label)) if outer >= 7 => {
            let (text, width) = clip(text, outer - 6);
            format!(
                "{}{}{}",
                edge("╭─ "),
                label(&text),
                edge(&format!(" {}╮", "─".repeat(outer - 5 - width)))
            )
        }
        _ => edge(&format!("╭{}╮", "─".repeat(outer - 2))),
    };
    out.push(format!("{margin}{top}"));
    for (index, row) in rows.iter().enumerate() {
        let (text, width) = clip(row, inner);
        let body = format!(" {text}{} ", " ".repeat(inner.saturating_sub(width)));
        out.push(format!(
            "{margin}{}{}{}",
            edge("│"),
            interior(index, &body),
            edge("│")
        ));
    }
    out.push(format!(
        "{margin}{}",
        edge(&format!("╰{}╯", "─".repeat(outer - 2)))
    ));
    out
}

/// Display width of `text`, ignoring CSI/OSC escape sequences.
fn width_of(text: &str) -> usize {
    clip(text, usize::MAX).1
}

/// Hard wrap by display width.
const STARTUP_LOGO: [&str; 4] = ["  ▓▓▓▓  ", "▓▓    ▓▓", "░░    ░░", "  ░░░░  "];
const STARTUP_LOGO_WIDTH: usize = 8;
const STARTUP_LOGO_GAP: usize = 2;

/// The greeting beside the logo, vertically centred against it (port of
/// `startupHeaderLines`).
fn startup_header(greeting: &[String], body_width: usize) -> Vec<String> {
    if body_width < STARTUP_LOGO_WIDTH {
        return greeting
            .iter()
            .flat_map(|line| wrap_words(line, body_width))
            .collect();
    }
    let text_width = body_width - STARTUP_LOGO_WIDTH - STARTUP_LOGO_GAP;
    if text_width < 16 {
        return STARTUP_LOGO
            .iter()
            .map(|l| l.trim_end().to_owned())
            .chain(
                greeting
                    .iter()
                    .flat_map(|line| wrap_words(line, body_width)),
            )
            .collect();
    }
    let text: Vec<String> = greeting
        .iter()
        .flat_map(|line| wrap_words(line, text_width))
        .collect();
    let top = STARTUP_LOGO.len().saturating_sub(text.len()) / 2;
    let height = STARTUP_LOGO.len().max(top + text.len());
    (0..height)
        .map(|index| {
            let logo = STARTUP_LOGO.get(index).copied().unwrap_or("");
            let text = index
                .checked_sub(top)
                .and_then(|i| text.get(i))
                .map_or("", String::as_str);
            format!(
                "{logo:<width$}{text}",
                width = STARTUP_LOGO_WIDTH + STARTUP_LOGO_GAP
            )
            .trim_end()
            .to_owned()
        })
        .collect()
}

/// Greedy word wrap (port of `wrapWordsCells`): words longer than the row are
/// cut by display width.
pub(crate) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if width_of(&candidate) <= width {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            result.push(std::mem::take(&mut current));
        }
        let mut chunks = wrap(word, width);
        current = chunks.pop().unwrap_or_default();
        result.extend(chunks);
    }
    result.push(current);
    result
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rows = vec![String::new()];
    let mut used = 0;
    for cluster in text.graphemes(true) {
        let w = crate::ui::transcript::blocks::display_width(cluster);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        if let Some(row) = rows.last_mut() {
            row.push_str(cluster);
        }
        used += w;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxed_rows_have_equal_display_width() {
        let rows = vec![
            "plain".to_owned(),
            "中文 mixed".to_owned(),
            "\x1b[2mdim\x1b[22m".to_owned(),
        ];
        let out = boxed(&rows, 40, &|e| e.to_owned(), &|_, b| b.to_owned(), None);
        for line in &out {
            assert_eq!(width_of(line), 38, "{line:?}");
        }
        let out = boxed(
            &rows,
            40,
            &|e| e.to_owned(),
            &|_, b| b.to_owned(),
            Some(("● working…  3s", &|s| s.to_owned())),
        );
        assert!(
            out[0].starts_with(&format!("{}╭─ ● working…  3s ─", " ".repeat(MARGIN))),
            "{:?}",
            out[0]
        );
        for line in &out {
            assert_eq!(width_of(line), 38, "{line:?}");
        }
    }

    #[test]
    fn emoji_sequences_count_as_one_wide_cluster() {
        // A ZWJ family, a VS16 heart and a flag are two columns each, as
        // Bun's `stringWidth` (the reference) counts them.
        assert_eq!(width_of("👨‍👩‍👧"), 2);
        assert_eq!(width_of("❤️"), 2);
        assert_eq!(width_of("🇨🇳"), 2);
        let (cut, used) = clip("ab👨‍👩‍👧cd", 3);
        assert_eq!((cut.as_str(), used), ("ab", 2), "a cluster is never split");
        assert_eq!(wrap("a👨‍👩‍👧b", 2), vec!["a", "👨‍👩‍👧", "b"]);
    }

    #[test]
    fn wrap_respects_wide_characters() {
        assert_eq!(wrap("中中中", 4), vec!["中中", "中"]);
        assert_eq!(wrap("abcd", 2), vec!["ab", "cd"]);
    }

    #[test]
    fn committed_chunks_start_with_a_blank_row_and_are_padded() {
        let text = chunk(&["◆ hello".to_owned(), "  world".to_owned()]);
        assert_eq!(text, "\n ◆ hello\n   world\n");
        assert_eq!(crate::ui::transcript::blocks::display_width("◆ hello"), 7);
    }
}
