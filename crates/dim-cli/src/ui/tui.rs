//! The interactive interface. The welcome banner is committed once into
//! scrollback; the conversation streams in as whole Markdown blocks (2.4), the
//! unclosed tail and the composer box live at the bottom and repaint by row
//! diff (2.3). The terminal layer (2.2) guarantees the exit contract.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::management::{self, editor};
use crate::model::session::{
    Area, Command, CompactReply, ContextReading, DIM_OAUTH_ID, DraftPart, ManagementAction,
    ManagementRefusal, ManagementRequest, ManagementResult, RenameReply, SessionPort, SubmitMode,
    SwitchKind, Update,
};
#[cfg(test)]
use crate::model::shell::Outcome as ShellOutcome;
use crate::model::shell::ShellRun;
use crate::model::view::{
    CompactionState, LineState, LineView, RowKind, SideLine, TaskTarget, ViewRow, ViewSnapshot,
};
use crate::permission::{file_label, tool_label, tool_policy_label};
use crate::ui::Env;
use crate::ui::approvals_panel::ApprovalsPanel;
use crate::ui::commands::{COMMANDS, Context as CommandContext, Needs};
use crate::ui::composer::{Composer, Outcome as ComposerOutcome, Shell, history};
use crate::ui::i18n::{self, t, tf};
use crate::ui::language_panel::LanguagePanel;
use crate::ui::lines_panel::{LinesPanel, short_id};
use crate::ui::management_panel::ManagementPanel;
use crate::ui::models_panel::ModelsPanel;
use crate::ui::oauth_view::OauthView;
use crate::ui::plugins_panel::PluginsPanel;
use crate::ui::progress::ProgressBar;
use crate::ui::prompts::{
    AskUserPrompt, Effect, ExitConfirm, ExitReason, PermissionPrompt, WriteTarget,
};
use crate::ui::providers_panel::ProvidersPanel;
use crate::ui::render::fullscreen::Document;
use crate::ui::render::fullscreen::MouseOutcome;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::render::panel::{clip, wrap, wrap_words};
use crate::ui::render::{Frame, FullScreen, Overlay, Screen};
use crate::ui::runtime_panel::RuntimePanel;
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
use crate::wire::Configuration;

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
const GIT_BRANCH_POLL: Duration = Duration::from_secs(10);
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
    history::flush();
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
    let palette = Theme::detect(&detect::env_from_process(), true, None)
        .1
        .palette;
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
    i18n::set(i18n::interface_locale(&surroundings.home));
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
    let (default_id, detected) = Theme::detect(&env, true, background.as_deref());
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
        active: saved.unwrap_or_else(|| default_id.to_owned()),
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
        let mouse = app.mouse && !app.native_selection();
        if full.is_some() && terminal.mouse() != mouse && terminal.set_mouse(mouse).is_err() {
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
                    app.set_fullscreen(entering);
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
                Outcome::Edit { area, target, text } => {
                    // A draft a refused write kept for this target continues in
                    // its own file; anything else starts from the document the
                    // panel read.
                    let (target, draft, reopened) = match app.kept_draft(area, &target) {
                        Some((target, draft)) => (target, draft, true),
                        None => {
                            let draft = match editor::Draft::open(&app.home, area, &text) {
                                Ok(draft) => draft,
                                Err(error) => {
                                    if let Some(Prompt::Management(panel)) = &mut app.prompt {
                                        panel.failed(error);
                                    }
                                    dirty = true;
                                    continue;
                                }
                            };
                            (target, draft, false)
                        }
                    };
                    let mut command = editor::command(draft.path());
                    let program = command.get_program().to_string_lossy().into_owned();
                    let ran = match terminal.run_external(&mut command) {
                        Ok(ran) => ran,
                        Err(_) => {
                            let code = terminal.lost();
                            app.host.shutdown();
                            return code;
                        }
                    };
                    app.write_draft(area, target, draft, reopened, program, ran);
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
    /// A management panel's edit: the document's text and what its write
    /// becomes, for the external editor.
    Edit {
        area: Area,
        target: WriteTarget,
        text: String,
    },
    /// `/web`: flip the daemon-owned Web entry switch without leaving.
    Web,
    /// Start the updated CLI from this path on the session.
    Restart(PathBuf),
    Exit(i32),
}

/// The adopted model as the label draws it: `provider / model`, and the
/// reasoning effort the configuration sends when it carries one.
struct ModelLabel {
    model: String,
    effort: Option<String>,
}

impl ModelLabel {
    fn new(configuration: &Configuration) -> Self {
        Self {
            model: format!("{} / {}", configuration.provider_id, configuration.model_id),
            effort: configuration
                .model_preferences
                .as_ref()
                .and_then(|preferences| preferences.reasoning_effort.clone()),
        }
    }

    fn full(&self) -> String {
        match &self.effort {
            Some(effort) => format!("{} · {effort}", self.model),
            None => self.model.clone(),
        }
    }

    /// The first shape of the label that `fits`, whole before whole minus the
    /// effort; `None` when even the model alone does not fit.
    fn fitted(&self, fits: impl Fn(&str) -> bool) -> Option<String> {
        let full = self.full();
        if fits(&full) {
            return Some(full);
        }
        if self.effort.is_some() && fits(&self.model) {
            return Some(self.model.clone());
        }
        None
    }
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
    reports: Vec<Block>,
    banner_committed: bool,
    snapshot: Option<ViewSnapshot>,
    /// The adopted model and its reasoning effort, once the session exists.
    label: Option<ModelLabel>,
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
    answered: HashSet<(String, String)>,
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
    /// Committed user rows (history ids) the model had not consumed yet:
    /// fullscreen redraws them once it does.
    message_blocks: Vec<usize>,
    /// The second their running time was last drawn for.
    card_second: u64,
    session_id: String,
    permission_policy: dim_agent_types::PermissionPolicy,
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
    /// A surface was opened from the Permissions page: closing it goes back
    /// to the page, as Process isolation and Command rules do.
    permissions_return: bool,
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
    /// The workspace's git branch for the banner and composer.
    branch: String,
    git_head: Option<PathBuf>,
    git_checked: Instant,
    /// The shell-mode command running, at most one.
    shell: Option<crate::ui::shell::Running>,
    /// A Ctrl+V clipboard read in progress.
    clipboard: Option<ClipboardRead>,
    /// A pasted image path being read and scaled off the interface thread.
    image_read: Option<ImageRead>,
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
    /// A management area's drafts while the editor runs and until their write
    /// comes back: a refused write keeps its own, so the edit key of the
    /// target it was taken for reopens the user's text.
    management_edits: Vec<ManagementDraft>,
    /// The entry a management action the panel sent names, with the label the
    /// panel showed then: the panel may close before the reply lands.
    management_named: Option<(Area, ManagementAction, Option<String>)>,
    /// Queued messages withdrawn for the draft, by input id, until the host
    /// confirms each withdrawal; a confirmed one goes into the draft.
    recovering: Vec<(String, String)>,
}

/// A management area's draft of one document: the text the editor was
/// given, and the target whose write expects it. One target holds at most
/// one draft, and another target's draft is never replaced by it.
struct ManagementDraft {
    area: Area,
    target: WriteTarget,
    draft: editor::Draft,
}

/// What a management reply says beyond its view: the owner's answer, or an
/// applied action's own line.
fn reported_note(result: &ManagementResult) -> Option<&str> {
    match result {
        ManagementResult::View { note, .. } => note.as_deref(),
        ManagementResult::Applied { note, .. } => note.as_deref(),
        ManagementResult::Detail(_) | ManagementResult::Refused(_) => None,
    }
}

/// The notice a management reply leaves: the line the action reports and,
/// when the read after it failed, why. The owner writes both parts — a
/// job's name, a run's state — so they are made safe, and kept to one line,
/// at the one place that joins them.
fn management_notice(line: &str, read_error: Option<&str>) -> String {
    let line = crate::sanitize::safe_line(line);
    match read_error {
        Some(error) => format!("{line} {}", crate::sanitize::safe_line(error)),
        None => line,
    }
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

/// A pasted group whose image paths a thread is reading; the tokens go in
/// at the cursor when its result arrives.
struct ImageRead {
    plan: Vec<crate::ui::composer::PlannedFile>,
    receive: std::sync::mpsc::Receiver<Result<Vec<Option<crate::images::Attached>>, String>>,
}

enum Prompt {
    Sessions(SessionsPanel),
    Tasks(TasksPanel),
    Lines(Box<LinesPanel>),
    Timeline(Box<TimelinePanel>),
    Models(ModelsPanel),
    Providers(Box<ProvidersPanel>),
    Plugins(PluginsPanel),
    Runtime(RuntimePanel),
    Management(ManagementPanel),
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
        let git_head = git_head(&cwd_key);
        let branch = git_head.as_deref().and_then(git_branch);
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
            message_blocks: Vec::new(),
            card_second: 0,
            session_id: String::new(),
            permission_policy: dim_agent_types::PermissionPolicy {
                preset: dim_agent_types::PermissionPreset::WorkspaceWrite,
                ..Default::default()
            },
            context: None,
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
            permissions_return: false,
            mouse,
            notify,
            notifications: Vec::new(),
            revealed: std::collections::HashMap::new(),
            notified_request: None,
            runs: Runs::default(),
            palette_stash: None,
            command_entry: false,
            branch: branch.unwrap_or_else(|| t("Not available").into()),
            git_head,
            git_checked: Instant::now(),
            shell: None,
            clipboard: None,
            image_read: None,
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
            management_edits: Vec::new(),
            management_named: None,
            recovering: Vec::new(),
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

    fn native_selection(&self) -> bool {
        match &self.prompt {
            Some(Prompt::Login(view)) => view.native_selection(),
            Some(Prompt::Providers(panel)) => panel.native_selection(),
            _ => false,
        }
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

    /// Sends the oldest queued message in as steering (Ctrl+S, and Enter on an
    /// empty draft).
    fn steer(&mut self) {
        if let Some((id, _)) = self.queued().into_iter().next() {
            self.host.send(Command::InputControl {
                input_id: id,
                action: "steer",
            });
        }
    }

    /// Whether a key steers the draft: Ctrl+S during a run with a draft.
    /// Without a draft, Ctrl+S steers the oldest queued message.
    fn steers_draft(&self, key: &KeyEvent) -> bool {
        key.code == KeyCode::Char('s')
            && key.mods.contains(Modifiers::CTRL)
            && self.running()
            && !self.composer.is_empty()
            && self.composer.shell().is_none()
            && !self.composer.palette_open()
            && !self.composer.mention_open()
    }

    /// Withdraws the newest queued message into the draft (Shift+Left).
    fn recall_newest_queued(&mut self) {
        if let Some((id, text)) = self.queued().pop() {
            self.withdraw(id, text);
        }
    }

    /// Withdraws every queued message, oldest first, for an interrupt. The
    /// withdrawals reach the session before its stop, so no queued message
    /// starts the next run.
    fn recall_all_queued(&mut self) {
        for (id, text) in self.queued() {
            self.withdraw(id, text);
        }
    }

    fn withdraw(&mut self, id: String, text: String) {
        self.recovering.push((id.clone(), text));
        self.host.send(Command::InputControl {
            input_id: id,
            action: "withdraw",
        });
    }

    /// Puts a withdrawn message into the draft after the text already there,
    /// once the session confirmed the withdrawal.
    fn restore_withdrawn(&mut self, input_id: &str) {
        let Some(at) = self.recovering.iter().position(|(id, _)| id == input_id) else {
            return;
        };
        let (_, text) = self.recovering.remove(at);
        let draft = self.composer.text();
        if draft.trim().is_empty() {
            self.composer.set_draft(&text);
        } else {
            self.composer.set_draft(&format!("{draft}\n{text}"));
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
        let approval = snapshot.approvals.iter().find(|a| {
            !self
                .answered
                .contains(&(a.branch_id.clone(), a.request_id.clone()))
        });
        let question = snapshot.questions.first();
        match &self.prompt {
            Some(Prompt::Exit(_))
            | Some(Prompt::Sessions(_))
            | Some(Prompt::Models(_))
            | Some(Prompt::Providers(_))
            | Some(Prompt::Plugins(_))
            | Some(Prompt::Runtime(_))
            | Some(Prompt::Management(_))
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
                if approval.is_some_and(|a| {
                    a.request_id == p.request_id && a.branch_id == p.branch_id
                }) => {}
            Some(Prompt::Ask(p)) if question.is_some_and(|q| q == &p.call) => {}
            _ => {
                if let Some(Prompt::Upgrade(dialog)) = self.prompt.take() {
                    self.upgrade = Some(dialog);
                }
                self.prompt = if let Some(a) = approval {
                    let mut prompt = PermissionPrompt::new(
                        a.request_id.clone(),
                        a.branch_id.clone(),
                        a.reason.clone(),
                        a.tool_name.clone(),
                        a.tool_input.clone(),
                        a.always.is_some(),
                    );
                    prompt.set_member(a.member.clone());
                    Some(Prompt::Permission(prompt))
                } else {
                    question.map(|q| {
                        let held = self.question_stash.take().filter(|held| held.call == *q);
                        Prompt::Ask(held.unwrap_or_else(|| AskUserPrompt::new(q.clone())))
                    })
                };
            }
        }
        if let Some(Prompt::Permission(prompt)) = &mut self.prompt
            && let Some(approval) = approval
        {
            prompt.set_member(approval.member.clone());
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
        if now.duration_since(self.git_checked) >= GIT_BRANCH_POLL {
            self.git_checked = now;
            if let Some(branch) = self.git_head.as_deref().and_then(git_branch)
                && branch != self.branch
            {
                self.branch = branch;
                changed = true;
            }
        }
        if let Some(Prompt::Tasks(panel)) = &mut self.prompt
            && let Some(snapshot) = &self.snapshot
            && panel.tick(snapshot)
        {
            changed = true;
        }
        if let Some(Prompt::Runtime(panel)) = &mut self.prompt
            && panel.tick(now)
        {
            self.host.send(Command::ListRuntimes);
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
        let read = match &self.image_read {
            Some(read) => match read.receive.try_recv() {
                Ok(result) => Some(Ok(result)),
                // The thread ended without a result: a decoder panicked, or
                // the send failed. Leaving the read pending would refuse
                // every later paste, so it is a failed read.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(())),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            },
            None => None,
        };
        if let Some(read) = read {
            let pending = self.image_read.take().expect("pending image read");
            match read {
                Ok(Ok(images)) => {
                    self.composer.insert_files(pending.plan, images);
                    self.notice = None;
                }
                Ok(Err(reason)) => self.set_notice(reason),
                Err(()) => self.set_notice(t("The image could not be read")),
            }
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
    /// running, without their `input/applied` yet.
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
            Update::Withdrawn { input_id } => self.restore_withdrawn(&input_id),
            Update::Ready(configuration) => {
                self.label = Some(ModelLabel::new(&configuration));
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
            Update::Report { text, warning } => self.reports.push(Block::Status {
                text,
                tone: if warning { Tone::Warning } else { Tone::Muted },
            }),
            Update::Web { enabled, open, url } => {
                if enabled && open {
                    match url {
                        Some(url) => self.reports.push(Block::Status {
                            text: format!("Web interface on: {url}"),
                            tone: Tone::Muted,
                        }),
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
            Update::Permissions(policy) => match policy {
                Ok(policy) => {
                    if let Some(Prompt::Approvals(panel)) = &mut self.prompt {
                        panel.set_policy(&policy);
                    }
                    self.permission_policy = policy;
                    if let Some(Prompt::Settings(panel)) = &self.prompt {
                        self.open_settings(panel.selected());
                    }
                }
                Err(error) => self.notices.push(error),
            },
            Update::PermissionSelected(change) => {
                let (key, value) = change.config();
                let label = match change {
                    crate::permission::Change::FileAccess(value) => {
                        format!("File access: {}", file_label(value))
                    }
                    crate::permission::Change::ToolApproval(value) => {
                        format!("Tool approval: {}", tool_label(value))
                    }
                };
                let message = crate::ui::config::write(&self.home, key, value).map_or_else(
                    || format!("{label} (also for new sessions)"),
                    |error| format!("{label} applied, but default could not be saved: {error}"),
                );
                self.set_notice(message);
            }
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
            Update::Runtimes {
                result,
                runtimes,
                plugins,
            } => {
                if let Some(Prompt::Runtime(panel)) = &mut self.prompt {
                    panel.set(result, runtimes, plugins);
                }
            }
            Update::Management(reply) => {
                // The panel may have closed since it sent the action; this is
                // the entry's label from when it did.
                let named = self.take_named(&reply.request);
                let creating = matches!(reply.request.action, ManagementAction::Add { .. });
                let write =
                    creating || matches!(reply.request.action, ManagementAction::Save { .. });
                let reload = reply.request.action == ManagementAction::Reload;
                // Only a reload of the area discards its drafts. Every other
                // reply — a toggle, a read, another area's reply — leaves the
                // kept text and the revision it came from for the edit key; a
                // revision the owner no longer honors answers the next write
                // with a conflict.
                if reload {
                    self.management_edits
                        .retain(|kept| kept.area != reply.request.area);
                }
                // One write is in flight at a time, and its reply settles only
                // the draft of the target it was sent for.
                let settled = if write {
                    self.take_draft(&reply.request)
                } else {
                    None
                };
                // A reply belongs to a panel only when that panel manages the
                // reply's area: another area's panel is as absent as a closed
                // one, and the notice still reports what the action came to.
                let same_area = matches!(
                    &self.prompt,
                    Some(Prompt::Management(panel)) if panel.area() == reply.request.area
                );
                if same_area {
                    let Some(Prompt::Management(panel)) = &mut self.prompt else {
                        unreachable!("the matched prompt is a management panel")
                    };
                    panel.set_result(&reply);
                    // The write took it: the file is no longer the user's
                    // text. That holds when only the read that reports the
                    // area failed, since the change itself was made. A
                    // refused write keeps the file and says where it is;
                    // after a conflict on an edit the area was read again, so
                    // the link to the stale revision goes and the next edit
                    // opens the current document. A conflict on a create has
                    // no revision to lose: the next add opens the user's text.
                    if let Some(settled) = settled {
                        match &reply.result {
                            ManagementResult::Refused(ManagementRefusal::Conflict { .. })
                                if creating =>
                            {
                                panel.conflicted(
                                    reply.request.area,
                                    true,
                                    &settled.draft.path().display().to_string(),
                                );
                                self.hold_draft(settled);
                            }
                            ManagementResult::Refused(ManagementRefusal::Conflict { .. }) => {
                                panel.conflicted(
                                    reply.request.area,
                                    false,
                                    &settled.draft.path().display().to_string(),
                                );
                            }
                            ManagementResult::Refused(_) => {
                                panel.kept(
                                    reply.request.area,
                                    &settled.draft.path().display().to_string(),
                                );
                                self.hold_draft(settled);
                            }
                            _ => settled.draft.remove(),
                        }
                    }
                } else if write {
                    // Esc closed the panel while the write was in flight;
                    // the reply still settles the draft and says what it
                    // came to.
                    match (&reply.result, settled) {
                        (ManagementResult::Refused(error), Some(settled)) => {
                            let where_it_is = tf(
                                "Your text is kept in {path}.",
                                &[("path", &settled.draft.path().display().to_string())],
                            );
                            self.set_notice(format!("{error} {where_it_is}"));
                            // A conflict on an edit drops the link, as it does
                            // with the panel open; a create has no revision to
                            // lose, and a refusal keeps it either way.
                            if creating
                                || matches!(
                                    &reply.result,
                                    ManagementResult::Refused(ManagementRefusal::Refused(_))
                                )
                            {
                                self.hold_draft(settled);
                            }
                        }
                        (ManagementResult::Refused(error), None) => {
                            self.set_notice(error.to_string())
                        }
                        (result, Some(settled)) => {
                            settled.draft.remove();
                            // The reply's own note, else the line its action
                            // reports; never a line that names only "saved".
                            let note = reported_note(result).map_or_else(
                                || {
                                    management::applied_line(
                                        &reply.request.action,
                                        named.as_deref(),
                                    )
                                },
                                str::to_owned,
                            );
                            let read_error = match result {
                                ManagementResult::Applied { read_error, .. } => {
                                    Some(read_error.as_str())
                                }
                                _ => None,
                            };
                            self.set_notice(management_notice(&note, read_error));
                        }
                        (_, None) => {}
                    }
                } else if reload {
                    match &reply.result {
                        ManagementResult::View { view, .. } => {
                            let text = crate::ui::management_panel::reloaded(
                                reply.request.area,
                                view.as_ref(),
                            );
                            self.reports.push(Block::Status {
                                text,
                                tone: Tone::Muted,
                            });
                        }
                        // The owner rescanned; only the read of the result
                        // failed, so the rescan stands and the read is what
                        // the notice reports after it.
                        ManagementResult::Applied { note, read_error } => {
                            let line = note.clone().unwrap_or_else(|| {
                                management::applied_line(&reply.request.action, None)
                            });
                            self.set_notice(management_notice(&line, Some(read_error)))
                        }
                        ManagementResult::Refused(error) => self.set_notice(error.to_string()),
                        // A reload is not an inspect: the adapter answers it
                        // with the area or a refusal, never a detail.
                        ManagementResult::Detail(_) => {}
                    }
                } else if let Some(note) = reported_note(&reply.result) {
                    // An action that opened no draft, such as a run, still
                    // reports what the owner answered.
                    let read_error = match &reply.result {
                        ManagementResult::Applied { read_error, .. } => Some(read_error.as_str()),
                        _ => None,
                    };
                    self.set_notice(management_notice(note, read_error));
                } else if let ManagementResult::Applied {
                    note: None,
                    read_error,
                } = &reply.result
                {
                    // The panel is gone and the owner named nothing beyond the
                    // view: the entry's label is the one it held when it sent
                    // the action.
                    let line = management::applied_line(&reply.request.action, named.as_deref());
                    self.set_notice(management_notice(&line, Some(read_error)));
                }
            }
            Update::Login(account) => {
                let signed_in = matches!(account, Ok(Some(_)));
                self.signed_in = signed_in;
                let dim = match &mut self.prompt {
                    Some(Prompt::Login(view)) => {
                        view.set_account(account);
                        Some(DIM_OAUTH_ID.to_owned())
                    }
                    Some(Prompt::Providers(panel)) => {
                        panel.set_account(account);
                        panel.dim_provider_id().map(str::to_owned)
                    }
                    _ => None,
                };
                if signed_in && let Some(provider_id) = dim {
                    self.host.send(Command::LoginUsage(provider_id));
                }
            }
            Update::Usage(usage) => match &mut self.prompt {
                Some(Prompt::Login(view)) => view.set_usage(usage),
                Some(Prompt::Providers(panel)) => panel.set_usage(usage),
                _ => {}
            },
            Update::UsageReport(usage) => self.reports.push(match usage {
                Ok(usage) => Block::AccountUsage(usage),
                Err(reason) => Block::Status {
                    text: format!(
                        "{} · {}",
                        t("DimAgent OAuth"),
                        tf("Usage: {reason}", &[("reason", &reason)])
                    ),
                    tone: Tone::Warning,
                },
            }),
            Update::Context(context) => self.context = context,
            Update::Compacted(reply) => {
                let (text, tone) = match reply {
                    CompactReply::Failed(message) => (
                        [t("Context compaction failed"), " · ", message.as_str()].concat(),
                        Tone::Warning,
                    ),
                    CompactReply::Unsupported => (
                        t("This daemon does not support context compaction").to_owned(),
                        Tone::Warning,
                    ),
                };
                self.reports.push(Block::Status { text, tone });
            }
            Update::Renamed(reply) => {
                let (text, tone) = match reply {
                    RenameReply::Titled(title) => {
                        (tf("Renamed to {title}", &[("title", &title)]), Tone::Muted)
                    }
                    RenameReply::Untitled => {
                        (t("The Session is now untitled.").to_owned(), Tone::Muted)
                    }
                    RenameReply::Unsupported => (
                        t("This daemon does not support renaming Sessions").to_owned(),
                        Tone::Warning,
                    ),
                    RenameReply::Failed(message) => (
                        [t("Renaming failed"), " · ", message.as_str()].concat(),
                        Tone::Warning,
                    ),
                };
                self.reports.push(Block::Status { text, tone });
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
            Update::IsolationBackend(result) => {
                if let Some(Prompt::Isolation(panel)) = &mut self.prompt {
                    panel.set_backend(result);
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
            Update::ApprovalPending {
                branch_id,
                request_id,
            } => {
                self.answered
                    .remove(&(branch_id.clone(), request_id.clone()));
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
                    Some(Prompt::Management(panel)) => panel.paste(&text),
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
                Prompt::Runtime(p) => p.handle_key(key),
                Prompt::Management(p) => p.handle_key(key),
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
        let (key, mode) = if self.steers_draft(&key) {
            (
                KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                SubmitMode::Steer,
            )
        } else {
            (key, SubmitMode::Queue)
        };
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
                if self.composer.mention_key(key).is_some() {
                    return Outcome::Continue;
                }
                self.set_notice(t("Wait for clipboard insertion before sending"));
                return Outcome::Continue;
            }
            KeyCode::Enter if key.mods.0 == 0 && self.image_read.is_some() => {
                self.set_notice(t("Wait for the image insertion before sending"));
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
                    self.recall_all_queued();
                    self.interrupt();
                } else if self.composer.shell().is_some() {
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
            KeyCode::Enter
                if key.mods.0 == 0 && self.composer.is_empty() && !self.queued().is_empty() =>
            {
                self.steer();
                return Outcome::Continue;
            }
            KeyCode::Left
                if key.mods.0 == Modifiers::SHIFT.0
                    && self.composer.is_empty()
                    && !self.queued().is_empty() =>
            {
                self.recall_newest_queued();
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
                history::append(&self.home, &self.cwd_key, &text);
                self.composer.push_history(&text);
                self.jump = Some(Jump::Bottom);
                if mode == SubmitMode::Queue && self.running() {
                    self.set_notice(t("Message queued"));
                }
                self.host.submit(text, mode);
            }
            ComposerOutcome::SubmitDraft { text, parts } => {
                let text = text.trim().to_owned();
                history::append(&self.home, &self.cwd_key, &text);
                self.composer.push_history(&text);
                self.jump = Some(Jump::Bottom);
                if mode == SubmitMode::Queue && self.running() {
                    self.set_notice(t("Message queued"));
                }
                self.host.send(Command::SubmitDraft(parts, mode));
            }
            ComposerOutcome::Shell { command, shell } => {
                let entry = format!("{}{command}", shell.prefix());
                history::append(&self.home, &self.cwd_key, &entry);
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
            Some(paths) => self.attach_paths(&paths),
            None => self.composer.paste(&text),
        }
    }

    /// Attaches a pasted group. The image paths are read and scaled off the
    /// interface thread; their tokens go in at the cursor when the read
    /// returns, so the frame never waits for a decode.
    fn attach_paths(&mut self, paths: &[std::path::PathBuf]) {
        if self.image_read.is_some() {
            self.set_notice(t(
                "An image is still being inserted; paste again when it is in",
            ));
            return;
        }
        // A clipboard read can still attach its image, which would take the
        // group past the count it was planned against.
        if self.clipboard.is_some() {
            self.set_notice(t(
                "The clipboard is still being inserted; paste again when it is in",
            ));
            return;
        }
        let plan = match self.composer.plan_files(paths) {
            Ok(plan) => plan,
            Err(error) => {
                self.set_notice(error);
                return;
            }
        };
        if plan.iter().any(|planned| planned.read) {
            let (send, receive) = std::sync::mpsc::channel();
            let reading = plan.clone();
            let started = std::thread::Builder::new()
                .name("dim-images".into())
                .spawn(move || {
                    let _ = send.send(crate::ui::composer::read_planned(&reading));
                });
            match started {
                Ok(_) => {
                    self.image_read = Some(ImageRead { plan, receive });
                    self.set_notice(t("Reading image…"));
                }
                Err(error) => self.set_notice(error.to_string()),
            }
            return;
        }
        let images = vec![None; plan.len()];
        self.composer.insert_files(plan, images);
        self.notice = None;
    }

    /// Ctrl+V: reads the native clipboard off the interface thread, or asks
    /// the terminal for its clipboard when the native one is out of reach.
    fn read_clipboard(&mut self) {
        if self.clipboard.is_some() {
            return;
        }
        if self.image_read.is_some() {
            self.set_notice(t(
                "An image is still being inserted; paste again when it is in",
            ));
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
        match source {
            Ok(Source::Text(text)) => {
                self.notice = None;
                self.paste(text);
            }
            Ok(Source::Files(paths)) => self.attach_paths(&paths),
            Ok(Source::Image(image)) => match self.composer.attach_image(image) {
                Ok(()) => self.notice = None,
                Err(reason) => self.set_notice(tf("Clipboard: {reason}", &[("reason", &reason)])),
            },
            Err(error) => {
                self.set_notice(tf("Clipboard: {reason}", &[("reason", &error.to_string())]))
            }
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
        history::append(&self.home, &self.cwd_key, &text);
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
            Effect::RetryApproval {
                request_id,
                branch_id,
            } => {
                self.answered
                    .remove(&(branch_id.clone(), request_id.clone()));
                self.host.send(Command::RetryApproval {
                    request_id,
                    branch_id,
                });
            }
            Effect::Approval {
                request_id,
                branch_id,
                allow,
                remember,
                grant,
            } => {
                self.answered
                    .insert((branch_id.clone(), request_id.clone()));
                self.host.send(Command::Approval {
                    request_id,
                    branch_id,
                    allow,
                    remember,
                    grant,
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
            Effect::OpenPermissions => {
                self.open_prompt(Prompt::Approvals(ApprovalsPanel::new(
                    &self.permission_policy,
                )));
            }
            Effect::OpenArea { area, note } => {
                // A surface opened from the Permissions page returns there on
                // Esc; any other caller opens with no return.
                self.permissions_return = matches!(self.prompt, Some(Prompt::Approvals(_)));
                self.open_management(area, note);
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
                let active = locale.unwrap_or_else(i18n::system_locale);
                i18n::set(active);
                self.host.send(Command::SetLocale(active.code()));
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
            Effect::Setting(setting) => {
                let from_permissions = setting == Setting::Isolation
                    && self.settings_return.is_none()
                    && matches!(self.prompt, Some(Prompt::Approvals(_)));
                self.setting(setting);
                if from_permissions {
                    self.settings_return = None;
                    self.permissions_return = true;
                }
            }
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
            Effect::SetPermission(change) => {
                self.prompt = None;
                self.host.send(Command::SetPermission(change));
            }
            Effect::Provider(action) => self.host.send(Command::Provider(action)),
            Effect::Plugin(action) => self.host.send(Command::Plugin(action)),
            Effect::Runtime(action) => self.host.send(Command::Runtime(action)),
            Effect::OpenRuntime(name) => {
                self.open_prompt(Prompt::Runtime(RuntimePanel::on(&name)));
                self.host.send(Command::ListRuntimes);
            }
            Effect::Management(request) => {
                self.remember_named(&request);
                self.host.send(Command::Management(request));
            }
            Effect::EditManagement { area, target, text } => {
                return Outcome::Edit { area, target, text };
            }
            Effect::OauthStart(start) => self.host.send(Command::OauthStart(start)),
            Effect::OauthCancel => self.host.send(Command::OauthCancel),
            Effect::OauthBack => {}
            Effect::OauthPaste(url) => self.host.send(Command::OauthPaste(url)),
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
            Effect::Logout(target) => self.host.send(Command::Logout(target)),
            Effect::LoginStatus(provider_id) => self.host.send(Command::LoginStatus(provider_id)),
            Effect::LoginUsage(provider_id) => self.host.send(Command::LoginUsage(provider_id)),
            Effect::OpenUrl(url) => {
                let opened = crate::browser::open(&url);
                self.set_notice(if opened {
                    t("Opened link")
                } else {
                    t("The browser could not be opened. Open the link manually, then return here.")
                });
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
                sandbox,
                network,
            } => self.host.send(Command::SaveIsolation {
                expected_revision,
                sandbox,
                network,
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
        if self.prompt.is_none() && std::mem::take(&mut self.permissions_return) {
            self.open_prompt(Prompt::Approvals(ApprovalsPanel::new(
                &self.permission_policy,
            )));
        }
        if self.prompt.is_none()
            && let Some(setting) = self.settings_return.take()
        {
            self.open_settings(setting);
        }
        if self.prompt.is_none() {
            self.sync_prompt();
            self.reopen_upgrade();
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
            .find(|a| {
                !self
                    .answered
                    .contains(&(a.branch_id.clone(), a.request_id.clone()))
            })
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

    /// Opens `/settings`: the panel, or a second-level switch it carries.
    fn settings(&mut self, args: &str) {
        let mut words = args.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (None, _, _) => self.open_settings(Setting::Theme),
            (Some("recording"), None, None) => {
                self.host.send(Command::Recording(String::new()));
            }
            (Some("recording"), Some(on @ ("on" | "off")), None) => {
                self.host.send(Command::Recording(on.to_owned()));
            }
            _ => self.set_notice("Usage: /settings [recording [on|off]]".to_owned()),
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
            acp_sessions: crate::ui::config::saved_show_acp(&self.home),
            file_access: file_label(self.permission_policy.file_access()).to_owned(),
            approvals: tool_policy_label(&self.permission_policy).to_owned(),
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
            Setting::FileAccess => {
                let panel = ApprovalsPanel::file(&self.permission_policy);
                self.open_prompt(Prompt::Approvals(panel));
            }
            Setting::Approvals => {
                let panel = ApprovalsPanel::tool(&self.permission_policy);
                self.open_prompt(Prompt::Approvals(panel));
            }
            Setting::Isolation => {
                let panel = crate::ui::isolation_panel::IsolationPanel::new();
                self.open_prompt(Prompt::Isolation(Box::new(panel)));
                self.host.send(Command::ReadIsolation);
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
            Setting::AcpSessions => {
                let show = !crate::ui::config::saved_show_acp(&self.home);
                if let Some(error) = crate::ui::config::save_show_acp(&self.home, show) {
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

    /// The draft a refused write kept for `target`, if its text is still
    /// there: the editor reopens it as it is, so an edit that continues
    /// keeps working in its own file, at the revision that text came from.
    /// Another target's kept draft is none of this target's business.
    fn kept_draft(
        &mut self,
        area: Area,
        target: &WriteTarget,
    ) -> Option<(WriteTarget, editor::Draft)> {
        let at = self
            .management_edits
            .iter()
            .position(|kept| kept.area == area && kept.target.same_target(target))?;
        let kept = self.management_edits.remove(at);
        if kept.draft.text().is_err() {
            self.management_edits.push(kept);
            return None;
        }
        Some((kept.target, kept.draft))
    }

    /// Records the label the open panel shows for the entry `request` names,
    /// which the notice owes the user when the panel closes before the reply.
    fn remember_named(&mut self, request: &ManagementRequest) {
        let Some(Prompt::Management(panel)) = &self.prompt else {
            return;
        };
        self.management_named = Some((
            request.area,
            request.action.clone(),
            panel.label_of(&request.action),
        ));
    }

    /// The label the panel held for the entry this reply names, when the
    /// panel sent the action and then closed.
    fn take_named(&mut self, request: &ManagementRequest) -> Option<String> {
        let same = self
            .management_named
            .as_ref()
            .is_some_and(|(area, action, _)| *area == request.area && *action == request.action);
        if same {
            self.management_named.take().and_then(|(_, _, name)| name)
        } else {
            None
        }
    }

    /// Takes the draft the reply's request was sent for, leaving every other
    /// target's kept draft where it is.
    fn take_draft(&mut self, request: &ManagementRequest) -> Option<ManagementDraft> {
        let at = self.management_edits.iter().position(|kept| {
            kept.area == request.area && kept.target.is_written_by(&request.action)
        })?;
        Some(self.management_edits.remove(at))
    }

    /// Holds a draft under its target: one target keeps one draft, and the
    /// file the newest text replaces is no longer linked.
    fn hold_draft(&mut self, kept: ManagementDraft) {
        while let Some(at) = self
            .management_edits
            .iter()
            .position(|held| held.area == kept.area && held.target.same_target(&kept.target))
        {
            self.management_edits.remove(at).draft.remove();
        }
        self.management_edits.push(kept);
    }

    /// Says why an edit could not go on, in the panel or as a notice when
    /// its panel is gone.
    fn failed(&mut self, message: String) {
        if let Some(Prompt::Management(panel)) = &mut self.prompt {
            panel.failed(message);
        } else {
            self.set_notice(message);
        }
    }

    /// Keeps a draft file and the target it was taken for, and says where the
    /// user's text is.
    fn keep_draft(
        &mut self,
        area: Area,
        target: WriteTarget,
        draft: editor::Draft,
        message: String,
    ) {
        self.failed(message);
        self.hold_draft(ManagementDraft {
            area,
            target,
            draft,
        });
    }

    /// What the external editor left: a non-zero exit and an unreadable or
    /// unparseable draft write nothing and keep their file, the unchanged
    /// document writes nothing, and a document is written with the revision
    /// the panel read, or created when the draft is a new entry.
    fn write_draft(
        &mut self,
        area: Area,
        target: WriteTarget,
        draft: editor::Draft,
        reopened: bool,
        program: String,
        result: std::io::Result<std::process::ExitStatus>,
    ) {
        let path = draft.path().display().to_string();
        let read = match result {
            Ok(status) if status.success() => draft.text(),
            Ok(status) => {
                // The text the editor leaves on a non-zero exit is not a
                // document to write; the file keeps it, as a refusal does.
                self.keep_draft(
                    area,
                    target,
                    draft,
                    format!(
                        "{} Nothing was written. Your text is kept in {path}.",
                        editor::exited(status)
                    ),
                );
                return;
            }
            Err(error) => {
                // An editor that could not start leaves no text of the
                // user's to keep — unless it reopened the user's own.
                let message = format!("{program}: {error}");
                if reopened {
                    self.keep_draft(
                        area,
                        target,
                        draft,
                        format!("{message} Your text is kept in {path}."),
                    );
                } else {
                    self.failed(message);
                    draft.remove();
                }
                return;
            }
        };
        /// What the draft asks next.
        enum Next {
            Keep,
            Unchanged,
            Write(serde_json::Value),
        }
        let next = {
            let Some(Prompt::Management(panel)) = &mut self.prompt else {
                return;
            };
            match read.and_then(|text| {
                serde_json::from_str::<serde_json::Value>(&text)
                    .map_err(|error| format!("{path}: {error}"))
            }) {
                Err(error) => {
                    panel.failed(error);
                    panel.kept(area, &path);
                    Next::Keep
                }
                Ok(document) if panel.document(&target) == Some(&document) => {
                    panel.unchanged();
                    Next::Unchanged
                }
                Ok(document) => Next::Write(document),
            }
        };
        match next {
            Next::Keep => self.hold_draft(ManagementDraft {
                area,
                target,
                draft,
            }),
            Next::Unchanged => {
                draft.remove();
            }
            Next::Write(document) => {
                if let Some(Prompt::Management(panel)) = &mut self.prompt {
                    panel.saving();
                }
                let action = match &target {
                    WriteTarget::Create => ManagementAction::Add { document },
                    WriteTarget::Area { revision } => ManagementAction::Save {
                        id: None,
                        expected_revision: revision.clone(),
                        document,
                    },
                    WriteTarget::Entry { id, revision } => ManagementAction::Save {
                        id: Some(id.clone()),
                        expected_revision: revision.clone(),
                        document,
                    },
                };
                self.hold_draft(ManagementDraft {
                    area,
                    target,
                    draft,
                });
                let request = ManagementRequest { area, action };
                self.remember_named(&request);
                self.host.send(Command::Management(request));
            }
        }
    }

    /// `/<area>` opens the management panel of `area`, which reads it.
    fn open_management(&mut self, area: Area, note: Option<String>) {
        self.open_prompt(Prompt::Management(ManagementPanel::new(area, note)));
        self.host.send(Command::Management(ManagementRequest {
            area,
            action: ManagementAction::List,
        }));
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

    /// `/runtime` opens the panel; `/runtime install NAME`,
    /// `/runtime source NAME download|system` and `/runtime auto NAME on|off`
    /// are the same calls the panel makes.
    fn runtime(&mut self, args: &str) {
        use crate::model::session::{RuntimeAction, RuntimeSource};
        let usage =
            "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]";
        let words: Vec<&str> = args.split_whitespace().collect();
        let action = match words.as_slice() {
            [] => None,
            ["install", name] => Some(RuntimeAction::Install((*name).to_owned())),
            ["source", name, source] => {
                RuntimeSource::parse(source).map(|source| RuntimeAction::Configure {
                    name: (*name).to_owned(),
                    source: Some(source),
                    auto: None,
                })
            }
            ["auto", name, state] => match *state {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            }
            .map(|auto| RuntimeAction::Configure {
                name: (*name).to_owned(),
                source: None,
                auto: Some(auto),
            }),
            _ => None,
        };
        if action.is_none() && !words.is_empty() {
            self.set_notice(t(usage));
            return;
        }
        self.open_prompt(Prompt::Runtime(RuntimePanel::new()));
        match action {
            Some(action) => self.host.send(Command::Runtime(action)),
            None => self.host.send(Command::ListRuntimes),
        }
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
            "rename" => {
                let title = args.trim();
                let title = if title == "--clear" {
                    None
                } else {
                    Some(title.to_owned())
                };
                self.host.send(Command::Rename(title));
                Outcome::Continue
            }
            "goal" => {
                match crate::goal::request(args) {
                    Some(request) => self.host.send(Command::Goal(request)),
                    None => self.reports.push(Block::Status {
                        text: crate::goal::USAGE.to_owned(),
                        tone: Tone::Muted,
                    }),
                }
                Outcome::Continue
            }
            "mcp" => {
                // Bare `/mcp` opens the panel; every argument form stays the
                // line command it was.
                if args.trim().is_empty() {
                    self.open_management(Area::Mcp, None);
                } else {
                    self.host.send(Command::Mcp(args.to_owned()));
                }
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
                self.open_prompt(Prompt::Sessions(SessionsPanel::new(
                    crate::ui::config::saved_show_acp(&self.home),
                )));
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
            "permissions" | "approvals" | "approval" => {
                // Arguments belong to the line prompt and to `dimcode
                // permissions`; the terminal UI's surface is the rule panel,
                // which opens holding where they work instead of ignoring
                // them, and says it until the user's first key there.
                if args.trim().is_empty() {
                    let panel = ApprovalsPanel::new(&self.permission_policy);
                    self.open_prompt(Prompt::Approvals(panel));
                } else {
                    self.open_management(
                        Area::Permissions,
                        Some(t("Use the line prompt or dimcode permissions.").to_owned()),
                    );
                }
                Outcome::Continue
            }
            "plugins" => {
                self.plugins(args);
                Outcome::Continue
            }
            "runtime" => {
                self.runtime(args);
                Outcome::Continue
            }
            "skills" => {
                self.open_management(Area::Skills, None);
                Outcome::Continue
            }
            "reload-skills" => {
                self.host.send(Command::Management(ManagementRequest {
                    area: Area::Skills,
                    action: ManagementAction::Reload,
                }));
                Outcome::Continue
            }
            "hooks" => {
                self.open_management(Area::Hooks, None);
                Outcome::Continue
            }
            "scheduled" => {
                self.open_management(Area::Scheduled, None);
                Outcome::Continue
            }
            "connect" => {
                self.open_prompt(Prompt::Providers(Box::new(ProvidersPanel::new())));
                self.host.send(Command::ListProviders);
                Outcome::Continue
            }
            "login" => {
                self.login_attempts += 1_000;
                self.open_prompt(Prompt::Login(Box::new(OauthView::dim(
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
                self.reports.push(Block::Status {
                    text: rows.join("\n"),
                    tone: Tone::Muted,
                });
                Outcome::Continue
            }
            "settings" => {
                self.settings(args);
                Outcome::Continue
            }
            "language" => {
                self.open_prompt(Prompt::Language(LanguagePanel::new(i18n::saved_locale(
                    &self.home,
                ))));
                Outcome::Continue
            }
            other => unreachable!("{other} is not in the command table"),
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
        self.refresh_messages();
        let (commits, sources, tail) = self.transcript(width);
        for (text, (source, placement)) in commits.into_iter().zip(sources) {
            if let Block::ToolGroup(group) = &source
                && !group.cards.is_empty()
                && self.fullscreen
            {
                self.card_blocks.push(self.dropped + self.history.len());
            }
            if let Block::User { pending: true, .. } = &source
                && self.fullscreen
            {
                self.message_blocks.push(self.dropped + self.history.len());
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
        for block in std::mem::take(&mut self.reports) {
            let text = chunk(&render_block(&block, width, &self.palette, &self.markdown));
            self.record(block);
            frame.commits.push(text);
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
        let live_budget = crate::ui::render::live_row_budget(
            size.1 as usize,
            if self.fullscreen {
                crate::ui::render::fullscreen::MIN_VIEW_ROWS
            } else {
                crate::ui::render::screen::MIN_HISTORY_ROWS as usize
            },
        );
        let status_rows = usize::from(!self.fullscreen);
        let input_budget = live_budget
            .saturating_sub(
                3 + status_rows
                    + if !self.fullscreen && self.composer.palette_open() {
                        3
                    } else {
                        0
                    },
            )
            .max(1);
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
        let inner = cols.saturating_sub(MARGIN + 6).max(1);
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
            let (input_rows, cursor) =
                self.composer
                    .render_rows(text_width as u16, input_budget, p);
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
        if self.fullscreen {
            let minimum = FULLSCREEN_COMPOSER_ROWS.min((size.1 as usize).saturating_sub(7).max(1));
            if rows.len() < minimum {
                rows.resize(minimum, String::new());
            }
        }
        let box_top = live.len();
        let activity = |label: &str| fg(label, &p.activity);
        let shell_title = shell.map(|shell| match shell {
            Shell::Recorded => t("! shell mode"),
            Shell::Private => t("!! shell mode · not sent to the model"),
        });
        let tinted = |label: &str| fg(label, shell_color);
        let top_status = status.as_deref().filter(|_| !self.fullscreen);
        let title = match (top_status, shell_title) {
            (Some(text), _) => Some((text, &activity as &dyn Fn(&str) -> String)),
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
            let room = outer.saturating_sub(2);
            let left = title.as_ref().map_or(0, |(text, _)| width_of(text) + 3);
            let model = self
                .label
                .as_ref()
                .and_then(|label| label.fitted(|text| left + width_of(text) + 3 < room))
                .map(|label| fg(&label, &p.highlight));
            let title = title.map(|(text, label)| label(text));
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
        let native_selection = self.native_selection();
        let panel_margin = if native_selection { 0 } else { MARGIN };
        let outer = if native_selection {
            cols
        } else {
            cols.saturating_sub(2 + MARGIN).max(4)
        };
        let margin = " ".repeat(panel_margin);
        let composer_rows = if replaced { 0 } else { rows.len() + 2 };
        let panel_rows = if native_selection {
            size.1 as usize
        } else {
            live_budget
                .saturating_sub(composer_rows + 1 + status_rows)
                .min(18)
        };
        let panel_cursor = |width: usize, height: usize| match &self.prompt {
            Some(Prompt::Sessions(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Tasks(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Lines(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Timeline(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Models(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Providers(prompt)) => Some(prompt.cursor(width, height)),
            Some(Prompt::Login(prompt)) => Some(prompt.cursor(width, height)),
            Some(Prompt::Ask(_)) if command_entry => None,
            Some(Prompt::Ask(prompt)) => Some(prompt.cursor(width, panel_rows)),
            Some(_) => Some(None),
            None => None,
        };
        if self.fullscreen {
            let budget = if native_selection {
                size.1 as usize
            } else {
                (size.1 as usize)
                    .saturating_sub(composer_rows + 2 + 2)
                    .max(6)
                    .min((size.1 as usize).saturating_sub(composer_rows + 1))
                    .min(OVERLAY_MAX_ROWS)
            };
            let list_width = if native_selection {
                cols
            } else {
                cols.saturating_sub(4).min(OVERLAY_LIST_WIDTH)
            };
            let caret = panel_cursor(list_width, budget)
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
                    self.composer.palette_overlay(width, budget, p)
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
                Some(Prompt::Runtime(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Management(prompt)) => list(prompt.render(list_width, budget, p)),
                Some(Prompt::Login(prompt)) => list(prompt.render(list_width, budget, p)),
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
            Some(Prompt::Ask(_)) if command_entry => {
                self.composer.palette_panel(outer, panel_rows, p)
            }
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
            Some(Prompt::Runtime(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Management(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Login(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Approvals(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Theme(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Language(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Settings(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Isolation(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Exit(prompt)) => Some(prompt.render(outer, p)),
            Some(Prompt::Upgrade(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Permission(prompt)) => Some(prompt.render_sized(outer, panel_rows, p)),
            Some(Prompt::Ask(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            None => self
                .composer
                .palette_panel(outer, panel_rows, p)
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
        frame.cursor = match panel_cursor(outer, panel_rows) {
            None => (!replaced).then_some(composer_cursor),
            Some(_) if self.fullscreen && !matches!(self.prompt, Some(Prompt::Ask(_))) => None,
            Some(caret) => caret.map(|(row, column)| {
                (
                    (panel_top + row) as u16,
                    (panel_margin + column).min(cols.saturating_sub(1)) as u16,
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
            Row(usize),
        }
        let mut live_text: Vec<TextBlock> = Vec::new();
        let mut live_items: Vec<LiveItem> = Vec::new();
        let mut settled_assistant: Vec<TextBlock> = Vec::new();
        let mut settled: Vec<(usize, String)> = Vec::new();
        let mut pending_messages: Vec<(usize, String)> = Vec::new();
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
                    live_items.push(LiveItem::Row(index));
                }
                RowKind::UserMessage if row.pending && !self.fullscreen => {
                    live_items.push(LiveItem::Row(index));
                    pending_messages.push((index, row.row_id.clone()));
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
                        settled.push((index, row.row_id.clone()));
                    }
                },
                _ => settled.push((index, row.row_id.clone())),
            }
        }

        // The live tail is drawn after everything committed, so it can hold
        // only the newest rows. A pending message with a settled row after it
        // is committed in Timeline order instead, in its final colour: a
        // committed row cannot take the hint back (timeline UI §4.8).
        let mut forced: std::collections::HashSet<usize> = std::collections::HashSet::new();
        if let Some(last) = settled.last().map(|(index, _)| *index) {
            for (index, id) in pending_messages.iter().filter(|(index, _)| *index < last) {
                live_items.retain(|item| !matches!(item, LiveItem::Row(at) if at == index));
                settled.push((*index, id.clone()));
                forced.insert(*index);
            }
        }
        settled.sort_by_key(|(index, _)| *index);
        let settled_ids: Vec<String> = settled.into_iter().map(|(_, id)| id).collect();

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
                    let mut block = project_row(&snapshot.rows, index, detail_of(index));
                    if forced.contains(&index)
                        && let Block::User { pending, .. } = &mut block
                    {
                        *pending = false;
                    }
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
                LiveItem::Row(index) => render_block(
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
        self.message_blocks.clear();
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
        self.message_blocks.clear();
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
        self.message_blocks.clear();
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

    fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        if on {
            self.redraw_window();
        } else {
            self.settle_committed_messages();
        }
        self.request_reflow();
    }

    /// Scrollback cannot recolour a row it printed, so a pending message
    /// committed in fullscreen is settled before the history goes there
    /// (timeline UI §4.8).
    fn settle_committed_messages(&mut self) {
        for index in std::mem::take(&mut self.message_blocks) {
            let Some(at) = index.checked_sub(self.dropped) else {
                continue;
            };
            let Some(Block::User {
                text, display, cut, ..
            }) = self.history.get(at).cloned()
            else {
                continue;
            };
            self.history[at] = Block::User {
                text,
                display,
                cut,
                pending: false,
            };
        }
    }

    /// Fullscreen redraws a committed user row the model has consumed since;
    /// scrollback keeps what it printed (timeline UI §4.8).
    fn refresh_messages(&mut self) {
        if !self.fullscreen || self.message_blocks.is_empty() {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let settled: Vec<usize> = self
            .message_blocks
            .iter()
            .copied()
            .filter(|index| {
                let Some(Block::User {
                    text,
                    cut,
                    pending: true,
                    ..
                }) = index
                    .checked_sub(self.dropped)
                    .and_then(|at| self.history.get(at))
                else {
                    return false;
                };
                snapshot.rows.iter().any(|row| {
                    row.kind == RowKind::UserMessage
                        && row.cut == *cut
                        && row.text == *text
                        && !row.pending
                })
            })
            .collect();
        for index in settled {
            let Some(at) = index.checked_sub(self.dropped) else {
                continue;
            };
            if let Some(Block::User { pending, .. }) = self.history.get_mut(at) {
                *pending = false;
                self.mark_rewritten(index);
            }
        }
        self.message_blocks.retain(|index| {
            index
                .checked_sub(self.dropped)
                .and_then(|at| self.history.get(at))
                .is_some_and(|block| matches!(block, Block::User { pending: true, .. }))
        });
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

    /// The shown line's goal, while the line follows it: a goal that
    /// continues, or one held paused.
    fn goal_label(&self) -> Option<&'static str> {
        match self.snapshot.as_ref()?.goal.as_ref()?.status {
            crate::goal::Status::Active => Some(t("◎ goal active")),
            crate::goal::Status::Paused(_) => Some(t("◎ goal paused")),
            _ => None,
        }
    }

    /// Fullscreen's bottom border label, the status corner: the run
    /// status, the slot (a notice in the highlight colour), the approvals
    /// mode when it is not the default one and the context use.
    fn dock_status(&self, run: Option<&str>) -> Option<String> {
        let p = &self.palette;
        let mut parts: Vec<String> = Vec::new();
        if let Some(run) = run {
            parts.push(fg(run, &p.activity));
        }
        if let Some(slot) = self.slot() {
            let color = if self.notice.is_some() {
                &p.highlight
            } else {
                &p.muted
            };
            parts.push(fg(&slot, color));
        }
        if let Some(goal) = self.goal_label() {
            parts.push(fg(goal, &p.muted));
        }
        if self.permission_policy.tool_approval() == dim_agent_types::ToolApproval::Bypass {
            let mode = tool_policy_label(&self.permission_policy);
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
        let approval = if self.permission_policy.tool_approval.is_none()
            && self.permission_policy.tool_approval() == dim_agent_types::ToolApproval::Ask
        {
            "Ask (legacy)"
        } else {
            tool_policy_label(&self.permission_policy)
        };
        let mut left = format!("{}AGENT  \u{23f5}\u{23f5} {}", " ".repeat(MARGIN), approval);
        if let Some(goal) = self.goal_label() {
            left.push_str("  ");
            left.push_str(goal);
        }
        if let Some(context) = self.context_label() {
            left.push_str("  ");
            left.push_str(&context);
        }
        if let Some(slot) = slot {
            left.push_str("  ");
            left.push_str(&slot);
        }
        let total = cols.saturating_sub(2);
        let label = self
            .label
            .as_ref()
            .and_then(|label| label.fitted(|text| width_of(&left) + 2 + width_of(text) <= total));
        let line = match label {
            Some(label) => {
                let gap = total - width_of(&left) - width_of(&label);
                format!("{left}{}{label}", " ".repeat(gap))
            }
            None => clip(&left, total).0,
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

/// The queue strip's hint, and its form when messages are not shown. Every
/// `/name` it can draw is a command in the table.
const QUEUE_HINT: &str = "shift+← edit last · ctrl+s steer now";
const QUEUE_HINT_MORE: &str = "+{count} more · shift+← edit last · ctrl+s steer now";

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
        t(QUEUE_HINT).to_owned()
    } else {
        tf(QUEUE_HINT_MORE, &[("count", &hidden.to_string())])
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
    let version = crate::version::current();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| t("Not available").into());
    let branch = branch.to_owned();
    let body_width = cols.saturating_sub(2 + MARGIN).max(4) - 4;
    let greeting = [
        tf(
            "Welcome to DIM {version}!",
            &[("version", version.as_str())],
        ),
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

fn git_head(cwd: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--git-path", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!path.is_empty()).then(|| std::path::Path::new(cwd).join(path))
}

fn git_branch(head: &std::path::Path) -> Option<String> {
    let head = std::fs::read_to_string(head).ok()?;
    let head = head.trim();
    if head.is_empty() {
        return None;
    }
    Some(
        head.strip_prefix("ref: refs/heads/")
            .unwrap_or("HEAD")
            .to_owned(),
    )
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
    fn narrow_composers_display_and_submit_the_whole_draft() {
        use crate::ui::render::panel::strip_escapes;

        for fullscreen in [false, true] {
            for cols in [32, 40, 48, 80] {
                for draft in [
                    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                    "检查手机窄屏输入框完整显示中文以及/work/project/crates/dim-cli/src/ui/tui.rs",
                ] {
                    let mut drawn = Drawn::new((cols, 24), None, true);
                    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                    drawn.app.host = Box::new(Recorder(sent.clone()));
                    drawn.app.fullscreen = fullscreen;
                    drawn.app.composer.set_draft(draft);
                    let frame = drawn.app.frame(drawn.size);
                    let shown: String = frame
                        .live
                        .iter()
                        .map(|row| strip_escapes(row))
                        .filter_map(|row| {
                            row.strip_prefix(" │ ")
                                .and_then(|row| row.strip_suffix(" │"))
                                .map(|row| row.trim().trim_start_matches("› ").to_owned())
                        })
                        .collect();
                    assert_eq!(shown, draft, "{cols} columns, fullscreen={fullscreen}");
                    drawn.key(KeyCode::Enter);
                    assert!(
                        matches!(sent.borrow().last(), Some(Command::Submit(text, _)) if text == draft)
                    );
                }
            }
        }
    }

    #[test]
    fn short_narrow_command_menus_keep_the_query_and_selection_visible() {
        use crate::ui::render::panel::visible_width;

        for locale in [i18n::Locale::En, i18n::Locale::ZhCn] {
            i18n::set(locale);
            for fullscreen in [false, true] {
                for size in [(32, 8), (40, 10), (32, 12), (40, 12), (48, 24), (80, 24)] {
                    let mut drawn = Drawn::new(size, None, true);
                    drawn.app.fullscreen = fullscreen;
                    drawn.key(KeyCode::Char('/'));
                    for command in ["/exit", "/keys", "/runtime", "/settings"] {
                        let frame = drawn.app.frame(size);
                        let rows = frame.overlay.as_ref().map_or(&frame.live, |o| &o.rows);
                        let width = frame
                            .overlay
                            .as_ref()
                            .map_or(size.0 as usize, |o| visible_width(&o.rows[0]));
                        assert!(
                            rows.iter().all(|row| visible_width(row) <= width),
                            "{size:?} fullscreen={fullscreen}: {rows:?}"
                        );
                        let mut emu =
                            crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                        let bytes = if fullscreen {
                            FullScreen::new(size, true).render(
                                &frame,
                                size,
                                &drawn.app.palette,
                                &drawn.app,
                            )
                        } else {
                            Screen::new(size, true).render(&frame, size)
                        };
                        emu.feed(&bytes);
                        let text = emu.rows_text().join("\n");
                        assert!(
                            text.contains(command),
                            "{size:?} fullscreen={fullscreen}: {text}"
                        );
                        let query = if fullscreen { "> " } else { "› /" };
                        assert!(text.contains(query), "query visible: {text}");
                        if fullscreen {
                            let dock = frame.live.len() - frame.dock;
                            assert!(
                                frame.overlay.as_ref().unwrap().rows.len() + dock
                                    <= size.1 as usize,
                                "palette fits above composer"
                            );
                        }
                        drawn.key(KeyCode::Down);
                    }
                }
            }
        }
        i18n::set(i18n::Locale::En);
    }

    #[test]
    fn narrow_update_dialogs_keep_actions_and_full_details_accessible() {
        use crate::ui::render::panel::visible_width;

        let shown = |drawn: &mut Drawn| {
            let size = drawn.size;
            let frame = drawn.app.frame(size);
            assert!(
                frame
                    .live
                    .iter()
                    .all(|row| visible_width(row) <= size.0 as usize)
            );
            let bytes = if drawn.app.fullscreen {
                FullScreen::new(size, true).render(&frame, size, &drawn.app.palette, &drawn.app)
            } else {
                Screen::new(size, true).render(&frame, size)
            };
            let mut emu = crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
            emu.feed(&bytes);
            let text = emu.rows_text().join("\n").replace('\0', "");
            for expected in [t("Update available"), "Enter", "Esc", "╰"] {
                assert!(
                    text.contains(expected),
                    "{size:?}: {expected:?} missing: {text}"
                );
            }
            text
        };
        for locale in [i18n::Locale::En, i18n::Locale::ZhCn] {
            i18n::set(locale);
            for fullscreen in [false, true] {
                for size in [
                    (32, 8),
                    (40, 10),
                    (32, 12),
                    (40, 12),
                    (40, 24),
                    (48, 24),
                    (80, 24),
                ] {
                    let mut drawn = Drawn::new(size, None, true);
                    drawn.app.fullscreen = fullscreen;
                    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                    drawn.app.host = Box::new(Recorder(sent.clone()));
                    let offer = crate::update::Offer {
                        current: "0.12.0-alpha.123".into(),
                        version: "0.12.0-alpha.124".into(),
                        manual: "npm install -g --prefer-online dimcode-alpha@0.12.0-alpha.124"
                            .into(),
                        install: Some(crate::update::Install {
                            version: "0.12.0-alpha.124".into(),
                            program: "npm".into(),
                            args: vec![
                                "install".into(),
                                "-g".into(),
                                "dimcode-alpha@0.12.0-alpha.124".into(),
                            ],
                            relaunch: "dima".into(),
                        }),
                    };
                    drawn.app.apply(Update::Upgrade(offer.clone()));
                    for label in ["Update now", "Skip this version", "Later"] {
                        assert!(shown(&mut drawn).contains(&format!("› {}", t(label))));
                        drawn.key(KeyCode::Tab);
                    }
                    drawn.key(KeyCode::Enter);
                    assert!(
                        matches!(sent.borrow().last(), Some(Command::UpgradeInstall(install)) if Some(install) == offer.install.as_ref())
                    );
                    assert!(shown(&mut drawn).contains(t("Close")));
                    drawn.app.apply(Update::UpgradeInstalled {
                        version: offer.version.clone(),
                        result: Err("npm EACCES: permission denied in /usr/local/lib/node_modules/dimcode-alpha\n请检查安装目录权限，然后重新尝试 END-OF-ERROR".into()),
                    });
                    assert!(
                        shown(&mut drawn).contains(&tf("Update failed: {error}", &[("error", "")]))
                    );
                    for _ in 0..30 {
                        drawn.key(KeyCode::Down);
                        shown(&mut drawn);
                    }
                    assert!(shown(&mut drawn).contains("END-OF-ERROR"));
                    drawn.app.apply(Update::UpgradeInstalled {
                        version: offer.version.clone(),
                        result: Ok(()),
                    });
                    assert!(shown(&mut drawn).contains(&format!("› {}", t("Restart now"))));
                    drawn.key(KeyCode::Right);
                    assert!(shown(&mut drawn).contains(&format!("› {}", t("Close"))));
                    drawn.key(KeyCode::Enter);
                    assert!(drawn.app.prompt.is_none());

                    drawn.app.apply(Update::Upgrade(crate::update::Offer {
                        install: None,
                        ..offer.clone()
                    }));
                    let mut details = String::new();
                    for _ in 0..30 {
                        details.push_str(&shown(&mut drawn));
                        drawn.key(KeyCode::Down);
                    }
                    let details: String = details
                        .chars()
                        .filter(|c| !c.is_whitespace() && *c != '│')
                        .collect();
                    assert!(details.contains(&offer.current));
                    assert!(details.contains("dimcode-alpha@0.12.0-alpha.124"));
                    drawn.size = (80, 24);
                    assert!(shown(&mut drawn).contains(&offer.current));
                    drawn.size = size;
                    drawn.key(KeyCode::Enter);
                    assert_eq!(sent.borrow().len(), 1, "only the explicit install is sent");
                    assert_eq!(
                        drawn.app.notice.as_ref().unwrap().0,
                        tf("Skipped {version}.", &[("version", &offer.version)])
                    );
                    assert!(drawn.app.prompt.is_none());
                }
            }
        }
        i18n::set(i18n::Locale::En);
    }

    #[test]
    fn localized_update_dialogs_fit_very_narrow_terminals() {
        use crate::ui::render::panel::visible_width;

        for (locale, _, _) in i18n::LOCALES {
            i18n::set(locale);
            for fullscreen in [false, true] {
                for cols in [24, 28, 32] {
                    let size = (cols, 12);
                    let mut drawn = Drawn::new(size, None, true);
                    drawn.app.fullscreen = fullscreen;
                    drawn.app.apply(Update::Upgrade(crate::update::Offer {
                        current: "1.0.0".into(),
                        version: "2.0.0".into(),
                        install: None,
                        manual: "npm install -g dimcode@2.0.0".into(),
                    }));
                    let frame = drawn.app.frame(size);
                    assert!(
                        frame
                            .live
                            .iter()
                            .all(|row| visible_width(row) <= cols as usize)
                    );
                    let bytes = if fullscreen {
                        FullScreen::new(size, true).render(
                            &frame,
                            size,
                            &drawn.app.palette,
                            &drawn.app,
                        )
                    } else {
                        Screen::new(size, true).render(&frame, size)
                    };
                    let mut emu = crate::ui::render::testterm::Emu::new(cols as usize, 12);
                    emu.feed(&bytes);
                    let rows = emu.rows_text();
                    let title: String = t("Update available").chars().take(4).collect();
                    assert!(
                        rows.iter().any(|row| row.trim_start().starts_with("╭─ ")
                            && row.replace('\0', "").contains(&title)),
                        "{locale:?} {size:?}: {rows:?}"
                    );
                    let text = rows.join("\n");
                    assert!(text.contains("Enter") && text.contains("Esc"), "{text}");
                    drawn.key(KeyCode::Esc);
                    assert!(drawn.app.prompt.is_none());
                }
            }
        }
        i18n::set(i18n::Locale::En);
    }

    #[test]
    fn short_composers_keep_the_caret_and_the_complete_draft() {
        for fullscreen in [false, true] {
            let mut drawn = Drawn::new((80, 24), None, true);
            drawn.app.fullscreen = fullscreen;
            let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            drawn.app.host = Box::new(Recorder(sent.clone()));
            let draft = (0..12)
                .map(|n| format!("line {n:02}"))
                .collect::<Vec<_>>()
                .join("\n");
            drawn.app.composer.set_draft(&draft);
            for (size, up, expected) in [
                ((80, 24), 0, "line 11"),
                ((32, 8), 0, "line 11"),
                ((32, 8), 11, "line 00"),
                ((40, 10), 0, "line 00"),
                ((80, 24), 0, "line 00"),
            ] {
                drawn.size = size;
                for _ in 0..up {
                    drawn
                        .app
                        .handle(InputEvent::Key(KeyEvent::new(KeyCode::Up, Modifiers::NONE)));
                }
                let frame = drawn.app.frame(size);
                let bytes = if fullscreen {
                    FullScreen::new(size, true).render(&frame, size, &drawn.app.palette, &drawn.app)
                } else {
                    Screen::new(size, true).render(&frame, size)
                };
                let mut emu =
                    crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                emu.feed(&bytes);
                assert!(emu.cursor_visible);
                assert!(
                    emu.row(emu.cy).contains(expected),
                    "{size:?} fullscreen={fullscreen}: {:?}",
                    emu.rows_text()
                );
            }
            drawn.key(KeyCode::Enter);
            assert!(
                matches!(sent.borrow().last(), Some(Command::Submit(text, _)) if text == &draft)
            );
        }
    }

    #[test]
    fn composer_tracks_git_switches_in_repositories_and_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let linked = root.path().join("linked");
        std::fs::create_dir(&repo).unwrap();
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&repo, &["init", "-q", "-b", "original"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-original",
                linked.to_str().unwrap(),
            ],
        );
        let nested = repo.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let theme = test_theme();
        let mut apps = Vec::new();
        for (cwd, expected) in [(&nested, "original"), (&linked, "linked-original")] {
            for fullscreen in [false, true] {
                let mut app = App::new(
                    Box::new(Idle),
                    MarkdownRenderer::new(&theme),
                    theme.palette.clone(),
                    root.path().join("home"),
                    cwd.to_string_lossy().into_owned(),
                );
                app.fullscreen = fullscreen;
                assert!(
                    app.frame((160, 30))
                        .live
                        .iter()
                        .any(|row| row.contains(&format!("({expected})")))
                );
                apps.push(app);
            }
        }
        git(&repo, &["switch", "-q", "-c", "feature/updated"]);
        git(&linked, &["switch", "-q", "-c", "feature/linked-updated"]);
        for (expected, linked_expected) in [
            ("feature/updated", "feature/linked-updated"),
            ("HEAD", "HEAD"),
            ("feature/updated", "feature/linked-updated"),
        ] {
            if expected == "HEAD" {
                git(&repo, &["switch", "-q", "--detach"]);
                git(&linked, &["switch", "-q", "--detach"]);
            } else {
                git(&repo, &["switch", "-q", expected]);
                git(&linked, &["switch", "-q", linked_expected]);
            }
            for (index, app) in apps.iter_mut().enumerate() {
                app.git_checked = Instant::now();
                assert!(!app.tick(), "a Git switch must wait for the next check");
                app.git_checked -= GIT_BRANCH_POLL;
                assert!(app.tick(), "a Git switch must request a repaint");
                let expected = if index < 2 { expected } else { linked_expected };
                let frame = app.frame((160, 30));
                let mut emu = crate::ui::render::testterm::Emu::new(160, 30);
                let bytes = if app.fullscreen {
                    FullScreen::new((160, 30), true).render(&frame, (160, 30), &app.palette, app)
                } else {
                    Screen::new((160, 30), true).render(&frame, (160, 30))
                };
                emu.feed(&bytes);
                assert!(
                    emu.rows_text()
                        .iter()
                        .any(|row| row.contains(&format!("({expected})"))),
                    "composer must show {expected}"
                );
            }
        }
        for (index, app) in apps.iter_mut().enumerate() {
            app.git_checked -= GIT_BRANCH_POLL;
            assert!(!app.tick(), "an unchanged HEAD must not request a repaint");
            let expected = if index < 2 {
                "feature/updated"
            } else {
                "feature/linked-updated"
            };
            let head = app.git_head.clone().unwrap();
            let saved = std::fs::read(&head).unwrap();
            std::fs::remove_file(&head).unwrap();
            for missing in [true, false] {
                if !missing {
                    std::fs::write(&head, "").unwrap();
                }
                app.git_checked -= GIT_BRANCH_POLL;
                assert!(!app.tick(), "a failed HEAD read must not request a repaint");
                assert!(
                    app.frame((160, 30))
                        .live
                        .iter()
                        .any(|row| row.contains(&format!("({expected})"))),
                    "a missing or empty HEAD must preserve the last branch label"
                );
            }
            std::fs::write(&head, saved).unwrap();
        }
    }

    #[test]
    fn pending_clipboard_allows_file_selection_but_waits_before_submission() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("first.txt"), "").unwrap();
        std::fs::write(home.path().join("second.txt"), "").unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            home.path().to_string_lossy().into_owned(),
        );
        typed(&mut app, "@first");
        let started = Instant::now();
        while !app
            .frame((80, 24))
            .live
            .iter()
            .any(|row| row.contains("@first.txt"))
        {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "file picker did not populate"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.composer.text(), "@first.txt ");

        let (send, receive) = std::sync::mpsc::channel();
        app.clipboard = Some(ClipboardRead::Native(receive));
        typed(&mut app, "compare with @second");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.composer.text(), "@first.txt compare with @second.txt ");
        assert!(sent.borrow().is_empty(), "file selection must not submit");

        press(&mut app, KeyCode::Enter);
        assert!(
            sent.borrow().is_empty(),
            "submission waits for clipboard data"
        );
        typed(&mut app, "@missing");
        press(&mut app, KeyCode::Enter);
        assert!(
            sent.borrow().is_empty(),
            "an empty picker must not bypass the clipboard wait"
        );
        for _ in 0.."@missing".len() {
            press(&mut app, KeyCode::Backspace);
        }
        send.send(Ok(crate::clipboard::Source::Text(" appended".into())))
            .unwrap();
        app.tick();
        let expected = app.composer.expanded_text().trim().to_owned();
        assert!(expected.ends_with(" appended"));
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().as_slice(), [Command::Submit(text, _)] if text == &expected)
        );
    }

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

    #[test]
    fn model_activity_stays_blue_across_themes_and_terminal_modes() {
        use crate::ui::theme::{Appearance, ColorMode};

        for (mode, appearance, sgr) in [
            (
                ColorMode::TrueColor,
                Appearance::Dark,
                "\x1b[38;2;56;189;248m",
            ),
            (ColorMode::Ansi256, Appearance::Dark, "\x1b[38;5;75m"),
            (ColorMode::Ansi16, Appearance::Dark, "\x1b[94m"),
            (
                ColorMode::TrueColor,
                Appearance::Light,
                "\x1b[38;2;3;105;161m",
            ),
            (
                ColorMode::Ansi256,
                Appearance::Light,
                "\x1b[38;2;3;105;161m",
            ),
            (ColorMode::Ansi16, Appearance::Light, "\x1b[94m"),
        ] {
            let mut drawn = Drawn::new((80, 24), None, true);
            drawn.app.theme_context = Some(ThemeContext {
                mode,
                appearance,
                background: None,
                active: "system".into(),
            });
            let named = match appearance {
                Appearance::Dark => "monokai-dark",
                Appearance::Light => "monokai-light",
            };
            for id in ["system", named] {
                drawn.app.paint_theme(id);
                for initialized in [true, false] {
                    let context = (!initialized).then(|| drawn.app.theme_context.take());
                    for fullscreen in [false, true] {
                        drawn.app.fullscreen = fullscreen;
                        for name in ["text", "thinking"] {
                            let mut projection = crate::projection::Projection::default();
                            let event = serde_json::from_value(serde_json::json!({
                                "sequence": 1, "create_at": 1, "type": "step", "name": name,
                                "data": {"turn_id": "t1", "request_id": "r1", "delta": "Checking the parser."}
                            }))
                            .unwrap();
                            projection.apply("main", &event);
                            projection.set_running(true);
                            drawn
                                .app
                                .apply(Update::Snapshot(Box::new(projection.snapshot())));
                            drawn.app.run_started = Some(Instant::now() - Duration::from_secs(42));
                            let frame = drawn.app.frame((80, 24));
                            let label = if name == "thinking" {
                                "thinking…"
                            } else {
                                "working…"
                            };
                            let row = frame.live.iter().find(|row| row.contains(label)).unwrap();
                            let start = row
                                .find(sgr)
                                .expect("the activity uses the terminal's blue");
                            let colored = row[start + sgr.len()..].split("\x1b[0m").next().unwrap();
                            let (pulse, status) = colored.split_once(' ').unwrap();
                            assert!(["·", "•", "●"].contains(&pulse));
                            assert_eq!(
                                status,
                                format!("{label}  42s"),
                                "{mode:?}, {appearance:?}, {id}, fullscreen={fullscreen}, initialized={initialized}: {row:?}"
                            );
                        }
                    }
                    if let Some(context) = context {
                        drawn.app.theme_context = context;
                    }
                }
            }
        }
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
    fn settings_omits_model_selection_and_model_command_keeps_it() {
        for fullscreen in [false, true] {
            let mut drawn = Drawn::new((100, 32), None, true);
            drawn.app.fullscreen = fullscreen;
            let screen = |drawn: &mut Drawn| {
                if fullscreen {
                    let frame = drawn.app.frame(drawn.size);
                    let mut screen = FullScreen::new(drawn.size, true);
                    let bytes = screen.render(&frame, drawn.size, &drawn.app.palette, &drawn.app);
                    let mut terminal = crate::ui::render::testterm::Emu::new(100, 32);
                    terminal.feed(&bytes);
                    terminal.rows_text().join("\n")
                } else {
                    drawn.emu.rows_text().join("\n")
                }
            };
            typed(&mut drawn.app, "/settings");
            drawn.key(KeyCode::Enter);
            let settings = screen(&mut drawn);
            assert!(settings.contains("Settings"), "{settings}");
            assert!(settings.contains("Process isolation"), "{settings}");
            assert!(settings.contains("File access"), "{settings}");
            assert!(settings.contains("Tool approval"), "{settings}");
            assert!(!settings.contains("Execution"), "{settings}");
            assert!(!settings.contains("Model"), "{settings}");
            drawn.key(KeyCode::Esc);
            typed(&mut drawn.app, "/model");
            drawn.key(KeyCode::Enter);
            let models = screen(&mut drawn);
            assert!(models.contains("Select a model"), "{models}");
        }
    }

    #[test]
    fn account_usage_stays_safe_and_fits_after_reflow_and_in_login() {
        use crate::model::session::Usage;
        use crate::ui::render::panel::{strip_escapes, visible_width};
        use crate::ui::theme::{Appearance, ColorMode};

        let usage: Usage = serde_json::from_value(serde_json::json!({
            "plan":{"name":"\u{1b}]2;injected-title\u{7}Pro","status":"active"},
            "credits":{"usedUnits":25000,"totalUnits":300000},
            "features":[{"key":"web_\u{1b}[2Jsearch","unlimited":true}]
        }))
        .unwrap();
        for theme in [
            test_theme(),
            Theme::system(ColorMode::TrueColor, Appearance::Light, None),
            Theme::named("monokai-dark", ColorMode::TrueColor, Appearance::Dark, None),
        ] {
            let home = tempfile::tempdir().unwrap();
            let mut app = App::new(
                Box::new(Idle),
                MarkdownRenderer::new(&theme),
                theme.palette.clone(),
                home.path().to_path_buf(),
                "/work".into(),
            );
            app.frame((80, 32));
            app.apply(Update::UsageReport(Ok(usage.clone())));
            let committed = app.frame((80, 32)).commits.join("\n");
            assert!(strip_escapes(&committed).contains("Pro · active"));
            assert!(!committed.contains("injected-title"));
            assert!(!committed.contains("\u{1b}[2J"));
            let index = app.len() - 1;
            for (cols, filled, empty) in [(80, 3, 37), (40, 3, 33), (16, 1, 11)] {
                let rows = app.rows(index, cols);
                let plain: Vec<_> = rows.iter().map(|row| strip_escapes(row)).collect();
                assert!(
                    rows.iter().all(|row| visible_width(row) <= cols),
                    "{rows:?}"
                );
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
                assert!(plain.iter().any(|row| row.trim() == bar), "{plain:?}");
                if cols >= 40 {
                    assert!(
                        plain.iter().any(|row| row.contains("Web search")),
                        "{plain:?}"
                    );
                }
            }

            let mut panel_usage = usage.clone();
            let plan = panel_usage.plan.as_mut().unwrap();
            plan.name = Some("A very long plan 名称".repeat(10));
            plan.status = Some("\u{1b}[2Jactive\nstatus".into());
            plan.price_amount = Some(100.0);
            plan.price_currency = Some("\u{1b}]2;injected-title\u{7}custom".into());
            plan.price_interval = Some("\u{1b}[2Jperiod".into());
            let mut view = OauthView::dim(DIM_OAUTH_ID, "DimAgent", 0);
            view.set_account(Ok(Some(crate::model::session::Account {
                issuer: "fixture".into(),
                subject: "signed-in".into(),
                name: None,
                email: None,
            })));
            view.set_usage(Ok(panel_usage));
            for width in [80, 40] {
                let rows = view.render(width, 32, &theme.palette);
                assert!(
                    rows.iter().all(|row| visible_width(row) <= width),
                    "{rows:?}"
                );
                assert!(rows.iter().all(|row| !row.contains(['\n', '\t'])));
                assert!(
                    rows.iter()
                        .all(|row| !row.contains("injected-title") && !row.contains("\u{1b}[2J"))
                );
                assert!(rows.iter().any(|row| strip_escapes(row).contains("…")));
            }
        }
    }

    #[test]
    fn process_isolation_select_and_save() {
        use crate::model::session::ExecutionSnapshot;
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
        for _ in 0..9 {
            app.handle_input(key(KeyCode::Down));
        }
        app.handle_input(key(KeyCode::Enter));
        assert!(matches!(sent.borrow().last(), Some(Command::ReadIsolation)));
        app.apply(Update::Isolation(Ok(ExecutionSnapshot {
            revision: "r1".into(),
            sandbox: false,
            network: false,
        })));
        app.handle_input(key(KeyCode::Down));
        app.handle_input(key(KeyCode::Enter));
        app.handle_input(key(KeyCode::Down));
        app.handle_input(key(KeyCode::Down));
        app.handle_input(key(KeyCode::Enter));
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::SaveIsolation {
                expected_revision,
                sandbox: true,
                network: false,
            }) if expected_revision == "r1"
        ));
    }

    #[test]
    fn permission_controls_refresh_the_full_policy_and_keep_their_own_fields() {
        use crate::permission::Change;
        use dim_agent_types::{FileAccess, PermissionPolicy, PermissionPreset, ToolApproval};
        for fullscreen in [false, true] {
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
            app.fullscreen = fullscreen;
            let key =
                |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
            for alias in ["permissions", "approvals", "approval"] {
                app.run_command(alias, "");
                app.handle_input(key(KeyCode::Tab));
                app.handle_input(key(KeyCode::BackTab));
                app.handle_input(key(KeyCode::Tab));
                app.handle_input(key(KeyCode::Enter));
                let rendered = app.frame((64, 40));
                let frame = rendered
                    .overlay
                    .as_ref()
                    .map_or(&rendered.live, |overlay| &overlay.rows)
                    .join("\n");
                assert!(frame.find("Bypass").unwrap() < frame.find("Hint:").unwrap());
                assert_eq!(frame.matches("Hint:").count(), 1);
                app.handle_input(key(KeyCode::Enter));
                assert!(matches!(
                    sent.borrow().last(),
                    Some(Command::SetPermission(Change::ToolApproval(
                        ToolApproval::Ask
                    )))
                ));
            }
            app.run_command("permissions", "");
            app.apply(Update::Permissions(Ok(PermissionPolicy {
                preset: PermissionPreset::ReadOnly,
                file_access: Some(FileAccess::Unrestricted),
                tool_approval: Some(ToolApproval::Bypass),
                allowed_tools: vec!["read".into()],
                disallowed_tools: vec!["blocked".into()],
            })));
            app.handle_input(key(KeyCode::Enter));
            for width in [40, 64, 112] {
                let rendered = app.frame((width, 40));
                let lines: Vec<_> = rendered
                    .overlay
                    .as_ref()
                    .map_or(&rendered.live, |overlay| &overlay.rows)
                    .iter()
                    .map(|line| crate::ui::render::panel::strip_escapes(line))
                    .collect();
                let hint = lines
                    .iter()
                    .position(|line| line.contains("Hint:"))
                    .unwrap();
                assert!(
                    lines[..hint]
                        .iter()
                        .any(|line| line.contains("Unrestricted"))
                );
                assert!(lines[hint - 1].trim().trim_matches('│').trim().is_empty());
                let text = lines
                    .iter()
                    .map(|line| line.trim().trim_matches('│').trim())
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(text.contains("Read only: no writes."));
                assert!(text.contains("writes inside; asks outside."));
                assert!(text.contains("System and session files stay protected."));
                assert!(text.contains("Tool approval still applies."));
            }
            app.handle_input(key(KeyCode::Up));
            app.handle_input(key(KeyCode::Enter));
            assert!(matches!(
                sent.borrow().last(),
                Some(Command::SetPermission(Change::FileAccess(
                    FileAccess::WorkspaceReadWrite
                )))
            ));
            assert_eq!(app.permission_policy.preset, PermissionPreset::ReadOnly);
            assert_eq!(
                app.permission_policy.tool_approval,
                Some(ToolApproval::Bypass)
            );
            assert_eq!(app.permission_policy.disallowed_tools, ["blocked"]);
            std::fs::write(
                home.path().join("config.json"),
                r#"{"permissionPreset":"read-only","toolApproval":"bypass","kept":true}"#,
            )
            .unwrap();
            app.apply(Update::PermissionSelected(Change::FileAccess(
                FileAccess::WorkspaceReadWrite,
            )));
            let saved: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(home.path().join("config.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                saved,
                serde_json::json!({"permissionPreset":"read-only","toolApproval":"bypass","fileAccess":"workspace-read-write","kept":true})
            );
            app.apply(Update::PermissionSelected(Change::ToolApproval(
                ToolApproval::Ask,
            )));
            let saved: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(home.path().join("config.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                saved,
                serde_json::json!({"permissionPreset":"read-only","toolApproval":"ask","fileAccess":"workspace-read-write","kept":true})
            );
            app.open_settings(Setting::Approvals);
            app.apply(Update::Permissions(Ok(PermissionPolicy {
                preset: PermissionPreset::ReadOnly,
                file_access: Some(FileAccess::ReadOnly),
                tool_approval: Some(ToolApproval::Ask),
                ..Default::default()
            })));
            let rendered = app.frame((112, 40));
            let frame = rendered
                .overlay
                .as_ref()
                .map_or(&rendered.live, |overlay| &overlay.rows)
                .join("\n");
            assert!(frame.contains("Read only") && !frame.contains("Unrestricted"));
            assert!(frame.contains("Ask") && !frame.contains("Bypass"));
            app.handle_input(key(KeyCode::Enter));
            let rendered = app.frame((112, 40));
            let frame = rendered
                .overlay
                .as_ref()
                .map_or(&rendered.live, |overlay| &overlay.rows)
                .join("\n");
            assert!(
                frame.contains("Bypass"),
                "Settings retains the Tool approval selection"
            );
            app.handle_input(key(KeyCode::Esc));
            app.handle_input(key(KeyCode::Esc));
            app.apply(Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                approvals: vec![crate::model::view::ApprovalView {
                    member: None,
                    request_id: "pending-exec".into(),
                    branch_id: "main".into(),
                    reason: "shell command".into(),
                    tool_name: "exec".into(),
                    tool_input: serde_json::json!({"command":"npm test"}),
                    always: None,
                }],
                ..Default::default()
            })));
            app.handle_input(key(KeyCode::BackTab));
            app.handle_input(key(KeyCode::Enter));
            assert!(matches!(app.prompt, Some(Prompt::Approvals(_))));
            app.handle_input(key(KeyCode::Down));
            app.handle_input(key(KeyCode::Down));
            app.handle_input(key(KeyCode::Enter));
            app.apply(Update::Isolation(Ok(
                crate::model::session::ExecutionSnapshot {
                    revision: "r1".into(),
                    sandbox: false,
                    network: false,
                },
            )));
            let rendered = app.frame((112, 34));
            let frame = rendered
                .overlay
                .as_ref()
                .map_or(&rendered.live, |overlay| &overlay.rows)
                .join("\n");
            assert!(frame.contains("commands use your OS permissions, not file-tool limits."));
            assert!(frame.find("Reload").unwrap() < frame.find("Hint:").unwrap());
            app.handle_input(key(KeyCode::Esc));
            assert!(matches!(app.prompt, Some(Prompt::Approvals(_))));
            app.handle_input(key(KeyCode::Esc));
            let rendered = app.frame((112, 40));
            let frame = rendered
                .overlay
                .as_ref()
                .map_or(&rendered.live, |overlay| &overlay.rows)
                .join("\n");
            assert!(
                frame.contains("npm test"),
                "The pending approval returns without another snapshot"
            );
            app.handle_input(key(KeyCode::Enter));
            assert!(matches!(
                sent.borrow().last(),
                Some(Command::Approval { request_id, .. }) if request_id == "pending-exec"
            ));
        }
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

    /// `/runtime` lists through the worker, the panel's keys reach it as
    /// runtime commands, the typed forms make the same calls, and a Pending
    /// plugin row's Enter opens it on the runtime the row waits for.
    #[test]
    fn runtime_commands_reach_the_worker() {
        use crate::model::session::{PluginItem, RuntimeAction, RuntimeItem, RuntimeSource};
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
            Some(Command::ListRuntimes) => "list".to_owned(),
            Some(Command::Runtime(action)) => format!("{action:?}"),
            Some(_) => "other".to_owned(),
            None => "none".to_owned(),
        };
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        let node = |status: &str| RuntimeItem {
            name: "node".into(),
            source: RuntimeSource::Download,
            auto: Some(true),
            version: Some("24.21.0".into()),
            status: status.into(),
            reason: None,
        };
        let python = RuntimeItem {
            name: "python".into(),
            source: RuntimeSource::System,
            auto: Some(true),
            version: Some("3.12.15".into()),
            status: "off".into(),
            reason: None,
        };
        let waiting = PluginItem {
            name: "goal".into(),
            in_use: None,
            off: false,
            locked: false,
            candidates: Vec::new(),
            status: Some("pending".into()),
            error: None,
            waiting_for: vec!["session-node".into()],
            refused: Vec::new(),
        };
        app.run_command("runtime", "");
        assert!(matches!(app.prompt, Some(Prompt::Runtime(_))));
        assert_eq!(last(&sent), "list");
        app.apply(Update::Runtimes {
            result: None,
            runtimes: Ok(vec![node("off"), python.clone()]),
            plugins: Ok(vec![waiting.clone()]),
        });
        app.handle_input(key(KeyCode::Enter));
        assert_eq!(
            last(&sent),
            format!("{:?}", RuntimeAction::Install("node".into()))
        );
        app.apply(Update::Runtimes {
            result: Some(Ok(())),
            runtimes: Ok(vec![node("acquiring"), python.clone()]),
            plugins: Ok(vec![]),
        });
        app.handle_input(key(KeyCode::Char('s')));
        assert_eq!(
            last(&sent),
            format!(
                "{:?}",
                RuntimeAction::Configure {
                    name: "node".into(),
                    source: Some(RuntimeSource::System),
                    auto: None,
                }
            )
        );
        let count = sent.borrow().len();
        std::thread::sleep(std::time::Duration::from_millis(1050));
        assert!(app.tick(), "an acquiring row is read again");
        assert_eq!(last(&sent), "list", "the rows are read once a second");
        assert_eq!(sent.borrow().len(), count + 1);
        app.apply(Update::Runtimes {
            result: None,
            runtimes: Ok(vec![node("ready")]),
            plugins: Ok(vec![]),
        });
        app.handle_input(key(KeyCode::Esc));
        assert!(app.prompt.is_none());

        app.run_command("runtime", "install node");
        assert_eq!(
            last(&sent),
            format!("{:?}", RuntimeAction::Install("node".into()))
        );
        app.run_command("runtime", "source node system");
        assert_eq!(
            last(&sent),
            format!(
                "{:?}",
                RuntimeAction::Configure {
                    name: "node".into(),
                    source: Some(RuntimeSource::System),
                    auto: None,
                }
            )
        );
        app.run_command("runtime", "auto python off");
        assert_eq!(
            last(&sent),
            format!(
                "{:?}",
                RuntimeAction::Configure {
                    name: "python".into(),
                    source: None,
                    auto: Some(false),
                }
            )
        );
        let count = sent.borrow().len();
        app.prompt = None;
        app.run_command("runtime", "source node floppy");
        assert_eq!(sent.borrow().len(), count, "an unknown source is refused");
        assert!(app.notice.is_some());

        app.run_command("plugins", "");
        app.apply(Update::Plugins(Ok(vec![waiting])));
        app.handle_input(key(KeyCode::Enter));
        match &app.prompt {
            Some(Prompt::Runtime(_)) => {}
            _ => panic!("a waiting plugin row opens /runtime"),
        }
        assert_eq!(last(&sent), "list");
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
    fn oauth_links_are_selectable_and_open_without_copy_buttons() {
        use crate::model::session::OauthEvent;
        use crate::ui::render::panel::strip_escapes;

        let url = "https://dimagent.cn/oauth/authorize?state=sample&redirect_uri=http%3A%2F%2Flocalhost%3A54321%2Fauth%2Fcallback&code_challenge=abcdefghijklmnopqrstuvwxyz0123456789";
        let typical = format!("{url}&scope={}", "a".repeat(339 - url.len() - 7));
        let long = format!("{url}&scope={}", "a".repeat(602 - url.len() - 7));
        for fullscreen in [true, false] {
            for (size, url, shortened) in [
                ((80, 24), url, false),
                ((48, 18), url, false),
                ((40, 12), typical.as_str(), true),
                ((48, 18), long.as_str(), true),
            ] {
                for mode in ["opened", "manual", "device", "device_complete"] {
                    let mut drawn = Drawn::new(size, None, true);
                    drawn.app.fullscreen = fullscreen;
                    drawn.app.run_command("login", "");
                    if mode.starts_with("device") {
                        drawn.key(KeyCode::Down);
                        drawn.key(KeyCode::Down);
                    }
                    drawn.key(KeyCode::Enter);
                    let (event, expected) = if mode.starts_with("device") {
                        let complete = (mode == "device_complete")
                            .then(|| "https://dimagent.cn/device?code=ABCD-EFGH".to_owned());
                        let target = complete.as_deref().unwrap_or("https://dimagent.cn/device");
                        (
                            OauthEvent::Device {
                                verification_uri: "https://dimagent.cn/device".into(),
                                verification_uri_complete: complete.clone(),
                                user_code: "ABCD-EFGH".into(),
                            },
                            target.to_owned(),
                        )
                    } else {
                        (
                            OauthEvent::Url {
                                url: url.into(),
                                opened: mode == "opened",
                            },
                            url.to_owned(),
                        )
                    };
                    drawn.app.apply(Update::Oauth {
                        attempt: 1_001,
                        event,
                    });
                    let frame = drawn.app.frame(size);
                    let text = if fullscreen {
                        let mut screen = FullScreen::new(size, true);
                        let bytes = screen.render(&frame, size, &drawn.app.palette, &drawn.app);
                        let mut terminal =
                            crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                        terminal.feed(&bytes);
                        terminal.rows_text().join("\n")
                    } else {
                        strip_escapes(&frame.live.join("\n"))
                    };
                    assert!(drawn.app.native_selection());
                    if fullscreen {
                        assert!(frame.overlay.as_ref().unwrap().rows.len() <= size.1 as usize);
                    }
                    let rows = text.lines().collect::<Vec<_>>();
                    let start = rows
                        .iter()
                        .position(|row| row.contains("https://dimagent.cn/"))
                        .unwrap();
                    let selected = rows[start..]
                        .iter()
                        .take_while(|row| {
                            !row.contains("Callback URL") && !row.contains("Restart device login")
                        })
                        .map(|row| {
                            row.trim()
                                .strip_prefix('│')
                                .expect("the link keeps the left dialog border")
                                .strip_suffix('│')
                                .expect("the link keeps the right dialog border")
                                .trim()
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if shortened && !mode.starts_with("device") {
                        let prefix = selected.replace('\n', "");
                        assert!(prefix.ends_with('…'));
                        assert!(expected.starts_with(prefix.trim_end_matches('…')));
                    } else {
                        assert_eq!(url::Url::parse(&selected).unwrap().as_str(), expected);
                    }
                    let rendered = if fullscreen {
                        frame.overlay.as_ref().unwrap().rows.join("\n")
                    } else {
                        frame.live.join("\n")
                    };
                    assert!(rendered.contains(&format!("\x1b]8;;{expected}\x1b\\")));
                    assert!(!text.contains("Copy OAuth"));
                    assert!(!text.contains("Copy verification"));
                    if mode.starts_with("device") {
                        assert!(text.contains("ABCD-EFGH"));
                    } else {
                        assert!(text.contains("Submit callback"));
                    }
                    assert!(drawn.app.reports.is_empty(), "no URL in the transcript");
                    let Some(Prompt::Login(view)) = &mut drawn.app.prompt else {
                        panic!("the sign-in dialog stays open");
                    };
                    assert!(matches!(
                        view.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
                        Effect::OpenUrl(url) if url == expected
                    ));
                    if !mode.starts_with("device") {
                        view.handle_key(KeyEvent::new(KeyCode::Down, Modifiers::NONE));
                        let frame = drawn.app.frame(size);
                        let (row, _) = if fullscreen {
                            frame.overlay.as_ref().unwrap().cursor.unwrap()
                        } else {
                            frame.cursor.unwrap()
                        };
                        let rows = if fullscreen {
                            &frame.overlay.as_ref().unwrap().rows
                        } else {
                            &frame.live
                        };
                        assert!(rows[row as usize].contains("Callback URL"));
                        let Some(Prompt::Login(view)) = &mut drawn.app.prompt else {
                            panic!("the callback stays in the sign-in dialog");
                        };
                        let callback =
                            "http://localhost:54321/auth/callback?code=example&state=sample";
                        view.paste(callback);
                        view.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
                        assert!(matches!(
                            view.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
                            Effect::OauthPaste(url) if url == callback
                        ));
                    } else {
                        drawn.app.apply(Update::Oauth {
                            attempt: 1_001,
                            event: OauthEvent::Done(Err(None)),
                        });
                        drawn.key(KeyCode::Esc);
                        assert!(!drawn.app.native_selection());
                    }
                    drawn.key(KeyCode::Esc);
                    assert!(!drawn.app.native_selection());
                    assert!(drawn.app.terminal_requests.is_empty());
                }
            }
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

    /// The user message the transcript tests draw, in the state `pending`
    /// gives it.
    fn message(pending: bool) -> std::sync::Arc<ViewRow> {
        std::sync::Arc::new(ViewRow {
            display: None,
            row_id: "u1".into(),
            kind: RowKind::UserMessage,
            text: "find the leak".into(),
            streaming: false,
            elapsed_ms: None,
            tone: None,
            tool_name: None,
            tool_input: None,
            batch: None,
            pending,
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
    }

    /// The status row a cancelled turn settles after a stop.
    fn stopped() -> std::sync::Arc<ViewRow> {
        std::sync::Arc::new(ViewRow {
            display: None,
            row_id: "stopped-1".into(),
            kind: RowKind::Status,
            text: "stopped".into(),
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
    }

    fn show(drawn: &mut Drawn, rows: Vec<std::sync::Arc<ViewRow>>) {
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            ..ViewSnapshot::default()
        })));
        drawn.draw();
    }

    fn attached() -> Drawn {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        drawn
    }

    #[test]
    fn a_message_waits_in_the_live_tail_until_the_model_consumes_it() {
        let mut drawn = attached();
        show(&mut drawn, vec![message(true)]);
        assert!(
            drawn.transcript().contains("find the leak"),
            "a pending message is drawn"
        );
        assert!(
            !drawn
                .app
                .history
                .iter()
                .any(|block| matches!(block, Block::User { .. })),
            "scrollback prints a message only once it has settled"
        );
        show(&mut drawn, vec![message(false)]);
        assert!(
            drawn
                .app
                .history
                .iter()
                .any(|block| matches!(block, Block::User { text, .. } if text == "find the leak")),
            "a consumed message settles into scrollback"
        );
    }

    #[test]
    fn scrollback_settles_a_pending_message_before_rows_that_came_after_it() {
        let mut drawn = attached();
        show(&mut drawn, vec![message(true), stopped()]);
        let text = drawn.transcript();
        assert!(
            text.find("find the leak") < text.find("stopped"),
            "the message stays where it was said, above the stop: {text}"
        );
        let user = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::User { .. }))
            .expect("the message is committed");
        let stop = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::Status { text, .. } if text == "stopped"))
            .expect("the stop is committed");
        assert!(user < stop, "the message is committed before the stop");
        assert!(
            matches!(drawn.app.history[user], Block::User { pending: false, .. }),
            "a committed row cannot keep the pending hint"
        );
        show(&mut drawn, vec![message(false), stopped()]);
        assert_eq!(
            drawn
                .app
                .history
                .iter()
                .filter(|block| matches!(block, Block::User { .. }))
                .count(),
            1,
            "consuming the message does not commit a second copy"
        );
    }

    #[test]
    fn fullscreen_settles_a_committed_message_in_place() {
        let mut drawn = attached();
        drawn.app.fullscreen = true;
        let size = (80, 40);
        let mut full = FullScreen::new(size, false);
        let paint = |drawn: &mut Drawn, full: &mut FullScreen| {
            let frame = drawn.app.frame(size);
            full.render(&frame, size, &drawn.app.palette, &drawn.app)
        };
        let displayed = |pending| {
            let mut row = message(pending);
            std::sync::Arc::make_mut(&mut row).display = Some("find the leak [Image #1]".into());
            row
        };
        show(&mut drawn, vec![displayed(true)]);
        assert!(
            drawn.app.history.iter().any(|block| matches!(
                block,
                Block::User { text, display, pending: true, .. } if text == "find the leak" && display.as_deref() == Some("find the leak [Image #1]")
            )),
            "fullscreen commits the message where it was said"
        );
        assert!(
            !paint(&mut drawn, &mut full).is_empty(),
            "the pending row is painted"
        );
        show(&mut drawn, vec![displayed(false)]);
        assert!(
            drawn.app.history.iter().any(|block| matches!(
                block,
                Block::User { text, display, pending: false, .. } if text == "find the leak" && display.as_deref() == Some("find the leak [Image #1]")
            )),
            "fullscreen rewrites the same row once the model reads it"
        );
        assert!(
            !paint(&mut drawn, &mut full).is_empty(),
            "the settled row is painted again"
        );
        assert!(
            !drawn
                .app
                .history
                .iter()
                .any(|block| matches!(block, Block::User { pending: true, .. })),
            "no pending copy is left behind"
        );
    }

    #[test]
    fn leaving_fullscreen_prints_a_pending_message_settled() {
        let mut drawn = attached();
        drawn.app.fullscreen = true;
        show(&mut drawn, vec![message(true)]);
        assert!(
            drawn.app.history.iter().any(|block| matches!(
                block,
                Block::User { text, pending: true, .. } if text == "find the leak"
            )),
            "fullscreen commits the pending row"
        );
        drawn.app.set_fullscreen(false);
        drawn.draw();
        assert!(
            drawn.app.history.iter().any(|block| matches!(
                block,
                Block::User { text, pending: false, .. } if text == "find the leak"
            )),
            "scrollback prints the row it cannot recolour"
        );
        assert_eq!(
            drawn
                .app
                .history
                .iter()
                .filter(|block| matches!(block, Block::User { .. }))
                .count(),
            1,
            "the hint is dropped, not printed a second time"
        );
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
        for (sequence, name) in [(2, "applied"), (3, "consumed")] {
            let control = serde_json::from_value(serde_json::json!({
                "id": format!("image-{name}"), "sequence": sequence, "create_at": sequence,
                "type": "input", "name": name, "data": {"input_id": "image"},
            }))
            .unwrap();
            projection.apply("main", &control);
        }
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
                Command::Submit(text, _) => assert!(!text.contains("[Image #1]")),
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
        let _ = drawn
            .app
            .handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
                KeyCode::Left,
                Modifiers::SHIFT,
            )));
        assert!(drawn.app.composer.is_empty());
        assert!(
            matches!(sent.borrow().last(), Some(Command::InputControl { input_id, action: "withdraw" }) if input_id == "queued-image")
        );
        drawn.app.apply(Update::Withdrawn {
            input_id: "queued-image".into(),
        });
        assert_eq!(drawn.app.composer.text(), "look [Image #7]");
        drawn.key(KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Submit(text, _)) if text == "look [Image #7]")
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
        for (size, origin, synchronized) in [
            ((80, 40), None, true),
            ((80, 40), None, false),
            ((80, 40), Some(0), true),
            ((80, 40), Some(3), false),
            ((32, 8), None, true),
            ((32, 8), None, false),
            ((40, 10), None, true),
            ((40, 10), None, false),
        ] {
            let mut drawn = Drawn::new(size, origin, synchronized);
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
            let label = format!("size {size:?} origin {origin:?} sync {synchronized}");
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

    /// The queue strip names only commands the table has, in every locale.
    #[test]
    fn the_queue_strip_names_only_commands_that_exist() {
        let previous = i18n::active();
        for (locale, code, _) in i18n::LOCALES {
            i18n::set(locale);
            for hint in [QUEUE_HINT, QUEUE_HINT_MORE] {
                let text = t(hint);
                for (at, _) in text.match_indices('/') {
                    let name: String = text[at + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect();
                    if name.is_empty() {
                        continue;
                    }
                    assert!(
                        COMMANDS.iter().any(|command| command.slash
                            && (command.name == name || command.aliases.contains(&name.as_str()))),
                        "{code}: {text:?} names /{name}, which is not a command"
                    );
                }
            }
        }
        i18n::set(previous);
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

    /// A session with `queued` messages waiting, while a run is in progress
    /// when `running` is set.
    fn session_with_queue(
        running: bool,
        queued: &[(&str, &str)],
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
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            projection_id: 7,
            focus: "main".into(),
            running,
            main_running: running,
            queued: queued
                .iter()
                .map(|(id, text)| crate::model::view::QueuedInput {
                    id: (*id).into(),
                    text: (*text).into(),
                    display: None,
                })
                .collect(),
            ..ViewSnapshot::default()
        })));
        (app, sent, home)
    }

    fn describe(commands: &[Command]) -> Vec<String> {
        commands
            .iter()
            .map(|command| match command {
                Command::Submit(text, mode) => format!("submit:{}:{}", mode.as_str(), text),
                Command::InputControl { input_id, action } => format!("{action}:{input_id}"),
                Command::Stop => "stop".into(),
                _ => "other".into(),
            })
            .collect()
    }

    /// During a run Enter queues the draft, and Ctrl+S steers a draft that is
    /// not empty.
    #[test]
    fn enter_queues_a_draft_and_ctrl_s_steers_it_during_a_run() {
        let (mut app, sent, _home) = session_with_queue(true, &[]);
        typed(&mut app, "queue me");
        press(&mut app, KeyCode::Enter);
        typed(&mut app, "steer me");
        let _ = app.handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            KeyCode::Char('s'),
            Modifiers::CTRL,
        )));
        assert_eq!(
            describe(&sent.borrow()),
            ["submit:queue:queue me", "submit:steer:steer me"]
        );
    }

    #[test]
    fn steering_waits_for_pending_clipboard_and_image_insertions() {
        for fullscreen in [false, true] {
            for image in [false, true] {
                let (mut app, sent, home) = session_with_queue(true, &[("a", "queued")]);
                app.fullscreen = fullscreen;
                app.composer.set_accepts_images(true);
                let (clipboard_send, clipboard_receive) = std::sync::mpsc::channel();
                let (image_send, image_receive) = std::sync::mpsc::channel();
                let path = home.path().join("screen.png");
                wide_png(&path, 32);
                let plan = app.composer.plan_files(&[path]).unwrap();
                let images = crate::ui::composer::read_planned(&plan).unwrap();
                if image {
                    app.image_read = Some(ImageRead {
                        plan,
                        receive: image_receive,
                    });
                } else {
                    app.clipboard = Some(ClipboardRead::Native(clipboard_receive));
                }
                typed(&mut app, "look");
                for key in [
                    KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                    KeyEvent::new(KeyCode::Char('s'), Modifiers::CTRL),
                ] {
                    app.handle(InputEvent::Key(key));
                    assert!(
                        sent.borrow().is_empty(),
                        "pending insertion must hold sending"
                    );
                    assert_eq!(app.composer.text(), "look");
                    let notice = if image {
                        "Wait for the image insertion before sending"
                    } else {
                        "Wait for clipboard insertion before sending"
                    };
                    assert_eq!(
                        app.notice.as_ref().map(|(text, _)| text.as_str()),
                        Some(notice)
                    );
                }
                if image {
                    image_send.send(Ok(images)).unwrap();
                } else {
                    clipboard_send
                        .send(Ok(crate::clipboard::Source::Text(" at this".into())))
                        .unwrap();
                }
                app.tick();
                app.handle(InputEvent::Key(KeyEvent::new(
                    KeyCode::Char('s'),
                    Modifiers::CTRL,
                )));
                if image {
                    assert!(matches!(
                        sent.borrow().as_slice(),
                        [Command::SubmitDraft(parts, SubmitMode::Steer)]
                            if matches!(parts.as_slice(), [DraftPart::Text(text), DraftPart::Block(dim_agent_types::ContentBlock::Image { .. })] if text == "look ")
                    ));
                } else {
                    assert_eq!(describe(&sent.borrow()), ["submit:steer:look at this"]);
                }
            }
        }
    }

    /// Esc withdraws the queue oldest first before the stop, and each text
    /// joins the draft only when its withdrawal is deleted.
    #[test]
    fn escape_withdraws_the_queue_before_the_stop_and_restores_the_drafts() {
        let (mut app, sent, _home) = session_with_queue(true, &[("a", "first"), ("b", "second")]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(
            describe(&sent.borrow()),
            ["withdraw:a", "withdraw:b", "stop"]
        );
        assert!(app.composer.is_empty());
        app.apply(Update::Withdrawn {
            input_id: "a".into(),
        });
        app.apply(Update::Withdrawn {
            input_id: "b".into(),
        });
        assert_eq!(app.composer.text(), "first\nsecond");
    }

    /// Shift+Left takes the newest queued message back, and its text enters
    /// the draft once the withdrawal is deleted.
    #[test]
    fn shift_left_withdraws_the_newest_queued_message_into_the_draft() {
        let (mut app, sent, _home) = session_with_queue(true, &[("a", "first"), ("b", "second")]);
        let _ = app.handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            KeyCode::Left,
            Modifiers::SHIFT,
        )));
        assert_eq!(describe(&sent.borrow()), ["withdraw:b"]);
        assert!(app.composer.is_empty());
        app.apply(Update::Withdrawn {
            input_id: "b".into(),
        });
        assert_eq!(app.composer.text(), "second");
    }

    /// Enter on an empty draft steers the oldest queued message, as Ctrl+S does.
    #[test]
    fn enter_on_an_empty_draft_steers_like_ctrl_s() {
        let (mut app, sent, _home) = session_with_queue(true, &[("a", "first"), ("b", "second")]);
        press(&mut app, KeyCode::Enter);
        let _ = app.handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            KeyCode::Char('s'),
            Modifiers::CTRL,
        )));
        assert_eq!(describe(&sent.borrow()), ["steer:a", "steer:a"]);
    }

    /// Esc with no run in progress keeps the draft.
    #[test]
    fn escape_keeps_an_idle_draft() {
        let (mut app, sent, _home) = session_with_queue(false, &[]);
        typed(&mut app, "keep");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.composer.text(), "keep");
        assert!(sent.borrow().is_empty());
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
        typed(&mut app, "usage");
        assert!(
            app.frame((80, 40))
                .live
                .iter()
                .any(|row| row.contains("Commands")),
            "the palette is drawn in the panel's place"
        );
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent.borrow().last(), Some(Command::Usage)),
            "the palette ran /usage"
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
            matches!(
                sent.borrow().last(),
                Some(Command::Management(ManagementRequest {
                    area: Area::Mcp,
                    action: ManagementAction::List,
                }))
            ),
            "the palette opened the MCP panel"
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

    #[test]
    fn settings_recording_sends_its_words() {
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
        app.run_command("settings", "recording on");
        assert!(matches!(sent.borrow().last(), Some(Command::Recording(words)) if words == "on"));
        app.run_command("settings", "recording");
        assert!(
            matches!(sent.borrow().last(), Some(Command::Recording(words)) if words.is_empty())
        );
    }

    fn image_app(work: &std::path::Path) -> (App, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            work.to_str().unwrap().into(),
        );
        (app, home)
    }

    fn wide_png(path: &std::path::Path, width: u32) {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(path, png.into_inner()).unwrap();
    }

    /// A pasted image path is read off the interface thread: the paste
    /// returns at once, and the token goes in when its result arrives.
    #[test]
    fn a_pasted_image_path_inserts_its_token_after_the_read() {
        use dim_agent_capabilities::images::MAX_EDGE;
        let width = MAX_EDGE * 2;
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("wide.png");
        wide_png(&image, width);
        let (mut app, _home) = image_app(work.path());
        app.handle(InputEvent::Paste(image.display().to_string()));
        assert_eq!(app.composer.text(), "");
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("Reading image…")
        );
        assert!(app.image_read.is_some());
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        while app.image_read.is_some() {
            assert!(Instant::now() < deadline, "the read did not return");
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            app.composer.display_text(),
            format!("[Image #1 · {width}×2 → {MAX_EDGE}×1]")
        );
        assert!(app.notice.is_none());
    }

    /// A paste while an image read runs is refused, and the pending group
    /// still lands on its own.
    #[test]
    fn a_paste_during_an_image_read_is_refused() {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("wide.png");
        wide_png(&image, 4096);
        let (mut app, _home) = image_app(work.path());
        let (send, receive) = std::sync::mpsc::channel();
        app.image_read = Some(ImageRead {
            plan: Vec::new(),
            receive,
        });
        app.handle(InputEvent::Paste(image.display().to_string()));
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("An image is still being inserted; paste again when it is in")
        );
        assert_eq!(app.composer.text(), "");
        drop(send);
    }

    /// A reader thread that ends without a result is a failed read: the
    /// pending state clears, the notice says so, and the next paste works.
    #[test]
    fn a_reader_thread_that_dies_fails_the_read() {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("wide.png");
        wide_png(&image, 4096);
        let (mut app, _home) = image_app(work.path());
        let (send, receive) = std::sync::mpsc::channel();
        drop(send);
        app.image_read = Some(ImageRead {
            plan: Vec::new(),
            receive,
        });
        app.tick();
        assert!(app.image_read.is_none());
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("The image could not be read")
        );
        assert_eq!(app.composer.text(), "");
        app.handle(InputEvent::Paste(image.display().to_string()));
        assert!(
            app.image_read.is_some(),
            "the next paste starts its own read"
        );
    }

    /// Ctrl+V during an image read says why it waits, the notice a path
    /// paste shows, instead of doing nothing.
    #[test]
    fn a_clipboard_paste_during_an_image_read_is_refused() {
        let work = tempfile::tempdir().unwrap();
        let (mut app, _home) = image_app(work.path());
        let (send, receive) = std::sync::mpsc::channel();
        app.image_read = Some(ImageRead {
            plan: Vec::new(),
            receive,
        });
        let _ = app.handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            KeyCode::Char('v'),
            Modifiers::CTRL,
        )));
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("An image is still being inserted; paste again when it is in")
        );
        assert!(app.clipboard.is_none());
        drop(send);
    }

    /// A path paste waits for a pending clipboard read: its image could still
    /// land and take the group past the count it was planned against.
    #[test]
    fn a_paste_during_a_clipboard_read_is_refused() {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("wide.png");
        wide_png(&image, 4096);
        let (mut app, _home) = image_app(work.path());
        let (send, receive) = std::sync::mpsc::channel();
        app.clipboard = Some(ClipboardRead::Native(receive));
        app.handle(InputEvent::Paste(image.display().to_string()));
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("The clipboard is still being inserted; paste again when it is in")
        );
        assert_eq!(app.composer.text(), "");
        assert!(app.image_read.is_none());
        drop(send);
    }

    /// A Save reply reaches the app after its panel closed (Esc is allowed
    /// while a write is in flight): the draft is settled and the outcome is
    /// said as a notice.
    #[test]
    fn a_save_reply_without_its_panel_still_settles_the_draft() {
        use crate::model::session::{ManagementReply, ManagementView};
        fn view() -> ManagementView {
            ManagementView::default()
        }
        fn save_reply(result: ManagementResult) -> Update {
            Update::Management(ManagementReply {
                request: ManagementRequest {
                    area: Area::Hooks,
                    action: ManagementAction::Save {
                        id: None,
                        expected_revision: "user:1".into(),
                        document: serde_json::json!({}),
                    },
                },
                result,
            })
        }
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/w".into(),
        );
        let draft = |app: &mut App| {
            app.hold_draft(ManagementDraft {
                area: Area::Hooks,
                target: WriteTarget::Area {
                    revision: "user:1".into(),
                },
                draft: editor::Draft::open(root.path(), Area::Hooks, "{}").unwrap(),
            });
            app.management_edits
                .last()
                .unwrap()
                .draft
                .path()
                .to_path_buf()
        };
        // Written: the file goes and the outcome is a notice.
        let path = draft(&mut app);
        app.apply(save_reply(ManagementResult::View {
            view: Box::new(view()),
            note: None,
        }));
        assert!(!path.exists(), "the written draft is removed");
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("Saved.")
        );
        assert!(app.management_edits.is_empty());
        // Written, and only the read after it failed: the write still took
        // the text, so the file goes too and the notice says what failed.
        let path = draft(&mut app);
        app.apply(save_reply(ManagementResult::Applied {
            note: None,
            read_error: "catalog unavailable".into(),
        }));
        assert!(!path.exists(), "an applied write keeps no draft");
        let (text, _) = app.notice.clone().unwrap();
        assert_eq!(text, "Saved. catalog unavailable");
        assert!(!text.contains("kept in"), "{text}");
        assert!(app.management_edits.is_empty());
        // A create's own answer is the notice, not a fixed "Saved.".
        app.management_edits.push(ManagementDraft {
            area: Area::Scheduled,
            target: WriteTarget::Create,
            draft: editor::Draft::open(root.path(), Area::Scheduled, "{}").unwrap(),
        });
        let path = app
            .management_edits
            .last()
            .unwrap()
            .draft
            .path()
            .to_path_buf();
        app.apply(Update::Management(ManagementReply {
            request: ManagementRequest {
                area: Area::Scheduled,
                action: ManagementAction::Add {
                    document: serde_json::json!({}),
                },
            },
            result: ManagementResult::View {
                view: Box::new(view()),
                note: Some("Added digest.".into()),
            },
        }));
        assert!(!path.exists(), "the created draft is removed");
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("Added digest.")
        );
        // A run opened no draft; its answer is reported too.
        app.apply(Update::Management(ManagementReply {
            request: ManagementRequest {
                area: Area::Scheduled,
                action: ManagementAction::Run { id: "job-1".into() },
            },
            result: ManagementResult::View {
                view: Box::new(view()),
                note: Some("Run job-1: skipped (overlap).".into()),
            },
        }));
        assert_eq!(
            app.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("Run job-1: skipped (overlap).")
        );
        // Refused: the file stays, the notice names it, and the link keeps
        // the revision the draft came from.
        let path = draft(&mut app);
        app.apply(save_reply(ManagementResult::Refused(
            ManagementRefusal::Refused("nope".into()),
        )));
        assert!(path.exists(), "the refused draft stays");
        let (text, _) = app.notice.clone().unwrap();
        assert!(text.contains("nope"), "{text}");
        assert!(text.contains(&path.display().to_string()), "{text}");
        let kept = app.management_edits.first().expect("the link stays");
        assert_eq!(
            kept.target,
            WriteTarget::Area {
                revision: "user:1".into()
            }
        );
        assert_eq!(kept.draft.path(), path.as_path());
        // A conflict keeps the file and names it, and drops the link.
        let path = draft(&mut app);
        app.apply(save_reply(ManagementResult::Refused(
            ManagementRefusal::Conflict {
                message: "The user document changed since it was read.".into(),
                view: Ok(Box::new(view())),
            },
        )));
        assert!(path.exists(), "the conflicted draft stays");
        let (text, _) = app.notice.clone().unwrap();
        assert!(text.contains("The user document changed"), "{text}");
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(app.management_edits.is_empty(), "a conflict drops the link");
    }

    /// A toggle sent from a panel that closes before the reply names the
    /// entry by the label the panel held when it sent the action.
    #[test]
    fn a_toggle_without_its_panel_names_the_entrys_label() {
        use crate::model::session::{
            ActionKind, ManagementEntry, ManagementGroup, ManagementReply, ManagementView,
        };
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/w".into(),
        );
        let mut panel = ManagementPanel::new(Area::Skills, None);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::List,
            },
            result: ManagementResult::View {
                view: Box::new(ManagementView {
                    groups: vec![ManagementGroup {
                        heading: "user".into(),
                        entries: vec![ManagementEntry {
                            id: "user:commit".into(),
                            label: "commit".into(),
                            detail: Vec::new(),
                            enabled: Some(true),
                            states: Vec::new(),
                            actions: vec![ActionKind::Toggle],
                            document: None,
                        }],
                    }],
                    ..ManagementView::default()
                }),
                note: None,
            },
        });
        app.prompt = Some(Prompt::Management(panel));
        let request = ManagementRequest {
            area: Area::Skills,
            action: ManagementAction::Toggle {
                id: "user:commit".into(),
                enabled: false,
            },
        };
        // What every panel send does before the command goes out.
        app.remember_named(&request);
        // Esc closes the panel while the toggle is in flight.
        app.prompt = None;
        app.apply(Update::Management(ManagementReply {
            request,
            result: ManagementResult::Applied {
                note: None,
                read_error: "catalog unavailable".into(),
            },
        }));
        let (text, _) = app.notice.clone().unwrap();
        assert_eq!(text, "Disabled commit. catalog unavailable");
    }

    /// A run whose owner answer carries an escape sequence and a newline
    /// reaches the status row on one line, without the escape.
    #[test]
    fn a_closed_panel_run_keeps_the_owners_answer_on_one_line() {
        use crate::model::session::{ManagementReply, ManagementView};
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/w".into(),
        );
        app.apply(Update::Management(ManagementReply {
            request: ManagementRequest {
                area: Area::Scheduled,
                action: ManagementAction::Run { id: "job-1".into() },
            },
            result: ManagementResult::View {
                view: Box::new(ManagementView::default()),
                note: Some("Run daily\u{1b}[31m: skipped\n(overlap).".into()),
            },
        }));
        let (text, _) = app.notice.clone().unwrap();
        assert_eq!(text, "Run daily: skipped (overlap).");
        assert!(
            !text.contains('\u{1b}') && !text.contains('\n') && !text.contains('\r'),
            "{text:?}"
        );
    }

    /// A reply for one area does not write its message into another area's
    /// panel that opened while the write was in flight: the notice reports
    /// it, and the open panel keeps its own state.
    #[test]
    fn another_areas_panel_keeps_its_own_message() {
        use crate::model::session::ManagementReply;
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/w".into(),
        );
        // The Scheduler's panel sent a create, then Esc closed it; the
        // Skills panel opened before the reply landed.
        app.hold_draft(ManagementDraft {
            area: Area::Scheduled,
            target: WriteTarget::Create,
            draft: editor::Draft::open(root.path(), Area::Scheduled, "{}").unwrap(),
        });
        let path = app
            .management_edits
            .last()
            .unwrap()
            .draft
            .path()
            .display()
            .to_string();
        app.prompt = Some(Prompt::Management(ManagementPanel::new(Area::Skills, None)));
        app.apply(Update::Management(ManagementReply {
            request: ManagementRequest {
                area: Area::Scheduled,
                action: ManagementAction::Add {
                    document: serde_json::Value::Null,
                },
            },
            result: ManagementResult::Refused(ManagementRefusal::Refused("nope".into())),
        }));
        let shown = match &app.prompt {
            Some(Prompt::Management(panel)) => panel.render(80, 20, &theme.palette).join(" "),
            _ => panic!("the Skills panel stays open"),
        };
        assert!(!shown.contains("nope"), "{shown}");
        assert!(!shown.contains(&path), "{shown}");
        assert!(!shown.contains("Your text is kept in"), "{shown}");
        let (text, _) = app.notice.clone().unwrap();
        assert!(text.contains("nope"), "{text}");
        assert!(text.contains(&path), "{text}");
        assert!(app.management_edits.first().unwrap().draft.path().exists());
    }

    /// A create the owner refuses, or a conflict on one, keeps the draft
    /// file and its link: a create has no revision to lose, so the next
    /// Ctrl+N reopens the user's text, with the panel open or closed.
    #[test]
    fn a_create_reply_keeps_the_draft_and_its_link() {
        use crate::model::session::{ManagementReply, ManagementView};
        fn create_reply(result: ManagementResult) -> Update {
            Update::Management(ManagementReply {
                request: ManagementRequest {
                    area: Area::Scheduled,
                    action: ManagementAction::Add {
                        document: serde_json::json!({}),
                    },
                },
                result,
            })
        }
        fn refused() -> ManagementResult {
            ManagementResult::Refused(ManagementRefusal::Refused("nope".into()))
        }
        fn conflict() -> ManagementResult {
            ManagementResult::Refused(ManagementRefusal::Conflict {
                message: "The jobs changed since they were read.".into(),
                view: Ok(Box::new(ManagementView::default())),
            })
        }
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/w".into(),
        );
        let draft = |app: &mut App| {
            app.hold_draft(ManagementDraft {
                area: Area::Scheduled,
                target: WriteTarget::Create,
                draft: editor::Draft::open(root.path(), Area::Scheduled, "{}").unwrap(),
            });
            app.management_edits
                .last()
                .unwrap()
                .draft
                .path()
                .to_path_buf()
        };
        // Esc closed the panel while the create was in flight.
        for result in [refused(), conflict()] {
            let path = draft(&mut app);
            app.apply(create_reply(result));
            assert!(path.exists(), "a create keeps its file: {path:?}");
            let kept = app.management_edits.first().expect("the link stays");
            assert_eq!(kept.draft.path(), path.as_path());
        }
        // The panel is open: the file and the link stay for the next Ctrl+N.
        for result in [refused(), conflict()] {
            app.prompt = Some(Prompt::Management(ManagementPanel::new(
                Area::Scheduled,
                None,
            )));
            let path = draft(&mut app);
            app.apply(create_reply(result));
            assert!(path.exists(), "a create keeps its file: {path:?}");
            let kept = app.management_edits.first().expect("the link stays");
            assert_eq!(kept.draft.path(), path.as_path());
        }
    }
}
