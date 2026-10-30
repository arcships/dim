//! The interactive interface. The welcome banner is committed once into
//! scrollback; the conversation streams in as whole Markdown blocks (2.4), the
//! unclosed tail and the composer box live at the bottom and repaint by row
//! diff (2.3). The terminal layer (2.2) guarantees the exit contract.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::model::session::{
    Command, CompactReply, ContextReading, DraftPart, SessionPort, SwitchKind, Update,
};
#[cfg(test)]
use crate::model::shell::Outcome as ShellOutcome;
use crate::model::shell::ShellRun;
use crate::model::view::{
    CompactionState, LineState, LineView, RowKind, SideLine, TaskTarget, ViewRow, ViewSnapshot,
};
use crate::ui::Env;
use crate::ui::approvals_panel::{ApprovalsPanel, status_label};
use crate::ui::commands::{COMMANDS, Context as CommandContext, Needs};
use crate::ui::composer::{Composer, Outcome as ComposerOutcome, Shell, history};
use crate::ui::i18n::{self, t, tf};
use crate::ui::language_panel::LanguagePanel;
use crate::ui::lines_panel::{LinesPanel, short_id};
use crate::ui::models_panel::ModelsPanel;
use crate::ui::oauth_view::{DIM_OAUTH_ID, OauthView};
use crate::ui::plugins_panel::PluginsPanel;
use crate::ui::progress::ProgressBar;
use crate::ui::prompts::{AskUserPrompt, Effect, ExitConfirm, ExitReason, PermissionPrompt};
use crate::ui::providers_panel::ProvidersPanel;
use crate::ui::render::fullscreen::Document;
use crate::ui::render::fullscreen::MouseOutcome;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::render::panel::{clip, wrap, wrap_words};
use crate::ui::render::{Frame, FullScreen, Overlay, Screen};
use crate::ui::sessions_panel::SessionsPanel;
use crate::ui::settings_panel::{Setting, SettingsPanel, Values as SettingValues};
use crate::ui::status::{Reporter, Runs, Status};
use crate::ui::tasks_panel::TasksPanel;
use crate::ui::terminal::{
    InputEvent, KeyCode, KeyEvent, Modifiers, Terminal, TerminalError, Wait,
};
use crate::ui::text::{RUNNING_MARK, STATUS_MARK, display_width, truncate_cells};
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::theme::{Palette, Theme, detect};
use crate::ui::theme_panel::ThemePanel;
use crate::ui::timeline_panel::{TimelineAction, TimelinePanel};
use crate::ui::title::WindowTitle;
use crate::ui::transcript::block::{Block, Tone, group_rows, opens_group, project_row};
use crate::ui::transcript::blocks::{
    SHELL_PREVIEW_LINES, boundary_line, gutter_text, live_thinking, render_block, render_shell,
    width_of,
};
#[cfg(test)]
use crate::ui::transcript::blocks::{THINKING_ROWS, settled_thinking};
use crate::ui::transcript::commit::CommitState;
use crate::ui::transcript::reveal::Reveal;
use crate::ui::transcript::stream::{StreamPlanner, TextBlock, render_open};
use crate::ui::transcript::tool_group::{
    Detail, action_at, card_actions, card_rows, project_group, render_group,
};
use crate::ui::upgrade_dialog::UpgradeDialog;
use crate::usage::format_tokens;

const COMPOSER_PLACEHOLDER: &str = "Ask anything...";
const SHELL_PLACEHOLDER: &str = "Run a shell command · Esc to exit";
/// `config.json` key of the tool detail toggle, shared with the TS CLI.
const TOOL_DETAILS_KEY: &str = "tuiToolDetailsExpanded";
/// Share of the compaction threshold the context reading warns at. Set by
/// the interface; the threshold itself is the one `session.context.usage`
/// returns (compaction requirements §4).
const CONTEXT_WARNING_PERCENT: u64 = 90;
/// Frames are coalesced to roughly 60 Hz.
/// A resize waits this long after the last size change before it is applied.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
/// ...but never longer than this from the first change of a drag.
const RESIZE_MAX_DELAY: Duration = Duration::from_millis(500);
/// Committed blocks kept as sources; older ones are forgotten.
const HISTORY_MAX_BLOCKS: usize = 5_000;
/// Rows of history a scrollback rewrite lays out at most.
const REWRITE_MAX_ROWS: usize = 10_000;
/// In-place rewrites fullscreen can catch up on before it lays out again.
const REWRITE_LOG: usize = 64;
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
    /// Pick the user message drawn as this block id (`/timeline`).
    Block(usize),
    /// Scroll so this block id is at the top (after a page loads above it).
    Show(usize),
}

/// The card id of fullscreen's load-more row: no line or delegation id.
const LOAD_MORE: &str = "\u{1}load-more";

fn apply_jump(full: &mut FullScreen, app: &App, jump: Jump) {
    match jump {
        Jump::Bottom => full.follow(),
        Jump::Top => full.scroll_to_top(),
        Jump::Pick => full.focus_older(app),
        Jump::Block(id) => full.pick_block(id, app),
        Jump::Show(id) => full.show_block(id, app),
    }
}

/// How long a transient notice stays in the status row.
const NOTICE_TTL: Duration = Duration::from_millis(2500);
/// The live region's pulse: one beat per 250 ms.
const PULSE: Duration = Duration::from_millis(250);

pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// How the interface ended.
pub enum Leave {
    Exit(i32),
    /// "Restart now" after an update: start the updated CLI on the session.
    Restart(Restart),
}

/// What starts the updated CLI where this one leaves off.
pub struct Restart {
    pub relaunch: PathBuf,
    pub session_id: Option<String>,
    pub fullscreen: bool,
}

pub fn run(host: Box<dyn SessionPort>, surroundings: Env) -> Leave {
    let mut restart = None;
    let code = run_terminal(host, surroundings, &mut restart);
    match restart {
        Some(restart) => Leave::Restart(restart),
        None => Leave::Exit(code),
    }
}

/// Where the Web workspace is, as `dimcode web` reports it.
pub struct WebOpened<'a> {
    /// The workspace URL without its sign-in token.
    pub url: &'a str,
    /// The sign-in URL, shown when no browser was opened with it.
    pub sign_in: Option<&'a str>,
    pub session_id: Option<&'a str>,
}

/// Prints `opened` in the welcome panel's box, or as plain lines when
/// stdout is not a terminal.
pub fn print_web_panel(opened: &WebOpened) {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        println!("Web workspace  {}", opened.url);
        match opened.sign_in {
            Some(url) => println!("Open this link to sign in: {url}"),
            None => println!("Opened in your browser."),
        }
        println!("Press Ctrl+C to stop the Web workspace.");
        return;
    }
    let palette = Theme::detect(&detect::env_from_process(), true, None).palette;
    let cols = crate::ui::terminal::window_size().map_or(80, |(cols, _)| usize::from(cols));
    print!("{}", web_panel(cols, &palette, opened));
    if let Some(url) = opened.sign_in {
        println!("\n {}\n {url}\n", t("Sign-in link:"));
    }
}

fn run_terminal(
    host: Box<dyn SessionPort>,
    surroundings: Env,
    restart: &mut Option<Restart>,
) -> i32 {
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
    let mut status = Reporter::from_env();
    let mut screen = Screen::new(terminal.size(), synchronized);
    let mut full = None;
    if surroundings.fullscreen {
        if terminal.set_fullscreen(true).is_err() {
            return terminal.lost();
        }
        full = Some(FullScreen::new(terminal.size(), synchronized));
    } else if let Some((row, column)) = terminal.cursor_position() {
        screen.set_origin(if column == 0 { row } else { row + 1 });
    }
    let background = terminal
        .background_report()
        .and_then(detect::parse_background_report);
    let env = detect::env_from_process();
    let mut progress = ProgressBar::from_env(&env);
    let mut title = WindowTitle::new(crate::ui::title::apple_terminal(), Instant::now());
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
    app.terminal_clipboard =
        terminal.clipboard_protocol() && crate::clipboard::terminal::native_unreachable();
    app.host.send(Command::LoginStatus(DIM_OAUTH_ID.into()));
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
        if app.shutdown.is_some() && !dirty {
            break 0;
        }
        if app.revealing() {
            dirty = true;
        }
        for request in std::mem::take(&mut app.terminal_requests) {
            if terminal.write(&request).is_err() {
                let code = terminal.lost();
                app.host.shutdown();
                return code;
            }
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
        let program_status = app.program_status();
        if let Some(report) = status.report(program_status.clone())
            && terminal.report_status(report).is_err()
        {
            let code = terminal.lost();
            app.host.shutdown();
            return code;
        }
        if let Some(bar) = progress.report(&program_status, Instant::now())
            && terminal.report_progress(bar).is_err()
        {
            let code = terminal.lost();
            app.host.shutdown();
            return code;
        }
        if let Some(bytes) = title.report(&program_status, app.topic().as_deref(), Instant::now())
            && terminal.write(&bytes).is_err()
        {
            let code = terminal.lost();
            app.host.shutdown();
            return code;
        }
        if full.is_some() && terminal.mouse() != app.mouse && terminal.set_mouse(app.mouse).is_err()
        {
            let code = terminal.lost();
            app.host.shutdown();
            return code;
        }
        let now = Instant::now();
        let current = terminal.size();
        let laid_out = match &full {
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
            let keyboard = termux_height_only(
                screen.size().0,
                size,
                std::env::var_os("TERMUX_VERSION").is_some(),
            );
            if reflow || !keyboard {
                let rows = app.reflow_history(size.0 as usize);
                let frame = app.frame(size);
                let wanted = screen.wanted_for(size, frame.live.len());
                let mut bytes = screen.rewrite(size, &rows, wanted);
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
            // A jump the frame itself landed (a `/timeline` target or a
            // loaded page) moves the view once its blocks are laid out.
            if let Some(full) = &mut full
                && let Some(jump) = app.jump.take()
            {
                apply_jump(full, &app, jump);
                dirty = true;
            }
        }
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
                        crate::clipboard::copy_local(&text);
                        if terminal
                            .write(&crate::clipboard::osc52(
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
                    MouseOutcome::Click {
                        block: Some(block),
                        row,
                    } if full.card_at(&app, block, row).is_some() => {
                        let card = full.card_at(&app, block, row).expect("clicked card");
                        app.open_card(&card);
                        dirty = true;
                    }
                    MouseOutcome::Click { block, row } => {
                        if app.click(block, row) && block.is_some() {
                            full.hold();
                        }
                        dirty = true;
                    }
                    MouseOutcome::Open(url) => {
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
            Wait::Event(InputEvent::Key(key))
                if key.code == KeyCode::Esc && full.as_ref().is_some_and(FullScreen::selecting) =>
            {
                if let Some(full) = &mut full {
                    full.clear_selection();
                }
                dirty = true;
            }
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
                    KeyCode::Enter if key.mods.0 == 0 && full.focused_card().is_some() => {
                        let card = full.focused_card().expect("picked card").to_owned();
                        app.open_card(&card);
                    }
                    KeyCode::Char('e') if key.mods.0 == 0 => {
                        if let Some(text) = full.focused_text(&app) {
                            app.composer.set_draft(&text);
                        }
                        full.clear_focus();
                        full.follow();
                    }
                    KeyCode::Char(pressed @ ('b' | 'f')) if key.mods.0 == 0 => {
                        if let (Some(text), Some(at)) =
                            (full.focused_text(&app), full.focused_cut(&app))
                        {
                            if pressed == 'b' {
                                app.draft_line(at, &text);
                            } else {
                                app.ask_fork(at, text.clone(), Some(text));
                            }
                        }
                        full.clear_focus();
                        full.follow();
                    }
                    KeyCode::Char('q') if key.mods.0 == 0 => {
                        if let Some(text) = full.focused_text(&app)
                            && !app.composer.quote(&text)
                            && app.composer.shell().is_some()
                        {
                            app.set_notice(t("Leave shell mode to quote a message"));
                        }
                        full.clear_focus();
                        full.follow();
                    }
                    _ => {
                        full.clear_focus();
                        match app.handle(InputEvent::Key(key)) {
                            Outcome::Exit(code) => break code,
                            Outcome::Web => {
                                app.host.send(Command::ToggleWeb);
                                dirty = true;
                            }
                            _ => {}
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
                    if let Some(full) = &mut full
                        && let Some(jump) = app.jump.take()
                    {
                        apply_jump(full, &app, jump);
                    }
                    dirty = true;
                }
                Outcome::SwitchMode => {
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
                    if entering {
                        app.redraw_window();
                    }
                    app.request_reflow();
                    dirty = true;
                }
                Outcome::Suspend => {
                    if terminal.suspend().is_err() {
                        let code = terminal.lost();
                        app.host.shutdown();
                        return code;
                    }
                    app.request_reflow();
                    status.forget();
                    progress.forget();
                    title.forget();
                    dirty = true;
                }
                Outcome::Exit(code) => break code,
                Outcome::Web => {
                    app.host.send(Command::ToggleWeb);
                    dirty = true;
                }
                Outcome::Restart(relaunch) => {
                    *restart = Some(Restart {
                        relaunch,
                        session_id: app.session_active().then(|| app.session_id.clone()),
                        fullscreen: app.fullscreen,
                    });
                    break 0;
                }
            },
        }
    };
    let mut bytes = match &mut full {
        Some(full) => {
            let _ = terminal.write(&full.leave());
            let _ = terminal.set_fullscreen(false);
            Vec::new()
        }
        None => screen.leave(),
    };
    if restart.is_none() {
        let resume =
            (app.session_active() && !app.session_id.is_empty()).then_some(app.session_id.as_str());
        bytes.extend(format!("\r\n{}\r\n", goodbye_panel(resume).join("\r\n")).into_bytes());
    }
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
    /// `/web`: flip the daemon-owned Web entry switch without leaving.
    Web,
    /// Start the updated CLI from this path on the session.
    Restart(PathBuf),
    Exit(i32),
}

struct App {
    host: Box<dyn SessionPort>,
    composer: Composer,
    cwd_key: String,
    home: PathBuf,
    /// How tool groups are drawn by default (`/tools`, which redraws every
    /// committed group too).
    tool_detail: Detail,
    /// Groups the user opened or closed by a click, by their first call's
    /// row id: a running group keeps its form when it commits.
    group_details: HashMap<String, Detail>,
    /// The live tail's group headers of the last frame: tail row, first
    /// call's row id.
    tail_groups: Vec<(usize, String)>,
    notices: Vec<String>,
    /// Multi-line status blocks to commit (reports), with their tone.
    reports: Vec<(String, Tone)>,
    banner_committed: bool,
    snapshot: Option<ViewSnapshot>,
    /// `provider / model` once the session exists.
    label: Option<String>,
    failure: Option<String>,
    /// The App Server asked this interface to leave: one more frame shows
    /// why, then the interface exits and the Session carries on.
    shutdown: Option<String>,
    failure_committed: bool,
    planner: StreamPlanner,
    commits: CommitState,
    markdown: MarkdownRenderer,
    palette: Palette,
    /// The modal prompt on top of the composer, if any.
    prompt: Option<Prompt>,
    /// The question a panel took the screen from: it comes back, with its
    /// selections and drafts, once that panel closes and the Session still
    /// asks it.
    question_stash: Option<AskUserPrompt>,
    /// Requests already answered here: the projection keeps listing them
    /// until the daemon's confirmation arrives.
    answered: HashSet<String>,
    notice: Option<(String, Instant)>,
    exit_armed_until: Option<Instant>,
    run_started: Option<Instant>,
    last_pulse: u64,
    /// The most message rows the queue strip has shown in this run: the
    /// strip keeps that height as the queue drains, so the composer does
    /// not hop while messages leave; an empty queue or the next run starts
    /// afresh.
    queue_rows: usize,
    /// The most reply rows the side panel has shown since it opened: it
    /// keeps that height, so the composer does not hop as the reply wraps.
    side_rows: usize,
    /// Every committed block, oldest first, as its source: both interfaces
    /// lay it out again from here (a scrollback rewrite, the fullscreen
    /// virtual list). Bounded by `HISTORY_MAX_BLOCKS`.
    history: std::collections::VecDeque<Block>,
    /// Blocks dropped from the front of `history` so far.
    dropped: usize,
    /// The history id of the block the newest stream batch began; its next
    /// batches rewrite it.
    stream_block: Option<usize>,
    /// How many times history blocks were rewritten, and the ids of the
    /// newest rewrites (bounded by `REWRITE_LOG`).
    rewrites: u64,
    rewrite_log: std::collections::VecDeque<usize>,
    /// Committed groups (history ids) whose delegation cards still change:
    /// fullscreen redraws them from the snapshot.
    card_blocks: Vec<usize>,
    /// The second their running time was last drawn for.
    card_second: u64,
    session_id: String,
    /// The default tool-approval mode (wire name).
    approvals: String,
    /// The next approvals reading is the answer to a change: say so.
    approvals_toast: bool,
    /// The main branch's context use.
    context: Option<ContextReading>,
    /// The first attempt tag of the next `/login` panel, so a closed panel's
    /// late sign-in updates never reach a newer one.
    login_attempts: u64,
    theme_context: Option<ThemeContext>,
    /// The palette changed: the visible rows are laid out again.
    reflow_requested: bool,
    /// Rows of the snapshot before this index are earlier history: not
    /// drawn until a `/timeline` jump or fullscreen's load-more row asks.
    window_floor: usize,
    /// How far scrollback has drawn back; never above `window_floor`.
    history_floor: usize,
    /// Fullscreen owes its load-more row before the next transcript commit.
    load_more: bool,
    /// The user message a `/timeline` jump or a loaded page lands on, once
    /// drawn: `(row id, pick it)`.
    landing: Option<(String, bool)>,
    /// A session was just switched; the next snapshot decides the window.
    awaiting_window: bool,
    /// What to write once the switched-to history is out.
    pending_boundary: Option<SwitchKind>,
    /// The line the transcript switched to, until its boundary row is out.
    pending_line: Option<String>,
    /// `(messages in the window, messages before it)` of the last switch.
    window_info: (usize, usize),
    /// Rows `[start, end)` queued to be drawn as an earlier-history page.
    history_request: Option<(usize, usize)>,
    /// The fullscreen interface is on screen.
    fullscreen: bool,
    /// The Dim account is signed in (the host's last report).
    signed_in: bool,
    /// The session's model offers `/fast`'s service tiers.
    service_tiers: bool,
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
    /// The session's runs as the terminal's program status follows them.
    runs: Runs,
    /// The draft set aside while Ctrl+O/Ctrl+P has the palette open; it
    /// comes back when command entry ends.
    palette_stash: Option<String>,
    /// Command entry over a pending question: the composer has the keys
    /// from the palette through the command's argument until it runs
    /// ([`Self::command_entry_open`]).
    command_entry: bool,
    /// The workspace's git branch for the banner, read once.
    branch: String,
    /// The shell-mode command running, at most one.
    shell: Option<crate::ui::shell::Running>,
    /// A Ctrl+V clipboard read in progress.
    clipboard: Option<ClipboardRead>,
    /// Ctrl+V reads through the terminal (OSC 5522) instead of natively.
    terminal_clipboard: bool,
    /// Clipboard reads started, numbering their request ids.
    clipboard_reads: u64,
    /// Requests owed to the terminal, written by the event loop.
    terminal_requests: Vec<Vec<u8>>,
    /// Finished `!!` runs owed to the transcript.
    shell_runs: Vec<ShellRun>,
    /// The output detail currently registered with the session worker.
    output_target: Option<(u64, TaskTarget)>,
    member_target: Option<(u64, String)>,
    /// The update dialog while another prompt has the screen; it comes
    /// back once that one closes.
    upgrade: Option<UpgradeDialog>,
    /// A line about to open on the shown line: the next submit is its
    /// opening input (timeline UI §4.2).
    line_draft: Option<LineDraft>,
    /// A fork the user asked for, until they answer (timeline UI §4.3).
    fork_confirm: Option<ForkAsk>,
    /// The picked message, set in the fork's composer once it is attached.
    fork_draft: Option<String>,
}

/// Where a line the user is drafting opens.
/// A fork waiting for its `y`.
struct ForkAsk {
    at: u64,
    /// What the fork's composer starts with.
    text: String,
    /// The picked message the fork cuts before; none for `/fork`, which
    /// cuts after the last turn.
    before: Option<String>,
}

struct LineDraft {
    session_id: String,
    projection_id: u64,
    parent: String,
    at: u64,
}

/// Widest the fullscreen command palette is drawn.
const OVERLAY_PALETTE_WIDTH: usize = 80;
/// Widest a fullscreen picker is drawn.
const OVERLAY_LIST_WIDTH: usize = 100;
/// Tallest a fullscreen overlay is drawn.
const OVERLAY_MAX_ROWS: usize = 30;
/// Least rows inside the fullscreen composer box.
const FULLSCREEN_COMPOSER_ROWS: usize = 3;

/// Messages a resume shows at first, and per load-more page.
const HISTORY_WINDOW: usize = 100;

enum ClipboardRead {
    Native(std::sync::mpsc::Receiver<crate::Result<crate::clipboard::Source>>),
    Terminal(crate::clipboard::terminal::Read),
}

enum Prompt {
    Sessions(SessionsPanel),
    Tasks(TasksPanel),
    Lines(Box<LinesPanel>),
    Timeline(Box<TimelinePanel>),
    Models(ModelsPanel),
    Providers(Box<ProvidersPanel>),
    Plugins(PluginsPanel),
    Login(Box<OauthView>),
    Approvals(ApprovalsPanel),
    Theme(ThemePanel),
    Language(LanguagePanel),
    Settings(SettingsPanel),
    Isolation(Box<crate::ui::isolation_panel::IsolationPanel>),
    Exit(ExitConfirm),
    Upgrade(UpgradeDialog),
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
        let mouse = crate::ui::config::read(&home, MOUSE_KEY).is_none_or(|v| v != "off");
        let notify = crate::ui::config::read(&home, crate::ui::notify::NOTIFY_KEY)
            .is_none_or(|v| v != false);
        let mut composer = Composer::new(past);
        composer.set_root(PathBuf::from(&cwd_key));
        Self {
            host,
            composer,
            cwd_key,
            tool_detail: saved_tool_detail(&home),
            group_details: HashMap::new(),
            tail_groups: Vec::new(),
            home,
            notices: Vec::new(),
            reports: Vec::new(),
            banner_committed: false,
            snapshot: None,
            label: None,
            failure: None,
            shutdown: None,
            failure_committed: false,
            planner: StreamPlanner::new(),
            commits: CommitState::default(),
            markdown,
            palette,
            prompt: None,
            question_stash: None,
            answered: HashSet::new(),
            notice: None,
            exit_armed_until: None,
            run_started: None,
            queue_rows: 0,
            side_rows: 0,
            last_pulse: 0,
            history: Default::default(),
            dropped: 0,
            stream_block: None,
            rewrites: 0,
            rewrite_log: Default::default(),
            card_blocks: Vec::new(),
            card_second: 0,
            session_id: String::new(),
            approvals: "workspace-write".to_owned(),
            context: None,
            approvals_toast: false,
            login_attempts: 0,
            theme_context: None,
            reflow_requested: false,
            window_floor: 0,
            history_floor: 0,
            load_more: false,
            landing: None,
            awaiting_window: false,
            pending_boundary: None,
            pending_line: None,
            window_info: (0, 0),
            history_request: None,
            fullscreen: false,
            signed_in: false,
            service_tiers: false,
            jump: None,
            settings_return: None,
            mouse,
            notify,
            notifications: Vec::new(),
            revealed: std::collections::HashMap::new(),
            notified_request: None,
            runs: Runs::default(),
            palette_stash: None,
            command_entry: false,
            branch: git_branch().unwrap_or_else(|| t("Not available").into()),
            shell: None,
            clipboard: None,
            terminal_clipboard: false,
            clipboard_reads: 0,
            terminal_requests: Vec::new(),
            shell_runs: Vec::new(),
            output_target: None,
            member_target: None,
            upgrade: None,
            line_draft: None,
            fork_confirm: None,
            fork_draft: None,
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
        self.prompt.is_none() && !self.composer.palette_open() && !self.composer.mention_open()
    }

    /// Command entry is open over a pending question that is not being
    /// edited: the composer owns the keys and takes the panel's place until
    /// the command runs, Esc, or a draft that stops being a command. It
    /// starts with `/`, Ctrl+O or Ctrl+P, or with a command draft the
    /// question arrived under, and lasts past the palette through the
    /// command's argument.
    fn command_entry_open(&self) -> bool {
        (self.command_entry || self.composer.palette_open())
            && matches!(&self.prompt, Some(Prompt::Ask(prompt)) if !prompt.typing)
    }

    /// Opens the command palette on the draft it replaces; the draft comes
    /// back when command entry ends ([`Self::handle`]).
    fn open_palette(&mut self) {
        if !self.composer.is_empty() {
            self.palette_stash = Some(self.composer.text().to_owned());
        }
        self.composer.set_draft("/");
    }

    /// Shows `prompt` in the prompt slot, keeping a pending question aside
    /// so the panel gives the question back with its answers when it closes.
    fn open_prompt(&mut self, prompt: Prompt) {
        if matches!(self.prompt, Some(Prompt::Sessions(_))) {
            self.host.send(Command::CloseSessions);
        }
        if let Some(Prompt::Ask(question)) = self.prompt.take() {
            self.question_stash = Some(question);
        }
        self.prompt = Some(prompt);
    }

    fn sync_sessions(&mut self) {
        if let Some(Prompt::Sessions(panel)) = &mut self.prompt
            && let Some(command) = panel.request()
        {
            self.host.send(command);
        }
    }

    /// Puts the question a panel took the screen from back, once the slot
    /// is free and the Session still asks it first. A question answered
    /// elsewhere or interrupted meanwhile is gone, and stays gone.
    fn restore_question(&mut self) {
        if self.prompt.is_some() || self.question_stash.is_none() {
            return;
        }
        let asked = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.questions.first());
        if asked == self.question_stash.as_ref().map(|held| &held.call) {
            self.prompt = self.question_stash.take().map(Prompt::Ask);
        } else {
            self.question_stash = None;
        }
    }

    fn running(&self) -> bool {
        self.failure.is_none() && self.snapshot.as_ref().is_some_and(|s| s.running)
    }

    /// What the command palette weighs availability against.
    fn context(&self) -> CommandContext {
        CommandContext {
            session: self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.session_id.is_empty()),
            running: self.running(),
            queued: !self.queued().is_empty(),
            fullscreen: self.fullscreen,
            signed_in: self.signed_in,
            service_tiers: self.service_tiers,
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
        let working = snapshot.main_running
            || snapshot.running
            || snapshot
                .lines
                .iter()
                .any(|line| line.state == LineState::Working);
        working.then_some(ExitReason::AgentRunning)
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
        let question = snapshot.questions.first();
        match &self.prompt {
            Some(Prompt::Exit(_))
            | Some(Prompt::Sessions(_))
            | Some(Prompt::Models(_))
            | Some(Prompt::Providers(_))
            | Some(Prompt::Plugins(_))
            | Some(Prompt::Login(_))
            | Some(Prompt::Approvals(_))
            | Some(Prompt::Theme(_))
            | Some(Prompt::Language(_))
            | Some(Prompt::Settings(_))
            | Some(Prompt::Isolation(_))
            | Some(Prompt::Lines(_))
            | Some(Prompt::Timeline(_)) => {}
            Some(Prompt::Tasks(_)) if approval.is_none() && question.is_none() => {}
            Some(Prompt::Upgrade(_)) if approval.is_none() && question.is_none() => {}
            Some(Prompt::Permission(p))
                if approval.is_some_and(|a| a.request_id == p.request_id) => {}
            Some(Prompt::Ask(p)) if question.is_some_and(|q| q == &p.call) => {}
            _ => {
                if let Some(Prompt::Upgrade(dialog)) = self.prompt.take() {
                    self.upgrade = Some(dialog);
                }
                self.prompt = if let Some(a) = approval {
                    Some(Prompt::Permission(PermissionPrompt::new(
                        a.request_id.clone(),
                        a.branch_id.clone(),
                        a.reason.clone(),
                        a.tool_name.clone(),
                        a.tool_input.clone(),
                        a.always.is_some(),
                    )))
                } else {
                    question.map(|q| {
                        let held = self.question_stash.take().filter(|held| held.call == *q);
                        Prompt::Ask(held.unwrap_or_else(|| AskUserPrompt::new(q.clone())))
                    })
                };
            }
        }
        self.reopen_upgrade();
    }

    /// Puts back what another prompt took the screen from: the question
    /// first, since a pending question outranks the update dialog, then the
    /// dialog.
    fn reopen_upgrade(&mut self) {
        self.restore_question();
        if self.prompt.is_none() {
            self.prompt = self.upgrade.take().map(Prompt::Upgrade);
        }
    }

    /// Time-driven changes: the pulse of a running turn, notice and arm
    /// expiry. True when the screen needs a frame.
    fn tick(&mut self) -> bool {
        self.sync_sessions();
        self.sync_task_details();
        let now = Instant::now();
        let mut changed = false;
        if let Some(Prompt::Tasks(panel)) = &mut self.prompt
            && let Some(snapshot) = &self.snapshot
            && panel.tick(snapshot)
        {
            changed = true;
        }
        if let Some((_, until)) = &self.notice
            && now >= *until
        {
            self.notice = None;
            changed = true;
        }
        if self.exit_armed_until.is_some_and(|until| now >= until) {
            self.exit_armed_until = None;
        }
        if self.failure.is_none() && self.shell.is_some() {
            changed = true;
        }
        let second = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if !self.card_blocks.is_empty() && second != self.card_second {
            self.card_second = second;
            self.refresh_cards();
            changed = true;
        }
        if self.composer.mention_tick() {
            changed = true;
        }
        let finished = match &self.clipboard {
            Some(ClipboardRead::Native(read)) => read.try_recv().ok(),
            Some(ClipboardRead::Terminal(read)) if read.expired(now) => {
                Some(Err("the terminal did not answer".into()))
            }
            _ => None,
        };
        if let Some(source) = finished {
            self.clipboard = None;
            self.paste_clipboard(source);
            changed = true;
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
            Update::ModelFeatures {
                images,
                service_tiers,
            } => {
                self.composer.set_accepts_images(images);
                self.service_tiers = service_tiers;
            }
            Update::Snapshot(snapshot) => {
                let was_running = self.running();
                let switched = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|shown| shown.focus != snapshot.focus);
                if switched {
                    self.switch_line(snapshot.focus.clone());
                }
                if let Some(Prompt::Tasks(panel)) = &mut self.prompt {
                    if panel.projection_id == snapshot.projection_id {
                        panel.set_tasks(&snapshot);
                    } else {
                        self.prompt = None;
                    }
                }
                if let Some(Prompt::Lines(panel)) = &mut self.prompt {
                    if panel.current(&snapshot) {
                        panel.set_lines(&snapshot);
                    } else {
                        self.prompt = None;
                    }
                }
                if let Some(Prompt::Timeline(panel)) = &mut self.prompt {
                    if panel.current(&snapshot) {
                        panel.set_rows(&snapshot);
                    } else {
                        self.prompt = None;
                    }
                }
                self.runs.observe(&snapshot);
                self.snapshot = Some(*snapshot);
                self.refresh_cards();
                if self.running() && !was_running {
                    self.run_started = Some(Instant::now());
                    self.last_pulse = 0;
                }
                if !self.running() {
                    self.run_started = None;
                    self.queue_rows = 0;
                }
                self.sync_prompt();
                self.queue_notification(was_running && !switched);
            }
            Update::QuestionReply {
                session_id,
                call,
                reply,
            } => {
                if session_id == self.session_id
                    && let Some(Prompt::Ask(prompt)) = &mut self.prompt
                    && prompt.call == call
                {
                    match reply {
                        crate::question::Reply::Accepted => {}
                        crate::question::Reply::NotPending => {
                            prompt.sending = false;
                            prompt.error =
                                Some(t("Question not ready; press Enter to send again").to_owned());
                        }
                        crate::question::Reply::Rejected(error)
                        | crate::question::Reply::Failed(error) => {
                            prompt.sending = false;
                            prompt.error = Some(error);
                        }
                    }
                }
            }
            Update::TaskOutput {
                projection_id,
                target,
                text,
                state,
            } => {
                if let Some(Prompt::Tasks(panel)) = &mut self.prompt
                    && panel.projection_id == projection_id
                {
                    panel.output(&target, text, state);
                }
            }
            Update::MemberConversation {
                projection_id,
                session_id,
                snapshot,
                state,
                model,
            } => {
                if let Some(Prompt::Tasks(panel)) = &mut self.prompt
                    && panel.projection_id == projection_id
                {
                    panel.member(&session_id, snapshot, state, model);
                }
            }
            Update::FilesChanged(count) => {
                self.notices.push(tf(
                    "files changed  {count}",
                    &[("count", &count.to_string())],
                ));
            }
            Update::Switched { session_id, kind } => self.switch_session(session_id, kind),
            Update::Sessions { id, paged, result } => {
                if let Some(Prompt::Sessions(panel)) = &mut self.prompt {
                    panel.page(&id, paged, result);
                }
            }
            Update::Toast(text) => self.set_notice(text),
            Update::Report { text, warning } => self
                .reports
                .push((text, if warning { Tone::Warning } else { Tone::Muted })),
            Update::Web { enabled, open, url } => {
                if enabled && open {
                    match url {
                        Some(url) => self
                            .reports
                            .push((format!("Web interface on: {url}"), Tone::Muted)),
                        None => self.notices.push("Web interface is on".to_owned()),
                    }
                } else if open {
                    self.notices.push(
                        "Web switch off; the entry is still held by another client".to_owned(),
                    );
                } else {
                    self.notices.push("Web interface off".to_owned());
                }
            }
            Update::Approvals(mode) => match mode {
                Ok(mode) => {
                    if std::mem::take(&mut self.approvals_toast) {
                        let title = crate::ui::approvals_panel::announced_title(&mode);
                        match crate::ui::config::save_permission(&self.home, &mode) {
                            Some(error) => self.set_notice(error),
                            None => self.set_notice(tf(
                                "Tool approvals: {title} (also for new sessions)",
                                &[("title", title)],
                            )),
                        }
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
            Update::Plugins(plugins) => {
                if let Some(Prompt::Plugins(panel)) = &mut self.prompt {
                    panel.set_items(plugins);
                }
            }
            Update::PluginResult {
                action,
                result,
                plugins,
            } => {
                if let Some(Prompt::Plugins(panel)) = &mut self.prompt {
                    panel.set_result(&action, result, plugins);
                }
            }
            Update::Login(account) => {
                let signed_in = matches!(account, Ok(Some(_)));
                self.signed_in = signed_in;
                if let Some(Prompt::Login(view)) = &mut self.prompt {
                    view.set_account(account);
                    if signed_in {
                        self.host.send(Command::LoginUsage(DIM_OAUTH_ID.into()));
                    }
                }
            }
            Update::Usage(usage) => {
                if let Some(Prompt::Login(view)) = &mut self.prompt {
                    view.set_usage(usage);
                }
            }
            Update::Context(context) => self.context = context,
            Update::Compacted(reply) => {
                let (text, tone) = match reply {
                    CompactReply::Completed => (t("Context compacted").to_owned(), Tone::Muted),
                    CompactReply::Skipped(reason) => (
                        [t("Context compaction skipped"), " · ", reason.as_str()].concat(),
                        Tone::Muted,
                    ),
                    CompactReply::Failed(message) => (
                        [t("Context compaction failed"), " · ", message.as_str()].concat(),
                        Tone::Warning,
                    ),
                    CompactReply::Cancelled(reason) => (
                        [t("Context compaction cancelled"), " · ", reason.as_str()].concat(),
                        Tone::Muted,
                    ),
                    CompactReply::Unsupported => (
                        t("This daemon does not support context compaction").to_owned(),
                        Tone::Warning,
                    ),
                };
                self.reports.push((text, tone));
            }
            Update::Oauth { attempt, event } => {
                let effect = match &mut self.prompt {
                    Some(Prompt::Providers(panel)) => panel.oauth_event(attempt, event),
                    Some(Prompt::Login(view)) => view.event(attempt, event),
                    _ => Effect::None,
                };
                self.apply_effect(effect);
            }
            Update::ProviderResult { result, providers } => {
                if let Some(Prompt::Providers(panel)) = &mut self.prompt {
                    panel.set_result(result);
                    panel.set_providers(providers);
                }
            }
            Update::Models(models) => {
                if let Some(Prompt::Models(panel)) = &mut self.prompt {
                    panel.set_models(models);
                }
            }
            Update::ModelsRefilled => {
                if matches!(self.prompt, Some(Prompt::Models(_))) {
                    self.host.send(Command::ListModels);
                }
            }
            Update::Isolation(result) => {
                if let Some(Prompt::Isolation(panel)) = &mut self.prompt {
                    panel.set_snapshot(result);
                }
            }
            Update::ModelPolicy(update) => {
                if let Some(Prompt::Models(panel)) = &mut self.prompt
                    && let Some(notice) = panel.set_policy(update)
                {
                    self.set_notice(notice);
                }
            }
            Update::ShellProgress {
                run_id,
                text,
                skipped,
            } => {
                if let Some(running) = self.shell.as_mut().filter(|r| r.run_id == run_id) {
                    running.progress(&text, skipped);
                }
            }
            Update::ShellDone { run_id, result } => {
                if self.shell.as_ref().is_some_and(|r| r.run_id == run_id) {
                    let running = self.shell.take().expect("shell run");
                    self.shell_done(running, result);
                }
            }
            Update::Notice(text) => {
                self.notices.push(text);
            }
            Update::Upgrade(offer) => {
                self.upgrade = Some(UpgradeDialog::new(offer));
                self.reopen_upgrade();
            }
            Update::UpgradeInstalled { version, result } => {
                match (&mut self.prompt, &mut self.upgrade) {
                    (Some(Prompt::Upgrade(dialog)), _) | (_, Some(dialog)) => {
                        dialog.installed(result)
                    }
                    _ => self.notices.push(UpgradeDialog::outcome(&version, &result)),
                }
            }
            Update::Failed(message) => {
                self.failure = Some(message);
                self.run_started = None;
            }
            Update::Shutdown { reason } => {
                self.notices.push(tf(
                    "The App Server closed this interface: {reason}",
                    &[("reason", &reason)],
                ));
                self.shutdown = Some(reason);
            }
        }
    }

    fn handle(&mut self, event: InputEvent) -> Outcome {
        let outcome = self.handle_input(event);
        self.restore_question();
        self.command_entry = self.command_entry_open() && self.composer.text().starts_with('/');
        if !self.command_entry
            && self.composer.is_empty()
            && let Some(draft) = self.palette_stash.take()
        {
            self.composer.set_draft(&draft);
        }
        outcome
    }

    fn handle_input(&mut self, event: InputEvent) -> Outcome {
        let key = match event {
            InputEvent::Paste(text) => {
                if self.command_entry_open() {
                    self.paste(text);
                    return Outcome::Continue;
                }
                match &mut self.prompt {
                    None => self.paste(text),
                    Some(Prompt::Providers(panel)) => panel.paste(&text),
                    Some(Prompt::Login(view)) => view.paste(&text),
                    Some(Prompt::Sessions(panel)) => panel.paste(&text),
                    Some(Prompt::Tasks(panel)) => panel.paste(&text),
                    Some(Prompt::Lines(panel)) => panel.paste(&text),
                    Some(Prompt::Models(panel)) => panel.paste(&text),
                    Some(Prompt::Theme(panel)) => panel.paste(&text),
                    Some(Prompt::Language(panel)) => panel.paste(&text),
                    Some(Prompt::Settings(panel)) => panel.paste(&text),
                    Some(Prompt::Isolation(panel)) => panel.paste(&text),
                    Some(Prompt::Ask(prompt)) => prompt.paste(&text),
                    Some(_) => {}
                }
                return Outcome::Continue;
            }
            InputEvent::Key(key) => key,
            InputEvent::Mouse(_) => return Outcome::Continue,
            InputEvent::Clipboard(reply) => {
                use crate::clipboard::terminal::Step;
                let Some(ClipboardRead::Terminal(read)) = &mut self.clipboard else {
                    return Outcome::Continue;
                };
                match read.feed(reply, Instant::now()) {
                    Step::Pending => {}
                    Step::Send(request) => self.terminal_requests.push(request),
                    Step::Done(source) => {
                        self.clipboard = None;
                        self.paste_clipboard(source);
                    }
                }
                return Outcome::Continue;
            }
        };
        let ctrl = key.mods.contains(Modifiers::CTRL);
        if self.failure.is_some() {
            if (ctrl && matches!(key.code, KeyCode::Char('c' | 'd')))
                || (key.code == KeyCode::Enter && self.composer.text().trim() == "/exit")
            {
                return Outcome::Exit(0);
            }
            if key.code == KeyCode::Enter {
                self.set_notice(t("Disconnected; draft kept. Exit and resume this Session."));
            } else if key.code == KeyCode::Esc {
                self.prompt = None;
            } else if self.prompt.is_none() {
                self.composer.handle_key(key);
            }
            return Outcome::Continue;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c();
        }
        if let Some(ask) = self.fork_confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) && key.mods == Modifiers::NONE {
                self.fork(ask.at, ask.text);
            }
            return Outcome::Continue;
        }
        // A question keeps the keys, but a command entered over it keeps
        // them from `/`, Ctrl+O or Ctrl+P through its argument: only Enter,
        // Esc or a draft that stops being a command gives them back.
        if matches!(&self.prompt, Some(Prompt::Ask(prompt)) if !prompt.typing) {
            if self.command_entry_open() {
                self.command_entry = true;
                if key.code == KeyCode::Esc
                    && !self.composer.palette_open()
                    && !self.composer.mention_open()
                {
                    self.composer.clear();
                    return Outcome::Continue;
                }
                return self.composer_key(key);
            }
            if (key.code == KeyCode::Char('/') && key.mods.0 & !Modifiers::SHIFT.0 == 0)
                || (ctrl && matches!(key.code, KeyCode::Char('o' | 'p')))
            {
                self.open_palette();
                self.command_entry = true;
                return Outcome::Continue;
            }
        }
        if let Some(prompt) = &mut self.prompt {
            let effect = match prompt {
                Prompt::Sessions(p) => p.handle_key(key),
                Prompt::Tasks(p) => p.handle_key(key),
                Prompt::Lines(p) => p.handle_key(key),
                Prompt::Timeline(p) => p.handle_key(key),
                Prompt::Models(p) => p.handle_key(key),
                Prompt::Providers(p) => p.handle_key(key),
                Prompt::Plugins(p) => p.handle_key(key),
                Prompt::Login(p) => match p.handle_key(key) {
                    Effect::OauthBack => Effect::Close,
                    Effect::OauthCancel => {
                        self.host.send(Command::OauthCancel);
                        Effect::Close
                    }
                    other => other,
                },
                Prompt::Approvals(p) => p.handle_key(key),
                Prompt::Theme(p) => p.handle_key(key),
                Prompt::Language(p) => p.handle_key(key),
                Prompt::Settings(p) => p.handle_key(key),
                Prompt::Isolation(p) => p.handle_key(key),
                Prompt::Exit(p) => p.handle_key(key),
                Prompt::Upgrade(p) => p.handle_key(key),
                Prompt::Permission(p) => p.handle_key(key),
                Prompt::Ask(p) => p.handle_key(key),
            };
            let outcome = self.apply_effect(effect);
            self.sync_task_details();
            return outcome;
        }
        self.composer_key(key)
    }

    /// A key for the composer: the palette, the shortcuts around it and the
    /// draft editor, with what a submit, a command or a shell line does.
    fn composer_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.mods.contains(Modifiers::CTRL);
        match key.code {
            KeyCode::Char('o' | 'p') if ctrl && !self.composer.palette_open() => {
                self.open_palette();
                return Outcome::Continue;
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    return Outcome::Exit(0);
                }
            }
            KeyCode::Char('v') if ctrl && self.composer.shell().is_none() => {
                self.read_clipboard();
                return Outcome::Continue;
            }
            KeyCode::Enter if key.mods.0 == 0 && self.clipboard.is_some() => {
                self.set_notice(t("Wait for clipboard insertion before sending"));
                return Outcome::Continue;
            }
            KeyCode::Char('s') if ctrl => {
                self.steer();
                return Outcome::Continue;
            }
            KeyCode::Esc if !self.composer.palette_open() && !self.composer.mention_open() => {
                if self.snapshot.as_mut().and_then(|s| s.side.take()).is_some() {
                    self.host.send(Command::CloseSide);
                } else if self.line_draft.take().is_some() {
                    self.composer.clear();
                } else if self.shell.is_some() {
                    self.stop_shell();
                } else if self.running() {
                    self.interrupt();
                } else if !self.composer.is_empty() || self.composer.shell().is_some() {
                    self.composer.clear();
                }
                return Outcome::Continue;
            }
            KeyCode::Enter
                if key.mods.0 == 0
                    && self.shell.is_some()
                    && (self.composer.shell().is_some()
                        || self.composer.text().trim_start().starts_with('!')) =>
            {
                self.set_notice(t("A shell command is already running"));
                return Outcome::Continue;
            }
            KeyCode::Up
                if key.mods.0 == 0 && self.composer.is_empty() && !self.queued().is_empty() =>
            {
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
            ComposerOutcome::Submit(text) if self.line_draft.is_some() => {
                self.open_line(&text);
            }
            ComposerOutcome::SubmitDraft { text, parts } if self.line_draft.is_some() => {
                if parts.iter().any(|part| matches!(part, DraftPart::Block(_))) {
                    self.set_notice(t(
                        "A line opens from text; images and files were not included",
                    ));
                }
                self.open_line(&text);
            }
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
            ComposerOutcome::SubmitDraft { text, parts } => {
                let text = text.trim().to_owned();
                let _ = history::append(&self.home, &self.cwd_key, &text);
                self.composer.push_history(&text);
                self.jump = Some(Jump::Bottom);
                if self.running() {
                    self.set_notice(t("Message queued"));
                }
                self.host.send(Command::SubmitDraft(parts));
            }
            ComposerOutcome::Shell { command, shell } => {
                let entry = format!("{}{command}", shell.prefix());
                let _ = history::append(&self.home, &self.cwd_key, &entry);
                self.composer.push_history(&entry);
                self.jump = Some(Jump::Bottom);
                let running = crate::ui::shell::Running::new(command, shell == Shell::Private);
                self.host.send(Command::ShellRun {
                    run_id: running.run_id.clone(),
                    command: running.command.clone(),
                    private: running.private,
                });
                self.shell = Some(running);
            }
            ComposerOutcome::Command { name, args } => return self.run_command(&name, &args),
            ComposerOutcome::Rejected(reason) => self.set_notice(reason),
            ComposerOutcome::Handled | ComposerOutcome::Pass => {}
        }
        Outcome::Continue
    }

    /// A terminal paste, which is also how a terminal delivers a file
    /// dragged onto it: local files and directories it names are inserted
    /// as `[name]` tokens, except into a shell command.
    fn paste(&mut self, text: String) {
        let base = std::path::Path::new(&self.cwd_key);
        let paths = match self.composer.shell() {
            Some(_) => None,
            None => crate::attachments::pasted_entries(&text, base),
        };
        match paths {
            Some(paths) => {
                if let Err(error) = self.composer.attach_files(&paths) {
                    self.set_notice(error);
                }
            }
            None => self.composer.paste(&text),
        }
    }

    /// Ctrl+V: reads the native clipboard off the interface thread, or asks
    /// the terminal for its clipboard when the native one is out of reach.
    fn read_clipboard(&mut self) {
        if self.clipboard.is_some() {
            return;
        }
        if self.terminal_clipboard {
            self.clipboard_reads += 1;
            let (read, request) = crate::clipboard::terminal::Read::start(
                format!("dim-{}", self.clipboard_reads),
                Instant::now(),
            );
            self.clipboard = Some(ClipboardRead::Terminal(read));
            self.terminal_requests.push(request);
            self.set_notice(t("Reading clipboard…"));
            return;
        }
        let (send, receive) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new()
            .name("dim-clipboard".into())
            .spawn(move || {
                let read = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(Into::into)
                    .and_then(|runtime| runtime.block_on(crate::clipboard::read()));
                let _ = send.send(read);
            });
        match started {
            Ok(_) => {
                self.clipboard = Some(ClipboardRead::Native(receive));
                self.set_notice(t("Reading clipboard…"));
            }
            Err(error) => {
                self.set_notice(tf("Clipboard: {reason}", &[("reason", &error.to_string())]))
            }
        }
    }

    fn paste_clipboard(&mut self, source: crate::Result<crate::clipboard::Source>) {
        use crate::clipboard::Source;
        let inserted = match source {
            Ok(Source::Text(text)) => {
                self.paste(text);
                Ok(())
            }
            Ok(Source::Files(paths)) => self.composer.attach_files(&paths),
            Ok(Source::Image(image)) => self.composer.attach_image(image),
            Err(error) => Err(error.to_string()),
        };
        match inserted {
            Ok(()) => self.notice = None,
            Err(reason) => self.set_notice(tf("Clipboard: {reason}", &[("reason", &reason)])),
        }
    }

    /// Asks the running shell command to end, once.
    fn stop_shell(&mut self) {
        if let Some(running) = self.shell.as_mut().filter(|r| !r.stopping) {
            running.stopping = true;
            self.host.send(Command::ShellStop {
                run_id: running.run_id.clone(),
            });
        }
    }

    /// A finished run: a recorded one comes back as the session's own
    /// event, so only a private run, or one that was not recorded, is
    /// committed here.
    fn shell_done(
        &mut self,
        running: crate::ui::shell::Running,
        result: Result<crate::model::shell::ShellResult, String>,
    ) {
        use crate::model::shell::Recorded;
        let run = running.finish(&result);
        match &result {
            Ok(reply) if !run.private => match &reply.recorded {
                Some(Recorded::Written { .. }) => return,
                Some(Recorded::Failed { error }) => self.set_notice(tf(
                    "Shell output was not recorded: {reason}",
                    &[("reason", &error.message)],
                )),
                None => {}
            },
            Ok(_) => {}
            Err(reason) => self.set_notice(tf("Shell: {reason}", &[("reason", reason)])),
        }
        self.shell_runs.push(run);
    }

    /// Ctrl+C is staged: in scrollback a busy session is interrupted first
    /// and asks for confirmation the second time; an idle one, and any
    /// session in fullscreen (where Esc interrupts), clears the draft, then
    /// warns, then exits — through the confirmation while busy.
    fn ctrl_c(&mut self) -> Outcome {
        if self.shell.is_some() && !matches!(self.prompt, Some(Prompt::Ask(_))) {
            self.stop_shell();
            return Outcome::Continue;
        }
        if self.composer.is_empty()
            && self.composer.shell().is_some()
            && !matches!(self.prompt, Some(Prompt::Ask(_)))
        {
            self.composer.clear();
            return Outcome::Continue;
        }
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

    fn interrupt(&mut self) {
        if let Some(Prompt::Ask(prompt)) = &self.prompt {
            self.host.send(Command::StopQuestion {
                session_id: self.session_id.clone(),
                branch_id: prompt.call.branch_id.clone(),
                stop_turn: self.running(),
            });
        } else {
            self.host.stop();
        }
        self.set_notice(t("Stopping"));
    }

    fn place_label(&self) -> String {
        if let Some(ask) = &self.fork_confirm {
            return match &ask.before {
                Some(message) => tf(
                    "Fork the session before “{message}”? (y/N)",
                    &[(
                        "message",
                        &truncate_cells(
                            &crate::sanitize::safe(message.lines().next().unwrap_or_default()),
                            32,
                        ),
                    )],
                ),
                None => t("Fork the session after the last turn? (y/N)").to_owned(),
            };
        }
        if let Some(draft) = &self.line_draft {
            let lines = self.snapshot.as_ref().map_or(&[][..], |s| &s.lines[..]);
            return tf(
                "⑂ new line from {id} · Esc cancel",
                &[("id", &line_name(lines, &draft.parent))],
            );
        }
        let workspace = workspace_label(&self.branch);
        match &self.snapshot {
            Some(snapshot) if !snapshot.focus.is_empty() && snapshot.focus != "main" => {
                format!(
                    "{workspace} · ⑂ {}",
                    line_path(&snapshot.lines, &snapshot.focus)
                )
            }
            _ => workspace,
        }
    }

    /// Opens the drafted line with `text` as its opening input.
    fn open_line(&mut self, text: &str) {
        let Some(draft) = self.line_draft.take() else {
            return;
        };
        let text = text.trim().to_owned();
        let _ = history::append(&self.home, &self.cwd_key, &text);
        self.composer.push_history(&text);
        self.host.send(Command::OpenLine {
            session_id: draft.session_id,
            projection_id: draft.projection_id,
            parent: draft.parent,
            at: draft.at,
            prompt: text,
        });
    }

    /// `b` on a picked message: the next submit opens a line of the shown
    /// line at `at` (timeline UI §4.2).
    fn draft_line(&mut self, at: u64, text: &str) {
        if let Some(draft) = self.line_at(at) {
            self.line_draft = Some(draft);
            self.composer.set_draft(text);
        }
    }

    /// A line of the shown line cut at `at`, unless the shown line ended.
    fn line_at(&mut self, at: u64) -> Option<LineDraft> {
        let Some(snapshot) = &self.snapshot else {
            self.set_notice(t("No active session"));
            return None;
        };
        if let Some(line) = snapshot.lines.iter().find(|line| line.id == snapshot.focus)
            && !matches!(
                line.state,
                LineState::Main | LineState::Working | LineState::Waiting
            )
        {
            self.set_notice(format!("{} {}", t("Line has ended:"), line.id));
            return None;
        }
        Some(LineDraft {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            parent: snapshot.focus.clone(),
            at,
        })
    }

    /// `f` on a picked message, or `/fork`: asks before forking `main` at
    /// `at` (timeline UI §4.3), naming the message it cuts before.
    fn ask_fork(&mut self, at: u64, text: String, before: Option<String>) {
        match &self.snapshot {
            None => self.set_notice(t("No active session")),
            Some(snapshot) if snapshot.focus != "main" => {
                self.set_notice(t("A fork copies main; b opens a line here"));
            }
            Some(_) => self.fork_confirm = Some(ForkAsk { at, text, before }),
        }
    }

    fn fork(&mut self, at: u64, text: String) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        self.host.send(Command::Fork {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            at,
        });
        self.fork_draft = Some(text);
    }

    /// The cut at the end of the shown line: its newest turn end.
    fn head_cut(&mut self) -> Option<u64> {
        let cut = self.snapshot.as_ref()?.ended.map(|(sequence, _)| sequence);
        if cut.is_none() {
            self.set_notice(t("No finished turn to cut at yet"));
        }
        cut
    }

    fn apply_effect(&mut self, effect: Effect) -> Outcome {
        match effect {
            Effect::None => {}
            Effect::Close => {
                if matches!(self.prompt, Some(Prompt::Sessions(_))) {
                    self.host.send(Command::CloseSessions);
                }
                self.prompt = None;
                self.reopen_upgrade();
            }
            Effect::Exit => return Outcome::Exit(130),
            Effect::Restart { confirmed } => {
                if let Some(Prompt::Upgrade(dialog)) = self.prompt.take() {
                    self.upgrade = Some(dialog);
                }
                if !confirmed && let Some(reason) = self.busy_reason() {
                    self.prompt = Some(Prompt::Exit(ExitConfirm::restart(reason)));
                    return Outcome::Continue;
                }
                if let Some(install) = self.upgrade.take().and_then(|dialog| dialog.offer.install) {
                    return Outcome::Restart(install.relaunch);
                }
            }
            Effect::UpgradeInstall(install) => self.host.send(Command::UpgradeInstall(install)),
            Effect::UpgradeSkip(version) => {
                self.prompt = None;
                match crate::update::skip(&self.home, &version) {
                    Ok(()) => self.set_notice(tf("Skipped {version}.", &[("version", &version)])),
                    Err(error) => self.notices.push(error.to_string()),
                }
            }
            Effect::Approval {
                request_id,
                branch_id,
                allow,
                remember,
            } => {
                self.answered.insert(request_id.clone());
                self.host.send(Command::Approval {
                    request_id,
                    branch_id,
                    allow,
                    remember,
                });
                self.prompt = None;
                self.sync_prompt();
            }
            Effect::Answers { call, answers } => {
                if let Some(Prompt::Ask(prompt)) = &mut self.prompt {
                    prompt.sending = true;
                    prompt.error = None;
                }
                self.host.send(Command::AskAnswers {
                    session_id: self.session_id.clone(),
                    call,
                    answers,
                });
            }
            Effect::Cancel { call } => {
                if let Some(Prompt::Ask(prompt)) = &mut self.prompt {
                    prompt.sending = true;
                    prompt.error = None;
                }
                self.host.send(Command::AskCancel {
                    session_id: self.session_id.clone(),
                    call,
                });
            }
            Effect::Notice(text) => self.set_notice(text),
            Effect::FocusLine(line) => {
                self.prompt = None;
                self.host.send(Command::FocusLine(line));
            }
            Effect::Timeline { row_id, action } => {
                self.prompt = None;
                self.timeline(&row_id, action);
            }
            Effect::EndLine {
                session_id,
                projection_id,
                branch_id,
            } => {
                self.host.send(Command::EndLine {
                    session_id,
                    projection_id,
                    branch_id,
                });
            }
            Effect::Resume(id) => {
                self.host.send(Command::CloseSessions);
                self.prompt = None;
                self.composer.clear();
                self.host.send(Command::Resume(id));
            }
            Effect::Refresh => match &mut self.prompt {
                Some(Prompt::Providers(_)) => self.host.send(Command::ListProviders),
                Some(Prompt::Plugins(_)) => self.host.send(Command::ListPlugins),
                Some(Prompt::Sessions(panel)) => {
                    panel.refresh();
                    self.sync_sessions();
                }
                _ => {}
            },
            Effect::FullAccess {
                request_id,
                branch_id,
            } => {
                self.approvals_toast = true;
                self.host.send(Command::SetApprovals("full-access".into()));
                let mut pending = vec![(request_id, branch_id)];
                if let Some(snapshot) = &self.snapshot {
                    pending.extend(
                        snapshot
                            .approvals
                            .iter()
                            .filter(|a| !self.answered.contains(&a.request_id))
                            .map(|a| (a.request_id.clone(), a.branch_id.clone())),
                    );
                }
                for (request_id, branch_id) in pending {
                    if !self.answered.insert(request_id.clone()) {
                        continue;
                    }
                    self.host.send(Command::Approval {
                        request_id,
                        branch_id,
                        allow: true,
                        remember: false,
                    });
                }
                self.prompt = None;
                self.sync_prompt();
            }
            Effect::StopTask {
                session_id,
                projection_id,
                target,
            } => self.host.send(Command::StopTask {
                session_id,
                projection_id,
                target,
            }),
            Effect::StopMember {
                session_id,
                projection_id,
                member_id,
            } => self.host.send(Command::StopMember {
                session_id,
                projection_id,
                member_id,
            }),
            Effect::StopTasks {
                session_id,
                projection_id,
            } => self.host.send(Command::StopTasks {
                session_id,
                projection_id,
            }),
            Effect::StopWorkflow {
                session_id,
                projection_id,
                run_id,
            } => self.host.send(Command::StopWorkflow {
                session_id,
                projection_id,
                run_id,
            }),
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
            Effect::Plugin(action) => self.host.send(Command::Plugin(action)),
            Effect::OauthStart(start) => self.host.send(Command::OauthStart(start)),
            Effect::OauthCancel => self.host.send(Command::OauthCancel),
            Effect::OauthBack => {}
            Effect::OauthPaste(url) => self.host.send(Command::OauthPaste(url)),
            Effect::Print(text) => self.reports.push((text, Tone::Muted)),
            Effect::Connected(provider_id) => {
                self.set_notice(tf("Connected {provider}", &[("provider", &provider_id)]));
                if provider_id == DIM_OAUTH_ID {
                    self.signed_in = true;
                }
                self.prompt = Some(Prompt::Models(ModelsPanel::new()));
                self.host.send(Command::ListModels);
            }
            Effect::OpenModels => {
                self.prompt = Some(Prompt::Models(ModelsPanel::new()));
                self.host.send(Command::ListModels);
            }
            Effect::Logout(provider_id) => self.host.send(Command::Logout(provider_id)),
            Effect::LoginUsage(provider_id) => self.host.send(Command::LoginUsage(provider_id)),
            Effect::OpenUrl(url) => {
                crate::login::open_browser(&url);
                self.set_notice(tf("Opened {url}", &[("url", &url)]));
            }
            Effect::ClearModelPreferences {
                provider_id,
                model_id,
                expected_version,
            } => {
                self.host.send(Command::ClearModelPreferences {
                    provider_id,
                    model_id,
                    expected_version,
                });
            }
            Effect::SelectModel {
                provider_id,
                model_id,
                reasoning_effort,
            } => {
                self.prompt = None;
                self.host.send(Command::SelectModel {
                    provider_id,
                    model_id,
                    reasoning_effort,
                });
            }
            Effect::ReadIsolation => self.host.send(Command::ReadIsolation),
            Effect::SaveIsolation {
                expected_revision,
                policy,
            } => self.host.send(Command::SaveIsolation {
                expected_revision,
                policy,
            }),
            Effect::ResetIsolation(revision) => self.host.send(Command::ResetIsolation(revision)),
            Effect::ReadModelPolicy => self.host.send(Command::ReadModelPolicy),
            Effect::SetModelPolicy {
                expected_revision,
                policy,
            } => self.host.send(Command::SetModelPolicy {
                expected_revision,
                policy,
            }),
        }
        if self.prompt.is_none()
            && let Some(setting) = self.settings_return.take()
        {
            self.open_settings(setting);
        }
        Outcome::Continue
    }

    fn sync_task_details(&mut self) {
        let target = match &self.prompt {
            Some(Prompt::Tasks(panel)) => panel
                .output_target()
                .map(|target| (panel.projection_id, target.clone())),
            _ => None,
        };
        if target != self.output_target {
            let projection_id = target
                .as_ref()
                .or(self.output_target.as_ref())
                .expect("changed output watch")
                .0;
            self.host.send(Command::WatchTaskOutput {
                projection_id,
                target: target.as_ref().map(|(_, target)| target.clone()),
            });
            self.output_target = target;
        }
        let member = match &self.prompt {
            Some(Prompt::Tasks(panel)) => panel
                .member_target()
                .map(|id| (panel.projection_id, id.to_owned())),
            _ => None,
        };
        if member != self.member_target {
            let projection_id = member
                .as_ref()
                .or(self.member_target.as_ref())
                .expect("changed member view")
                .0;
            self.host.send(Command::WatchMember {
                projection_id,
                member_id: member.as_ref().map(|(_, id)| id.clone()),
            });
            self.member_target = member;
        }
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
                snapshot.questions.first().map(|q| {
                    (
                        format!("question:{}:{}", q.request_id, q.call_id),
                        Event::Question,
                    )
                })
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

    /// What the terminal's program status says (`ui::status`): an
    /// interface that lost its App Server has failed whatever it was doing.
    fn program_status(&self) -> Status {
        match &self.snapshot {
            _ if self.failure.is_some() => Status::Error,
            Some(snapshot) => self.runs.status(snapshot, &self.answered),
            None => Status::Idle,
        }
    }

    /// The window title's topic: the session's first message on `main`
    /// (`ui::title`), whichever line is shown.
    fn topic(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        crate::ui::title::topic(snapshot.main_topic.as_deref()?)
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
        self.open_prompt(Prompt::Settings(SettingsPanel::new(&values, select)));
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
                self.open_prompt(Prompt::Language(LanguagePanel::new(i18n::saved_locale(
                    &self.home,
                ))));
            }
            Setting::Approvals => {
                let panel = ApprovalsPanel::new(&self.approvals);
                self.open_prompt(Prompt::Approvals(panel));
            }
            Setting::Isolation => {
                let panel = crate::ui::isolation_panel::IsolationPanel::new(&self.approvals);
                self.open_prompt(Prompt::Isolation(Box::new(panel)));
                self.host.send(Command::ReadIsolation);
            }
            Setting::Model => {
                self.open_prompt(Prompt::Models(ModelsPanel::new()));
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
        let detail = self.tool_detail.flipped();
        self.tool_detail = detail;
        self.group_details.clear();
        for block in &mut self.history {
            match block {
                Block::ToolGroup(group) => group.detail = detail,
                Block::Thinking { expanded, .. } | Block::Compaction { expanded, .. } => {
                    *expanded = detail == Detail::Expanded
                }
                _ => {}
            }
        }
        self.request_reflow();
        if let Some(error) = crate::ui::config::write(
            &self.home,
            TOOL_DETAILS_KEY,
            (detail == Detail::Expanded).into(),
        ) {
            self.set_notice(tf("Tool details not saved: {error}", &[("error", &error)]));
        }
    }

    /// `/plugins` opens the list; `/plugins install PATH` installs the
    /// directory at PATH (relative to the workspace) and opens the list on
    /// the outcome.
    fn plugins(&mut self, args: &str) {
        let args = args.trim();
        if args.is_empty() {
            self.open_prompt(Prompt::Plugins(PluginsPanel::new()));
            self.host.send(Command::ListPlugins);
            return;
        }
        let Some(path) = args
            .strip_prefix("install")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim)
        else {
            self.set_notice(t("Usage: /plugins [install PATH]"));
            return;
        };
        let path = match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => PathBuf::from(&self.cwd_key).join(path),
        };
        self.open_prompt(Prompt::Plugins(PluginsPanel::installing(&path)));
        self.host.send(Command::Plugin(
            crate::model::session::PluginAction::Install(path),
        ));
    }

    fn run_command(&mut self, name: &str, args: &str) -> Outcome {
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
            "compact" => {
                self.host.send(Command::Compact);
                Outcome::Continue
            }
            "mcp" => {
                self.host.send(Command::Mcp(args.to_owned()));
                Outcome::Continue
            }
            "effort" | "fast" | "agent" => {
                let text = format!("/{name} {args}");
                self.host
                    .send(Command::Configure(text.trim_end().to_owned()));
                Outcome::Continue
            }
            "suspend" => Outcome::Suspend,
            "exit" => Outcome::Exit(0),
            "tui" => Outcome::SwitchMode,
            "new" => {
                self.host.send(Command::NewSession);
                Outcome::Continue
            }
            "web" => Outcome::Web,
            "resume" => {
                self.open_prompt(Prompt::Sessions(SessionsPanel::new()));
                self.sync_sessions();
                Outcome::Continue
            }
            "lines" => {
                if let Some(snapshot) = &self.snapshot {
                    let panel = LinesPanel::new(snapshot);
                    self.open_prompt(Prompt::Lines(Box::new(panel)));
                }
                Outcome::Continue
            }
            "timeline" => {
                if let Some(snapshot) = &self.snapshot {
                    let panel = TimelinePanel::new(snapshot);
                    self.open_prompt(Prompt::Timeline(Box::new(panel)));
                }
                Outcome::Continue
            }
            "branch" if args.is_empty() => {
                self.composer.set_draft("/branch ");
                Outcome::Continue
            }
            "branch" => {
                match self.head_cut().and_then(|at| self.line_at(at)) {
                    Some(draft) => {
                        self.line_draft = Some(draft);
                        self.open_line(args);
                    }
                    None => self.composer.set_draft(&format!("/branch {args}")),
                }
                Outcome::Continue
            }
            "fork" => {
                if let Some(at) = self.head_cut() {
                    self.ask_fork(at, args.to_owned(), None);
                }
                Outcome::Continue
            }
            "tasks" => {
                if let Some(snapshot) = &self.snapshot {
                    let panel = TasksPanel::new(snapshot);
                    let projection_id = snapshot.projection_id;
                    self.open_prompt(Prompt::Tasks(panel));
                    self.host.send(Command::TaskHistory { projection_id });
                }
                Outcome::Continue
            }
            "theme" => {
                if let Some(context) = &self.theme_context {
                    let panel = ThemePanel::new(context.mode, context.appearance, &context.active);
                    self.open_prompt(Prompt::Theme(panel));
                }
                Outcome::Continue
            }
            "approval" => {
                self.prompt = Some(Prompt::Approvals(ApprovalsPanel::choice(&self.approvals)));
                Outcome::Continue
            }
            "approvals" => {
                let panel = ApprovalsPanel::new(&self.approvals);
                self.open_prompt(Prompt::Approvals(panel));
                Outcome::Continue
            }
            "plugins" => {
                self.plugins(args);
                Outcome::Continue
            }
            "connect" => {
                self.open_prompt(Prompt::Providers(Box::new(ProvidersPanel::new())));
                self.host.send(Command::ListProviders);
                Outcome::Continue
            }
            "login" => {
                self.login_attempts += 1_000;
                self.open_prompt(Prompt::Login(Box::new(OauthView::new(
                    DIM_OAUTH_ID,
                    "DimAgent",
                    self.login_attempts,
                ))));
                self.host.send(Command::LoginStatus(DIM_OAUTH_ID.into()));
                Outcome::Continue
            }
            "models" => {
                self.open_prompt(Prompt::Models(ModelsPanel::new()));
                self.host.send(Command::ListModels);
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
                self.open_prompt(Prompt::Language(LanguagePanel::new(i18n::saved_locale(
                    &self.home,
                ))));
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
        let ready = self.arm_window();
        if ready && let Some(shown) = self.pending_line.take() {
            let lines = self.snapshot.as_ref().map_or(&[][..], |s| &s.lines[..]);
            let label = match line_name(lines, &shown) {
                name if shown == "main" || name == short_id(&shown) => format!("⑂ {name}"),
                name => format!("⑂ {name} · {}", short_id(&shown)),
            };
            let row = boundary_line(&label, width);
            let text = chunk(&[fg(&row, &self.palette.muted)]);
            self.record(Block::Boundary(label));
            frame.commits.push(text);
        }
        if ready && std::mem::take(&mut self.load_more) && self.fullscreen {
            let hidden = self.snapshot.as_ref().map_or(0, |snapshot| {
                snapshot.rows[..self.window_floor.min(snapshot.rows.len())]
                    .iter()
                    .filter(|row| row.kind.is_message())
                    .count()
            });
            if hidden > 0 {
                let block = Block::LoadMore(hidden);
                let text = chunk(&render_block(&block, width, &self.palette, &self.markdown));
                self.record(block);
                frame.commits.push(text);
            }
        }
        let (commits, sources, tail) = self.transcript(width);
        for (text, (source, placement)) in commits.into_iter().zip(sources) {
            if let Block::ToolGroup(group) = &source
                && !group.cards.is_empty()
                && self.fullscreen
            {
                self.card_blocks.push(self.dropped + self.history.len());
            }
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
                SwitchKind::Forked => {
                    let sid = self.session_id.get(..8).unwrap_or(&self.session_id);
                    tf(
                        "forked {id} · {count} messages",
                        &[("id", sid), ("count", &self.window_info.0.to_string())],
                    )
                }
                SwitchKind::Resumed => {
                    let (count, hidden) = self.window_info;
                    let sid = self.session_id.get(..8).unwrap_or(&self.session_id);
                    let mut label = tf(
                        "resumed {id} · {count} messages",
                        &[("id", sid), ("count", &count.to_string())],
                    );
                    if hidden > 0 {
                        label.push_str(t(" · earlier history: /timeline"));
                    }
                    label
                }
            };
            let line = boundary_line(&label, width);
            let text = chunk(&[fg(&line, &self.palette.muted)]);
            self.record(Block::Boundary(label));
            frame.commits.push(text);
        }
        if ready && let Some((row_id, pick)) = self.landing.take() {
            let block = self.snapshot.as_ref().and_then(|snapshot| {
                let row = snapshot.rows.iter().find(|row| row.row_id == row_id)?;
                self.block_of(row)
            });
            if let Some(id) = block {
                self.jump = Some(if pick {
                    Jump::Block(id)
                } else {
                    Jump::Show(id)
                });
            }
        }
        for notice in std::mem::take(&mut self.notices) {
            let rows = gutter_text(STATUS_MARK, &notice, width, &self.palette.muted);
            let text = chunk(&rows);
            let tone = Tone::Muted;
            self.record(Block::Status { text: notice, tone });
            frame.commits.push(text);
        }
        for (text, tone) in std::mem::take(&mut self.reports) {
            let color = match tone {
                Tone::Muted => &self.palette.muted,
                Tone::Warning => &self.palette.warning,
                Tone::Danger => &self.palette.danger,
            };
            let rows = gutter_text(STATUS_MARK, &text, width, color);
            let chunk_text = chunk(&rows);
            self.record(Block::Status { text, tone });
            frame.commits.push(chunk_text);
        }
        for run in std::mem::take(&mut self.shell_runs) {
            let block = Block::Shell(run);
            let text = chunk(&render_block(&block, width, &self.palette, &self.markdown));
            self.record(block);
            frame.commits.push(text);
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
            let resume = if self.session_id.is_empty() {
                "dimcode".to_owned()
            } else {
                format!("dimcode --resume {}", self.session_id)
            };
            frame.commits.push(pad_block(&[tf(
                "Disconnected; draft kept. Exit and run: {command}",
                &[("command", &resume)],
            )]));
        }

        let mut live = tail;
        if let Some(running) = self.shell.as_ref().filter(|_| self.prompt.is_none()) {
            let (output, total) = running.tail(SHELL_PREVIEW_LINES);
            let status = if self.failure.is_some() {
                t("Disconnected; command outcome unknown").to_owned()
            } else {
                tf(
                    "Running… · Ctrl+C to interrupt · {seconds}s",
                    &[("seconds", &running.started.elapsed().as_secs().to_string())],
                )
            };
            let rows = render_shell(
                &running.command,
                running.private,
                &output,
                total - output.len(),
                Some((&status, &self.palette.muted)),
                width,
                &self.palette,
            );
            live.push(String::new());
            live.extend(
                rows.into_iter()
                    .map(|r| format!("{}{r}", " ".repeat(MARGIN))),
            );
        }
        frame.dock = live.len();
        if self.prompt.is_none() {
            let queued: Vec<_> = self
                .snapshot
                .as_ref()
                .into_iter()
                .flat_map(|snapshot| &snapshot.queued)
                .map(|input| {
                    (
                        input.id.clone(),
                        input.display.as_deref().unwrap_or(&input.text).to_owned(),
                    )
                })
                .collect();
            // An empty queue draws no strip; the composer keeps its row and
            // the strip's rows are given back above it.
            self.queue_rows = if queued.is_empty() {
                0
            } else {
                self.queue_rows.max(queued.len().min(QUEUE_ROWS))
            };
            if self.queue_rows > 0 {
                live.extend(queue_strip(&queued, self.queue_rows, cols, &self.palette));
            }
        }
        let side = match self.snapshot.as_ref().and_then(|s| s.side.as_ref()) {
            Some(side) if self.prompt.is_none() => {
                let most = (size.1 as usize / 3).max(1);
                let reply = side_reply(
                    side,
                    cols,
                    most,
                    self.tool_detail,
                    &self.palette,
                    &self.markdown,
                );
                self.side_rows = self.side_rows.max(reply.len()).min(most);
                Some(side_panel(side, reply, self.side_rows, cols, &self.palette))
            }
            Some(_) => None,
            None => {
                self.side_rows = 0;
                None
            }
        };
        // Fullscreen lays the transcript out above the dock, so the panel
        // pushes it up and gives the rows back when it closes; scrollback
        // never lifts the composer, so the panel opens below it like a menu.
        let (side_above, side_below) = if self.fullscreen {
            (side, None)
        } else {
            (None, side)
        };
        if let Some(panel) = side_above {
            let margin = " ".repeat(MARGIN);
            live.extend(panel.into_iter().map(|row| format!("{margin}{row}")));
        }
        frame.anchor = live.len();
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
        let shell = self.composer.shell();
        let prompt = match shell {
            Some(shell) => format!("{} ", shell.prefix()),
            None if self.fullscreen => String::new(),
            None => "\u{203a} ".to_owned(),
        };
        let prompt = prompt.as_str();
        let prompt_width = prompt.chars().count();
        let text_width = inner.saturating_sub(prompt_width).max(1);
        let p = &self.palette;
        self.composer.set_context(self.context());
        let mut rows: Vec<String> = Vec::new();
        let shell_color = match shell {
            Some(Shell::Private) => &p.muted,
            _ => &p.warning,
        };
        let prompt_color = if self.prompt.is_some() {
            &p.faint
        } else if shell.is_some() {
            shell_color
        } else {
            &p.accent
        };
        let command_entry = self.command_entry_open();
        let palette_overlay = self.fullscreen
            && self.composer.palette_open()
            && (self.prompt.is_none() || command_entry);
        let (cursor_row, cursor_col) = if self.composer.is_empty() || palette_overlay {
            let placeholder = match shell {
                Some(_) => t(SHELL_PLACEHOLDER),
                None if self.fullscreen => "",
                None => t(COMPOSER_PLACEHOLDER),
            };
            rows.push(format!(
                "{}{}",
                fg(prompt, prompt_color),
                fg(placeholder, &p.muted)
            ));
            (0, prompt_width)
        } else {
            let (input_rows, cursor) = self.composer.render_rows(text_width as u16, p);
            for (i, row) in input_rows.into_iter().enumerate() {
                let lead = if i == 0 {
                    fg(prompt, prompt_color)
                } else {
                    " ".repeat(prompt_width)
                };
                rows.push(format!("{lead}{row}"));
            }
            (cursor.1 as usize, prompt_width + cursor.0 as usize)
        };
        if self.fullscreen && rows.len() < FULLSCREEN_COMPOSER_ROWS {
            rows.resize(FULLSCREEN_COMPOSER_ROWS, String::new());
        }
        let box_top = live.len();
        let muted = |label: &str| fg(label, &p.muted);
        let shell_title = shell.map(|shell| match shell {
            Shell::Recorded => t("! shell mode"),
            Shell::Private => t("!! shell mode · not sent to the model"),
        });
        let tinted = |label: &str| fg(label, shell_color);
        let top_status = status.as_deref().filter(|_| !self.fullscreen);
        let title = match (top_status, shell_title) {
            (Some(text), _) => Some((text, &muted as &dyn Fn(&str) -> String)),
            (None, Some(title)) => Some((title, &tinted as &dyn Fn(&str) -> String)),
            (None, None) => None,
        };
        let border = if shell.is_some() && self.prompt.is_none() {
            shell_color
        } else {
            &p.border
        };
        let edge = |edge: &str| fg(edge, border);
        let mut composer_box = boxed(&rows, cols, &edge, &|_, body| body.to_owned(), title);
        if self.fullscreen {
            let outer = cols.saturating_sub(2 + MARGIN).max(4);
            let margin = " ".repeat(MARGIN);
            let title = title.map(|(text, label)| label(text));
            let model = self.label.as_deref().map(|label| fg(label, &p.highlight));
            let docked = self.dock_status(status.as_deref());
            let place = fg(
                &fit_place(self.place_label(), docked.as_deref(), outer),
                &p.muted,
            );
            let last = composer_box.len() - 1;
            composer_box[0] = format!(
                "{margin}{}",
                border_line(("╭", "╮"), title.as_deref(), model.as_deref(), outer, &edge)
            );
            composer_box[last] = format!(
                "{margin}{}",
                border_line(("╰", "╯"), docked.as_deref(), Some(&place), outer, &edge)
            );
        } else {
            let outer = cols.saturating_sub(2 + MARGIN).max(4);
            let margin = " ".repeat(MARGIN);
            let place = fg(&fit_place(self.place_label(), None, outer), &p.muted);
            let last = composer_box.len() - 1;
            composer_box[last] = format!(
                "{margin}{}",
                border_line(("╰", "╯"), None, Some(&place), outer, &edge)
            );
        }
        let replaced = !command_entry
            && matches!(
                self.prompt,
                Some(Prompt::Permission(_) | Prompt::Ask(_) | Prompt::Exit(_) | Prompt::Upgrade(_))
            );
        if !replaced {
            live.extend(composer_box);
        }
        let outer = cols.saturating_sub(2 + MARGIN).max(4);
        let margin = " ".repeat(MARGIN);
        let composer_rows = if replaced { 0 } else { rows.len() + 2 };
        let panel_rows = (size.1 as usize)
            .saturating_sub(composer_rows + 1 + 4 + 1)
            .min(18);
        let panel_cursor = |width: usize| match &self.prompt {
            Some(Prompt::Sessions(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Tasks(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Lines(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Timeline(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Models(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Providers(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Login(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Ask(_)) if command_entry => None,
            Some(Prompt::Ask(prompt)) => Some(prompt.cursor(width, panel_rows)),
            Some(_) => Some(None),
            None => None,
        };
        if self.fullscreen {
            let budget = (size.1 as usize)
                .saturating_sub(composer_rows + 2 + 2)
                .min(OVERLAY_MAX_ROWS);
            let list_width = cols.saturating_sub(4).min(OVERLAY_LIST_WIDTH);
            let caret = panel_cursor(list_width)
                .flatten()
                .map(|(row, column)| (row as u16, column as u16));
            let list = |rows: Vec<String>| {
                Some(Overlay {
                    rows,
                    cursor: caret,
                })
            };
            frame.overlay = match &self.prompt {
                _ if palette_overlay => {
                    let width = cols.saturating_sub(4).min(OVERLAY_PALETTE_WIDTH);
                    let page = budget.saturating_sub(5).clamp(3, 16);
                    self.composer
                        .palette_overlay(width, page, p)
                        .map(|(rows, query)| Overlay {
                            rows,
                            cursor: Some((2, (4 + query) as u16)),
                        })
                }
                Some(Prompt::Sessions(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Tasks(prompt)) => {
                    list(prompt.render(list_width, budget, p, &self.markdown))
                }
                Some(Prompt::Lines(prompt)) => {
                    list(prompt.render(list_width, budget, p, &self.markdown))
                }
                Some(Prompt::Timeline(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Models(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Providers(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Plugins(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Login(prompt)) => list(prompt.render(list_width, p)),
                Some(Prompt::Approvals(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Theme(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Language(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Settings(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Isolation(prompt)) => list(prompt.render(list_width, budget, p)),
                _ => None,
            };
        }
        let panel = match &self.prompt {
            _ if frame.overlay.is_some() => None,
            Some(Prompt::Ask(_)) if command_entry => self.composer.palette_panel(outer, p),
            Some(Prompt::Sessions(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Tasks(prompt)) => {
                Some(prompt.render(outer, panel_rows, p, &self.markdown))
            }
            Some(Prompt::Lines(prompt)) => {
                Some(prompt.render(outer, panel_rows, p, &self.markdown))
            }
            Some(Prompt::Timeline(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Models(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Providers(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Plugins(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Login(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Approvals(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Theme(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Language(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Settings(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Isolation(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Exit(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Upgrade(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Permission(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Ask(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            None => self
                .composer
                .palette_panel(outer, p)
                .or_else(|| self.composer.mention_panel(outer, p))
                .or(side_below),
        };
        let panel_top = live.len();
        if let Some(panel) = panel {
            live.extend(panel.into_iter().map(|row| format!("{margin}{row}")));
        }
        if !self.fullscreen {
            live.push(self.status_line(cols));
        }
        let composer_cursor = (
            (box_top + 1 + cursor_row) as u16,
            (MARGIN + 2 + cursor_col).min(cols.saturating_sub(1)) as u16,
        );
        frame.cursor = match panel_cursor(outer) {
            None => (!replaced).then_some(composer_cursor),
            Some(_) if self.fullscreen && !matches!(self.prompt, Some(Prompt::Ask(_))) => None,
            Some(caret) => caret.map(|(row, column)| {
                (
                    (panel_top + row) as u16,
                    (MARGIN + column).min(cols.saturating_sub(1)) as u16,
                )
            }),
        };
        frame.live = live;
        frame
    }

    fn arm_window(&mut self) -> bool {
        if !self.awaiting_window {
            return true;
        }
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        if !snapshot.history_complete {
            return false;
        }
        self.awaiting_window = false;
        let messages: Vec<usize> = snapshot
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind.is_message())
            .map(|(i, _)| i)
            .collect();
        let hidden = messages.len().saturating_sub(HISTORY_WINDOW);
        self.window_floor = if hidden > 0 { messages[hidden] } else { 0 };
        self.history_floor = self.window_floor;
        self.load_more = true;
        self.window_info = (messages.len() - hidden, hidden);
        true
    }

    /// Commits owed this frame and the live rows (unclosed streaming tail,
    /// the reasoning preview and running tools, in transcript order).
    #[allow(clippy::type_complexity)]
    fn transcript(&mut self, width: usize) -> (Vec<String>, Vec<(Block, Placement)>, Vec<String>) {
        if !self.arm_window() {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let Some(snapshot) = &self.snapshot else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        let p = &self.palette;

        let is_message = |row: &ViewRow| row.kind.is_message();
        let floor = self.window_floor;

        enum LiveItem {
            Text(String),
            Thinking(String),
            Tool(usize),
            Compaction(usize),
        }
        let mut live_text: Vec<TextBlock> = Vec::new();
        let mut live_items: Vec<LiveItem> = Vec::new();
        let mut settled_assistant: Vec<TextBlock> = Vec::new();
        let mut settled_ids: Vec<String> = Vec::new();
        let mut held = false;
        for (index, row) in snapshot.rows.iter().enumerate() {
            if index < floor || held {
                continue;
            }
            let text = row.text.as_str();
            match row.kind {
                RowKind::StreamingAssistant => {
                    let shown = self.shown(&row.row_id);
                    live_text.push(TextBlock {
                        id: &row.row_id,
                        text: &text[..shown.min(text.len())],
                    });
                    live_items.push(LiveItem::Text(row.row_id.clone()));
                }
                RowKind::Thinking if row.text.trim().is_empty() => {}
                RowKind::Thinking if row.streaming => {
                    live_items.push(LiveItem::Thinking(row.text.clone()));
                }
                RowKind::ToolCall if !opens_group(&snapshot.rows, index) => {}
                RowKind::ToolCall
                    if group_rows(&snapshot.rows, index)
                        .iter()
                        .any(|call| call.pending) =>
                {
                    live_items.push(LiveItem::Tool(index));
                }
                RowKind::Compaction
                    if row
                        .compaction
                        .as_ref()
                        .is_some_and(|c| c.state == CompactionState::Running) =>
                {
                    live_items.push(LiveItem::Compaction(index));
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
                _ => settled_ids.push(row.row_id.clone()),
            }
        }

        let markdown = &self.markdown;
        let render = |source: &str, width: usize| markdown.render(source, width, 0);
        let fresh = self.commits.plan(&settled_ids).fresh;
        // A parked stream pairs only with an answer that has not gone out:
        // an earlier answer with identical text is already in scrollback.
        settled_assistant.retain(|b| fresh.iter().any(|id| id == b.id));
        let mut plan = self.planner.plan(
            SESSION_REVISION,
            &live_text,
            &settled_assistant,
            width,
            &render,
        );
        let mut commits: Vec<String> = Vec::new();
        let mut sources: Vec<(Block, Placement)> = Vec::new();
        let (overrides, global) = (&self.group_details, self.tool_detail);
        let detail_of = |index: usize| {
            overrides
                .get(&snapshot.rows[index].row_id)
                .copied()
                .unwrap_or(global)
        };
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
            let source = batch.source.map(|s| Block::Assistant(s.text));
            (text, source.map(|source| (source, placement)))
        };
        let draw = |index: usize,
                    finalized: Option<crate::ui::transcript::stream::Batch>|
         -> (String, Option<(Block, Placement)>) {
            match (snapshot.rows[index].kind, finalized) {
                (RowKind::AssistantMessage, Some(batch)) if batch.lines.is_empty() => {
                    (String::new(), None)
                }
                (RowKind::AssistantMessage, Some(batch)) => streamed(batch),
                _ => {
                    let block = project_row(&snapshot.rows, index, detail_of(index));
                    let lines = render_block(&block, width, p, markdown);
                    if lines.is_empty() {
                        return (String::new(), None);
                    }
                    (chunk(&lines), Some((block, Placement::New)))
                }
            }
        };
        if let Some((start, end)) = self.history_request.take() {
            let len = snapshot.rows.len();
            let (start, end) = (start.min(len), end.min(len));
            let count = snapshot.rows[start..end]
                .iter()
                .filter(|r| is_message(r))
                .count();
            let header = tf(
                "Earlier history · {id} · {count} messages",
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
        for id in fresh {
            let Some(index) = snapshot.rows.iter().position(|r| r.row_id == id) else {
                continue;
            };
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

        let pad = " ".repeat(MARGIN);
        let mut tail: Vec<String> = Vec::new();
        let mut tail_groups = Vec::new();
        for item in &live_items {
            let rows: Vec<String> = match item {
                LiveItem::Text(id) => match self.planner.pending(id) {
                    Some(pending) if pending.diagram => {
                        let lines = pending.text.lines().count().saturating_sub(1);
                        let lead = "    ";
                        vec![
                            format!("{lead}{}", fg("mermaid", &p.faint)),
                            format!(
                                "{lead}{}",
                                fg(
                                    &tf(
                                        "drawing diagram… {count} lines",
                                        &[("count", &lines.to_string())]
                                    ),
                                    &p.muted
                                )
                            ),
                        ]
                    }
                    Some(pending) if !pending.text.trim().is_empty() => {
                        let written = pending.written;
                        let rows = render_open(pending.text, pending.open_fence, &|text| {
                            render(text, width)
                        });
                        if written > 0 {
                            tail.extend(
                                rows.into_iter().skip(written).map(|r| format!("{pad}{r}")),
                            );
                            continue;
                        }
                        rows
                    }
                    _ => Vec::new(),
                },
                LiveItem::Thinking(text) => {
                    live_thinking(text, self.tool_detail == Detail::Expanded, width, p)
                }
                LiveItem::Tool(index) => {
                    tail_groups.push((tail.len() + 1, snapshot.rows[*index].row_id.clone()));
                    render_group(
                        &project_group(&group_rows(&snapshot.rows, *index), detail_of(*index)),
                        width,
                        p,
                        RUNNING_MARK,
                    )
                }
                LiveItem::Compaction(index) => render_block(
                    &project_row(&snapshot.rows, *index, detail_of(*index)),
                    width,
                    p,
                    markdown,
                ),
            };
            if rows.is_empty() {
                continue;
            }
            tail.push(String::new());
            tail.extend(rows.into_iter().map(|r| format!("{pad}{r}")));
        }
        self.tail_groups = tail_groups;
        (commits, sources, tail)
    }

    fn switch_line(&mut self, line: String) {
        self.planner.reset();
        self.commits = CommitState::default();
        self.answered.clear();
        self.runs = Runs::default();
        self.window_floor = 0;
        self.history_floor = 0;
        self.load_more = false;
        self.landing = None;
        self.window_info = (0, 0);
        self.history_request = None;
        self.awaiting_window = true;
        self.pending_line = Some(line);
        self.line_draft = None;
        self.fork_confirm = None;
        self.card_blocks.clear();
        if self.fullscreen {
            self.dropped += self.history.len();
            self.history.clear();
            self.stream_block = None;
            self.jump = Some(Jump::Bottom);
        }
    }

    fn switch_session(&mut self, session_id: String, kind: SwitchKind) {
        let first = self.session_id.is_empty();
        // Home's first message started this Session: nothing shown to clear.
        if first
            && kind == SwitchKind::New
            && self
                .snapshot
                .as_ref()
                .is_none_or(|shown| shown.session_id.is_empty())
        {
            self.session_id = session_id;
            self.awaiting_window = true;
            return;
        }
        self.session_id = session_id;
        self.planner.reset();
        self.commits = CommitState::default();
        self.answered.clear();
        self.prompt = None;
        self.snapshot = None;
        self.runs = Runs::default();
        self.window_floor = 0;
        self.history_floor = 0;
        self.load_more = false;
        self.landing = None;
        self.window_info = (0, 0);
        self.history_request = None;
        // Home has no window to wait for.
        self.awaiting_window = !self.session_id.is_empty();
        self.pending_line = None;
        self.pending_boundary = match kind {
            SwitchKind::New if first => None,
            kind => Some(kind),
        };
        self.composer.clear();
        self.line_draft = None;
        self.fork_confirm = None;
        if let Some(text) = self.fork_draft.take()
            && kind == SwitchKind::Forked
        {
            self.composer.set_draft(&text);
        }
        self.card_blocks.clear();
        if self.fullscreen {
            self.dropped += self.history.len();
            self.history.clear();
            self.stream_block = None;
            self.banner_committed = false;
            self.jump = Some(Jump::Bottom);
        }
    }

    /// A `/timeline` key on the user message `row_id` of the shown line:
    /// the actions of a picked message, or a jump to it.
    fn timeline(&mut self, row_id: &str, action: TimelineAction) {
        let Some(snapshot) = &self.snapshot else {
            self.set_notice(t("No active session"));
            return;
        };
        let Some(index) = snapshot.rows.iter().position(|row| row.row_id == row_id) else {
            return;
        };
        let (text, cut) = (snapshot.rows[index].text.clone(), snapshot.rows[index].cut);
        match action {
            TimelineAction::Jump => self.jump_to(index),
            TimelineAction::Edit => self.composer.set_draft(&text),
            TimelineAction::Quote => {
                if !self.composer.quote(&text) && self.composer.shell().is_some() {
                    self.set_notice(t("Leave shell mode to quote a message"));
                }
            }
            TimelineAction::Branch => {
                if let Some(at) = cut {
                    self.draft_line(at, &text);
                }
            }
            TimelineAction::Fork => {
                if let Some(at) = cut {
                    self.ask_fork(at, text.clone(), Some(text));
                }
            }
        }
    }

    /// Fullscreen picks the message at row `index`, drawing from it first
    /// when it is earlier history; scrollback can only draw what it has not
    /// drawn yet, from that message on.
    fn jump_to(&mut self, index: usize) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let row_id = snapshot.rows[index].row_id.clone();
        if self.fullscreen {
            if index < self.window_floor {
                self.rebuild_window(index);
            }
            self.landing = Some((row_id, true));
        } else if index < self.history_floor {
            self.history_request = Some((index, self.history_floor));
            self.history_floor = index;
        } else {
            self.set_notice(t("Already on screen above; /tui jumps to it"));
        }
    }

    /// Fullscreen's load-more row: draws the previous page above the drawn
    /// messages and keeps the oldest drawn message at the top of the view.
    fn load_earlier(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let floor = self.window_floor.min(snapshot.rows.len());
        let messages: Vec<usize> = snapshot.rows[..floor]
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind.is_message())
            .map(|(i, _)| i)
            .collect();
        if messages.is_empty() {
            return;
        }
        let start = messages[messages.len().saturating_sub(HISTORY_WINDOW)];
        self.landing = snapshot.rows[floor..]
            .iter()
            .find(|row| row.kind == RowKind::UserMessage && row.line.is_none())
            .map(|row| (row.row_id.clone(), false));
        self.rebuild_window(start);
    }

    /// Entering fullscreen: the shown line is drawn again from its window,
    /// earlier history scrollback drew page by page in order under the
    /// load-more row, so nothing else scrollback committed (an earlier
    /// Session among it) carries over.
    fn redraw_window(&mut self) {
        if !self.awaiting_window {
            self.rebuild_window(self.history_floor.min(self.window_floor));
        }
    }

    /// Draws the fullscreen transcript again from row `floor`, the way a
    /// line switch draws a line's.
    fn rebuild_window(&mut self, floor: usize) {
        self.planner.reset();
        self.commits = CommitState::default();
        self.window_floor = floor;
        self.history_floor = floor;
        self.history_request = None;
        self.load_more = true;
        self.card_blocks.clear();
        self.dropped += self.history.len();
        self.history.clear();
        self.stream_block = None;
    }

    /// The block id a drawn user message `row` became.
    fn block_of(&self, row: &ViewRow) -> Option<usize> {
        self.history
            .iter()
            .rposition(|block| {
                matches!(block, Block::User { text, cut, .. } if *cut == row.cut && *text == row.text)
            })
            .map(|index| self.dropped + index)
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
    /// A click on transcript row `row` of committed block `block` (stable
    /// id), or of the live tail: on a tool group's header it opens or closes
    /// that group. Whether it did.
    fn click(&mut self, block: Option<usize>, row: usize) -> bool {
        let Some(id) = block else {
            if let Some((_, first)) = self.tail_groups.iter().find(|(at, _)| *at == row) {
                let current = self
                    .group_details
                    .get(first)
                    .copied()
                    .unwrap_or(self.tool_detail);
                self.group_details.insert(first.clone(), current.flipped());
                return true;
            }
            return false;
        };
        if row != 1 {
            return false;
        }
        let index = id.checked_sub(self.dropped);
        match index.and_then(|index| self.history.get_mut(index)) {
            Some(Block::ToolGroup(group)) => group.detail = group.detail.flipped(),
            Some(Block::Thinking { expanded, .. } | Block::Compaction { expanded, .. }) => {
                *expanded = !*expanded
            }
            _ => return false,
        }
        self.mark_rewritten(id);
        true
    }

    /// Block `id` changed in place: fullscreen lays it out again.
    fn mark_rewritten(&mut self, id: usize) {
        self.rewrites += 1;
        self.rewrite_log.push_back(id);
        if self.rewrite_log.len() > REWRITE_LOG {
            self.rewrite_log.pop_front();
        }
    }

    /// Fullscreen redraws committed delegation cards as their facts and
    /// observations change and their time runs; a card stops changing once
    /// its delegation ended. Scrollback keeps what it printed.
    fn refresh_cards(&mut self) {
        if !self.fullscreen || self.card_blocks.is_empty() {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut changed = Vec::new();
        let mut open = Vec::new();
        for id in &self.card_blocks {
            let Some(Block::ToolGroup(group)) = id
                .checked_sub(self.dropped)
                .and_then(|index| self.history.get(index))
            else {
                continue;
            };
            let Some(mut index) = snapshot.rows.iter().position(|row| {
                row.member
                    .as_ref()
                    .is_some_and(|card| group.cards.first() == Some(&card.task.id))
            }) else {
                continue;
            };
            while !opens_group(&snapshot.rows, index) {
                index -= 1;
            }
            let rows = group_rows(&snapshot.rows, index);
            if rows
                .iter()
                .filter_map(|row| row.member.as_ref())
                .any(|card| card.task.finished.is_none())
            {
                open.push(*id);
            }
            let fresh = project_group(&rows, group.detail);
            if fresh != *group {
                changed.push((*id, fresh));
            }
        }
        self.card_blocks = open;
        for (id, group) in changed {
            if let Some(block) = id
                .checked_sub(self.dropped)
                .and_then(|index| self.history.get_mut(index))
            {
                *block = Block::ToolGroup(group);
                self.mark_rewritten(id);
            }
        }
    }

    /// Opens a delegation card's member: its read-only conversation, or its
    /// result when there is no member Session to read.
    /// Opens what a picked or clicked card stands for: a delegation's
    /// member, or the line a marker opened.
    fn open_card(&mut self, request_event_id: &str) {
        if request_event_id == LOAD_MORE {
            self.load_earlier();
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if snapshot
            .lines
            .iter()
            .any(|line| line.id == request_event_id)
        {
            self.host
                .send(Command::FocusLine(request_event_id.to_owned()));
            return;
        }
        let Some(card) = snapshot.rows.iter().find_map(|row| {
            row.member
                .as_ref()
                .filter(|card| card.task.id == request_event_id)
        }) else {
            self.set_notice(t("That delegation is no longer in this transcript"));
            return;
        };
        let mut panel = TasksPanel::new(snapshot);
        match panel.open_card(card.task.clone()) {
            Effect::Notice(text) => self.set_notice(text),
            _ => self.prompt = Some(Prompt::Tasks(panel)),
        }
    }

    fn rewrite_stream_block(&mut self, source: Block) {
        let index = self
            .stream_block
            .and_then(|id| id.checked_sub(self.dropped));
        match index.and_then(|index| self.history.get_mut(index)) {
            Some(block) => {
                *block = source;
                self.mark_rewritten(self.stream_block.unwrap_or(0));
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

    /// The one status slot: a transient notice, else what the session is
    /// waiting for.
    fn slot(&self) -> Option<String> {
        match &self.notice {
            Some((text, _)) => Some(text.clone()),
            None if self.failure.is_some() => Some(t("Disconnected · draft kept").to_owned()),
            None => match &self.snapshot {
                Some(s) if !s.questions.is_empty() => Some(t("answer required").to_owned()),
                Some(s) if !s.approvals.is_empty() => Some(t("permission required").to_owned()),
                Some(s) if s.running => Some(t("esc interrupt").to_owned()),
                Some(s) if s.tasks.iter().any(|task| task.finished.is_none()) => Some(tf(
                    "{count} in background · /tasks",
                    &[(
                        "count",
                        &s.tasks
                            .iter()
                            .filter(|task| task.finished.is_none())
                            .count()
                            .to_string(),
                    )],
                )),
                _ => None,
            },
        }
    }

    /// `1.8K / 256K`: the main branch's context use against its window, with
    /// a note once it nears the automatic compaction threshold.
    fn context_label(&self) -> Option<String> {
        let reading = self.context?;
        let mut label = format!(
            "{} / {}",
            format_tokens(reading.input as f64),
            format_tokens(reading.window as f64)
        );
        if self.context_near() {
            label.push_str(" · ");
            label.push_str(t("near the compaction threshold"));
        }
        Some(label)
    }

    /// Whether the context use is at or above [`CONTEXT_WARNING_PERCENT`] of
    /// the threshold the server returned.
    fn context_near(&self) -> bool {
        let Some(reading) = self.context else {
            return false;
        };
        reading.threshold > 0
            && u128::from(reading.input) * 100
                >= u128::from(reading.threshold) * u128::from(CONTEXT_WARNING_PERCENT)
    }

    /// Fullscreen's bottom border label, the status corner: the run
    /// status, the slot (a notice in the highlight colour), the approvals
    /// mode when it is not the default one and the context use.
    fn dock_status(&self, run: Option<&str>) -> Option<String> {
        let p = &self.palette;
        let mut parts: Vec<String> = Vec::new();
        if let Some(run) = run {
            parts.push(fg(run, &p.muted));
        }
        if let Some(slot) = self.slot() {
            let color = if self.notice.is_some() {
                &p.highlight
            } else {
                &p.muted
            };
            parts.push(fg(&slot, color));
        }
        if self.approvals != "workspace-write" {
            let mode = status_label(&self.approvals);
            parts.push(fg(&format!("\u{23f5}\u{23f5} {mode}"), &p.muted));
        }
        if let Some(context) = self.context_label() {
            let color = if self.context_near() {
                &p.warning
            } else {
                &p.muted
            };
            parts.push(fg(&context, color));
        }
        (!parts.is_empty()).then(|| parts.join("  "))
    }

    fn status_line(&self, cols: usize) -> String {
        let slot = self.slot();
        let mut left = format!(
            "{}AGENT  \u{23f5}\u{23f5} {}",
            " ".repeat(MARGIN),
            t(status_label(&self.approvals))
        );
        if let Some(context) = self.context_label() {
            left.push_str("  ");
            left.push_str(&context);
        }
        if let Some(slot) = slot {
            left.push_str("  ");
            left.push_str(&slot);
        }
        let total = cols.saturating_sub(2);
        let line = match &self.label {
            Some(label) if width_of(&left) + 2 + width_of(label) <= total => {
                let gap = total - width_of(&left) - width_of(label);
                format!("{left}{}{label}", " ".repeat(gap))
            }
            _ => clip(&left, total).0,
        };
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
        if self.context_near()
            && let Some(context) = self.context_label()
            && let Some(at) = line.find(&context)
        {
            let (before, rest) = line.split_at(at);
            let (context, after) = rest.split_at(context.len().min(rest.len()));
            return format!(
                "{}{}{}",
                fg(before, &self.palette.muted),
                fg(context, &self.palette.warning),
                fg(after, &self.palette.muted)
            );
        }
        fg(&line, &self.palette.muted)
    }
}

impl Document for App {
    fn rewritten(&self, since: u64) -> (u64, Option<Vec<usize>>) {
        let count = (self.rewrites - since) as usize;
        let ids = (count <= self.rewrite_log.len())
            .then(|| self.rewrite_log.iter().rev().take(count).copied().collect());
        (self.rewrites, ids)
    }

    fn cards(&self, index: usize) -> Vec<String> {
        match &self.history[index] {
            Block::ToolGroup(group) => card_actions(group).into_iter().map(str::to_owned).collect(),
            Block::Line { id, .. } => vec![id.clone()],
            Block::LoadMore(_) => vec![LOAD_MORE.to_owned()],
            _ => Vec::new(),
        }
    }

    fn card_rows(&self, index: usize, cols: usize, card: &str) -> Option<(usize, usize)> {
        let group = match &self.history[index] {
            Block::ToolGroup(group) => group,
            Block::Line { .. } | Block::LoadMore(_) => {
                return Some((1, self.rows(index, cols).len()));
            }
            _ => return None,
        };
        let width = cols.saturating_sub(2 + MARGIN).max(1);
        let (start, end) = card_rows(group, width, &self.palette, card)?;
        Some((start + 1, end + 1))
    }

    fn card_at(&self, index: usize, cols: usize, row: usize) -> Option<String> {
        let group = match &self.history[index] {
            Block::ToolGroup(group) => group,
            Block::Line { id, .. } => return (row > 0).then(|| id.clone()),
            Block::LoadMore(_) => return (row > 0).then(|| LOAD_MORE.to_owned()),
            _ => return None,
        };
        let width = cols.saturating_sub(2 + MARGIN).max(1);
        action_at(group, width, &self.palette, row.checked_sub(1)?).map(str::to_owned)
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
            Block::Assistant(text) => {
                let width = cols.saturating_sub(2 + MARGIN).max(1);
                1 + text
                    .lines()
                    .map(|line| display_width(line).div_ceil(width).max(1))
                    .sum::<usize>()
            }
            _ => self.rows(index, cols).len(),
        }
    }

    fn message(&self, index: usize) -> Option<&str> {
        match &self.history[index] {
            Block::User { text, .. } => Some(text),
            _ => None,
        }
    }

    fn cut(&self, index: usize) -> Option<u64> {
        match &self.history[index] {
            Block::User { cut, .. } => *cut,
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

/// Message rows the queue strip shows at most.
const QUEUE_ROWS: usize = 5;

/// The queue, drawn in a titled border above the composer: the next message
/// to run reads at the top. `height` message rows are drawn (at least the
/// queue's, at most [`QUEUE_ROWS`]), blank past the queue's end, so the
/// strip holds its height while the queue drains. An empty queue is not
/// drawn at all.
fn queue_strip(items: &[(String, String)], height: usize, cols: usize, p: &Palette) -> Vec<String> {
    use crate::ui::render::panel::bordered;
    use crate::ui::text::truncate_cells;
    let outer = cols.saturating_sub(2 + MARGIN).max(4);
    let inner = outer.saturating_sub(4).max(1);
    let shown = &items[..items.len().min(height)];
    let hidden = items.len() - shown.len();
    let mut rows: Vec<String> = shown
        .iter()
        .map(|(_, text)| {
            let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
            fg(&truncate_cells(&format!("› {preview}"), inner), &p.accent)
        })
        .collect();
    rows.resize_with(height, String::new);
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
    bordered(&rows, outer, None, &p.border, Some(&title), None)
        .into_iter()
        .map(|row| format!("{margin}{row}"))
        .collect()
}

/// The side panel's line drawn as its transcript, without the opening
/// input its title already names: its newest blocks, laid out back to
/// front until they fill `height` rows, so a long line costs no more than
/// the panel shows.
fn side_reply(
    side: &SideLine,
    cols: usize,
    height: usize,
    detail: Detail,
    p: &Palette,
    markdown: &MarkdownRenderer,
) -> Vec<String> {
    let inner = cols
        .saturating_sub(2 + MARGIN)
        .max(4)
        .saturating_sub(4)
        .max(1);
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut rows = 0;
    for (index, row) in side.rows.iter().enumerate().rev() {
        if rows >= height {
            break;
        }
        if row.kind == RowKind::UserMessage
            || row.kind == RowKind::ToolCall && !opens_group(&side.rows, index)
        {
            continue;
        }
        let block = match row.kind {
            RowKind::StreamingAssistant => Block::Assistant(row.text.clone()),
            _ => project_row(&side.rows, index, detail),
        };
        let rendered = render_block(&block, inner, p, markdown);
        rows += rendered.len() + 1;
        blocks.push(rendered);
    }
    blocks.reverse();
    let mut lines: Vec<String> = Vec::new();
    for block in blocks {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(block);
    }
    lines
}

/// The side panel above the composer (timeline UI §4.7): the line's
/// transcript, its last `height` rows, over the line's state.
fn side_panel(
    side: &SideLine,
    reply: Vec<String>,
    height: usize,
    cols: usize,
    p: &Palette,
) -> Vec<String> {
    use crate::ui::render::panel::bordered;
    let outer = cols.saturating_sub(2 + MARGIN).max(4);
    let inner = outer.saturating_sub(4).max(1);
    let mut rows: Vec<String> = reply[reply.len().saturating_sub(height)..].to_vec();
    rows.resize_with(height, String::new);
    let state = match side.state {
        LineState::Working => format!("{}…", t("working")),
        LineState::Waiting => t("done").to_owned(),
        _ => crate::ui::lines_panel::state_label(&side.state),
    };
    rows.push(fg(&truncate_cells(&format!("⑂ {state}"), inner), &p.muted));
    let title = paint(
        &truncate_cells(
            &format!(
                "⑂ {} · {}",
                crate::sanitize::safe(side.purpose.lines().next().unwrap_or_default()),
                short_id(&side.id)
            ),
            inner.saturating_sub(4),
        ),
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    );
    let hint = fg(t("Esc close"), &p.muted);
    bordered(&rows, outer, None, &p.border, Some(&title), Some(&hint))
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
    let body_width = cols.saturating_sub(2 + MARGIN).max(4) - 4;
    let greeting = [
        tf("Welcome to DIM {version}!", &[("version", version)]),
        t("Type / for commands · Ctrl+O for the palette").to_owned(),
    ];
    let header = startup_header(&greeting, body_width);
    let mut info = vec![String::new()];
    info.extend(labeled_rows(
        &[(t("Workspace"), &cwd), (t("Git branch"), &branch)],
        body_width,
    ));
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

/// The Web workspace panel: the logo beside the greeting, then where the
/// workspace is and how to get back to the terminal, in the welcome box.
fn web_panel(cols: usize, p: &Palette, opened: &WebOpened) -> String {
    let body_width = cols.saturating_sub(2 + MARGIN).max(4) - 4;
    let greeting = [
        t("DIM Web is ready").to_owned(),
        if opened.sign_in.is_some() {
            t("Open the sign-in link below").to_owned()
        } else {
            t("Opened in your browser").to_owned()
        },
    ];
    let header = startup_header(&greeting, body_width);
    let resume = opened.session_id.map(|id| format!("dimcode --resume {id}"));
    let mut pairs = vec![(t("Web"), opened.url)];
    if let Some(resume) = &resume {
        pairs.push((t("Resume"), resume.as_str()));
    }
    let mut info = vec![String::new()];
    info.extend(labeled_rows(&pairs, body_width));
    info.extend([
        String::new(),
        t("Press Ctrl+C to stop the Web workspace.").to_owned(),
    ]);
    let rows: Vec<String> = header.iter().chain(info.iter()).cloned().collect();
    let header_rows = header.len();
    let last = rows.len() - 1;
    let mut text = boxed(
        &rows,
        cols,
        &|edge| fg(edge, &p.accent),
        &|index, body| {
            let color = if index < header_rows {
                &p.accent
            } else if index == last {
                &p.muted
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

/// `label  value` rows aligned like the welcome panel's, values wrapped
/// under their column; stacked when the box is narrow.
fn labeled_rows(pairs: &[(&str, &str)], body_width: usize) -> Vec<String> {
    let inline = body_width >= 32;
    let widest = pairs
        .iter()
        .map(|(label, _)| width_of(label))
        .max()
        .unwrap_or(0);
    let label_width = if inline {
        (widest + 2).clamp(8, body_width.saturating_sub(12).max(8))
    } else {
        body_width
    };
    let value_width = body_width.saturating_sub(label_width).max(1);
    let mut rows = Vec::new();
    for (label, value) in pairs {
        if !inline {
            rows.push((*label).to_owned());
            rows.extend(wrap(value, body_width));
            continue;
        }
        for (index, part) in wrap(value, value_width).into_iter().enumerate() {
            let lead = if index == 0 { *label } else { "" };
            let pad = " ".repeat(label_width.saturating_sub(width_of(lead)));
            rows.push(format!("{lead}{pad}{part}"));
        }
    }
    rows
}

fn fit_place(place: String, status: Option<&str>, outer: usize) -> String {
    let room = outer.saturating_sub(2);
    let fits =
        |label: &str| status.map_or(0, |status| width_of(status) + 3) + width_of(label) + 3 < room;
    if fits(&place) {
        return place;
    }
    match place.split_once(" · ⑂ ") {
        Some((_, line)) if fits(&format!("⑂ {line}")) => format!("⑂ {line}"),
        _ => place,
    }
}

/// `main ▸ parser race ▸ the guard · after message 4`: the lines from `main` down to
/// `line`, and where `line` was cut (timeline UI §4.5).
fn line_path(lines: &[LineView], line: &str) -> String {
    let mut path = vec![line_name(lines, line)];
    let mut at = lines.iter().find(|view| view.id == line);
    let cut = at.and_then(|view| view.cut);
    while let Some(parent) = at.and_then(|view| view.parent.as_deref()) {
        path.push(line_name(lines, parent));
        at = lines.iter().find(|view| view.id == parent);
    }
    path.reverse();
    let path = path.join(" ▸ ");
    match cut {
        Some(cut) => format!(
            "{path} · {}",
            tf(
                "after message {count}",
                &[("count", &cut.messages.to_string())]
            )
        ),
        None => path,
    }
}

/// What a line is called on screen: the first line of its purpose, cut to
/// a few words; its short id when it has none.
fn line_name(lines: &[LineView], line: &str) -> String {
    const NAME_COLUMNS: usize = 24;
    match lines.iter().find(|view| view.id == line) {
        Some(view) if !view.purpose.is_empty() => {
            let first = view.purpose.lines().next().unwrap_or_default();
            truncate_cells(&crate::sanitize::safe(first), NAME_COLUMNS)
        }
        _ => short_id(line).to_owned(),
    }
}

/// The working directory, `~` for the home, and the git branch: what the
/// prompt box writes in its bottom border.
fn workspace_label(branch: &str) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = match cwd.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => cwd,
    };
    format!("{cwd} ({branch})")
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
/// Its margin and padding are the welcome box's, so the two line up.
fn goodbye_panel(resume: Option<&str>) -> Vec<String> {
    let margin = " ".repeat(MARGIN);
    let mut body = vec![t("Bye!").to_owned()];
    if let Some(session_id) = resume {
        body.extend([
            String::new(),
            t("To resume this session:").to_owned(),
            format!("  dimcode --resume {session_id}"),
        ]);
    }
    let inner = body.iter().map(|line| width_of(line)).max().unwrap_or(0) + 2;
    std::iter::once(format!("{margin}╭{}╮", "─".repeat(inner)))
        .chain(body.iter().map(|line| {
            format!(
                "{margin}│ {line}{} │",
                " ".repeat(inner - 2 - width_of(line))
            )
        }))
        .chain(std::iter::once(format!("{margin}╰{}╯", "─".repeat(inner))))
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
/// into the top border (`╭─ status ───╮`).
fn boxed(
    rows: &[String],
    cols: usize,
    edge: Painter,
    interior: &dyn Fn(usize, &str) -> String,
    status: Option<(&str, Painter)>,
) -> Vec<String> {
    let outer = cols.saturating_sub(2 + MARGIN).max(4);
    let inner = outer - 4;
    let margin = " ".repeat(MARGIN);
    let mut out = Vec::with_capacity(rows.len() + 2);
    let status = status.map(|(text, label)| label(&clip(text, outer.saturating_sub(6)).0));
    out.push(format!(
        "{margin}{}",
        border_line(("╭", "╮"), status.as_deref(), None, outer, edge)
    ));
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

/// One border row `outer` columns wide between `corners`: a styled label
/// after `─ ` on the left, another before ` ─` on the right, as the prompt
/// box carries them. A label that does not fit is left out, the right one
/// first.
fn border_line(
    corners: (&str, &str),
    left: Option<&str>,
    right: Option<&str>,
    outer: usize,
    edge: Painter,
) -> String {
    let room = outer.saturating_sub(2);
    let left = left.map(|label| (label, width_of(label) + 3));
    let mut right = right.map(|label| (label, width_of(label) + 3));
    let used = |l: Option<(&str, usize)>, r: Option<(&str, usize)>| {
        l.map_or(0, |l| l.1) + r.map_or(0, |r| r.1)
    };
    if used(left, right) >= room {
        right = None;
    }
    let left = left.filter(|l| l.1 < room);
    let fill = room - used(left, right);
    let mut line = edge(corners.0);
    if let Some((label, _)) = left {
        line.push_str(&edge("─ "));
        line.push_str(label);
        line.push_str(&edge(" "));
    }
    line.push_str(&edge(&"─".repeat(fill)));
    if let Some((label, _)) = right {
        line.push_str(&edge(" "));
        line.push_str(label);
        line.push_str(&edge(" ─"));
    }
    line.push_str(&edge(corners.1));
    line
}

/// "DIM" in the ANSI Shadow block style (port of `startup-logo.ts`); every
/// glyph is one cell wide.
const STARTUP_LOGO: [&str; 5] = [
    "██████╗ ██╗███╗   ███╗",
    "██╔══██╗██║████╗ ████║",
    "██║  ██║██║██╔████╔██║",
    "██████╔╝██║██║╚██╔╝██║",
    "╚═════╝ ╚═╝╚═╝ ╚═╝ ╚═╝",
];
const STARTUP_LOGO_WIDTH: usize = 22;
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
    let text_width = body_width.saturating_sub(STARTUP_LOGO_WIDTH + STARTUP_LOGO_GAP);
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

/// How tool groups and thoughts start: expanded unless the user saved
/// collapsed (`/settings` › Tool details, or `/tools`).
fn saved_tool_detail(home: &std::path::Path) -> Detail {
    match crate::ui::config::read(home, TOOL_DETAILS_KEY).and_then(|v| v.as_bool()) {
        Some(false) => Detail::Collapsed,
        _ => Detail::Expanded,
    }
}

/// A Termux resize that changes only the height comes from the on-screen
/// keyboard; it uses the ordinary frame path instead of rewriting scrollback.
fn termux_height_only(previous_columns: u16, size: (u16, u16), termux: bool) -> bool {
    termux && size.0 == previous_columns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_termux_keyboard_toggle_uses_the_frame_path() {
        assert!(termux_height_only(80, (80, 20), true));
        assert!(!termux_height_only(80, (100, 20), true));
        assert!(!termux_height_only(80, (80, 20), false));
    }

    #[test]
    fn tool_details_start_expanded_until_collapsed_is_saved() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(saved_tool_detail(home.path()), Detail::Expanded);
        assert_eq!(
            crate::ui::config::write(home.path(), TOOL_DETAILS_KEY, false.into()),
            None
        );
        assert_eq!(saved_tool_detail(home.path()), Detail::Collapsed);
        assert_eq!(
            crate::ui::config::write(home.path(), TOOL_DETAILS_KEY, true.into()),
            None
        );
        assert_eq!(saved_tool_detail(home.path()), Detail::Expanded);
    }

    fn test_theme() -> Theme {
        use crate::ui::theme::{Appearance, ColorMode};
        Theme::system(ColorMode::TrueColor, Appearance::Dark, None)
    }

    /// A thought only grows while it runs and never shrinks when it
    /// settles, in either detail: the rows under it would hop otherwise.
    /// A short thought takes only its own rows.
    #[test]
    fn a_thought_grows_as_it_runs_and_settles_no_shorter() {
        let p = &test_theme().palette;
        let short = "A short thought.";
        let long = (1..=12)
            .map(|n| format!("Paragraph {n} of the thought."))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(live_thinking(short, true, 60, p).len(), 3);
        assert_eq!(settled_thinking(short, Some(1000), true, 60, p).len(), 3);
        let cjk = "先看**结构**：模块、尺寸，以及集成测试。\n\n再看生成的架构图（crates.mmd），确认依赖方向是否正确……然后读功能清单。";
        for text in [long.as_str(), cjk] {
            let mut last = 0;
            for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
                let rows = live_thinking(&text[..end], true, 24, p).len();
                assert!(rows >= last, "shrank at {end}: {rows} < {last}");
                assert!(rows <= 2 + THINKING_ROWS, "{end}");
                last = rows;
            }
        }
        for text in [short, long.as_str()] {
            let live = live_thinking(text, true, 60, p);
            let settled = settled_thinking(text, Some(1000), true, 60, p);
            assert!(settled.len() >= live.len(), "{text:?}");

            let live = live_thinking(text, false, 60, p);
            let settled = settled_thinking(text, Some(1000), false, 60, p);
            assert_eq!(live.len(), 1, "{text:?}");
            assert_eq!(settled.len(), 1, "{text:?}");
        }
        let row = live_thinking(&long, false, 40, p).remove(0);
        assert!(row.contains("thought."), "{row:?}");
        assert!(!row.contains("Paragraph 1 "), "{row:?}");
    }

    /// A session that never answers: the interface alone is under test.
    struct Idle;

    impl SessionPort for Idle {
        fn send(&self, _: Command) {}
        fn drain(&self) -> Vec<Update> {
            Vec::new()
        }
        fn shutdown(self: Box<Self>) {}
    }

    /// A session that keeps every command the interface sends.
    struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<Command>>>);

    impl SessionPort for Recorder {
        fn send(&self, command: Command) {
            self.0.borrow_mut().push(command);
        }
        fn drain(&self) -> Vec<Update> {
            Vec::new()
        }
        fn shutdown(self: Box<Self>) {}
    }

    #[test]
    fn isolation_settings_edit_save_conflict_and_reset_without_model_execution() {
        use dim_agent_types::isolation::{
            IsolationEscalation, IsolationPolicy, IsolationPolicyEffective,
            IsolationPolicySnapshot, IsolationPolicySource, Sandbox,
        };
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        app.run_command("settings", "");
        for _ in 0..7 {
            app.handle_input(key(KeyCode::Down));
        }
        app.handle_input(key(KeyCode::Enter));
        assert!(matches!(sent.borrow().last(), Some(Command::ReadIsolation)));
        let policy = IsolationPolicy {
            sandbox: Some(Sandbox::Host),
            escalation: Some(IsolationEscalation::Deny),
            environment: Some(Vec::new()),
            read_only_paths: Some(vec!["/usr".into()]),
            ..IsolationPolicy::default()
        };
        let snapshot = IsolationPolicySnapshot {
            revision: "r1".into(),
            path: home.path().join("host-isolation.json"),
            source: IsolationPolicySource::File,
            effective: IsolationPolicyEffective {
                sandbox: Sandbox::Host,
                escalation: IsolationEscalation::Deny,
            },
            policy,
        };
        app.apply(Update::Isolation(Ok(snapshot)));
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Up));
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Down));
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Enter));
        for _ in 0..3 {
            app.handle_input(key(KeyCode::Down));
        }
        app.handle_input(key(KeyCode::Enter));
        match sent.borrow().last() {
            Some(Command::SaveIsolation {
                expected_revision,
                policy,
            }) => {
                assert_eq!(expected_revision, "r1");
                assert_eq!(policy.sandbox, Some(Sandbox::Network));
                assert_eq!(policy.escalation, Some(IsolationEscalation::Deny));
                assert_eq!(policy.environment, Some(Vec::new()));
                assert_eq!(policy.read_only_paths, Some(vec!["/usr".into()]));
                assert_eq!(policy.writable_paths, None);
            }
            _ => panic!("settings save must use local management, not model exec"),
        }
        app.apply(Update::Isolation(Err("isolation_policy_conflict".into())));
        for _ in 0..3 {
            app.handle_input(key(KeyCode::Down));
        }
        app.handle_input(key(KeyCode::Enter));
        assert!(
            matches!(sent.borrow().last(), Some(Command::SaveIsolation { expected_revision, policy }) if expected_revision == "r1" && policy.sandbox == Some(Sandbox::Network))
        );
        app.apply(Update::Isolation(Err("isolation_policy_conflict".into())));
        for _ in 0..4 {
            app.handle_input(key(KeyCode::Down));
        }
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Char('y')));
        assert!(
            matches!(sent.borrow().last(), Some(Command::ResetIsolation(revision)) if revision == "r1")
        );
    }

    /// A picked message opens a line of the shown line with the submitted
    /// text, and forks only `main`, after a confirmation, carrying the text
    /// into the fork (timeline UI §4.2, §4.3).
    #[test]
    fn a_picked_message_opens_a_line_or_forks_main() {
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let snapshot = |focus: &str| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                projection_id: 7,
                focus: focus.into(),
                ..ViewSnapshot::default()
            }))
        };
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        app.apply(snapshot("main"));
        app.draft_line(4, "try RwLock");
        assert!(app.place_label().contains("new line from main"));
        app.handle_input(key(KeyCode::Char('!')));
        app.handle_input(key(KeyCode::Enter));
        match sent.borrow().last() {
            Some(Command::OpenLine {
                session_id,
                projection_id,
                parent,
                at,
                prompt,
            }) => assert_eq!(
                (
                    session_id.as_str(),
                    *projection_id,
                    parent.as_str(),
                    *at,
                    prompt.as_str()
                ),
                ("s1", 7, "main", 4, "try RwLock!")
            ),
            _ => panic!("the submit opens the line"),
        }
        assert!(app.line_draft.is_none(), "one submit opens one line");
        app.draft_line(4, "again");
        app.handle_input(key(KeyCode::Esc));
        assert!(app.line_draft.is_none() && app.composer.is_empty());

        app.apply(snapshot("b-7c"));
        app.ask_fork(4, "from a line".into(), Some("try RwLock".into()));
        assert!(app.fork_confirm.is_none(), "a fork copies main only");
        app.apply(snapshot("main"));
        app.ask_fork(4, String::new(), None);
        assert!(app.place_label().contains("after the last turn"));
        app.handle_input(key(KeyCode::Char('n')));
        app.ask_fork(4, "try RwLock".into(), Some("try RwLock".into()));
        assert!(
            app.place_label().contains("before “try RwLock”"),
            "the question names the message"
        );
        app.handle_input(key(KeyCode::Char('n')));
        assert!(app.fork_confirm.is_none());
        let count = sent.borrow().len();
        app.ask_fork(4, "try RwLock".into(), Some("try RwLock".into()));
        app.handle_input(key(KeyCode::Char('y')));
        assert!(matches!(
            sent.borrow()[count..],
            [Command::Fork {
                projection_id: 7,
                at: 4,
                ..
            }]
        ));
        app.apply(Update::Switched {
            session_id: "s2".into(),
            kind: SwitchKind::Forked,
        });
        assert_eq!(app.composer.text(), "try RwLock");
    }

    /// `/branch` takes its prompt inline and opens the line in one submit;
    /// the line answers in the side panel, which `Esc` closes before
    /// anything else (timeline UI §4.7).
    #[test]
    fn branch_takes_its_prompt_inline_and_answers_beside() {
        use crate::model::view::TurnEnd;
        use crate::ui::render::panel::strip_escapes;
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let snapshot = |side: Option<SideLine>| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                projection_id: 7,
                focus: "main".into(),
                ended: Some((9, TurnEnd::Completed)),
                side,
                ..ViewSnapshot::default()
            }))
        };
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        let typed = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.handle_input(key(KeyCode::Char(c)));
            }
        };
        let screen = |app: &mut App| {
            app.frame((80, 40))
                .live
                .iter()
                .map(|row| strip_escapes(row))
                .collect::<Vec<_>>()
                .join("\n")
        };
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            focus: "main".into(),
            ..ViewSnapshot::default()
        })));
        typed(&mut app, "/branch before any turn ends");
        app.handle_input(key(KeyCode::Enter));
        assert_eq!(
            app.composer.text(),
            "/branch before any turn ends",
            "a line that cannot open keeps its prompt"
        );
        assert!(sent.borrow().is_empty());
        app.composer.clear();
        app.apply(snapshot(None));

        typed(&mut app, "/bra");
        app.handle_input(key(KeyCode::Tab));
        assert_eq!(
            app.composer.text(),
            "/branch ",
            "choosing fills the command"
        );
        assert!(screen(&mut app).contains("/branch <prompt>"));
        app.handle_input(key(KeyCode::Enter));
        assert_eq!(app.composer.text(), "/branch ", "no prompt, nothing sent");
        assert!(sent.borrow().is_empty());

        typed(&mut app, "try the parser");
        app.handle_input(key(KeyCode::Enter));
        assert!(app.composer.is_empty() && app.line_draft.is_none());
        assert!(
            matches!(
                sent.borrow().as_slice(),
                [Command::OpenLine { parent, at: 9, prompt, .. }]
                    if parent == "main" && prompt == "try the parser"
            ),
            "one submit opens the line at the end of the shown line"
        );

        let row = |kind, text: &str, tool: Option<&str>| {
            std::sync::Arc::new(ViewRow {
                row_id: text.into(),
                kind,
                text: text.into(),
                display: None,
                streaming: false,
                elapsed_ms: None,
                tone: None,
                tool_name: tool.map(Into::into),
                tool_input: tool.map(|_| serde_json::json!({"path": "cache.rs"})),
                batch: tool.map(|_| "q1".into()),
                pending: tool.is_some(),
                parked: false,
                result_text: None,
                result_is_error: false,
                diff: None,
                exit_code: None,
                member: None,
                cut: None,
                line: None,
                compaction: None,
            })
        };
        app.apply(snapshot(Some(SideLine {
            id: "b-7c".into(),
            purpose: "try the parser".into(),
            state: LineState::Working,
            rows: vec![
                row(RowKind::UserMessage, "try the parser", None),
                row(RowKind::ToolCall, "", Some("read")),
                row(
                    RowKind::StreamingAssistant,
                    "The lock is held across the await.",
                    None,
                ),
            ],
        })));
        let shown = screen(&mut app);
        assert!(shown.contains("⑂ try the parser · b-7c"), "{shown}");
        assert!(shown.contains("Exploring"), "the process shows: {shown}");
        assert!(shown.contains("The lock is held across the await."));
        assert!(shown.contains("⑂ working…"));
        let long: Vec<_> = (0..200)
            .map(|n| row(RowKind::AssistantMessage, &format!("reply {n}"), None))
            .collect();
        let side = SideLine {
            id: "b-7c".into(),
            purpose: "long".into(),
            state: LineState::Working,
            rows: long,
        };
        let tail = side_reply(
            &side,
            80,
            10,
            Detail::Collapsed,
            &app.palette,
            &app.markdown,
        );
        let tail = strip_escapes(&tail.join("\n"));
        assert!(
            tail.contains("reply 199") && !tail.contains("reply 150"),
            "{tail}"
        );
        if let Some(side) = app.snapshot.as_mut().and_then(|s| s.side.as_mut()) {
            side.state = LineState::Waiting;
        }
        let shown = screen(&mut app);
        assert!(
            shown.contains("⑂ done") && !shown.contains("waiting"),
            "a line whose turn ended reads done: {shown}"
        );

        app.handle_input(key(KeyCode::Esc));
        assert!(matches!(sent.borrow().last(), Some(Command::CloseSide)));
        assert!(!screen(&mut app).contains("b-7c"), "closed at once");
        let count = sent.borrow().len();
        app.handle_input(key(KeyCode::Esc));
        assert_eq!(sent.borrow().len(), count, "one Esc closes one panel");
    }

    /// `/plugins` lists through the worker, the panel's changes reach it as
    /// plugin commands, and `/plugins install PATH` installs the directory
    /// relative to the workspace.
    #[test]
    fn plugins_commands_reach_the_worker() {
        use crate::model::session::{PluginAction, PluginCandidate, PluginItem};
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let last = |sent: &std::cell::RefCell<Vec<Command>>| match sent.borrow().last() {
            Some(Command::ListPlugins) => "list".to_owned(),
            Some(Command::Plugin(action)) => format!("{action:?}"),
            Some(_) => "other".to_owned(),
            None => "none".to_owned(),
        };
        app.run_command("plugins", "");
        assert!(matches!(app.prompt, Some(Prompt::Plugins(_))));
        assert_eq!(last(&sent), "list");
        app.apply(Update::Plugins(Ok(vec![PluginItem {
            name: "web".into(),
            in_use: None,
            off: true,
            locked: false,
            candidates: vec![PluginCandidate {
                id: "default".into(),
            }],
            status: Some("off".into()),
            error: None,
            waiting_for: Vec::new(),
            refused: Vec::new(),
        }])));
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        app.handle_input(key(KeyCode::Enter));
        assert_eq!(
            last(&sent),
            format!("{:?}", PluginAction::Enable("web".into()))
        );
        app.apply(Update::PluginResult {
            action: PluginAction::Enable("web".into()),
            result: Ok(Default::default()),
            plugins: Ok(Vec::new()),
        });
        app.handle_input(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            KeyCode::Char('r'),
            Modifiers::CTRL,
        )));
        assert_eq!(last(&sent), "list");
        app.handle_input(key(KeyCode::Esc));
        assert!(app.prompt.is_none());
        app.run_command("plugins", "install tools/my-tool");
        assert_eq!(
            last(&sent),
            format!(
                "{:?}",
                PluginAction::Install(std::path::Path::new("/work").join("tools/my-tool"))
            )
        );
        let count = sent.borrow().len();
        app.prompt = None;
        app.run_command("plugins", "remove web");
        assert_eq!(sent.borrow().len(), count, "git and remove stay in the CLI");
        assert!(app.prompt.is_none());
    }

    /// The interface drawn into an emulated terminal, frame by frame.
    struct Drawn {
        app: App,
        screen: Screen,
        emu: crate::ui::render::testterm::Emu,
        size: (u16, u16),
        _home: tempfile::TempDir,
    }

    impl Drawn {
        fn new(size: (u16, u16), origin: Option<u16>, synchronized: bool) -> Self {
            let home = tempfile::tempdir().unwrap();
            let theme = test_theme();
            let app = App::new(
                Box::new(Idle),
                MarkdownRenderer::new(&theme),
                theme.palette.clone(),
                home.path().to_path_buf(),
                "/work".into(),
            );
            let mut screen = Screen::new(size, synchronized);
            if let Some(row) = origin {
                screen.set_origin(row);
            }
            let emu = crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
            let mut drawn = Self {
                app,
                screen,
                emu,
                size,
                _home: home,
            };
            drawn.draw();
            drawn
        }

        fn draw(&mut self) {
            let frame = self.app.frame(self.size);
            let bytes = self.screen.render(&frame, self.size);
            self.emu.feed(&bytes);
        }

        fn key(&mut self, code: KeyCode) {
            let key = crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE);
            let _ = self.app.handle(InputEvent::Key(key));
            self.draw();
        }

        fn ask(&mut self, call: crate::question::Call) {
            self.app.prompt = Some(Prompt::Ask(AskUserPrompt::new(call)));
            self.draw();
        }

        fn answered(&mut self) {
            self.app.prompt = None;
            self.draw();
        }

        /// Screen row of the composer's top border, or the prompt's in its
        /// place: the first box under the history.
        fn anchor(&self) -> usize {
            let rows = self.emu.rows_text();
            let banner = rows
                .iter()
                .rposition(|r| r.contains("Git branch"))
                .map_or(0, |b| b + 2);
            rows.iter()
                .enumerate()
                .skip(banner)
                .find(|(_, r)| r.trim_start().starts_with('╭'))
                .map(|(y, _)| y)
                .unwrap_or_else(|| panic!("no box on screen: {rows:#?}"))
        }

        fn scrolled(&self) -> usize {
            self.emu.scrollback.len()
        }

        /// Everything written so far: scrollback, then the screen.
        fn transcript(&self) -> String {
            self.emu.transcript().join("\n")
        }

        fn show(&mut self, rows: &[(&str, &str)], focus: &str, complete: bool) {
            let rows = rows
                .iter()
                .map(|(id, text)| {
                    std::sync::Arc::new(ViewRow {
                        row_id: (*id).into(),
                        kind: RowKind::AssistantMessage,
                        text: (*text).into(),
                        display: None,
                        streaming: false,
                        elapsed_ms: None,
                        tone: None,
                        tool_name: None,
                        tool_input: None,
                        batch: None,
                        pending: false,
                        parked: false,
                        result_text: None,
                        result_is_error: false,
                        diff: None,
                        exit_code: None,
                        member: None,
                        cut: None,
                        line: None,
                        compaction: None,
                    })
                })
                .collect();
            self.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                rows,
                focus: focus.into(),
                history_complete: complete,
                ..ViewSnapshot::default()
            })));
            self.draw();
        }
    }

    #[test]
    fn escape_commits_the_interrupt_after_the_answer_tail_and_before_stopped() {
        use crate::model::view::INTERRUPT_REASON;
        use crate::projection::Projection;
        use serde_json::json;

        for fullscreen in [false, true] {
            let mut drawn = Drawn::new((100, 40), None, true);
            drawn.app.fullscreen = fullscreen;
            let mut projection = Projection::default();
            let event: crate::wire::Event = serde_json::from_value(json!({
                "sequence": 1, "create_at": 1, "type": "step", "name": "text",
                "data": {"turn_id": "t1", "request_id": "r1", "delta": "Answer tail."}
            }))
            .unwrap();
            projection.apply("main", &event);
            projection.set_running(true);
            let publish = |drawn: &mut Drawn, projection: &Projection| {
                let mut snapshot = projection.snapshot();
                snapshot.session_id = "s1".into();
                snapshot.focus = "main".into();
                snapshot.history_complete = true;
                drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
            };
            publish(&mut drawn, &projection);
            drawn.draw();
            drawn.key(KeyCode::Esc);
            drawn.key(KeyCode::Esc);
            assert_eq!(
                drawn.app.notice.as_ref().map(|(text, _)| text.as_str()),
                Some(t("Stopping"))
            );
            assert!(!drawn.transcript().contains(INTERRUPT_REASON));

            for value in [
                json!({
                    "sequence": 2, "create_at": 2, "type": "step", "name": "model-output-end",
                    "data": {"turn_id": "t1", "request_id": "r1", "status": "cancelled", "reason": INTERRUPT_REASON}
                }),
                json!({
                    "sequence": 3, "create_at": 3, "type": "turn", "name": "end",
                    "data": {"turn_id": "t1", "status": "cancelled", "reason": INTERRUPT_REASON}
                }),
            ] {
                projection.apply("main", &serde_json::from_value(value).unwrap());
            }
            projection.set_running(false);
            publish(&mut drawn, &projection);
            for (_, shown) in drawn.app.revealed.values_mut() {
                *shown = 0;
            }
            let (commits, _, _) = drawn.app.transcript(97);
            assert!(
                commits.is_empty(),
                "the answer is still revealing: {commits:?}"
            );

            let now = Instant::now();
            for (pace, shown) in drawn.app.revealed.values_mut() {
                *shown = pace.advance(now + Duration::from_secs(1), "Answer tail.".len(), true);
            }
            drawn.draw();
            drawn.draw();
            let text = drawn.transcript();
            let answer = text.find("Answer tail.").unwrap();
            let interrupt = text.find(INTERRUPT_REASON).unwrap();
            let stopped = text.find("stopped").unwrap();
            assert!(answer < interrupt && interrupt < stopped, "{text}");
            assert_eq!(text.matches(INTERRUPT_REASON).count(), 1, "{text}");

            let mut resumed = Drawn::new((100, 40), None, true);
            publish(&mut resumed, &projection);
            resumed.draw();
            let text = resumed.transcript();
            assert!(
                text.find("Answer tail.").unwrap() < text.find(INTERRUPT_REASON).unwrap(),
                "{text}"
            );
            assert!(
                text.find(INTERRUPT_REASON).unwrap() < text.find("stopped").unwrap(),
                "{text}"
            );
        }
    }

    #[test]
    fn switching_lines_draws_each_transcript_under_its_boundary() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.show(&[("user-1", "main one")], "main", true);
        drawn.show(
            &[("user-2", "line one"), ("user-3", "line two")],
            "b-7c",
            true,
        );
        let text = drawn.transcript();
        assert_eq!(text.matches("line one").count(), 1, "{text}");
        assert_eq!(text.matches("line two").count(), 1, "{text}");
        assert!(text.contains("⑂ b-7c"), "the switch is marked: {text}");
        let line_switch = text.find("⑂ b-7c").unwrap();
        let main_one_after = text[line_switch..].matches("main one").count();
        assert_eq!(main_one_after, 0, "main's rows do not print on the line");
        drawn.show(&[("user-1", "main one")], "main", true);
        let text = drawn.transcript();
        assert!(text.contains("⑂ main"), "the return is marked: {text}");
        assert_eq!(text.matches("main one").count(), 2, "{text}");
        assert_eq!(text.matches("line one").count(), 1, "{text}");
    }

    #[test]
    fn the_scrollback_composer_names_the_shown_line() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.show(&[("user-1", "main one")], "main", true);
        drawn.show(&[("user-2", "line one")], "b-7c", true);
        let rows = drawn.emu.rows_text();
        let border = rows
            .iter()
            .rev()
            .find(|row| row.contains('╰'))
            .expect("the composer's bottom border");
        assert!(border.contains("⑂ b-7c"), "{border}");
        let text = drawn.transcript();
        let boundary = text
            .rfind("──── ⑂ b-7c ────")
            .expect("the switch's boundary row");
        let replay = text.rfind("line one").expect("the line's row");
        assert!(boundary < replay, "{text}");
    }

    #[test]
    fn a_line_switch_is_no_result_and_every_running_line_counts() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.notify = true;
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            main_running: true,
            focus: "main".into(),
            ..ViewSnapshot::default()
        })));
        drawn.app.notifications.clear();
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            main_running: true,
            focus: "b-7c".into(),
            ..ViewSnapshot::default()
        })));
        assert!(
            !drawn
                .app
                .notifications
                .iter()
                .any(|event| matches!(event, crate::ui::notify::Event::Complete)),
            "{:?}",
            drawn.app.notifications
        );
        assert_eq!(drawn.app.busy_reason(), Some(ExitReason::AgentRunning));
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            focus: "b-7c".into(),
            lines: vec![
                crate::model::view::LineView {
                    id: "main".into(),
                    parent: None,
                    purpose: String::new(),
                    state: LineState::Main,
                    cut: None,
                },
                crate::model::view::LineView {
                    id: "b-91".into(),
                    parent: Some("main".into()),
                    purpose: "review".into(),
                    state: LineState::Working,
                    cut: Some(crate::model::view::LineCut {
                        sequence: 1,
                        messages: 0,
                    }),
                },
            ],
            ..ViewSnapshot::default()
        })));
        assert_eq!(drawn.app.busy_reason(), Some(ExitReason::AgentRunning));
    }

    #[test]
    fn the_window_topic_stays_mains_message_while_a_line_is_shown() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows: vec![std::sync::Arc::new(ViewRow {
                row_id: "r1".into(),
                kind: RowKind::UserMessage,
                text: "line work".into(),
                display: None,
                streaming: false,
                elapsed_ms: None,
                tone: None,
                tool_name: None,
                tool_input: None,
                batch: None,
                pending: false,
                parked: false,
                result_text: None,
                result_is_error: false,
                diff: None,
                exit_code: None,
                member: None,
                cut: None,
                line: None,
                compaction: None,
            })],
            focus: "b-7c".into(),
            main_topic: Some("fix the login bug".into()),
            ..ViewSnapshot::default()
        })));
        assert_eq!(drawn.app.topic().as_deref(), Some("fix the login bug"));
    }

    #[test]
    fn a_line_shows_its_own_rows_after_a_long_main_history() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        let main: Vec<(String, String)> = (0..120)
            .map(|n| (format!("m{n}"), format!("main row {n}")))
            .collect();
        let main: Vec<(&str, &str)> = main
            .iter()
            .map(|(id, text)| (id.as_str(), text.as_str()))
            .collect();
        drawn.show(&main, "main", true);
        let text = drawn.transcript();
        assert!(!text.contains("main row 19"), "{text}");
        assert!(text.contains("main row 20"), "{text}");
        drawn.show(
            &[
                ("l1", "line first"),
                ("l2", "line second"),
                ("l3", "line third"),
            ],
            "b-7c",
            true,
        );
        let text = drawn.transcript();
        for row in ["line first", "line second", "line third"] {
            assert!(text.contains(row), "{row} is drawn: {text}");
        }
    }

    /// A resumed session of `count` user messages `ask N`, each cut at `N`.
    fn resume_asks(drawn: &mut Drawn, count: usize) {
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        let rows = (0..count)
            .map(|n| {
                std::sync::Arc::new(ViewRow {
                    row_id: format!("u{n}"),
                    kind: RowKind::UserMessage,
                    text: format!("ask {n}"),
                    display: None,
                    streaming: false,
                    elapsed_ms: None,
                    tone: None,
                    tool_name: None,
                    tool_input: None,
                    batch: None,
                    pending: false,
                    parked: false,
                    result_text: None,
                    result_is_error: false,
                    diff: None,
                    exit_code: None,
                    member: None,
                    cut: Some(n as u64),
                    line: None,
                    compaction: None,
                })
            })
            .collect();
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            ..ViewSnapshot::default()
        })));
        drawn.draw();
    }

    #[test]
    fn a_timeline_jump_in_scrollback_draws_earlier_history_from_the_message() {
        let mut drawn = Drawn::new((80, 40), None, true);
        resume_asks(&mut drawn, 120);
        let text = drawn.transcript();
        assert!(
            !text.contains("ask 19\n") && text.contains("ask 20"),
            "{text}"
        );
        assert!(text.contains("earlier history: /timeline"), "{text}");
        drawn.app.run_command("timeline", "");
        assert!(matches!(drawn.app.prompt, Some(Prompt::Timeline(_))));
        drawn.key(KeyCode::Home);
        drawn.key(KeyCode::Down);
        drawn.key(KeyCode::Down);
        drawn.key(KeyCode::Enter);
        assert!(drawn.app.prompt.is_none(), "a jump closes the panel");
        let text = drawn.transcript();
        let page = text.find("Earlier history").expect("the page is drawn");
        assert!(text[page..].contains("ask 2"), "{text}");
        assert!(!text[page..].contains("ask 1\n"), "{text}");
        drawn.app.run_command("timeline", "");
        drawn.key(KeyCode::Enter);
        assert_eq!(
            drawn.app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some(t("Already on screen above; /tui jumps to it"))
        );
    }

    #[test]
    fn fullscreen_tops_earlier_history_with_a_load_more_row() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.fullscreen = true;
        resume_asks(&mut drawn, 230);
        let first = |app: &App| {
            app.history
                .iter()
                .position(|block| matches!(block, Block::LoadMore(_)))
        };
        let at = first(&drawn.app).expect("the load-more row");
        assert!(matches!(drawn.app.history[at], Block::LoadMore(130)));
        assert!(
            matches!(&drawn.app.history[at + 1], Block::User { text, .. } if text == "ask 130")
        );
        assert_eq!(drawn.app.cards(at), [LOAD_MORE]);
        drawn.app.open_card(LOAD_MORE);
        drawn.draw();
        let at = first(&drawn.app).expect("still earlier history");
        assert!(matches!(drawn.app.history[at], Block::LoadMore(30)));
        let kept = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::User { text, .. } if text == "ask 130"))
            .expect("the old top is drawn again");
        assert_eq!(drawn.app.jump, Some(Jump::Show(drawn.app.dropped + kept)));
        drawn.app.jump = None;
        drawn.app.open_card(LOAD_MORE);
        drawn.draw();
        assert_eq!(first(&drawn.app), None, "everything is drawn");
        assert!(
            drawn
                .app
                .history
                .iter()
                .any(|block| matches!(block, Block::User { text, .. } if text == "ask 0"))
        );
    }

    #[test]
    fn a_fullscreen_timeline_jump_picks_the_message_drawing_it_first() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.fullscreen = true;
        resume_asks(&mut drawn, 120);
        drawn.app.run_command("timeline", "");
        drawn.key(KeyCode::Home);
        drawn.key(KeyCode::Down);
        drawn.key(KeyCode::Enter);
        let picked = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::User { text, .. } if text == "ask 1"))
            .expect("the message is drawn");
        assert_eq!(
            drawn.app.jump,
            Some(Jump::Block(drawn.app.dropped + picked))
        );
    }

    #[test]
    fn image_markers_stay_out_of_reused_message_drafts() {
        let mut projection = crate::projection::Projection::default();
        let input: crate::wire::Event = serde_json::from_value(serde_json::json!({
            "id": "image", "sequence": 1, "create_at": 1, "type": "input", "name": "message",
            "data": {"user": {"content": [
                {"type": "text", "text": "look [Image #7]"},
                {"type": "image", "source": {"type": "url", "mediaType": "image/png", "url": "https://invalid.example/image.png"}},
            ]}, "mode": "queue"},
        })).unwrap();
        projection.apply("main", &input);
        let mut snapshot = projection.snapshot();
        snapshot.session_id = "s1".into();
        snapshot.focus = "main".into();
        snapshot.history_complete = true;
        let mut drawn = Drawn::new((80, 40), None, true);
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        drawn.app.host = Box::new(Recorder(sent.clone()));
        drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
        drawn.draw();
        assert!(drawn.transcript().contains("[Image #1]"));
        let row_id = drawn.app.snapshot.as_ref().unwrap().rows[0].row_id.clone();
        let message = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::User { .. }))
            .unwrap();
        assert_eq!(
            Document::message(&drawn.app, message),
            Some("look [Image #7]")
        );
        for action in [
            TimelineAction::Edit,
            TimelineAction::Quote,
            TimelineAction::Branch,
            TimelineAction::Fork,
        ] {
            drawn.app.composer.clear();
            drawn.app.line_draft = None;
            drawn.app.timeline(&row_id, action);
            if action == TimelineAction::Fork {
                drawn.key(KeyCode::Char('y'));
                drawn.app.apply(Update::Switched {
                    session_id: "forked".into(),
                    kind: SwitchKind::Forked,
                });
            }
            assert!(drawn.app.composer.text().contains("look [Image #7]"));
            assert!(!drawn.app.composer.text().contains("[Image #1]"));
            drawn.key(KeyCode::Enter);
            match sent.borrow().last().unwrap() {
                Command::Submit(text) => assert!(!text.contains("[Image #1]")),
                Command::OpenLine { prompt, .. } => assert_eq!(prompt, "look [Image #7]"),
                _ => panic!("message reuse must submit text or open a line"),
            }
        }
        projection.set_running(true);
        let mut queued = input;
        queued.id = "queued-image".into();
        queued.sequence = 2;
        projection.apply("main", &queued);
        let mut snapshot = projection.snapshot();
        snapshot.session_id = "s1".into();
        snapshot.focus = "main".into();
        drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
        drawn.app.composer.clear();
        drawn.key(KeyCode::Up);
        assert_eq!(drawn.app.composer.text(), "look [Image #7]");
        assert!(
            matches!(sent.borrow().last(), Some(Command::InputControl { input_id, action: "withdraw" }) if input_id == "queued-image")
        );
        drawn.key(KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Submit(text)) if text == "look [Image #7]")
        );
    }

    #[test]
    fn timeline_letters_edit_quote_and_branch_the_message() {
        let mut drawn = Drawn::new((80, 40), None, true);
        resume_asks(&mut drawn, 3);
        drawn.app.run_command("timeline", "");
        drawn.key(KeyCode::Up);
        drawn.key(KeyCode::Char('e'));
        assert!(drawn.app.prompt.is_none());
        assert_eq!(drawn.app.composer.text(), "ask 1");
        drawn.app.composer.clear();
        drawn.app.run_command("timeline", "");
        drawn.key(KeyCode::Char('b'));
        let draft = drawn.app.line_draft.as_ref().expect("a line draft");
        assert_eq!((draft.parent.as_str(), draft.at), ("main", 2));
        assert_eq!(drawn.app.composer.text(), "ask 2");
    }

    /// Entering fullscreen lays out only the shown Session's transcript,
    /// not what scrollback committed before it.
    #[test]
    fn entering_fullscreen_reloads_the_shown_transcript() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.show(&[("m1", "first session work")], "main", true);
        drawn.app.apply(Update::Switched {
            session_id: "s2".into(),
            kind: crate::model::session::SwitchKind::Resumed,
        });
        drawn.show(&[("m2", "second session work")], "main", true);
        let committed = |app: &App| format!("{:?}", app.history);
        assert!(committed(&drawn.app).contains("first session work"));

        drawn.app.fullscreen = true;
        drawn.app.redraw_window();
        let _ = drawn.app.frame(drawn.size);
        let shown = committed(&drawn.app);
        assert!(shown.contains("second session work"), "{shown}");
        assert!(!shown.contains("first session work"), "{shown}");
    }

    #[test]
    fn a_session_switch_drops_a_line_boundary_the_old_one_still_owed() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.show(&[("m1", "main work")], "main", true);
        drawn.show(&[("l1", "line work")], "b-7c", true);
        let snapshot = |session: &str, focus: &str, complete: bool| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: session.into(),
                focus: focus.into(),
                history_complete: complete,
                ..ViewSnapshot::default()
            }))
        };
        drawn.app.apply(snapshot("s1", "main", false));
        drawn.draw();
        assert!(
            !drawn.transcript().contains("⑂ main"),
            "the boundary of the line shown first is still owed"
        );
        drawn.app.apply(Update::Switched {
            session_id: "s2".into(),
            kind: crate::model::session::SwitchKind::Resumed,
        });
        drawn.app.apply(snapshot("s2", "main", true));
        drawn.draw();
        let text = drawn.transcript();
        assert!(text.contains("resumed s2"), "{text}");
        assert!(
            !text.contains("⑂ main"),
            "the new session starts at main: {text}"
        );
        assert_eq!(text.matches("⑂ b-7c").count(), 1, "{text}");
    }

    fn questions() -> crate::question::Call {
        use crate::question::{Call, Choice, Question};
        let choices = |n: usize| {
            (0..n)
                .map(|i| Choice {
                    label: format!("option {i}"),
                    description: (i % 2 == 0).then(|| format!("about option {i}")),
                })
                .collect()
        };
        let question = |id: &str, n: usize| Question {
            id: id.into(),
            header: Some(id.into()),
            question: format!("Which {id}?"),
            options: choices(n),
            multi_select: false,
            allow_custom: true,
        };
        Call {
            branch_id: "main".into(),
            request_id: "ask".into(),
            call_id: "call".into(),
            questions: vec![question("tall", 6), question("short", 2)],
        }
    }

    /// The command menu and a question, opened and closed again and again,
    /// at the bottom of the screen or with room below: the composer never
    /// moves up while there is room, history scrolls at most once, and every
    /// round after the first leaves the screen exactly as the first did.
    #[test]
    fn menus_and_questions_never_lift_the_composer_twice() {
        for (origin, synchronized) in [
            (None, true),
            (None, false),
            (Some(0), true),
            (Some(3), false),
        ] {
            let mut drawn = Drawn::new((80, 40), origin, synchronized);
            let start = drawn.anchor();
            let round = |drawn: &mut Drawn| {
                let scrolled = drawn.scrolled();
                let mut last = drawn.anchor();
                let mut check = |drawn: &Drawn, what: &str| {
                    let at = drawn.anchor();
                    let bottom = !drawn.emu.row(drawn.size.1 as usize - 1).is_empty();
                    assert!(
                        at >= last || bottom,
                        "{what}: the composer moved up from {last} to {at}: {:#?}",
                        drawn.emu.rows_text()
                    );
                    last = at;
                };
                drawn.key(KeyCode::Char('/'));
                check(drawn, "menu opened");
                drawn.key(KeyCode::Down);
                drawn.key(KeyCode::Esc);
                drawn.key(KeyCode::Esc);
                check(drawn, "menu closed");
                drawn.ask(questions());
                check(drawn, "question asked");
                drawn.key(KeyCode::Tab);
                check(drawn, "short question");
                drawn.key(KeyCode::Tab);
                check(drawn, "review");
                drawn.key(KeyCode::BackTab);
                drawn.key(KeyCode::BackTab);
                check(drawn, "tall question again");
                drawn.answered();
                check(drawn, "answered");
                (
                    drawn.scrolled() - scrolled,
                    drawn.anchor(),
                    drawn.emu.rows_text(),
                )
            };
            let first = round(&mut drawn);
            let label = format!("origin {origin:?} sync {synchronized}");
            if origin.is_some() {
                assert_eq!(first.0, 0, "{label}: room below, nothing scrolls");
                assert_eq!(
                    first.1, start,
                    "{label}: the composer stayed: {:#?}",
                    first.2
                );
            }
            for _ in 0..4 {
                let again = round(&mut drawn);
                assert_eq!(again.0, 0, "{label}: scrolled again: {:#?}", again.2);
                assert_eq!(again.1, first.1, "{label}");
                assert_eq!(again.2, first.2, "{label}");
            }
            assert_eq!(drawn.emu.flickers, 0, "{label}");
        }
    }

    /// The queue strip keeps the height it reached while messages leave.
    #[test]
    fn the_queue_strip_holds_its_height_as_the_queue_drains() {
        let p = &test_theme().palette;
        let item = |n: usize| (format!("id{n}"), format!("message {n}"));
        let three: Vec<_> = (1..=3).map(item).collect();
        let full = queue_strip(&three, 3, 80, p);
        assert_eq!(queue_strip(&three[..1], 3, 80, p).len(), full.len());
    }

    /// A finished command keeps the status row its running block had.
    #[test]
    fn a_settled_shell_run_keeps_its_status_row() {
        let theme = test_theme();
        let p = &theme.palette;
        let markdown = MarkdownRenderer::new(&theme);
        let output = vec!["one".to_owned(), "two".to_owned()];
        let live = render_shell("ls", false, &output, 0, Some(("Running…", &p.muted)), 60, p);
        let settled = render_block(
            &Block::Shell(ShellRun {
                command: "ls".to_owned(),
                outcome: ShellOutcome::Exited(0),
                output,
                total_lines: 2,
                private: false,
            }),
            60,
            p,
            &markdown,
        );
        assert_eq!(settled.len(), live.len());
    }

    #[test]
    fn the_goodbye_box_lines_up_with_the_welcome_box() {
        let welcome = boxed(
            &["Welcome".to_owned()],
            40,
            &|e| e.to_owned(),
            &|_, b| b.to_owned(),
            None,
        );
        let edge = |row: &str| row.find(|c: char| c != ' ');
        for row in goodbye_panel(Some("session")) {
            assert_eq!(edge(&row), edge(&welcome[0]), "{row:?}");
        }
        let text = |row: &str| row.find(|c: char| c.is_alphanumeric());
        assert_eq!(
            text(&goodbye_panel(None)[1]),
            text(&welcome[1]),
            "{:?}",
            goodbye_panel(None)
        );
    }

    #[test]
    fn startup_panels_keep_their_greeting_and_width_in_narrow_terminals() {
        let theme = test_theme();
        let opened = WebOpened {
            url: "http://127.0.0.1:43201/",
            sign_in: None,
            session_id: None,
        };
        for cols in [120, 31, 30, 29, 24, 15] {
            let welcome = banner(cols, &theme.palette, "main");
            let web = web_panel(cols, &theme.palette, &opened);
            for panel in [&welcome, &web] {
                for row in panel.lines() {
                    assert_eq!(width_of(row), cols - 2, "{cols}: {row:?}");
                }
            }
            if cols >= 29 {
                assert!(welcome.contains(env!("CARGO_PKG_VERSION")), "{welcome}");
                assert!(web.contains(t("DIM Web is ready")), "{web}");
            }
        }
    }

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
        assert_eq!(crate::ui::text::display_width("◆ hello"), 7);
    }

    /// An interface whose Session asks one question, and what it sends.
    fn asked() -> (
        App,
        std::rc::Rc<std::cell::RefCell<Vec<Command>>>,
        tempfile::TempDir,
    ) {
        asked_over(ViewSnapshot::default())
    }

    /// [`asked`] over a snapshot whose other fields are `snapshot`'s.
    fn asked_over(
        mut snapshot: ViewSnapshot,
    ) -> (
        App,
        std::rc::Rc<std::cell::RefCell<Vec<Command>>>,
        tempfile::TempDir,
    ) {
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let mut call = questions();
        call.questions.truncate(1);
        snapshot.session_id = "s1".into();
        snapshot.projection_id = 7;
        snapshot.focus = "main".into();
        snapshot.questions = vec![call];
        app.apply(Update::Snapshot(Box::new(snapshot)));
        (app, sent, home)
    }

    fn press(app: &mut App, code: KeyCode) {
        let _ = app.handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            code,
            Modifiers::NONE,
        )));
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn selection(app: &App) -> Option<Vec<String>> {
        match &app.prompt {
            Some(Prompt::Ask(prompt)) => prompt.selections.get("tall").cloned(),
            _ => None,
        }
    }

    /// With a question pending and its answer to be chosen, `/` and Ctrl+O
    /// open the palette, the keys go to it, and Enter runs a command there:
    /// nothing is answered or declined, and the question keeps its
    /// selections.
    #[test]
    fn a_command_runs_over_a_question_that_keeps_its_answers() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            selection(&app).as_deref(),
            Some(&["option 0".to_owned()][..])
        );

        press(&mut app, KeyCode::Char('/'));
        assert!(app.composer.palette_open(), "the palette opened over it");
        typed(&mut app, "mcp");
        assert!(
            app.frame((80, 40))
                .live
                .iter()
                .any(|row| row.contains("Commands")),
            "the palette is drawn in the panel's place"
        );
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Mcp(args)) if args.is_empty()),
            "the palette ran /mcp"
        );
        assert_eq!(
            selection(&app).as_deref(),
            Some(&["option 0".to_owned()][..]),
            "the question keeps its selection"
        );
        assert!(
            !sent
                .borrow()
                .iter()
                .any(|c| matches!(c, Command::AskAnswers { .. } | Command::AskCancel { .. })),
            "nothing was answered or declined"
        );
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        match sent.borrow().last() {
            Some(Command::AskAnswers { answers, .. }) => assert_eq!(
                answers[0].selected,
                vec!["option 0".to_owned()],
                "the question answers with the selection it kept"
            ),
            _ => panic!("the question answers"),
        }
    }

    /// A question stays pending while the palette is up and comes back with
    /// its state when the palette closes.
    #[test]
    fn closing_the_palette_gives_the_keys_back_to_the_question() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char(' '));
        let ctrl_o = crate::ui::terminal::KeyEvent::new(KeyCode::Char('o'), Modifiers::CTRL);
        let _ = app.handle(InputEvent::Key(ctrl_o));
        assert!(app.composer.palette_open(), "Ctrl+O opened the palette");
        typed(&mut app, "mc");
        press(&mut app, KeyCode::Esc);
        assert!(app.composer.palette_open(), "Esc resets the query first");
        press(&mut app, KeyCode::Esc);
        assert!(!app.composer.palette_open(), "the second Esc closes it");
        assert!(matches!(&app.prompt, Some(Prompt::Ask(_))));
        assert_eq!(
            selection(&app).as_deref(),
            Some(&["option 0".to_owned()][..])
        );
        assert!(
            !sent
                .borrow()
                .iter()
                .any(|c| matches!(c, Command::AskCancel { .. })),
            "Esc in the palette does not decline"
        );

        press(&mut app, KeyCode::Esc);
        assert!(
            matches!(sent.borrow().last(), Some(Command::AskCancel { .. })),
            "Esc on the question still declines"
        );
    }

    /// `/branch` needs its prompt: choosing it fills `/branch `, the
    /// composer keeps the keys through the prompt, and Enter runs the
    /// command with the question still pending and its panel hidden.
    #[test]
    fn an_argument_command_takes_its_argument_over_the_question() {
        use crate::model::view::TurnEnd;
        let (mut app, sent, _home) = asked_over(ViewSnapshot {
            ended: Some((8, TurnEnd::Completed)),
            ..ViewSnapshot::default()
        });
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "branch");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.composer.text(),
            "/branch ",
            "the row filled the command"
        );
        assert!(
            !app.composer.palette_open(),
            "the argument is not a palette"
        );

        typed(&mut app, "review the patch");
        assert_eq!(app.composer.text(), "/branch review the patch");
        let live = app.frame((80, 40)).live.join("\n");
        assert!(
            live.contains("/branch review the patch"),
            "the composer shows the argument where the palette was"
        );
        assert!(
            !live.contains("Which tall?"),
            "the question's panel stays hidden"
        );

        press(&mut app, KeyCode::Enter);
        assert!(app.composer.is_empty(), "the command ran");
        assert!(
            matches!(
                sent.borrow().last(),
                Some(Command::OpenLine { at: 8, prompt, .. })
                    if prompt == "review the patch"
            ),
            "Enter ran the branch command with its prompt"
        );
        assert!(
            matches!(&app.prompt, Some(Prompt::Ask(_))),
            "the question is still pending"
        );
        assert_eq!(
            selection(&app).as_deref(),
            Some(&["option 0".to_owned()][..]),
            "it keeps its selection"
        );
    }

    /// A space after `/mcp` starts its argument form rather than ending
    /// command entry, so the argument reaches the command.
    #[test]
    fn a_space_after_a_command_keeps_its_argument_over_the_question() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "mcp");
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.composer.text(), "/mcp ");
        typed(&mut app, "list");
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Mcp(args)) if args == "list"),
            "the argument form ran /mcp with its argument"
        );
        assert!(matches!(&app.prompt, Some(Prompt::Ask(_))));
    }

    /// Esc while an argument is typed cancels command entry: the keys go
    /// back to the question with its selection, and neither the question
    /// nor a running turn is stopped.
    #[test]
    fn esc_in_the_argument_phase_returns_to_the_question() {
        let (mut app, sent, _home) = asked_over(ViewSnapshot {
            running: true,
            ..ViewSnapshot::default()
        });
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "branch");
        press(&mut app, KeyCode::Enter);
        typed(&mut app, "half typed");
        press(&mut app, KeyCode::Esc);
        assert!(app.composer.is_empty(), "the command draft is cleared");
        assert!(
            matches!(&app.prompt, Some(Prompt::Ask(_))),
            "the question is still open"
        );
        assert_eq!(
            selection(&app).as_deref(),
            Some(&["option 0".to_owned()][..]),
            "it keeps its selection"
        );
        assert!(
            !sent
                .borrow()
                .iter()
                .any(|c| matches!(c, Command::AskCancel { .. } | Command::StopQuestion { .. })),
            "Esc did not decline the question or interrupt the turn"
        );

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::AskAnswers { .. })),
            "the keys are back with the question"
        );
    }

    /// Backspacing the command away ends command entry and gives the keys
    /// back to the question.
    #[test]
    fn backspacing_the_command_away_returns_to_the_question() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "mcp");
        for _ in 0..4 {
            press(&mut app, KeyCode::Backspace);
        }
        assert!(app.composer.is_empty());
        press(&mut app, KeyCode::Esc);
        assert!(
            matches!(sent.borrow().last(), Some(Command::AskCancel { .. })),
            "Esc on the question declines it again"
        );
    }

    /// A question the Session drops while its argument is typed ends
    /// command entry: the draft stays and the composer runs it as usual.
    #[test]
    fn a_question_dropped_while_typing_keeps_the_command_draft() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "mcp");
        press(&mut app, KeyCode::Char(' '));
        typed(&mut app, "list");
        assert_eq!(app.composer.text(), "/mcp list");

        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            projection_id: 7,
            focus: "main".into(),
            ..ViewSnapshot::default()
        })));
        assert!(app.prompt.is_none(), "the question is gone");
        assert_eq!(app.composer.text(), "/mcp list", "the draft stays");
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Mcp(args)) if args == "list"),
            "the composer runs the command it kept"
        );
    }

    /// A command that opens its own panel keeps the question aside and
    /// hands it back when that panel closes.
    #[test]
    fn a_panel_command_gives_the_question_back() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "models");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.prompt, Some(Prompt::Models(_))));
        assert!(matches!(sent.borrow().last(), Some(Command::ListModels)));

        press(&mut app, KeyCode::Esc);
        assert_eq!(
            selection(&app).as_deref(),
            Some(&["option 0".to_owned()][..]),
            "the question came back with its selection"
        );
    }

    /// A question answered elsewhere while its panel is open is not
    /// brought back when the panel closes.
    #[test]
    fn a_question_dropped_under_a_panel_stays_gone() {
        let (mut app, _sent, _home) = asked();
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "models");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.prompt, Some(Prompt::Models(_))));

        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            projection_id: 7,
            focus: "main".into(),
            ..ViewSnapshot::default()
        })));
        assert!(
            matches!(app.prompt, Some(Prompt::Models(_))),
            "the panel stays open"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none(), "the question is gone for good");
    }

    /// A question dropped under the open palette leaves the palette as the
    /// ordinary one.
    #[test]
    fn a_question_dropped_under_the_palette_leaves_the_palette() {
        let (mut app, sent, _home) = asked();
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "mc");
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            projection_id: 7,
            focus: "main".into(),
            ..ViewSnapshot::default()
        })));
        assert!(app.prompt.is_none());
        assert_eq!(app.composer.text(), "/mc", "the query stays");
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Mcp(args)) if args.is_empty()),
            "the palette ran /mcp"
        );
    }

    /// `/` on the free-text row opens the palette rather than starting the
    /// answer; once the row edits, `/` is text and the answer may start
    /// with a path.
    #[test]
    fn the_free_text_answer_takes_slash_only_once_it_edits() {
        let (mut app, sent, _home) = asked();
        for _ in 0..6 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('/'));
        assert!(
            app.composer.palette_open(),
            "`/` opens the palette on the row"
        );
        assert!(
            matches!(&app.prompt, Some(Prompt::Ask(prompt)) if !prompt.typing),
            "it did not start the answer"
        );
        press(&mut app, KeyCode::Esc);
        assert!(!app.composer.palette_open());

        press(&mut app, KeyCode::Enter);
        assert!(matches!(&app.prompt, Some(Prompt::Ask(prompt)) if prompt.typing));
        typed(&mut app, "/usr/local");
        match &app.prompt {
            Some(Prompt::Ask(prompt)) => assert_eq!(prompt.draft.text(), "/usr/local"),
            _ => panic!("the answer is being edited"),
        }
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        match sent.borrow().last() {
            Some(Command::AskAnswers { answers, .. }) => assert_eq!(
                answers[0].custom.as_deref(),
                Some("/usr/local"),
                "/ inside the answer is text"
            ),
            _ => panic!("the answer carries the path"),
        }
    }
}
