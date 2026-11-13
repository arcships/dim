//! The interactive interface. The welcome banner is committed once into
//! scrollback; the conversation streams in as whole Markdown blocks (2.4), the
//! unclosed tail and the composer box live at the bottom and repaint by row
//! diff (2.3). The terminal layer (2.2) guarantees the exit contract.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::management::{self, editor};
use crate::model::session::{
    Area, Command, CompactReply, ContextReading, DIM_OAUTH_ID, DeleteOutcome, DeleteReply,
    DraftPart, ManagementAction, ManagementRefusal, ManagementRequest, ManagementResult,
    PromptField, PromptSetting, RenameReply, SessionPort, SubmitMode, SuggestionSetting,
    SwitchKind, Update,
};
#[cfg(test)]
use crate::model::shell::Outcome as ShellOutcome;
use crate::model::shell::ShellRun;
use crate::model::view::{
    CompactionState, Earlier, LineState, LineView, RowKind, SideLine, TaskFinish, TaskOutcome,
    TaskTarget, ViewRow, ViewSnapshot,
};
use crate::ui::Env;
use crate::ui::Initial;
use crate::ui::approvals_panel::ApprovalsPanel;
use crate::ui::commands::{COMMANDS, Context as CommandContext, Needs};
use crate::ui::composer::{Composer, Outcome as ComposerOutcome, Shell, history};
use crate::ui::i18n::{self, t, tf};
use crate::ui::language_panel::LanguagePanel;
use crate::ui::lines_panel::{LinesPanel, short_id};
use crate::ui::list_panel::{Event as ListEvent, Item, ListPanel};
use crate::ui::management_panel::ManagementPanel;
use crate::ui::models_panel::ModelsPanel;
use crate::ui::oauth_view::OauthView;
use crate::ui::plugins_panel::PluginsPanel;
use crate::ui::progress::ProgressBar;
use crate::ui::prompts::{
    AskUserPrompt, Effect, EffortPrompt, ExitConfirm, ExitReason, PermissionPrompt, WriteTarget,
};
use crate::ui::providers_panel::ProvidersPanel;
use crate::ui::queue_panel::QueuePanel;
use crate::ui::render::fullscreen::Document;
use crate::ui::render::fullscreen::MouseOutcome;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::render::panel::{clip, wrap, wrap_words};
use crate::ui::render::{Frame, FullScreen, Overlay, Screen};
use crate::ui::report_panel;
use crate::ui::runtime_panel::RuntimePanel;
use crate::ui::sessions_panel::SessionsPanel;
use crate::ui::settings_panel::{Setting, SettingsPanel, Values as SettingValues};
use crate::ui::status::{Reporter, Runs, Status};
use crate::ui::tasks_panel::TasksPanel;
use crate::ui::terminal::{
    InputEvent, KeyCode, KeyEvent, Modifiers, Terminal, TerminalError, Wait,
};
use crate::ui::text::{
    RUNNING_MARK, STATUS_MARK, display_width, truncate_cells, truncate_cells_start,
};
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::theme::{Palette, Theme, detect};
use crate::ui::theme_panel::ThemePanel;
use crate::ui::timeline_panel::{TimelineAction, TimelinePanel};
use crate::ui::title::WindowTitle;
use crate::ui::transcript::block::{Block, Tone, group_rows, opens_group, project_row};
use crate::ui::transcript::blocks::{
    SHELL_PREVIEW_LINES, boundary_line, gutter_text, live_thinking, render_block, render_shell,
    streamed_thinking, thinking_prefix, width_of,
};
#[cfg(test)]
use crate::ui::transcript::blocks::{THINKING_ROWS, settled_thinking};
use crate::ui::transcript::commit::CommitState;
use crate::ui::transcript::reveal::Reveal;
use crate::ui::transcript::stream::{StreamPlanner, TextBlock, render_open};
use crate::ui::transcript::tool_group::{
    Detail, ToolGroup, action_at, card_actions, card_rows, disclosure_at, disclosure_rows,
    group_id, project_fullscreen_group, project_group, render_group,
};
use crate::ui::upgrade_dialog::UpgradeDialog;
use crate::usage::format_tokens;
use crate::wire::Configuration;

const COMPOSER_PLACEHOLDER: &str = "Ask anything... · Ctrl+J newline";
const SHELL_PLACEHOLDER: &str = "Run a shell command · Esc to exit";
/// `config.json` key of the three tool display forms.
const TOOL_DETAILS_KEY: &str = "tuiToolDetails";
const LEGACY_TOOL_DETAILS_KEY: &str = "tuiToolDetailsExpanded";
const THINKING_DETAILS_KEY: &str = "tuiThinkingDetailsExpanded";
const DEFAULT_SEND_MODE_KEY: &str = "tuiDefaultSendMode";
/// Share of the compaction threshold the context reading warns at. Set by
/// the interface; the threshold itself is the one `session.context.usage`
/// returns (compaction requirements §4).
const CONTEXT_WARNING_PERCENT: u64 = 90;
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
const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000_u64.div_ceil(240));
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
    Thought {
        id: String,
        stream: ThoughtStream,
        finished: bool,
    },
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

/// The preset as the status line names it.
fn permission_label(preset: dim_agent_types::PermissionPreset) -> &'static str {
    match preset {
        dim_agent_types::PermissionPreset::ReadOnly => "Read only",
        dim_agent_types::PermissionPreset::WorkspaceWrite => "Workspace write",
        dim_agent_types::PermissionPreset::FullAccess => "Full access",
    }
}

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
const PULSE_FRAMES: [&str; 4] = ["·", "•", "●", "•"];

pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

pub fn run(host: Box<dyn SessionPort>, surroundings: Env) -> i32 {
    let code = run_terminal(host, surroundings);
    history::flush();
    code
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

/// Writes each notification the run queued since the last frame (`ui::notify`)
/// through `write`, in the order they came, and empties the queue. `write` is
/// the terminal's write; an error ends the interface as that one does.
fn write_notifications(
    notifications: &mut Vec<crate::ui::notify::Event>,
    terminal: &crate::ui::notify::Terminal,
    mut write: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for event in std::mem::take(notifications) {
        write(&crate::ui::notify::sequence(terminal, &event))?;
    }
    Ok(())
}

fn run_terminal(host: Box<dyn SessionPort>, surroundings: Env) -> i32 {
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
    // A terminal that answered the program status query keeps the record of
    // the run, so the title and the bar leave the job to it.
    let program_status = terminal.program_status();
    let env = detect::env_from_process();
    let mut progress = ProgressBar::from_env(&env, program_status);
    let mut title = WindowTitle::from_env(program_status, Instant::now());
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
    let cwd_key = surroundings.workspace.clone();
    let mut app = App::new(
        host,
        MarkdownRenderer::new(&theme),
        theme.palette.clone(),
        surroundings.home.clone(),
        cwd_key,
    );
    app.fullscreen = full.is_some();
    app.initial = surroundings.initial;
    app.awaiting_window = surroundings.resume;
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
        // A management panel may ask for the editor with its first view, as
        // `/workflow:save` opens the add step: the reply arrives here, and
        // the editor runs through the step the edit key uses.
        if let Some(Effect::EditManagement {
            area,
            target,
            text,
            seed,
        }) = app.take_management_effect()
        {
            if let Err(code) = edit_management(&mut app, &mut terminal, area, target, text, seed) {
                app.host.shutdown();
                return code;
            }
            status.forget();
            progress.forget();
            title.forget();
            dirty = true;
        }
        if let Some(code) = app.exit_code
            && !dirty
        {
            break code;
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
        if write_notifications(&mut app.notifications, &notify_terminal, |bytes| {
            terminal.write(bytes)
        })
        .is_err()
        {
            let code = terminal.lost();
            app.host.shutdown();
            return code;
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
        if dirty && due && resize.is_none() {
            let size = current;
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
        let wait = match (dirty && resize.is_none(), last_frame) {
            (true, Some(at)) if !terminal.writing() => {
                FRAME_INTERVAL.saturating_sub(now.duration_since(at))
            }
            _ if app.running()
                || terminal.writing()
                || app.failure.is_none() && app.awaiting_window =>
            {
                FRAME_INTERVAL
            }
            _ => SESSION_POLL,
        };
        // The first message waits for the first frame so it follows the
        // banner, then enters the interface as the submission event.
        let next = if app.initial.is_some() && last_frame.is_some() {
            Wait::Event(InputEvent::Submit)
        } else {
            terminal.next_event(wait)
        };
        match next {
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
                        if app.click(block, row, full.layout_cols() as usize) {
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
                    && ((key.code == KeyCode::Tab && key.mods.0 == 0)
                        || full.as_ref().is_some_and(FullScreen::focused)) =>
            {
                let Some(full) = &mut full else { continue };
                dirty = true;
                match key.code {
                    KeyCode::Tab | KeyCode::Up if key.mods.0 == 0 => full.focus_older(&app),
                    KeyCode::BackTab | KeyCode::Down => full.focus_newer(&app),
                    KeyCode::Tab => full.focus_newer(&app),
                    KeyCode::Esc => full.clear_focus(),
                    KeyCode::Enter | KeyCode::Char(' ')
                        if key.mods.0 == 0 && full.focused_tool().is_some() =>
                    {
                        let (block, row) = full.focused_tool().expect("picked tool");
                        if app.click(block, row, full.layout_cols() as usize) {
                            full.hold();
                        }
                    }
                    KeyCode::Enter if key.mods.0 == 0 && full.focused_card().is_some() => {
                        let card = full.focused_card().expect("picked card").to_owned();
                        app.open_card(&card);
                    }
                    KeyCode::Char('e') if key.mods.0 == 0 => {
                        if let Some(text) = full.focused_text(&app) {
                            app.set_draft(&text);
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
                    if key.code == KeyCode::PageUp && full.at_top() {
                        app.show_earlier();
                    } else {
                        match key.code {
                            KeyCode::Home => full.scroll_to_top(),
                            KeyCode::End => full.follow(),
                            code => full.page(code == KeyCode::PageDown),
                        }
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
                Outcome::Edit {
                    area,
                    target,
                    text,
                    seed,
                } => {
                    if let Err(code) =
                        edit_management(&mut app, &mut terminal, area, target, text, seed)
                    {
                        app.host.shutdown();
                        return code;
                    }
                    status.forget();
                    progress.forget();
                    title.forget();
                    dirty = true;
                }
                Outcome::EditPrompt {
                    field,
                    text,
                    revision,
                } => {
                    if let Err(code) = edit_prompt(&mut app, &mut terminal, field, text, revision) {
                        app.host.shutdown();
                        return code;
                    }
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
    /// A management panel's edit: the document's text and what its write
    /// becomes, for the external editor.
    Edit {
        area: Area,
        target: WriteTarget,
        text: String,
        seed: Option<serde_json::Value>,
    },
    /// A prompt row's edit: the field, the text the editor opens with, and
    /// the revision it was displayed at.
    EditPrompt {
        field: PromptField,
        text: String,
        revision: String,
    },
    /// `/web`: flip the daemon-owned Web entry switch without leaving.
    Web,
    Exit(i32),
}

/// Opens the external editor on a prompt field and sends what it leaves.
/// `Err` is the code the terminal lost.
fn edit_prompt(
    app: &mut App,
    terminal: &mut Terminal,
    field: PromptField,
    text: String,
    revision: String,
) -> std::result::Result<(), i32> {
    let draft = match app.prompt_draft.as_ref().map(|(kept, _)| *kept) {
        Some(kept) if kept == field => app.prompt_draft.take().expect("the kept prompt draft").1,
        _ => match editor::Draft::open_text(&app.home, field.key(), "md", &text) {
            Ok(draft) => draft,
            Err(error) => {
                app.set_notice(error);
                return Ok(());
            }
        },
    };
    let path = draft.path().display().to_string();
    let mut command = editor::command(draft.path());
    let program = command.get_program().to_string_lossy().into_owned();
    let ran = match terminal.run_external(&mut command) {
        Ok(ran) => ran,
        Err(_) => return Err(terminal.lost()),
    };
    match ran {
        Ok(status) if status.success() => {}
        Ok(status) => {
            app.prompt_draft = Some((field, draft));
            app.set_notice(format!(
                "{} Nothing was written. Your text is kept in {path}.",
                editor::exited(status)
            ));
            return Ok(());
        }
        Err(error) => {
            app.prompt_draft = Some((field, draft));
            app.set_notice(format!("{program}: {error}"));
            return Ok(());
        }
    }
    let text = match draft.text() {
        Ok(text) => text,
        Err(error) => {
            app.prompt_draft = Some((field, draft));
            app.set_notice(format!("{error} Your text is kept in {path}."));
            return Ok(());
        }
    };
    if text.trim().is_empty() {
        draft.remove();
        app.set_notice(format!(
            "An empty draft writes nothing; press r on {} to reset it",
            field.key()
        ));
        return Ok(());
    }
    let current = app
        .prompt_setting
        .as_ref()
        .and_then(|setting| setting.text(field))
        .unwrap_or_default();
    if text == current {
        draft.remove();
        app.set_notice(format!("{}: unchanged", field.key()));
        return Ok(());
    }
    let request = app.next_prompt_request;
    app.next_prompt_request = app.next_prompt_request.wrapping_add(1);
    app.pending_prompt = Some(PendingPrompt {
        request,
        field,
        draft: Some(draft),
    });
    app.host.send(Command::SavePromptSetting {
        field,
        text,
        revision,
        request,
    });
    Ok(())
}

/// Opens the external editor on a management draft and writes what it
/// leaves. `Err` is the code the terminal lost.
fn edit_management(
    app: &mut App,
    terminal: &mut Terminal,
    area: Area,
    target: WriteTarget,
    text: String,
    seed: Option<serde_json::Value>,
) -> std::result::Result<(), i32> {
    // A draft a refused write kept continues in its own file; anything else
    // starts from the document the panel read.
    let (target, draft, seed, reopened) = match app.kept_draft(&area, &target) {
        Some((target, draft, seed)) => (target, draft, seed, true),
        None => {
            let draft = match editor::Draft::open(&app.home, area.clone(), &text) {
                Ok(draft) => draft,
                Err(error) => {
                    if let Some(Prompt::Management(panel)) = &mut app.prompt {
                        panel.failed(error);
                    }
                    return Ok(());
                }
            };
            (target, draft, seed, false)
        }
    };
    let mut command = editor::command(draft.path());
    let program = command.get_program().to_string_lossy().into_owned();
    let ran = match terminal.run_external(&mut command) {
        Ok(ran) => ran,
        Err(_) => return Err(terminal.lost()),
    };
    app.write_draft(
        ManagementDraft {
            area,
            target,
            draft,
            seed,
        },
        reopened,
        program,
        ran,
    );
    app.request_reflow();
    Ok(())
}

/// The startup preview or adopted model: `provider / model` and its
/// reasoning effort when present.
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

#[derive(Default, Clone, Copy)]
struct ThoughtStream {
    written: usize,
    block: Option<usize>,
}

struct ThoughtFrame {
    stream: ThoughtStream,
    commit: String,
    source: Block,
    tail: Vec<String>,
}

struct App {
    host: Box<dyn SessionPort>,
    composer: Composer,
    cwd_key: String,
    /// The workspace shown in the composer's label, with the home written
    /// `~`: the workspace the attached Session reports, or [`Self::cwd_key`]
    /// at Home. Never read from the process's directory, which a frame would
    /// otherwise see change under it. Display form only: a caller that needs
    /// a path uses [`Self::git_head`] or the Session's cwd.
    workspace: String,
    home: PathBuf,
    /// How tool groups are drawn by default (`/tools`, which redraws every
    /// committed group too).
    tool_detail: Detail,
    thinking_detail: Detail,
    default_send_mode: SubmitMode,
    tool_choices: HashMap<(String, String), HashMap<String, bool>>,
    tail_groups: Vec<(usize, ToolGroup)>,
    notices: Vec<String>,
    reports: Vec<Block>,
    report_error: Option<crate::report::Captured>,
    report_sequence: u64,
    banner_committed: bool,
    snapshot: Option<ViewSnapshot>,
    /// Home's startup preview or the attached Session's adopted model.
    label: Option<ModelLabel>,
    failure: Option<String>,
    /// Whether running the same command again could open the Session this
    /// failure left; false only for a Session the daemon refuses for its
    /// protocol version, which no run of this client opens.
    failure_resumable: bool,
    exit_code: Option<i32>,
    failure_committed: bool,
    planner: StreamPlanner,
    thoughts: HashMap<String, ThoughtStream>,
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
    /// The second the task line was last laid out for: its elapsed times
    /// follow this, and it repaints the row so an ended task leaves on time.
    task_second: u64,
    /// Whether the task line was drawn in the frame the pulse last asked for:
    /// the frame that erases it is the one where it stopped being drawn.
    task_row: bool,
    session_id: String,
    /// The invocation's first message, submitted once the first frame is
    /// drawn.
    initial: Option<Initial>,
    /// A submitted first message no Session has taken yet: a refusal puts
    /// it back into the draft.
    unsent: Option<Initial>,
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
    /// An earlier read whose page is to be revealed when it lands: set by
    /// `show_earlier`, cleared once the page is shown or the read ends
    /// without one.
    reveal_pending: bool,
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
    permissions_return: Option<ApprovalsPanel>,
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
    /// Background tasks already reported as ended, by task id: a task the
    /// live stream ended is reported once.
    notified_tasks: HashSet<String>,
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
    branch: Option<String>,
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
    compaction_targets: Option<(
        u64,
        std::collections::BTreeSet<crate::model::view::CompactionTarget>,
    )>,
    /// The update dialog while another prompt has the screen; it comes
    /// back once that one closes.
    upgrade: Option<UpgradeDialog>,
    /// A line about to open on the shown line: the next submit is its
    /// opening input (timeline UI §4.2).
    line_draft: Option<LineDraft>,
    /// The picked message, set in the fork's composer once it is attached.
    fork_draft: Option<String>,
    /// A management area's drafts while the editor runs and until their write
    /// comes back: a refused write keeps its own, so the edit key of the
    /// target it was taken for reopens the user's text.
    management_edits: Vec<ManagementDraft>,
    /// The entry a management action the panel sent names, with the label the
    /// panel showed then: the panel may close before the reply lands.
    management_named: Option<(Area, ManagementAction, Option<String>)>,
    /// The message the Session offered for the turn its composer belongs to.
    suggestion: Option<Suggestion>,
    /// The `prompt-suggestion` switch as last read or written; none until a
    /// read answers.
    suggestion_setting: Option<SuggestionSetting>,
    /// Whether this Session's method table lists the suggestion methods:
    /// without them, `/settings` neither probes the source nor shows the row.
    suggestion_methods: bool,
    /// A flip asked for before the switch was known: the read that answers
    /// it sends the flip.
    suggestion_flip: bool,
    /// The `prompts` source as last read or written.
    prompt_setting: Option<PromptSetting>,
    /// A draft a refused prompt write kept, for the next edit of its field.
    prompt_draft: Option<(PromptField, editor::Draft)>,
    /// The prompt write in flight: only its own reply settles it.
    pending_prompt: Option<PendingPrompt>,
    /// The next prompt write's id.
    next_prompt_request: u64,
}

struct PendingPrompt {
    request: u64,
    field: PromptField,
    /// The save's draft; a reset keeps no draft and leaves its copy behind.
    draft: Option<editor::Draft>,
}

/// An offer the Session made for the message the user would send next.
struct Suggestion {
    turn_id: String,
    text: String,
}

/// A management area's draft of one document: the text the editor was
/// given, and the target whose write expects it. One target holds at most
/// one draft, and another target's draft is never replaced by it.
struct ManagementDraft {
    area: Area,
    target: WriteTarget,
    draft: editor::Draft,
    /// The document a run's editor was first opened with: a kept draft
    /// reopens with this seed, not with the user's edited text.
    seed: Option<serde_json::Value>,
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

struct ForkAsk {
    at: u64,
    /// What the fork's composer starts with.
    text: String,
    /// The picked message the fork cuts before; none for `/fork`, which
    /// cuts after the last turn.
    before: Option<String>,
    create: bool,
    timeline: Option<Box<TimelinePanel>>,
}

impl ForkAsk {
    fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                self.create = !self.create;
                Effect::None
            }
            KeyCode::Enter if self.create => Effect::Fork,
            KeyCode::Enter | KeyCode::Esc => Effect::Close,
            KeyCode::Char('y' | 'Y' | '1') if key.mods == Modifiers::NONE => Effect::Fork,
            KeyCode::Char('n' | 'N' | '2') if key.mods == Modifiers::NONE => Effect::Close,
            _ => Effect::None,
        }
    }

    fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        use crate::ui::render::panel::{bordered, choices};
        let inner = width.saturating_sub(4).max(1);
        let question = match &self.before {
            Some(message) => tf(
                "Fork the session before “{message}”?",
                &[("message", &crate::sanitize::safe_line(message))],
            ),
            None => t("Fork the session after the last turn?").to_owned(),
        };
        let mut body = wrap_words(&question, inner);
        let mut hints = wrap_words(t("↑↓ select · Enter confirm · Esc cancel"), inner);
        if body.len() + 4 + hints.len() > rows {
            hints.clear();
        }
        let body_rows = rows.saturating_sub(4 + hints.len());
        if body.len() > body_rows {
            body.truncate(body_rows);
            if let Some(last) = body.last_mut() {
                *last = truncate_cells(&format!("{last} …"), inner);
            }
        }
        let explanation = wrap_words(
            t("Creates a new session; the current session stays unchanged."),
            inner,
        );
        if body.len() + 1 + explanation.len() <= body_rows {
            body.push(String::new());
            body.extend(explanation);
        }
        let mut content: Vec<String> = body.iter().map(|line| fg(line, &p.text)).collect();
        for (index, label) in [t("1. Create fork"), t("2. Cancel")].iter().enumerate() {
            content.push(choices(
                &[truncate_cells(label, inner.saturating_sub(2))],
                if self.create == (index == 0) { 0 } else { 1 },
                p,
            ));
        }
        content.extend(hints.into_iter().map(|line| fg(&line, &p.muted)));
        bordered(
            &content,
            width,
            None,
            &p.border,
            Some(t("Fork session")),
            None,
        )
    }
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

/// The rows the live region leaves a scrollback panel: below the composer,
/// the blank row above the panel and the status line, never more than 18.
fn live_panel_rows(live_budget: usize, composer_rows: usize, status_rows: usize) -> usize {
    live_budget
        .saturating_sub(composer_rows + 1 + status_rows)
        .min(18)
}

/// The rows a fullscreen overlay gives a panel: the terminal less the
/// composer, status row and two rows on either side, never under six unless
/// the terminal cannot hold that.
fn overlay_panel_rows(terminal_rows: usize, composer_rows: usize) -> usize {
    terminal_rows
        .saturating_sub(composer_rows + 1 + 2 + 2)
        .max(6)
        .min(terminal_rows.saturating_sub(composer_rows + 2))
        .min(OVERLAY_MAX_ROWS)
}

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
    Queue(QueuePanel),
    Export(ListPanel),
    Report(Box<report_panel::Panel>),
    Fork(ForkAsk),
    Models(ModelsPanel),
    Providers(Box<ProvidersPanel>),
    Plugins(PluginsPanel),
    Runtime(RuntimePanel),
    Management(Box<ManagementPanel>),
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
    Effort(EffortPrompt),
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
        // The `@` index, pasted paths and dropped files all resolve against
        // the local `--cwd` (cli-attachment-input), which an attach does not
        // change; the border's workspace label is the Session's and does.
        let mut composer = Composer::new(past);
        composer.set_root(PathBuf::from(&cwd_key));
        let mut app = Self {
            host,
            composer,
            workspace: String::new(),
            cwd_key,
            tool_detail: saved_tool_detail(&home),
            thinking_detail: saved_detail(&home, THINKING_DETAILS_KEY),
            default_send_mode: if crate::ui::config::read(&home, DEFAULT_SEND_MODE_KEY)
                .is_some_and(|value| value == SubmitMode::Steer.as_str())
            {
                SubmitMode::Steer
            } else {
                SubmitMode::Queue
            },
            tool_choices: HashMap::new(),
            tail_groups: Vec::new(),
            home,
            notices: Vec::new(),
            reports: Vec::new(),
            report_error: None,
            report_sequence: 0,
            banner_committed: false,
            snapshot: None,
            label: None,
            failure: None,
            failure_resumable: true,
            exit_code: None,
            failure_committed: false,
            planner: StreamPlanner::new(),
            thoughts: HashMap::new(),
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
            task_second: 0,
            task_row: false,
            session_id: String::new(),
            initial: None,
            unsent: None,
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
            reveal_pending: false,
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
            permissions_return: None,
            mouse,
            notify,
            notifications: Vec::new(),
            revealed: std::collections::HashMap::new(),
            notified_request: None,
            notified_tasks: HashSet::new(),
            runs: Runs::default(),
            palette_stash: None,
            command_entry: false,
            branch: None,
            git_head: None,
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
            compaction_targets: None,
            upgrade: None,
            line_draft: None,
            fork_draft: None,
            management_edits: Vec::new(),
            management_named: None,
            suggestion: None,
            suggestion_setting: None,
            suggestion_methods: false,
            suggestion_flip: false,
            prompt_setting: None,
            prompt_draft: None,
            pending_prompt: None,
            next_prompt_request: 0,
        };
        app.show_workspace(None);
        app
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
        self.set_draft("/");
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

    /// Whether the user's attention is free for an offer: an empty composer,
    /// no prompt or panel, no question or approval waiting and no goal
    /// driving the Session.
    fn suggestion_wanted(&self) -> bool {
        self.failure.is_none()
            && self.prompt.is_none()
            && self.composer.is_empty()
            && self.snapshot.as_ref().is_none_or(|snapshot| {
                snapshot.questions.is_empty()
                    && snapshot.approvals.is_empty()
                    && snapshot
                        .goal
                        .as_ref()
                        .is_none_or(|goal| goal.status != crate::goal::Status::Active)
            })
    }

    /// A `turn/end` with `Completed` reached this interface: ask the Session
    /// for the message the user would send next, when nothing else has the
    /// user's attention.
    fn turn_completed(&mut self, turn_id: &str) {
        if self.suggestion_wanted() {
            self.host.send(Command::Suggest {
                turn_id: turn_id.to_owned(),
            });
        }
    }

    /// Takes the offer into the draft, unsent.
    fn accept_suggestion(&mut self) {
        let Some(suggestion) = self.suggestion.take() else {
            return;
        };
        self.set_draft(&suggestion.text);
        self.host.send(Command::SuggestionRespond {
            turn_id: suggestion.turn_id,
            accept: true,
        });
    }

    /// Hides the offer and reports that it was not taken.
    fn dismiss_suggestion(&mut self) {
        let Some(suggestion) = self.suggestion.take() else {
            return;
        };
        self.host.send(Command::SuggestionRespond {
            turn_id: suggestion.turn_id,
            accept: false,
        });
    }

    /// Puts `text` in the composer as an edit, which dismisses an offer.
    fn set_draft(&mut self, text: &str) {
        self.dismiss_suggestion();
        self.composer.set_draft(text);
    }

    /// Writes the `prompt-suggestion` switch the panel flipped, keeping the
    /// length limit the last read reported.
    fn save_suggestion_flip(&mut self) {
        let Some(setting) = self.suggestion_setting.clone() else {
            return;
        };
        self.host.send(Command::SaveSuggestionSetting {
            expected_revision: setting.revision,
            enabled: !setting.document.enabled,
            max_chars: setting.document.max_chars,
        });
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

    /// Steers the oldest held message into the current run.
    fn steer(&mut self) {
        if let Some(id) = self.queued().into_iter().next() {
            self.host.send(Command::InputControl {
                input_id: id,
                action: "steer",
            });
        }
    }

    fn submits_in_alternate_mode(&self, key: &KeyEvent) -> bool {
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
        if let Some(id) = self.queued().pop() {
            self.withdraw(id);
        }
    }

    /// Withdraws every queued message, oldest first, for an interrupt. The
    /// withdrawals reach the session before its stop, so no queued message
    /// starts the next run.
    fn recall_all_queued(&mut self) {
        for id in self.queued() {
            self.host.send(Command::InputControl {
                input_id: id,
                action: "withdraw",
            });
        }
    }

    fn withdraw(&mut self, id: String) {
        if self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .queued
                .iter()
                .any(|input| input.id == id && !input.recallable)
        }) {
            self.notices.push(
                t("Messages with attachments stay queued; use /queue to steer or delete them")
                    .into(),
            );
            return;
        }
        self.host.send(Command::InputControl {
            input_id: id,
            action: "withdraw",
        });
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
            | Some(Prompt::Timeline(_))
            | Some(Prompt::Queue(_))
            | Some(Prompt::Export(_))
            | Some(Prompt::Report(_))
            | Some(Prompt::Fork(_)) => {}
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
        self.tick_at(Instant::now())
    }

    /// [`Self::tick`] at `now`, so a test crosses a poll without waiting.
    fn tick_at(&mut self, now: Instant) -> bool {
        self.sync_sessions();
        self.sync_task_details();
        let mut changed = false;
        if now.duration_since(self.git_checked) >= GIT_BRANCH_POLL {
            self.git_checked = now;
            if let Some(branch) = self.git_head.as_deref().and_then(git_branch)
                && self.branch.as_ref() != Some(&branch)
            {
                self.branch = Some(branch);
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
        // The task row names a running task's elapsed time and drops an ended
        // one after TASK_FADE, so it needs a frame on every new second while
        // it is drawn, and one more once it is not: the pulse of a running
        // turn is not enough, and a task that ends while the turn is idle
        // would otherwise stay on screen.
        if second != self.task_second {
            let shown = self.task_line_shown();
            if shown || self.task_row {
                self.task_second = second;
                self.task_row = shown;
                changed = true;
            }
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
                if self.fullscreen {
                    let running: Vec<_> = self.history.iter().enumerate().filter_map(|(at, block)| {
                        matches!(block, Block::ToolGroup(group) if group.state == crate::ui::transcript::tool_group::State::Running)
                            .then_some(self.dropped + at)
                    }).collect();
                    for id in running {
                        self.mark_rewritten(id);
                    }
                }
                changed = true;
            }
        }
        changed
    }

    /// Queued inputs, oldest first: submitted while a turn was already
    /// running, without their `input/applied` yet.
    fn queued(&self) -> Vec<String> {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .queued
                    .iter()
                    .filter(|input| input.held)
                    .map(|input| input.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Withdrawn(input) => {
                self.dismiss_suggestion();
                self.composer.restore(input);
            }
            Update::Exported { format, path } => self.notices.push(tf(
                "Exported {format}: {path}",
                &[
                    ("format", format.label()),
                    ("path", &crate::sanitize::safe(&path.display().to_string())),
                ],
            )),
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
                if let Some(Prompt::Queue(panel)) = &mut self.prompt {
                    if panel.current(&snapshot) {
                        panel.set_rows(&snapshot);
                    } else {
                        self.prompt = None;
                    }
                }
                self.runs.observe(&snapshot);
                let previous = self.snapshot.replace(*snapshot);
                self.follow_floors(previous.as_ref());
                self.apply_earlier(previous.as_ref());
                self.refresh_compactions();
                self.refresh_cards();
                self.refresh_tool_origins();
                self.sync_tool_origins();
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
                self.queue_background_notifications();
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
            Update::Switched {
                session_id,
                kind,
                cwd,
            } => {
                if !session_id.is_empty() {
                    self.unsent = None;
                }
                self.show_workspace(cwd.as_deref());
                self.switch_session(session_id, kind)
            }
            Update::Skills(entries) => {
                self.composer.set_skills(
                    entries
                        .into_iter()
                        .filter(|entry| {
                            // A name with whitespace cannot be referenced
                            // (Skills §9), so it is not offered.
                            entry.enabled
                                && entry.shadowed_by.is_none()
                                && !entry.name.chars().any(char::is_whitespace)
                        })
                        .map(|entry| crate::ui::composer::palette::Skill {
                            name: entry.name,
                            description: entry.description,
                            display_name: None,
                            display_description: None,
                        })
                        .collect(),
                );
            }
            Update::Sessions { id, paged, result } => {
                if let Some(Prompt::Sessions(panel)) = &mut self.prompt {
                    panel.page(&id, paged, result);
                }
            }
            Update::RepositoryWorkspaces(workspaces) => {
                if let Some(Prompt::Sessions(panel)) = &mut self.prompt {
                    panel.repository(workspaces);
                }
            }
            Update::Toast(text) => self.set_notice(text),
            Update::Reportable { error, show_error } => {
                let hint = t(crate::report::HINT);
                let text = if show_error {
                    format!("{}\n{hint}", crate::sanitize::safe_line(&error.message))
                } else {
                    hint.to_owned()
                };
                if show_error
                    || !matches!(error.operation, crate::report::Operation::ModelGeneration)
                {
                    self.reports.push(Block::Status {
                        text,
                        tone: Tone::Warning,
                    });
                }
                if self.session_id.is_empty()
                    && matches!(error.operation, crate::report::Operation::SessionRead)
                {
                    self.report_error = Some(error);
                }
            }
            Update::ReportReply { panel, reply } => {
                if let Some(Prompt::Report(report)) = &mut self.prompt
                    && report.id == panel
                {
                    if let Some(action) = report.reply(reply) {
                        self.host
                            .send(Command::Report(crate::report::Request { panel, action }));
                    }
                } else if let crate::report::Reply::Prepared(session, prepared) = reply {
                    self.host.send(Command::Report(crate::report::Request {
                        panel,
                        action: crate::report::Action::Cancel {
                            draft: crate::report::Draft {
                                session,
                                report_id: prepared.report_id,
                                plan_id: prepared.plan.plan_id,
                            },
                            generation: false,
                        },
                    }));
                }
            }
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
            Update::PermissionSelected(preset) => {
                let label = format!("Permission: {}", crate::permission::name(preset));
                let message = crate::ui::config::write(
                    &self.home,
                    "permission",
                    serde_json::json!(crate::permission::name(preset)),
                )
                .map_or_else(
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
                // A run is a write only when it carried a draft: a run with
                // no document, as the Scheduler's, settles none.
                let write = match &reply.request.action {
                    ManagementAction::Add { .. } | ManagementAction::Save { .. } => true,
                    ManagementAction::Run { document, .. } => document.is_some(),
                    _ => false,
                };
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
                    Some(Prompt::Management(panel)) if panel.area() == &reply.request.area
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
                                    &reply.request.area,
                                    true,
                                    &settled.draft.path().display().to_string(),
                                );
                                self.hold_draft(settled);
                            }
                            ManagementResult::Refused(ManagementRefusal::Conflict { .. }) => {
                                panel.conflicted(
                                    &reply.request.area,
                                    false,
                                    &settled.draft.path().display().to_string(),
                                );
                            }
                            ManagementResult::Refused(_) => {
                                panel.kept(
                                    &reply.request.area,
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
                                &reply.request.area,
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
            Update::Deleted(reply) => {
                let DeleteReply {
                    session_id,
                    name,
                    outcome,
                } = reply;
                let (text, tone) = match &outcome {
                    DeleteOutcome::Deleted => {
                        (tf("Deleted {name}.", &[("name", &name)]), Tone::Muted)
                    }
                    DeleteOutcome::NothingRemoved => (
                        tf(
                            "Nothing was removed: no stored Session {name}.",
                            &[("name", &name)],
                        ),
                        Tone::Warning,
                    ),
                    DeleteOutcome::Unsupported => (
                        t(crate::session_command::DELETE_UNSUPPORTED).to_owned(),
                        Tone::Warning,
                    ),
                    DeleteOutcome::ClosedNotRemoved(message) | DeleteOutcome::Failed(message) => (
                        [t("Delete failed"), " · ", message.as_str()].concat(),
                        Tone::Warning,
                    ),
                };
                if let Some(Prompt::Sessions(panel)) = &mut self.prompt {
                    panel.settled(&session_id, &outcome);
                    self.sync_sessions();
                }
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
                if let Err(error) = &models {
                    self.set_notice(error.clone());
                }
                if let Some(Prompt::Models(panel)) = &mut self.prompt {
                    panel.set_models(models);
                }
            }
            Update::EffortChoices(result) => {
                if let Some(Prompt::Effort(prompt)) = &mut self.prompt {
                    prompt.set_choices(result);
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
            Update::SuggestionMethods(offered) => {
                self.suggestion_methods = offered;
                if let Some(Prompt::Settings(panel)) = &self.prompt {
                    self.open_settings(panel.selected());
                }
            }
            Update::SuggestionSetting(result) => match result {
                Ok(setting) => {
                    self.suggestion_setting = Some(setting.clone());
                    if let Some(Prompt::Settings(panel)) = &mut self.prompt {
                        panel.set_prompt_suggestion(setting.document.enabled);
                    }
                    if std::mem::take(&mut self.suggestion_flip) {
                        self.save_suggestion_flip();
                    }
                }
                Err(error) => {
                    self.suggestion_flip = false;
                    self.suggestion_setting = None;
                    if let Some(Prompt::Settings(panel)) = &mut self.prompt {
                        panel.set_prompt_suggestion_unavailable();
                    }
                    self.set_notice(tf("Prompt suggestion: {error}", &[("error", &error)]));
                }
            },
            Update::PromptSettings {
                result,
                field,
                request,
            } => {
                let settled = request.is_some_and(|request| {
                    self.pending_prompt
                        .as_ref()
                        .is_some_and(|pending| pending.request == request)
                });
                match result {
                    Ok(setting) => {
                        self.prompt_setting = Some(setting.clone());
                        if settled {
                            if let Some(pending) = self.pending_prompt.take()
                                && let Some(draft) = pending.draft
                            {
                                draft.remove();
                            }
                            if field.is_some() {
                                self.set_notice(t("Prompt saved; applies to new Sessions"));
                            }
                        }
                        if let Some(Prompt::Settings(panel)) = &mut self.prompt {
                            panel.set_prompts(&setting);
                        }
                    }
                    Err(error) => {
                        if settled {
                            let pending = self.pending_prompt.take().expect("settled prompt");
                            if let Some(draft) = pending.draft {
                                let path = draft.path().display().to_string();
                                self.prompt_draft = Some((pending.field, draft));
                                self.set_notice(tf(
                                    "{field}: {error} Your text is kept in {path}",
                                    &[
                                        ("field", pending.field.key()),
                                        ("error", &error),
                                        ("path", &path),
                                    ],
                                ));
                            } else {
                                self.set_notice(tf(
                                    "{field}: {error}",
                                    &[("field", pending.field.key()), ("error", &error)],
                                ));
                            }
                        } else if request.is_none() {
                            if let Some(Prompt::Settings(panel)) = &mut self.prompt {
                                panel.set_prompts_unavailable();
                            }
                            self.set_notice(tf("Prompts: {error}", &[("error", &error)]));
                        }
                    }
                }
            }
            Update::TurnCompleted { turn_id } => self.turn_completed(&turn_id),
            Update::SuggestionOffer { turn_id, text } => {
                self.suggestion = Some(Suggestion { turn_id, text });
            }
            Update::SuggestionCleared { turn_id } => {
                if self
                    .suggestion
                    .as_ref()
                    .is_some_and(|suggestion| suggestion.turn_id == turn_id)
                {
                    self.suggestion = None;
                }
            }
            Update::SuggestionThrottled => {
                self.notices.push(
                    t(
                        "Suggestions are paused in this session because offers went unused. \
                       Turn them off and on in /settings to resume.",
                    )
                    .to_owned(),
                );
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
            Update::ModelDefault { folder, model } => {
                let notice = match folder {
                    Some(folder) => tf(
                        "Default for new sessions in {folder}: {model}",
                        &[("folder", &crate::ui::tilde(&folder)), ("model", &model)],
                    ),
                    None => tf("Default for new sessions: {model}", &[("model", &model)]),
                };
                self.notices.push(notice);
            }
            Update::Notice(text) => {
                self.notices.push(t(&text).to_owned());
            }
            Update::Upgrade(offer) => {
                self.upgrade = Some(UpgradeDialog::new(offer));
                self.reopen_upgrade();
            }
            Update::ApprovalPending {
                branch_id,
                request_id,
            } => {
                self.answered
                    .remove(&(branch_id.clone(), request_id.clone()));
            }
            Update::Failed(message) => {
                if let Some(Prompt::Report(report)) = &mut self.prompt {
                    report.disconnected(&message);
                }
                self.restore_unsent();
                self.failure = Some(message);
                self.run_started = None;
            }
            Update::SubmitNotSent(message) => {
                self.notices.push(message);
                self.restore_unsent();
            }
            Update::StartupFailed { message, resumable } => {
                self.prompt = None;
                self.failure = Some(message);
                self.failure_resumable = resumable;
                // A first message the invocation supplied has no Session
                // to take it: keep it in the draft and stay, rather than
                // exiting nonzero with nothing sent.
                if self.initial.is_some() || self.unsent.is_some() {
                    self.restore_unsent();
                } else {
                    self.exit_code = Some(1);
                }
            }
            Update::Shutdown { reason } => {
                self.notices.push(tf(
                    "The App Server closed this interface: {reason}",
                    &[("reason", &reason)],
                ));
                self.exit_code = Some(0);
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
            self.set_draft(&draft);
        }
        outcome
    }

    fn handle_input(&mut self, event: InputEvent) -> Outcome {
        let key = match event {
            InputEvent::Submit => return self.submit_initial(),
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
                    Some(Prompt::Report(panel)) => panel.paste(&text),
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
        if let Some(Prompt::Report(report)) = &mut self.prompt {
            let panel = report.id;
            let action = match report.key(key, std::path::Path::new(&self.cwd_key)) {
                report_panel::Input::None => None,
                report_panel::Input::Send(action) => Some(action),
                report_panel::Input::Close => {
                    if let Some(path) = report.saved_path() {
                        self.notices.push(format!(
                            "{} {}",
                            t("Saved ZIP"),
                            crate::sanitize::safe_line(&path.display().to_string())
                        ));
                    }
                    let draft = report.draft.take();
                    self.prompt = None;
                    draft.map(|draft| crate::report::Action::Cancel {
                        draft,
                        generation: false,
                    })
                }
            };
            if let Some(action) = action {
                self.host
                    .send(Command::Report(crate::report::Request { panel, action }));
            }
            return Outcome::Continue;
        }
        if self.failure.is_some() {
            if (ctrl && matches!(key.code, KeyCode::Char('c' | 'd')))
                || (key.code == KeyCode::Enter && self.composer.text().trim() == "/exit")
            {
                return Outcome::Exit(0);
            }
            if key.code == KeyCode::Enter {
                self.set_notice(if self.failure_resumable {
                    t("Disconnected; draft kept. Exit and resume this Session.")
                } else {
                    t("Cannot open this Session; draft kept.")
                });
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
                Prompt::Queue(p) => p.handle_key(key),
                Prompt::Export(p) => match p.handle_key(key) {
                    ListEvent::Pick { id: format, .. } => Effect::Export(if format == "markdown" {
                        crate::export::Format::Markdown
                    } else {
                        crate::export::Format::Json
                    }),
                    ListEvent::Close => Effect::Close,
                    ListEvent::None | ListEvent::Refresh => Effect::None,
                },
                Prompt::Fork(p) => p.handle_key(key),
                Prompt::Report(_) => unreachable!("report handles its own input"),
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
                Prompt::Effort(p) => p.handle_key(key),
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
        let (key, mode) = if self.submits_in_alternate_mode(&key) {
            (
                KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                self.default_send_mode.opposite(),
            )
        } else if self.running() {
            (key, self.default_send_mode)
        } else {
            (key, SubmitMode::Queue)
        };
        let ctrl = key.mods.contains(Modifiers::CTRL);
        if self.suggestion.is_some()
            && self.composer.is_empty()
            && self.prompt.is_none()
            && key.mods.0 == 0
        {
            let accept = match key.code {
                KeyCode::Right => true,
                KeyCode::Tab => {
                    !self.fullscreen
                        && !self.composer.palette_open()
                        && !self.composer.mention_open()
                }
                _ => false,
            };
            if accept {
                self.accept_suggestion();
                return Outcome::Continue;
            }
        }
        match key.code {
            KeyCode::BackTab | KeyCode::Tab
                if self.keyboard_free()
                    && (key.mods == Modifiers::SHIFT
                        || (key.code == KeyCode::BackTab && key.mods == Modifiers::NONE)) =>
            {
                self.host.send(Command::SetPermission(
                    crate::permission::PresetSelection::Next,
                ));
                return Outcome::Continue;
            }
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
                if self.default_send_mode == SubmitMode::Queue {
                    self.steer();
                }
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
                } else if self.suggestion.is_some() {
                    self.dismiss_suggestion();
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
            KeyCode::Enter
                if key.mods.0 == 0 && self.composer.is_empty() && self.task_line_shown() =>
            {
                self.run_command("tasks", "");
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
            ComposerOutcome::Command { name, args } => return self.run_command(&name, &args),
            outcome => {
                self.dispatch_composer(outcome, mode);
            }
        }
        if self.suggestion.is_some() && !self.composer.is_empty() {
            self.dismiss_suggestion();
        }
        Outcome::Continue
    }

    /// What a composer outcome does, the same for Enter and for the
    /// invocation's first message.
    fn dispatch_composer(&mut self, outcome: ComposerOutcome, mode: SubmitMode) -> Outcome {
        match outcome {
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

    /// Sends the invocation's first message as its first typed one: the text
    /// becomes the draft and the images its tokens, then the composer
    /// submits them through the same outcome the Enter key produces. A
    /// submission no Session takes leaves the message unsent so a refusal
    /// can put it back into the draft.
    fn submit_initial(&mut self) -> Outcome {
        let Some(initial) = self.initial.take() else {
            return Outcome::Continue;
        };
        self.composer.set_draft(&initial.text);
        for image in &initial.images {
            let _ = self.composer.attach_image(image.clone());
        }
        // A failed session has taken the connection: the message is not
        // sent, and stays a draft the user can carry to a restart.
        if self.failure.is_some() {
            return Outcome::Continue;
        }
        let outcome = self.composer.submit();
        let submitted = matches!(
            outcome,
            ComposerOutcome::Submit(_) | ComposerOutcome::SubmitDraft { .. }
        ) && self.line_draft.is_none();
        if submitted && self.session_id.is_empty() {
            self.unsent = Some(initial);
        }
        self.dispatch_composer(outcome, SubmitMode::Queue)
    }

    /// Puts a first message no Session took back into the draft, unless the
    /// user has started another one: that newer draft wins, and the loss of
    /// the first message is reported so it is not silent.
    fn restore_unsent(&mut self) {
        let Some(initial) = self.unsent.take() else {
            return;
        };
        if !self.composer.is_empty() {
            self.notices
                .push(t("The first message was not sent; your new draft was kept.").to_owned());
            return;
        }
        self.composer.set_draft(&initial.text);
        for image in initial.images {
            let _ = self.composer.attach_image(image);
        }
    }

    /// A terminal paste, which is also how a terminal delivers a file
    /// dragged onto it: local files and directories it names are inserted
    /// as `[name]` tokens, except into a shell command.
    fn paste(&mut self, text: String) {
        self.dismiss_suggestion();
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

    /// The composer's bottom-right label in at most `cells`: the workspace
    /// and its branch, or the shown line's place. A shown line outranks the
    /// workspace, which then keeps its whole form beside it: the whole
    /// place, then `⑂` alone, and only then the whole place again for
    /// [`border_line`] to drop. That last form is deliberately wider than
    /// `cells`, so a shown line is never displaced by a shortened path;
    /// [`workspace_label`] never returns one, so the form only appears here.
    fn place_label(&self, cells: usize) -> String {
        if let Some(draft) = &self.line_draft {
            let lines = self.snapshot.as_ref().map_or(&[][..], |s| &s.lines[..]);
            return tf(
                "⑂ new line from {id} · Esc cancel",
                &[("id", &line_name(lines, &draft.parent))],
            );
        }
        let branch = self.branch.as_deref();
        let Some(snapshot) = self
            .snapshot
            .as_ref()
            .filter(|s| !s.focus.is_empty() && s.focus != "main")
        else {
            return workspace_label(&self.workspace, branch, cells);
        };
        // A shown line outranks the workspace, which keeps its whole form
        // beside it: the whole place, then `⑂` alone, then the whole place
        // for the border to drop.
        let line = format!("⑂ {}", line_path(&snapshot.lines, &snapshot.focus));
        let whole = format!("{} · {line}", whole_workspace(&self.workspace, branch));
        if width_of(&whole) <= cells {
            return whole;
        }
        if width_of(&line) <= cells {
            return line;
        }
        whole
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
            self.set_draft(text);
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
            Some(_) => {
                let timeline = match self.prompt.take() {
                    Some(Prompt::Timeline(panel)) => Some(panel),
                    prompt => {
                        self.prompt = prompt;
                        None
                    }
                };
                self.open_prompt(Prompt::Fork(ForkAsk {
                    at,
                    text,
                    before,
                    create: false,
                    timeline,
                }));
            }
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
                self.prompt = match self.prompt.take() {
                    Some(Prompt::Fork(ask)) => ask.timeline.map(Prompt::Timeline),
                    _ => None,
                };
            }
            Effect::Fork => {
                if let Some(Prompt::Fork(ask)) = self.prompt.take() {
                    self.fork(ask.at, ask.text);
                }
            }
            Effect::Exit => return Outcome::Exit(130),
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
            Effect::InputControl { input_id, action } => {
                self.host.send(Command::InputControl { input_id, action });
            }
            Effect::Export(format) => {
                self.prompt = None;
                self.host.send(Command::Export(format));
            }
            Effect::FocusLine(line) => {
                self.prompt = None;
                self.host.send(Command::FocusLine(line));
            }
            Effect::Timeline { row_id, action } => {
                if action != TimelineAction::Fork {
                    self.prompt = None;
                }
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
            Effect::DeleteSession { session_id, name } => {
                self.host.send(Command::DeleteSession { session_id, name });
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
                self.permissions_return = match self.prompt.take() {
                    Some(Prompt::Approvals(panel)) => Some(panel),
                    prompt => {
                        self.prompt = prompt;
                        None
                    }
                };
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
                    && matches!(self.prompt, Some(Prompt::Approvals(_)));
                if from_permissions && let Some(Prompt::Approvals(panel)) = self.prompt.take() {
                    self.permissions_return = Some(panel);
                }
                let settings_return = self.settings_return;
                self.setting(setting);
                if from_permissions {
                    self.settings_return = settings_return;
                }
            }
            Effect::EditPrompt(field) => {
                if self.pending_prompt.is_some() {
                    self.set_notice(t("A prompt save is still in progress"));
                    return Outcome::Continue;
                }
                let Some(setting) = &self.prompt_setting else {
                    self.set_notice(t("Prompt settings are not available"));
                    return Outcome::Continue;
                };
                let text = setting.text(field).unwrap_or_default().to_owned();
                let revision = setting.revision.clone();
                return Outcome::EditPrompt {
                    field,
                    text,
                    revision,
                };
            }
            Effect::ResetPrompt(field) => {
                if self.pending_prompt.is_some() {
                    self.set_notice(t("A prompt save is still in progress"));
                    return Outcome::Continue;
                }
                let Some(setting) = &self.prompt_setting else {
                    self.set_notice(t("Prompt settings are not available"));
                    return Outcome::Continue;
                };
                let revision = setting.revision.clone();
                let old = setting.text(field).map(str::to_owned);
                if let Some(text) = old {
                    match editor::Draft::open_text(&self.home, field.key(), "md", &text) {
                        Ok(draft) => {
                            let path = draft.path().display().to_string();
                            self.set_notice(tf(
                                "Resetting {field}; the old text is kept in {path}",
                                &[("field", field.key()), ("path", &path)],
                            ));
                        }
                        Err(error) => {
                            self.set_notice(error);
                            return Outcome::Continue;
                        }
                    }
                }
                let request = self.next_prompt_request;
                self.next_prompt_request = self.next_prompt_request.wrapping_add(1);
                self.pending_prompt = Some(PendingPrompt {
                    request,
                    field,
                    draft: None,
                });
                self.host.send(Command::ResetPromptSetting {
                    field,
                    revision,
                    request,
                });
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
            Effect::SetPermission(preset) => {
                self.prompt = None;
                self.host.send(Command::SetPermission(
                    crate::permission::PresetSelection::Preset(preset),
                ));
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
            Effect::EditManagement {
                area,
                target,
                text,
                seed,
            } => {
                return Outcome::Edit {
                    area,
                    target,
                    text,
                    seed,
                };
            }
            Effect::OauthStart(start) => self.host.send(Command::OauthStart(start)),
            Effect::OauthCancel => self.host.send(Command::OauthCancel),
            Effect::OauthBack => {}
            Effect::OauthPaste(url) => self.host.send(Command::OauthPaste(url)),
            Effect::RefreshAccountModels {
                provider_id,
                attempt,
            } => self.host.send(Command::RefreshAccountModels {
                provider_id,
                attempt,
            }),
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
                label,
            } => {
                self.prompt = None;
                self.host.send(Command::SelectModel {
                    provider_id,
                    model_id,
                    reasoning_effort,
                    label,
                });
            }
            Effect::SetEffort { model, value } => {
                self.prompt = None;
                if self
                    .label
                    .as_ref()
                    .is_some_and(|label| label.model == model)
                {
                    self.host
                        .send(Command::Configure(format!("/effort {value}")));
                } else {
                    self.set_notice(t(
                        "Model changed; reopen /effort to choose its reasoning effort.",
                    ));
                }
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
        if self.prompt.is_none()
            && let Some(mut panel) = self.permissions_return.take()
        {
            panel.set_policy(&self.permission_policy);
            self.open_prompt(Prompt::Approvals(panel));
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

    fn sync_compaction_details(&mut self) {
        use crate::model::view::CompactionTarget;
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut targets = std::collections::BTreeSet::new();
        if !self.awaiting_window && snapshot.history_complete {
            for block in &self.history {
                if let Block::Compaction {
                    expanded: true,
                    target: Some(target),
                    ..
                } = block
                {
                    targets.insert(target.clone());
                }
            }
            if self.tool_detail == Detail::Expanded
                && let Some(side) = &snapshot.side
            {
                for row in &side.rows {
                    if let Some(detail) = row
                        .compaction
                        .as_ref()
                        .and_then(|view| view.detail.as_ref())
                    {
                        targets.insert(CompactionTarget {
                            session_id: snapshot.session_id.clone(),
                            branch_id: side.id.clone(),
                            event_id: detail.event_id.clone(),
                            sequence: detail.sequence,
                        });
                    }
                }
            }
            if let Some(Prompt::Tasks(panel)) = &self.prompt {
                targets.extend(panel.compaction_targets());
            }
        }
        if targets.is_empty() && self.compaction_targets.is_none() {
            return;
        }
        let next = (snapshot.projection_id, targets);
        if self.compaction_targets.as_ref() != Some(&next) {
            self.host.send(Command::CompactionDetails {
                projection_id: next.0,
                targets: next.1.clone(),
            });
            self.compaction_targets = (!next.1.is_empty()).then_some(next);
        }
    }

    fn sync_tool_origins(&self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        for (index, row) in snapshot.rows.iter().enumerate() {
            if row.kind != RowKind::ToolCall || !opens_group(&snapshot.rows, index) {
                continue;
            }
            let rows = group_rows(&snapshot.rows, index);
            if rows.iter().all(|call| {
                call.pending
                    || call.tool_lookup_done
                    || call.tool_name.is_some() && call.tool_input.is_some()
            }) {
                continue;
            }
            let group = self.fullscreen.then(|| {
                project_fullscreen_group(
                    &rows,
                    self.tool_detail,
                    self.tool_choices.get(&self.tool_scope()),
                )
            });
            if group.is_none() && self.tool_detail != Detail::Expanded {
                continue;
            }
            let single_root = rows
                .iter()
                .filter(|call| call.parent_call_id.is_none())
                .count()
                == 1;
            for call in rows {
                if call.pending
                    || call.tool_lookup_done
                    || call.tool_name.is_some() && call.tool_input.is_some()
                {
                    continue;
                }
                if let Some(group) = &group {
                    let node = if single_root
                        && call.parent_call_id.is_none()
                        && call.tool_name.as_deref() == Some("codemode")
                    {
                        group.id.clone()
                    } else {
                        format!("call:{}", call.row_id)
                    };
                    if !group.is_open(&node) {
                        continue;
                    }
                }
                if let (Some(request_id), Some(call_id)) = (&call.batch, &call.call_id) {
                    self.host.send(Command::ToolOrigin {
                        session_id: snapshot.session_id.clone(),
                        projection_id: snapshot.projection_id,
                        branch_id: snapshot.focus.clone(),
                        request_id: request_id.clone(),
                        call_id: call_id.clone(),
                    });
                }
            }
        }
    }

    fn refresh_tool_origins(&mut self) {
        let choices = self.tool_choices.get(&self.tool_scope());
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut changed = Vec::new();
        for (index, block) in self.history.iter_mut().enumerate() {
            let Block::ToolGroup(group) = block else {
                continue;
            };
            if !group.awaiting_origins {
                continue;
            }
            let Some(source) = snapshot
                .rows
                .iter()
                .position(|row| row.row_id == group.source)
            else {
                continue;
            };
            let rows = group_rows(&snapshot.rows, source);
            if rows
                .iter()
                .filter(|row| {
                    row.tool_lookup_done || row.tool_name.is_some() && row.tool_input.is_some()
                })
                .count()
                != group.known_origins
            {
                *group = if self.fullscreen {
                    project_fullscreen_group(&rows, self.tool_detail, choices)
                } else {
                    project_group(&rows, group.detail)
                };
                changed.push(index + self.dropped);
            }
        }
        if !changed.is_empty() {
            for index in changed {
                self.mark_rewritten(index);
            }
            self.request_reflow();
        }
    }

    fn release_compaction_details(&mut self) {
        if let Some((projection_id, _)) = self.compaction_targets.take() {
            self.host.send(Command::CompactionDetails {
                projection_id,
                targets: Default::default(),
            });
        }
        for block in &mut self.history {
            if let Block::Compaction {
                target,
                detail: Some(detail),
                text,
                ..
            } = block
            {
                *target = None;
                *text = "".into();
                detail.state = crate::model::view::CompactionDetailState::Failed(
                    "This attachment is no longer open".into(),
                );
            }
        }
    }

    fn refresh_compactions(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut changed = Vec::new();
        for (index, block) in self.history.iter_mut().enumerate() {
            let Block::Compaction {
                target: Some(target),
                text,
                detail,
                expanded,
                ..
            } = block
            else {
                continue;
            };
            if target.session_id != snapshot.session_id || target.branch_id != snapshot.focus {
                continue;
            }
            let view = snapshot
                .rows
                .iter()
                .filter_map(|row| row.compaction.as_ref())
                .find(|view| {
                    view.detail.as_ref().is_some_and(|at| {
                        at.event_id == target.event_id && at.sequence == target.sequence
                    })
                });
            let Some(view) = view else { continue };
            if *detail != view.detail || (*expanded && !std::sync::Arc::ptr_eq(text, &view.text)) {
                *detail = view.detail.clone();
                if *expanded {
                    *text = view.text.clone();
                }
                changed.push(self.dropped + index);
            }
        }
        if !changed.is_empty() {
            for index in changed {
                self.mark_rewritten(index);
            }
            self.request_reflow();
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

    /// Reports each background task the live stream ended, once per task: a
    /// redraw or a return to the Session does not report it again, and a
    /// finish a read replayed is history, not news.
    fn queue_background_notifications(&mut self) {
        use crate::ui::notify::Event;
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let ended: Vec<(String, String, TaskOutcome)> = snapshot
            .tasks
            .iter()
            .filter_map(|task| {
                let finish = task.finished.as_ref()?;
                (!finish.replayed).then(|| {
                    (
                        task.id.clone(),
                        crate::sanitize::safe(&task.label),
                        finish.outcome,
                    )
                })
            })
            .collect();
        for (id, label, outcome) in ended {
            if !self.notified_tasks.insert(id) {
                continue;
            }
            if self.notify {
                self.notifications
                    .push(Event::BackgroundFinished { label, outcome });
            }
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

    /// The window title's topic: the Session's stored title when it has one,
    /// else its first message on `main` (`ui::title`), whichever line is
    /// shown.
    fn topic(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        let topic = snapshot
            .session_title
            .as_deref()
            .or(snapshot.main_topic.as_deref())?;
        crate::ui::title::topic(topic)
    }

    /// Opens `/settings`: the panel, or a second-level switch it carries.
    fn settings(&mut self, args: &str) {
        let mut words = args.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (None, _, _) => {
                if self.suggestion_methods {
                    self.host.send(Command::ReadSuggestionSetting);
                }
                self.host.send(Command::ReadPromptSettings);
                self.open_settings(Setting::Theme);
            }
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
            default_send_mode: self.default_send_mode,
            tool_detail: self.tool_detail,
            thinking_expanded: self.thinking_detail == Detail::Expanded,
            mouse: self.mouse,
            notify: self.notify,
            acp_sessions: crate::ui::config::saved_show_acp(&self.home),
            prompt_suggestion_supported: self.suggestion_methods,
            prompt_suggestion: self
                .suggestion_setting
                .as_ref()
                .map(|setting| setting.document.enabled),
            prompts: self.prompt_origins(),
            permission: crate::permission::name(self.permission_policy.preset).to_owned(),
        };
        self.open_prompt(Prompt::Settings(SettingsPanel::new(&values, select)));
    }

    /// Each prompt text's origin, as the last read or write reported it.
    fn prompt_origins(&self) -> [Option<bool>; 3] {
        PromptField::ALL.map(|field| {
            self.prompt_setting
                .as_ref()
                .map(|setting| setting.text(field).is_some())
        })
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
            Setting::BasePrompt | Setting::SummaryPrompt | Setting::SkillsPrompt => {
                self.settings_return = None;
            }
            Setting::Permission => {
                let panel = ApprovalsPanel::new(&self.permission_policy);
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
            Setting::DefaultSendMode => {
                let mode = self.default_send_mode.opposite();
                if let Some(error) = crate::ui::config::write(
                    &self.home,
                    DEFAULT_SEND_MODE_KEY,
                    mode.as_str().into(),
                ) {
                    self.set_notice(error);
                } else {
                    self.default_send_mode = mode;
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
            Setting::ThinkingDetails => {
                self.thinking_detail = self.thinking_detail.flipped();
                let expanded = self.thinking_detail == Detail::Expanded;
                for block in &mut self.history {
                    if let Block::Thinking {
                        expanded: value, ..
                    } = block
                    {
                        *value = expanded;
                    }
                }
                self.request_reflow();
                if let Some(error) =
                    crate::ui::config::write(&self.home, THINKING_DETAILS_KEY, expanded.into())
                {
                    self.set_notice(tf(
                        "Thinking details not saved: {error}",
                        &[("error", &error)],
                    ));
                }
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
            Setting::PromptSuggestion => {
                self.settings_return = None;
                if self.suggestion_setting.is_some() {
                    self.save_suggestion_flip();
                } else {
                    self.suggestion_flip = true;
                    self.host.send(Command::ReadSuggestionSetting);
                }
                self.open_settings(setting);
            }
        }
    }

    fn toggle_tool_details(&mut self) {
        let detail = self.tool_detail.next();
        self.tool_detail = detail;
        for block in &mut self.history {
            match block {
                Block::ToolGroup(group) => group.detail = detail,
                Block::Compaction {
                    expanded,
                    text,
                    detail: lazy,
                    ..
                } => {
                    *expanded = detail == Detail::Expanded;
                    if !*expanded && lazy.is_some() {
                        *text = "".into();
                    }
                }
                Block::Hook { expanded, .. } => *expanded = detail == Detail::Expanded,
                Block::Error { expanded, .. } => *expanded = detail != Detail::Collapsed,
                _ => {}
            }
        }
        self.refresh_tool_choices();
        self.request_reflow();
        self.sync_compaction_details();
        self.sync_tool_origins();
        if let Some(error) =
            crate::ui::config::write(&self.home, TOOL_DETAILS_KEY, detail.label().into())
        {
            self.set_notice(tf("Tool details not saved: {error}", &[("error", &error)]));
        }
    }

    /// The draft a refused write kept for `target`, if its text is still
    /// there: the editor reopens it as it is, so an edit that continues
    /// keeps working in its own file, at the revision that text came from.
    /// Another target's kept draft is none of this target's business.
    fn kept_draft(
        &mut self,
        area: &Area,
        target: &WriteTarget,
    ) -> Option<(WriteTarget, editor::Draft, Option<serde_json::Value>)> {
        let at = self
            .management_edits
            .iter()
            .position(|kept| kept.area == *area && kept.target.same_target(target))?;
        let kept = self.management_edits.remove(at);
        if kept.draft.text().is_err() {
            self.management_edits.push(kept);
            return None;
        }
        Some((kept.target, kept.draft, kept.seed))
    }

    /// Records the label the open panel shows for the entry `request` names,
    /// which the notice owes the user when the panel closes before the reply.
    fn remember_named(&mut self, request: &ManagementRequest) {
        let Some(Prompt::Management(panel)) = &self.prompt else {
            return;
        };
        self.management_named = Some((
            request.area.clone(),
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
        seed: Option<serde_json::Value>,
        message: String,
    ) {
        self.failed(message);
        self.hold_draft(ManagementDraft {
            area,
            target,
            draft,
            seed,
        });
    }

    /// What the external editor left: a non-zero exit and an unreadable or
    /// unparseable draft write nothing and keep their file, the unchanged
    /// document writes nothing, and a document is written with the revision
    /// the panel read, or created when the draft is a new entry.
    fn write_draft(
        &mut self,
        kept: ManagementDraft,
        reopened: bool,
        program: String,
        result: std::io::Result<std::process::ExitStatus>,
    ) {
        let ManagementDraft {
            area,
            target,
            draft,
            seed,
        } = kept;
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
                    seed,
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
                        seed,
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
                    panel.kept(&area, &path);
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
                seed,
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
                        source: None,
                        expected_revision: revision.clone(),
                        document,
                    },
                    WriteTarget::Entry {
                        id,
                        source,
                        revision,
                    } => ManagementAction::Save {
                        id: Some(id.clone()),
                        source: source.clone(),
                        expected_revision: revision.clone(),
                        document,
                    },
                    WriteTarget::Run { id, source } => ManagementAction::Run {
                        id: id.clone(),
                        source: source.clone(),
                        document: Some(document),
                        seed: seed.clone(),
                    },
                };
                self.hold_draft(ManagementDraft {
                    area: area.clone(),
                    target,
                    draft,
                    seed,
                });
                let request = ManagementRequest { area, action };
                self.remember_named(&request);
                self.host.send(Command::Management(request));
            }
        }
    }

    /// `/<area>` opens the management panel of `area`, which reads it.
    fn open_management(&mut self, area: Area, note: Option<String>) {
        self.open_prompt(Prompt::Management(Box::new(ManagementPanel::new(
            area.clone(),
            note,
        ))));
        self.host.send(Command::Management(ManagementRequest {
            area,
            action: ManagementAction::List,
        }));
    }

    /// `/workflow:save` opens the panel in its create editor: the first view
    /// opens the editor on the create document.
    fn open_management_create(&mut self, area: Area) {
        self.open_prompt(Prompt::Management(Box::new(
            ManagementPanel::opening_create(area.clone()),
        )));
        self.host.send(Command::Management(ManagementRequest {
            area,
            action: ManagementAction::List,
        }));
    }

    /// The editor a management panel asked for without a key, for the event
    /// loop to run once the reply that asked is applied.
    fn take_management_effect(&mut self) -> Option<Effect> {
        match &mut self.prompt {
            Some(Prompt::Management(panel)) => panel.take_effect(),
            _ => None,
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
            "earlier" => {
                if self.fullscreen {
                    self.show_earlier();
                } else {
                    self.host.send(Command::Earlier);
                }
                Outcome::Continue
            }
            "export" => {
                let mut panel = ListPanel::new(
                    "Export session",
                    t("Enter export · Esc cancel"),
                    "",
                    "",
                    "",
                    false,
                )
                .without_filter();
                panel.set_items(
                    Ok(vec![
                        Item::new("markdown", "Markdown (.md)", ""),
                        Item::new("json", "JSON (.json)", ""),
                    ]),
                    Some("markdown"),
                );
                self.open_prompt(Prompt::Export(panel));
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
            "loop" => {
                match crate::loop_view::request(args) {
                    Some(request) => self.host.send(Command::Loop(request)),
                    None => self.reports.push(Block::Status {
                        text: crate::loop_view::USAGE.to_owned(),
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
                if name == "effort" && args.trim().is_empty() {
                    self.open_prompt(Prompt::Effort(EffortPrompt::new()));
                }
                let text = format!("/{name} {args}");
                self.host
                    .send(Command::Configure(text.trim_end().to_owned()));
                Outcome::Continue
            }
            "report" => {
                if !args.trim().is_empty() {
                    self.set_notice(t("Usage: /report"));
                    return Outcome::Continue;
                }
                let target = if !self.session_id.is_empty() {
                    crate::report::Target::Session(self.session_id.clone())
                } else if let Some(error) = self.report_error.clone() {
                    crate::report::Target::Error(error)
                } else {
                    self.set_notice(t("No session to report."));
                    return Outcome::Continue;
                };
                self.report_sequence += 1;
                let panel = self.report_sequence;
                self.open_prompt(Prompt::Report(Box::new(report_panel::Panel::new(
                    panel,
                    target.clone(),
                ))));
                self.host.send(Command::Report(crate::report::Request {
                    panel,
                    action: crate::report::Action::Prepare(target),
                }));
                Outcome::Continue
            }
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
                self.host.send(Command::FindRepository);
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
            "queue" => {
                if let Some(snapshot) = &self.snapshot {
                    let panel = QueuePanel::new(snapshot);
                    self.open_prompt(Prompt::Queue(panel));
                }
                Outcome::Continue
            }
            "branch" if args.is_empty() => {
                self.set_draft("/branch ");
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
            "workflow" => {
                self.open_management(Area::Workflow, None);
                Outcome::Continue
            }
            "workflow:save" => {
                self.open_management_create(Area::Workflow);
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
                let rows =
                    crate::ui::keys::report(self.fullscreen, self.mouse, self.default_send_mode);
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
            let text = banner(
                cols,
                &self.palette,
                self.branch.as_deref().unwrap_or(t("Not available")),
            );
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
                let text = chunk(&render_block(
                    &block,
                    width,
                    &self.palette,
                    &self.markdown,
                    self.fullscreen,
                ));
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
                Placement::Thought {
                    id,
                    mut stream,
                    finished,
                } => {
                    if let Some(index) = stream.block.and_then(|id| id.checked_sub(self.dropped)) {
                        self.history[index] = source;
                    } else {
                        stream.block = Some(self.dropped + self.history.len());
                        self.record(source);
                    }
                    if finished {
                        self.thoughts.remove(&id);
                    } else {
                        self.thoughts.insert(id, stream);
                    }
                }
            }
            if !text.is_empty() {
                frame.commits.push(text);
            }
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
            let text = chunk(&render_block(
                &block,
                width,
                &self.palette,
                &self.markdown,
                self.fullscreen,
            ));
            self.record(block);
            frame.commits.push(text);
        }
        for run in std::mem::take(&mut self.shell_runs) {
            let block = Block::Shell(run);
            let text = chunk(&render_block(
                &block,
                width,
                &self.palette,
                &self.markdown,
                self.fullscreen,
            ));
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
            // A Session the daemon refuses for its protocol version is refused
            // again by every run of this client, so no command is offered.
            let status = if self.failure_resumable {
                let resume = if self.session_id.is_empty() {
                    "dimcode".to_owned()
                } else {
                    format!("dimcode --resume {}", self.session_id)
                };
                Block::Status {
                    text: tf(
                        "Disconnected; draft kept. Exit and run: {command}",
                        &[("command", &resume)],
                    ),
                    tone: Tone::Muted,
                }
            } else {
                Block::Status {
                    text: t("Cannot open this Session; draft kept.").to_owned(),
                    tone: Tone::Muted,
                }
            };
            frame.commits.push(chunk(&render_block(
                &status,
                width,
                &self.palette,
                &self.markdown,
                self.fullscreen,
            )));
            self.record(status);
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
                .filter(|input| input.held)
                .map(|input| {
                    (
                        input.id.clone(),
                        format!(
                            "{} · {}",
                            t("Held"),
                            input.display.as_deref().unwrap_or(&input.text)
                        ),
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
                let key = match self.default_send_mode {
                    SubmitMode::Queue => "Ctrl+S",
                    SubmitMode::Steer => "Enter",
                };
                let hint = tf(QUEUE_HINT, &[("key", key)]);
                live.extend(queue_strip(
                    &queued,
                    self.queue_rows,
                    cols,
                    &self.palette,
                    &hint,
                ));
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
                    self.thinking_detail,
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
        // The tasks running apart from the turn sit on one row above the
        // composer, in both interfaces; a task that just ended keeps its place
        // for TASK_FADE seconds so it does not vanish from under the eye. No
        // task draws no row, so an idle Session keeps the space. It goes above
        // the anchor, so the row and the executor it names give their space
        // back rather than moving the composer up.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if self.prompt.is_none()
            && let Some(snapshot) = &self.snapshot
            && let Some(line) =
                task_line(shown_tasks(&snapshot.tasks, now), now, width, &self.palette)
        {
            live.push(format!("{}{line}", " ".repeat(MARGIN)));
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
        let status_rows = 1;
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
            let started = self.run_started.unwrap_or_else(Instant::now);
            let elapsed = started.elapsed();
            let beat = (elapsed.as_millis() / PULSE.as_millis()) as usize;
            let retry = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.retry.as_ref());
            if let Some(retry) = retry {
                let seconds = crate::projection::retry_seconds(retry.retry_at_ms);
                let reason = crate::projection::failure_phrase(retry.kind, retry.reason.as_deref());
                if seconds == 0 {
                    return format!(
                        "{} {}",
                        PULSE_FRAMES[beat % PULSE_FRAMES.len()],
                        tf(
                            "Waiting for the provider · {reason} · Esc stops",
                            &[("reason", &reason)]
                        )
                    );
                }
                return format!(
                    "{} {}",
                    PULSE_FRAMES[beat % PULSE_FRAMES.len()],
                    tf(
                        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops",
                        &[
                            ("attempt", &retry.attempt.to_string()),
                            ("max", &retry.max_attempts.to_string()),
                            ("seconds", &seconds.to_string()),
                            ("reason", &reason),
                        ]
                    )
                );
            }
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
                PULSE_FRAMES[beat % PULSE_FRAMES.len()],
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
            let offer = self
                .suggestion
                .as_ref()
                .filter(|_| shell.is_none() && self.prompt.is_none() && !palette_overlay)
                .filter(|_| cols >= 20)
                .map(|suggestion| truncate_cells(&suggestion.text, text_width))
                .filter(|text| !text.is_empty());
            let placeholder = match offer {
                Some(text) => text,
                None if shell.is_some() => t(SHELL_PLACEHOLDER).to_owned(),
                None => t(COMPOSER_PLACEHOLDER).to_owned(),
            };
            rows.push(format!(
                "{}{}",
                fg(prompt, prompt_color),
                fg(&placeholder, &p.muted)
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
            let place = self.composer_place(docked.as_deref(), outer);
            let last = composer_box.len() - 1;
            composer_box[0] = format!(
                "{margin}{}",
                border_line(("╭", "╮"), title.as_deref(), model.as_deref(), outer, &edge)
            );
            composer_box[last] = format!(
                "{margin}{}",
                border_line(
                    ("╰", "╯"),
                    docked.as_deref(),
                    place.as_deref(),
                    outer,
                    &edge
                )
            );
        } else {
            let outer = cols.saturating_sub(2 + MARGIN).max(4);
            let margin = " ".repeat(MARGIN);
            let place = self.composer_place(None, outer);
            let last = composer_box.len() - 1;
            composer_box[last] = format!(
                "{margin}{}",
                border_line(("╰", "╯"), None, place.as_deref(), outer, &edge)
            );
        }
        let replaced = !command_entry
            && matches!(
                self.prompt,
                Some(
                    Prompt::Permission(_)
                        | Prompt::Ask(_)
                        | Prompt::Effort(_)
                        | Prompt::Exit(_)
                        | Prompt::Upgrade(_)
                        | Prompt::Fork(_)
                        | Prompt::Timeline(_)
                        | Prompt::Queue(_)
                        | Prompt::Export(_)
                        | Prompt::Report(_)
                )
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
            live_panel_rows(live_budget, composer_rows, status_rows)
        };
        let panel_cursor = |width: usize, height: usize| match &self.prompt {
            Some(Prompt::Sessions(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Tasks(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Lines(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Timeline(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Queue(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Export(prompt)) => Some(prompt.cursor(width)),
            Some(Prompt::Report(prompt)) => Some(prompt.cursor()),
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
                overlay_panel_rows(size.1 as usize, composer_rows)
            };
            let list_width = if native_selection {
                cols
            } else {
                cols.saturating_sub(4).min(OVERLAY_LIST_WIDTH)
            };
            frame.overlay = match &self.prompt {
                _ if palette_overlay => {
                    let width = cols.saturating_sub(4).min(OVERLAY_PALETTE_WIDTH);
                    self.composer.palette_overlay(width, budget, p)
                }
                prompt => {
                    let rows = match prompt {
                        Some(Prompt::Sessions(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Tasks(prompt)) => {
                            Some(prompt.render(list_width, budget, p, &self.markdown))
                        }
                        Some(Prompt::Lines(prompt)) => {
                            Some(prompt.render(list_width, budget, p, &self.markdown))
                        }
                        Some(Prompt::Timeline(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Queue(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Export(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Report(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Fork(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Models(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Providers(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Plugins(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Runtime(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Management(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Login(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Approvals(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Theme(prompt)) => Some(prompt.render(list_width, budget, p)),
                        Some(Prompt::Language(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Settings(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        Some(Prompt::Isolation(prompt)) => {
                            Some(prompt.render(list_width, budget, p))
                        }
                        _ => None,
                    };
                    // A panel's cursor follows the rows it just drew, so it
                    // is read after the render.
                    let cursor = panel_cursor(list_width, budget)
                        .flatten()
                        .map(|(row, column)| (row as u16, column as u16));
                    rows.map(|rows| Overlay { rows, cursor })
                }
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
            Some(Prompt::Queue(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Export(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Report(prompt)) => Some(prompt.render(outer, panel_rows, p)),
            Some(Prompt::Fork(prompt)) => Some(prompt.render(outer, panel_rows, p)),
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
            Some(Prompt::Effort(prompt)) => Some(prompt.render(outer, panel_rows, p)),
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
        live.push(self.status_line(cols));
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
        self.sync_compaction_details();
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

    fn thought_frames(&mut self, width: usize) -> HashMap<String, ThoughtFrame> {
        let mut frames = HashMap::new();
        let Some(snapshot) = &self.snapshot else {
            return frames;
        };
        if self.fullscreen {
            return frames;
        }
        let expanded = self.thinking_detail == Detail::Expanded;
        for row in snapshot.rows.iter().skip(self.window_floor) {
            if row.kind != RowKind::Thinking || row.text.trim().is_empty() {
                continue;
            }
            if let Some(previous) = &row.provisional_id
                && let Some(stream) = self.thoughts.remove(previous)
            {
                self.thoughts.insert(row.row_id.clone(), stream);
            }
            if !(row.streaming && expanded) && !self.thoughts.contains_key(&row.row_id) {
                continue;
            }
            if row.streaming && !expanded {
                continue;
            }
            let mut stream = *self.thoughts.entry(row.row_id.clone()).or_default();
            let text = crate::sanitize::safe(&row.text).replace("**", "");
            let source_text = if row.streaming {
                thinking_prefix(&text, width)
            } else {
                text.clone()
            };
            let elapsed_ms = if row.streaming { None } else { row.elapsed_ms };
            let source = Block::Thinking {
                text: source_text.clone(),
                elapsed_ms,
                expanded,
                streamed: true,
            };
            let rows = if expanded {
                streamed_thinking(&text, elapsed_ms, width, &self.palette)
            } else {
                render_block(&source, width, &self.palette, &self.markdown, false)
            };
            let stable = if row.streaming && source_text.is_empty() {
                0
            } else {
                render_block(&source, width, &self.palette, &self.markdown, false).len()
            };
            let continues = stream.written > 0;
            let new = &rows[stream.written.min(stable)..stable];
            let commit = if new.is_empty() {
                String::new()
            } else if continues {
                pad_block(new)
            } else {
                chunk(new)
            };
            stream.written = stable;
            frames.insert(
                row.row_id.clone(),
                ThoughtFrame {
                    stream,
                    commit,
                    source,
                    tail: rows.into_iter().skip(stable).collect(),
                },
            );
        }
        self.thoughts.retain(|id, _| {
            snapshot
                .rows
                .iter()
                .any(|row| row.row_id == *id && !row.text.trim().is_empty())
        });
        frames
    }

    /// Commits owed this frame and the live rows (unclosed streaming tail,
    /// the reasoning preview and running tools, in transcript order).
    #[allow(clippy::type_complexity)]
    fn transcript(&mut self, width: usize) -> (Vec<String>, Vec<(Block, Placement)>, Vec<String>) {
        if !self.arm_window() {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let thought_frames = self.thought_frames(width);
        let Some(snapshot) = &self.snapshot else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        let p = &self.palette;

        let is_message = |row: &ViewRow| row.kind.is_message();
        let floor = self.window_floor;

        enum LiveItem {
            Text(String),
            Thinking(usize),
            Tool(usize),
            Row(usize),
        }
        let mut live_text: Vec<TextBlock> = Vec::new();
        let mut live_items: Vec<LiveItem> = Vec::new();
        let mut settled_assistant: Vec<TextBlock> = Vec::new();
        let mut settled: Vec<(usize, String)> = Vec::new();
        let mut pending_messages: Vec<(usize, String)> = Vec::new();
        // The hook rows that ride in the tail after the message they belong
        // to, as `(row, id, parent id)`.
        let mut tail_hooks: Vec<(usize, String, String)> = Vec::new();
        // The messages this pass put in the tail, which their hook rows follow.
        let mut tail_messages: HashSet<String> = HashSet::new();
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
                    live_items.push(LiveItem::Thinking(index));
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
                    tail_messages.insert(row.row_id.clone());
                    pending_messages.push((index, row.row_id.clone()));
                }
                // A hook's context belongs to its message's group: while the
                // message waits in the tail, so does its context, directly
                // after it. It is never a settlement barrier of its own.
                RowKind::HookContext
                    if row
                        .parent_row
                        .as_deref()
                        .is_some_and(|parent| tail_messages.contains(parent)) =>
                {
                    live_items.push(LiveItem::Row(index));
                    tail_hooks.push((
                        index,
                        row.row_id.clone(),
                        row.parent_row.clone().expect("a hook row names its parent"),
                    ));
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
        // committed row cannot take the hint back (timeline UI §4.8). Its
        // hook rows commit with it, in content order, and never without it.
        let mut forced: HashSet<usize> = HashSet::new();
        let mut forced_rows: HashSet<String> = HashSet::new();
        if let Some(last) = settled.last().map(|(index, _)| *index) {
            for (index, id) in pending_messages.iter().filter(|(index, _)| *index < last) {
                settled.push((*index, id.clone()));
                forced.insert(*index);
                forced_rows.insert(id.clone());
            }
            for (index, id, parent) in &tail_hooks {
                if forced_rows.contains(parent) {
                    settled.push((*index, id.clone()));
                    forced.insert(*index);
                }
            }
        }
        live_items.retain(|item| !matches!(item, LiveItem::Row(at) if forced.contains(at)));
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
        let global = self.tool_detail;
        let choices = self.tool_choices.get(&self.tool_scope());
        let detail_of = |index: usize| {
            if snapshot.rows[index].kind == RowKind::Thinking {
                return self.thinking_detail;
            }
            global
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
            if let Some(thought) = thought_frames.get(&snapshot.rows[index].row_id) {
                return (
                    thought.commit.clone(),
                    Some((
                        thought.source.clone(),
                        Placement::Thought {
                            id: snapshot.rows[index].row_id.clone(),
                            stream: thought.stream,
                            finished: true,
                        },
                    )),
                );
            }
            match (snapshot.rows[index].kind, finalized) {
                (RowKind::AssistantMessage, Some(batch)) if batch.lines.is_empty() => {
                    (String::new(), None)
                }
                (RowKind::AssistantMessage, Some(batch)) => streamed(batch),
                _ => {
                    let mut block = project_row(&snapshot.rows, index, detail_of(index));
                    if self.fullscreen && snapshot.rows[index].kind == RowKind::ToolCall {
                        block = Block::ToolGroup(project_fullscreen_group(
                            &group_rows(&snapshot.rows, index),
                            global,
                            choices,
                        ));
                    }
                    if forced.contains(&index)
                        && let Block::User { pending, .. } = &mut block
                    {
                        *pending = false;
                    }
                    let lines = render_block(&block, width, p, markdown, self.fullscreen);
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
                LiveItem::Thinking(index) => {
                    let row = &snapshot.rows[*index];
                    if let Some(thought) = thought_frames.get(&row.row_id) {
                        if !thought.commit.is_empty() {
                            commits.push(thought.commit.clone());
                            sources.push((
                                thought.source.clone(),
                                Placement::Thought {
                                    id: row.row_id.clone(),
                                    stream: thought.stream,
                                    finished: false,
                                },
                            ));
                        }
                        if thought.stream.written > 0 {
                            tail.extend(thought.tail.iter().map(|r| format!("{pad}{r}")));
                            continue;
                        }
                        thought.tail.clone()
                    } else {
                        live_thinking(
                            &row.text,
                            self.thinking_detail == Detail::Expanded,
                            width,
                            p,
                        )
                    }
                }
                LiveItem::Tool(index) => {
                    let rows = group_rows(&snapshot.rows, *index);
                    let group = if self.fullscreen {
                        project_fullscreen_group(&rows, global, choices)
                    } else {
                        project_group(&rows, global)
                    };
                    let activity = if self.fullscreen {
                        PULSE_FRAMES[self.last_pulse as usize % PULSE_FRAMES.len()]
                    } else {
                        RUNNING_MARK
                    };
                    let rendered = render_group(&group, width, p, activity);
                    tail_groups.push((tail.len() + 1, group));
                    rendered
                }
                LiveItem::Row(index) => render_block(
                    &project_row(&snapshot.rows, *index, detail_of(*index)),
                    width,
                    p,
                    markdown,
                    self.fullscreen,
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
        self.release_compaction_details();
        self.planner.reset();
        self.thoughts.clear();
        self.commits = CommitState::default();
        self.answered.clear();
        self.runs = Runs::default();
        self.window_floor = 0;
        self.history_floor = 0;
        self.load_more = false;
        self.reveal_pending = false;
        self.landing = None;
        self.window_info = (0, 0);
        self.history_request = None;
        self.awaiting_window = true;
        self.pending_line = Some(line);
        self.line_draft = None;
        if matches!(self.prompt, Some(Prompt::Fork(_))) {
            self.prompt = None;
        }
        self.card_blocks.clear();
        self.message_blocks.clear();
        if self.fullscreen {
            self.dropped += self.history.len();
            self.history.clear();
            self.stream_block = None;
            self.jump = Some(Jump::Bottom);
        }
    }

    /// Points the composer's label and its Git source at the workspace the
    /// daemon reported for the Session being shown, or back at the launch
    /// workspace for `None` (Home, or a daemon older than the attach record).
    /// The branch is read once here, without waiting for the next poll.
    ///
    /// The `@` mention root does not move: interactive local file paths keep
    /// the CLI's selected `--cwd` as their base, whichever Session is
    /// attached (cli-attachment-input), and the reported workspace may be a
    /// directory this client cannot see. When the border names a directory
    /// other than that base, the file picker's title names the base, so the
    /// screen still says which files a mention reads.
    fn show_workspace(&mut self, cwd: Option<&str>) {
        let cwd = cwd.unwrap_or(&self.cwd_key);
        self.workspace = crate::ui::tilde(cwd);
        // Paths compare by component, so `A/` is `A`; not canonicalized: I/O
        // per switch, and a resolved symlink is a display question.
        self.composer
            .name_root(Path::new(cwd) != Path::new(&self.cwd_key));
        self.git_head = git_head(cwd);
        self.branch = self.git_head.as_deref().and_then(git_branch);
    }

    fn switch_session(&mut self, session_id: String, kind: SwitchKind) {
        self.report_error = None;
        self.release_compaction_details();
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
        // Home keeps the provider and model it drew but no preference: no
        // configuration is adopted there until its first message arrives.
        if session_id.is_empty()
            && let Some(label) = &mut self.label
        {
            label.effort = None;
        }
        self.session_id = session_id;
        self.planner.reset();
        self.thoughts.clear();
        self.commits = CommitState::default();
        self.answered.clear();
        if !matches!(self.prompt, Some(Prompt::Report(_))) {
            self.prompt = None;
        }
        self.snapshot = None;
        self.runs = Runs::default();
        // Another Session's tasks are its own: this interface has reported
        // none of them.
        self.notified_tasks.clear();
        self.window_floor = 0;
        self.history_floor = 0;
        self.load_more = false;
        self.reveal_pending = false;
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
        self.suggestion = None;
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
            TimelineAction::Edit => self.set_draft(&text),
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

    /// Shows what comes before the drawn top, for PageUp at that top, the
    /// load-more row and fullscreen `/earlier`: loaded rows first, a read from
    /// the daemon when none are loaded, and nothing at the branch start.
    fn show_earlier(&mut self) {
        if self.hidden_messages() > 0 {
            self.load_earlier();
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        match snapshot.earlier {
            Earlier::Unread => {
                self.host.send(Command::Earlier);
                self.reveal_pending = true;
            }
            Earlier::Reading => self.reveal_pending = true,
            Earlier::AtStart => {}
        }
    }

    /// Message rows loaded above `window_floor`, which the drawn window hides.
    fn hidden_messages(&self) -> usize {
        let Some(snapshot) = &self.snapshot else {
            return 0;
        };
        snapshot.rows[..self.window_floor.min(snapshot.rows.len())]
            .iter()
            .filter(|row| row.kind.is_message())
            .count()
    }

    /// Reveals the page an earlier read just prepended, keeping the oldest
    /// drawn message at the top of the view.
    fn reveal_prepended(&mut self) {
        if self.hidden_messages() > 0 {
            self.load_earlier();
        }
        self.reveal_pending = false;
    }

    /// The pending reveal, once a snapshot arrived. A prepended page is shown;
    /// a read that ended without one drops the request, so a later unrelated
    /// prepend is not revealed by it. A live-turn snapshot during the read
    /// changes nothing, which is why `Earlier::Reading` keeps it.
    fn apply_earlier(&mut self, previous: Option<&ViewSnapshot>) {
        if !self.reveal_pending {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        // Rows were prepended when the old first row now sits below the top: a
        // front trim changes the first row too and must not count.
        let prepended = previous.is_some_and(|previous| {
            previous.rows.first().is_some_and(|first| {
                snapshot
                    .rows
                    .iter()
                    .position(|row| row.row_id == first.row_id)
                    .is_some_and(|index| index > 0)
            })
        });
        if prepended {
            self.reveal_prepended();
        } else if snapshot.earlier != Earlier::Reading {
            self.reveal_pending = false;
        }
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
        self.thoughts.clear();
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

    /// Keeps the drawn window on the rows it showed, after the handler has
    /// stored a new snapshot: each floor takes the new index of the row it
    /// named, a floor of 0 included. Identity covers a prepend, an append and a
    /// page trimmed from the front by the same rule (timeline UI §4.6).
    ///
    /// Nothing moves while a window is not armed: a line or session switch
    /// resets both floors and `arm_window` sets them from scratch, so an
    /// unarmed floor belongs to no row of this snapshot.
    ///
    /// When the floor's row is gone, the first row after it that survives takes
    /// over, which covers a front trim; when none survives, 0, which covers a
    /// trim that removed the whole window and the rows after it.
    fn follow_floors(&mut self, previous: Option<&ViewSnapshot>) {
        if self.awaiting_window {
            return;
        }
        let (Some(previous), Some(snapshot)) = (previous, &self.snapshot) else {
            return;
        };
        let index_of = |id: &str| snapshot.rows.iter().position(|row| row.row_id == id);
        let moved = |floor: usize| -> usize {
            let Some(rows) = previous.rows.get(floor..).filter(|rows| !rows.is_empty()) else {
                return floor.min(snapshot.rows.len());
            };
            if let Some(index) = index_of(&rows[0].row_id) {
                return index;
            }
            let present: std::collections::HashSet<&str> = snapshot
                .rows
                .iter()
                .map(|row| row.row_id.as_str())
                .collect();
            rows.iter()
                .find(|row| present.contains(row.row_id.as_str()))
                .and_then(|row| index_of(&row.row_id))
                .unwrap_or(0)
        };
        self.window_floor = moved(self.window_floor.min(previous.rows.len()));
        self.history_floor = moved(self.history_floor.min(previous.rows.len()));
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
    fn record(&mut self, mut source: Block) {
        if let Block::Compaction {
            target,
            detail: Some(detail),
            ..
        } = &mut source
            && let Some(snapshot) = &self.snapshot
        {
            *target = Some(crate::model::view::CompactionTarget {
                session_id: snapshot.session_id.clone(),
                branch_id: snapshot.focus.clone(),
                event_id: detail.event_id.clone(),
                sequence: detail.sequence,
            });
        }
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
    fn click(&mut self, block: Option<usize>, row: usize, cols: usize) -> bool {
        if let Some((node, open)) = self.tool_disclosure(block, row, cols) {
            self.tool_choices
                .entry(self.tool_scope())
                .or_default()
                .insert(node, !open);
            self.refresh_tool_choices();
            self.sync_tool_origins();
            return true;
        }
        let Some(id) = block else { return false };
        if row != 1 {
            return false;
        }
        let index = id.checked_sub(self.dropped);
        match index.and_then(|index| self.history.get_mut(index)) {
            Some(Block::Compaction {
                expanded,
                text,
                detail,
                ..
            }) => {
                *expanded = !*expanded;
                if !*expanded && detail.is_some() {
                    *text = "".into();
                }
            }
            Some(
                Block::Thinking { expanded, .. }
                | Block::Hook { expanded, .. }
                | Block::Error { expanded, .. },
            ) => *expanded = !*expanded,
            _ => return false,
        }
        self.mark_rewritten(id);
        self.sync_compaction_details();
        true
    }

    fn tool_scope(&self) -> (String, String) {
        (
            self.session_id.clone(),
            self.snapshot
                .as_ref()
                .map_or("main", |snapshot| snapshot.focus.as_str())
                .to_owned(),
        )
    }

    fn tool_disclosure(
        &self,
        block: Option<usize>,
        row: usize,
        cols: usize,
    ) -> Option<(String, bool)> {
        let width = cols.saturating_sub(2 + MARGIN).max(1);
        let (group, row) = match block {
            Some(id) => {
                let Block::ToolGroup(group) = self.history.get(id.checked_sub(self.dropped)?)?
                else {
                    return None;
                };
                (group, row.checked_sub(1)?)
            }
            None => self
                .tail_groups
                .iter()
                .rev()
                .find_map(|(start, group)| row.checked_sub(*start).map(|row| (group, row)))?,
        };
        disclosure_at(group, width, &self.palette, row).map(|(id, open)| (id.to_owned(), open))
    }

    fn refresh_tool_choices(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let choices = self.tool_choices.get(&self.tool_scope());
        let mut openings = HashMap::new();
        for (index, row) in snapshot.rows.iter().enumerate() {
            if opens_group(&snapshot.rows, index) {
                openings.entry(group_id(row)).or_insert(index);
            }
        }
        let mut changed = Vec::new();
        for (at, block) in self.history.iter().enumerate() {
            let Block::ToolGroup(group) = block else {
                continue;
            };
            let Some(&index) = openings.get(&group.id) else {
                continue;
            };
            let rows = group_rows(&snapshot.rows, index);
            let fresh = if self.fullscreen {
                project_fullscreen_group(&rows, self.tool_detail, choices)
            } else {
                project_group(&rows, self.tool_detail)
            };
            if fresh != *group {
                changed.push((at, fresh))
            }
        }
        for (at, group) in changed {
            self.history[at] = Block::ToolGroup(group);
            self.mark_rewritten(self.dropped + at);
        }
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
            self.refresh_tool_choices();
            for block in &mut self.history {
                if let Block::Compaction {
                    expanded,
                    text,
                    detail,
                    ..
                } = block
                {
                    *expanded = self.tool_detail == Detail::Expanded;
                    if !*expanded && detail.is_some() {
                        *text = "".into();
                    }
                }
            }
            self.sync_compaction_details();
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
            let fresh = project_fullscreen_group(
                &rows,
                self.tool_detail,
                self.tool_choices.get(&self.tool_scope()),
            );
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
            self.show_earlier();
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
            Block::Banner => banner(
                cols,
                &self.palette,
                self.branch.as_deref().unwrap_or(t("Not available")),
            ),
            Block::ToolGroup(group) if self.fullscreen => chunk(&render_group(
                group,
                width,
                &self.palette,
                PULSE_FRAMES[self.last_pulse as usize % PULSE_FRAMES.len()],
            )),
            block => chunk(&render_block(
                block,
                width,
                &self.palette,
                &self.markdown,
                self.fullscreen,
            )),
        }
    }

    /// The history for a scrollback rewrite, laid out for `cols` columns,
    /// one string per row: the newest `REWRITE_MAX_ROWS` rows at most.
    fn reflow_history(&mut self, cols: usize) -> Vec<String> {
        let width = cols.saturating_sub(2 + MARGIN).max(1);
        let frames = self.thought_frames(width);
        for (id, stream) in &mut self.thoughts {
            if let Some(frame) = frames.get(id) {
                if let Some(index) = stream.block.and_then(|id| id.checked_sub(self.dropped)) {
                    self.history[index] = frame.source.clone();
                    stream.written = frame.stream.written;
                } else {
                    stream.written = 0;
                }
            } else {
                stream.written = 0;
            }
        }
        let mut chunks = Vec::new();
        let mut rows = 0;
        for (index, source) in self.history.iter().enumerate().rev() {
            if self.thoughts.iter().any(|(id, stream)| {
                stream.block == Some(self.dropped + index) && !frames.contains_key(id)
            }) {
                continue;
            }
            if matches!(source, Block::Thinking { text, .. } if text.is_empty()) {
                continue;
            }
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
                _ => None,
            },
        }
    }

    /// How many tasks the Session runs apart from the turn, as its own
    /// status segment: none while every task has ended, so an idle Session
    /// with nothing in the background draws no segment at all.
    fn background_label(&self) -> Option<String> {
        let count = self
            .snapshot
            .as_ref()?
            .tasks
            .iter()
            .filter(|task| task.finished.is_none())
            .count();
        (count > 0).then(|| {
            tf(
                "{count} in background · /tasks",
                &[("count", &count.to_string())],
            )
        })
    }

    /// Whether the task line draws now: a task runs, or one ended within
    /// [`TASK_FADE`] seconds.
    fn task_line_shown(&self) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        !shown_tasks(&snapshot.tasks, now).is_empty()
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
    /// status, the slot (a notice in the highlight colour) and the context use.
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
        if let Some(background) = self.background_label() {
            parts.push(fg(&background, &p.muted));
        }
        if let Some(goal) = self.goal_label() {
            parts.push(fg(goal, &p.muted));
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

    /// The composer's bottom-right label, built to fit the cells the border
    /// will allow it beside `status`; `None` when nothing fits. The place and
    /// the unread badge share that one budget, so neither can drift from what
    /// [`border_line`] draws.
    fn composer_place(&self, status: Option<&str>, outer: usize) -> Option<String> {
        let cells = outer.saturating_sub(6 + status.map_or(0, |label| width_of(label) + 3));
        if cells == 0 {
            return None;
        }
        let place = fg(&self.place_label(cells), &self.palette.muted);
        let count = self
            .snapshot
            .as_ref()
            .map_or(0, |view| view.unread_lines.len());
        if count == 0 {
            return Some(place);
        }
        let full = format!("{} ({count}) · /lines", t("Unread"));
        let compact = format!("/lines ({count})");
        let Some(label) = [full.as_str(), compact.as_str(), "/lines", "*"]
            .into_iter()
            .find(|label| width_of(label) <= cells)
        else {
            return Some(place);
        };
        let width = width_of(label);
        let label = fg(label, &self.palette.highlight);
        if width + 2 + width_of(&place) <= cells && width_of(&place) != 0 {
            Some(format!("{label}  {place}"))
        } else {
            Some(label)
        }
    }

    fn status_line(&self, cols: usize) -> String {
        use dim_agent_types::PermissionPreset;
        let p = &self.palette;
        let preset = self.permission_policy.preset;
        let color = match preset {
            PermissionPreset::ReadOnly => &p.permission_read,
            PermissionPreset::WorkspaceWrite => &p.permission_write,
            PermissionPreset::FullAccess => &p.permission_full,
        };
        let permission = fg(&format!("⏵⏵ {}", permission_label(preset)), color);
        let hint = fg(t("(shift+tab to cycle)"), &p.muted);
        let mut left = format!("{}{permission} {hint}", " ".repeat(MARGIN));
        let total = cols.saturating_sub(2);
        if self.fullscreen {
            return clip(&left, total).0;
        }
        let prefix_width = width_of(&left);
        let slot = self.slot();
        if let Some(goal) = self.goal_label() {
            left.push_str(&fg(&format!("  {goal}"), &p.muted));
        }
        if let Some(context) = self.context_label() {
            let color = if self.context_near() {
                &p.warning
            } else {
                &p.muted
            };
            left.push_str(&fg(&format!("  {context}"), color));
        }
        if let Some(slot) = slot {
            let color = if self.notice.is_some() {
                &p.highlight
            } else {
                &p.muted
            };
            left.push_str(&fg(&format!("  {slot}"), color));
        }
        if let Some(background) = self.background_label() {
            left.push_str(&fg(&format!("  {background}"), &p.muted));
        }
        let label = self
            .label
            .as_ref()
            .and_then(|label| label.fitted(|text| prefix_width + 2 + width_of(text) <= total));
        let line = match label {
            Some(ref label) => {
                let (left, width) = clip(&left, total - width_of(label) - 2);
                let gap = total - width - width_of(label);
                format!("{left}{}", " ".repeat(gap))
            }
            None => clip(&left, total).0,
        };
        match label {
            Some(label) => format!("{line}{}", fg(&label, &self.palette.muted)),
            None => line,
        }
    }
}

impl Document for App {
    fn disclosure(&self, block: Option<usize>, cols: usize, row: usize) -> Option<String> {
        let (node, _) = self.tool_disclosure(block, row, cols)?;
        Some(serde_json::to_string(&(self.tool_scope(), node)).expect("tool identity is JSON"))
    }

    fn tool_rows(&self, index: usize, cols: usize) -> Vec<usize> {
        let Block::ToolGroup(group) = &self.history[index] else {
            return Vec::new();
        };
        disclosure_rows(group, cols.saturating_sub(2 + MARGIN).max(1), &self.palette)
            .into_iter()
            .map(|row| row + 1)
            .collect()
    }

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
const QUEUE_HINT: &str = "/queue manage · Shift+← edit last · empty draft: {key} steer";
const QUEUE_HINT_MORE: &str = "+{count} more · {hint}";

/// The queue, drawn in a titled border above the composer: the next message
/// to run reads at the top. `height` message rows are drawn (at least the
/// queue's, at most [`QUEUE_ROWS`]), blank past the queue's end, so the
/// strip holds its height while the queue drains. An empty queue is not
/// drawn at all.
fn queue_strip(
    items: &[(String, String)],
    height: usize,
    cols: usize,
    p: &Palette,
    hint: &str,
) -> Vec<String> {
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
    let more;
    let hint = if hidden > 0 {
        more = tf(
            QUEUE_HINT_MORE,
            &[("count", &hidden.to_string()), ("hint", hint)],
        );
        &more
    } else {
        hint
    };
    rows.push(fg(&truncate_cells(hint, inner), &p.muted));
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

/// A background task's end stays on the persistent line this long (seconds)
/// after it ended, so it does not vanish from under the eye that just saw it.
const TASK_FADE: u64 = 5;

/// `elapsed` as the task line writes it: `0s`, `12s`, `1m12s`, `2h05m`.
fn task_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m{:02}s", seconds / 60, seconds % 60);
    }
    format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
}

/// The tasks the line shows at `now` (Unix seconds): every unfinished task,
/// then each finished one for [`TASK_FADE`] seconds after it ended. The
/// second value is the task's end, absent while it runs.
fn shown_tasks(
    tasks: &[crate::model::view::TaskView],
    now: u64,
) -> Vec<(&crate::model::view::TaskView, Option<u64>)> {
    tasks
        .iter()
        .filter_map(|task| match &task.finished {
            None => Some((task, None)),
            Some(finish) => {
                let ended = finish.at / 1000;
                (ended + TASK_FADE > now).then_some((task, Some(ended)))
            }
        })
        .collect()
}

/// The line of tasks running apart from the turn: `◐ 2 · cargo build 1m12s ·
/// cargo test 34s` while both run, with `✓`, `✗` or `⊘` in place of the
/// elapsed time's lead once one ends. Without a running task the count has
/// nothing to say, so the row starts at the first entry's mark:
/// `✓ cargo build 1m12s`. A single row, cut to `width` cells; `None` when
/// there is nothing to show.
fn task_line(
    tasks: Vec<(&crate::model::view::TaskView, Option<u64>)>,
    now: u64,
    width: usize,
    p: &Palette,
) -> Option<String> {
    use crate::ui::text::truncate_cells;
    if tasks.is_empty() {
        return None;
    }
    let running = tasks.iter().filter(|(_, end)| end.is_none()).count();
    let head = (running > 0).then(|| format!("\u{25d0} {running}"));
    let mut row = head.clone().unwrap_or_default();
    let mut room = width.saturating_sub(display_width(&row));
    for (shown, (task, end)) in tasks.iter().enumerate() {
        let started = task.started_at / 1000;
        let at = end.unwrap_or(now).max(started);
        let elapsed = task_elapsed(at - started);
        let label = crate::sanitize::safe(&task.label);
        let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
        let mark = task
            .finished
            .as_ref()
            .map_or("", |finish| finish.outcome.mark());
        let lead = if shown == 0 && head.is_none() {
            ""
        } else {
            " · "
        };
        let entry = format!("{lead}{mark}{label} {elapsed}");
        let fits = display_width(&entry) <= room;
        if !fits {
            if shown == 0 {
                // The first entry still names what runs, shortened to fit.
                let tail = format!(" {elapsed}");
                let fixed = display_width(lead) + display_width(mark) + tail.len();
                let label = truncate_cells(&label, room.saturating_sub(fixed).max(1));
                row.push_str(&format!("{lead}{mark}{label}{tail}"));
            } else {
                row.push_str(&truncate_cells(" · …", room.max(1)));
            }
            break;
        }
        row.push_str(&entry);
        room -= display_width(&entry);
    }
    let failed = tasks
        .iter()
        .any(|(task, end)| end.is_some() && task.finished.as_ref().is_some_and(TaskFinish::failed));
    let color = if running > 0 {
        &p.activity
    } else if failed {
        &p.danger
    } else {
        &p.success
    };
    Some(fg(&truncate_cells(&row, width.max(1)), color))
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
    thinking_detail: Detail,
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
            RowKind::Thinking => project_row(&side.rows, index, thinking_detail),
            _ => project_row(&side.rows, index, detail),
        };
        let rendered = render_block(&block, inner, p, markdown, false);
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

/// The workspace and its branch, whole: `~/src/app (main)`.
fn whole_workspace(workspace: &str, branch: Option<&str>) -> String {
    match branch {
        Some(branch) => format!("{workspace} ({branch})"),
        None => workspace.to_owned(),
    }
}

/// The narrowest path tail worth putting a branch beside: below it
/// `…o (rex-team/T-4440f2)` names neither the checkout nor the line, so the
/// path alone says more.
const MIN_TAIL: usize = 8;

/// The workspace label in at most `cells` columns, degrading in place
/// rather than being dropped whole by the border: the whole label, the whole
/// label with the path's front cut once its tail is at least [`MIN_TAIL`],
/// the path alone when the branch gives way, then the path alone with its
/// front cut. No form exceeds `cells`, so the caller sizes the border by
/// measuring the result.
fn workspace_label(workspace: &str, branch: Option<&str>, cells: usize) -> String {
    let whole = whole_workspace(workspace, branch);
    if width_of(&whole) <= cells {
        return whole;
    }
    if let Some(branch) = branch {
        let suffix = format!(" ({branch})");
        let tail = truncate_cells_start(workspace, cells.saturating_sub(width_of(&suffix)));
        if width_of(&tail) >= MIN_TAIL {
            return format!("{tail}{suffix}");
        }
    }
    if width_of(workspace) <= cells {
        return workspace.to_owned();
    }
    truncate_cells_start(workspace, cells)
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
    let mut body = vec![
        t("Bye!").to_owned(),
        String::new(),
        t("Feedback or ideas? Join us on Discord:").to_owned(),
        "https://discord.gg/ds35K3TadP".to_owned(),
    ];
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
/// first; a caller that can say the same thing shorter composes it that way
/// before calling, as the composer does for its workspace label.
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

fn saved_detail(home: &std::path::Path, key: &str) -> Detail {
    match crate::ui::config::read(home, key).and_then(|v| v.as_bool()) {
        Some(true) => Detail::Expanded,
        _ => Detail::Collapsed,
    }
}

fn saved_tool_detail(home: &std::path::Path) -> Detail {
    match crate::ui::config::read(home, TOOL_DETAILS_KEY)
        .as_ref()
        .and_then(|v| v.as_str())
    {
        Some("collapsed") => Detail::Collapsed,
        Some("summary") => Detail::Summary,
        Some("expanded") => Detail::Expanded,
        _ => saved_detail(home, LEGACY_TOOL_DETAILS_KEY),
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
                    let commands =
                        crate::ui::composer::palette::filter_commands("/", drawn.app.context());
                    for command in commands {
                        let command = format!("/{}", command.name);
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
                            text.contains(&command),
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
                    if fullscreen {
                        drawn
                            .app
                            .composer
                            .set_draft(&format!("/{}", "abcdefghij".repeat(4)));
                        let frame = drawn.app.frame(size);
                        let bytes = FullScreen::new(size, true).render(
                            &frame,
                            size,
                            &drawn.app.palette,
                            &drawn.app,
                        );
                        let mut emu =
                            crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                        emu.feed(&bytes);
                        let text = emu.rows_text().join("\n");
                        assert!(
                            text.contains("> abcdefghij"),
                            "long query visible: {size:?}: {text}"
                        );
                    }
                }
            }
        }
        i18n::set(i18n::Locale::En);
    }

    /// From the smallest size the App gives a panel to the panel's own full
    /// height, in both layouts: the composer's rows stay on the screen, the
    /// panel never draws more rows than its budget and draws nothing under
    /// two rows, and it ends above the composer. A drawn panel keeps the
    /// hint's closing key and, whenever it has a list row, the selected
    /// entry. The panel with notes keeps its first three list rows while it
    /// can still show a note.
    #[test]
    fn list_panels_keep_their_row_budget_on_short_screens() {
        use crate::model::session::{
            ActionKind, Area, ExecutionSnapshot, ManagementAction, ManagementEntry,
            ManagementGroup, ManagementReply, ManagementRequest, ManagementResult, ManagementView,
            SessionItem,
        };
        use crate::model::view::{LineState, LineView, ViewSnapshot};
        use crate::ui::render::panel::{strip_escapes, visible_width};

        let sessions = || {
            let mut panel = SessionsPanel::new(false);
            panel.set_items(Ok((0..11)
                .map(|i| SessionItem {
                    id: format!("s{i}"),
                    title: Some(format!("item{i}")),
                    preview: None,
                    cwd: "/work".into(),
                    updated_at: u64::MAX,
                    model: "p/m".into(),
                    origin: None,
                })
                .collect()));
            Prompt::Sessions(panel)
        };
        let workflow = || {
            let mut panel = ManagementPanel::new(Area::Workflow, None);
            panel.set_result(&ManagementReply {
                request: ManagementRequest {
                    area: Area::Workflow,
                    action: ManagementAction::List,
                },
                result: ManagementResult::View {
                    view: Box::new(ManagementView {
                        actions: vec![ActionKind::Reload],
                        groups: vec![ManagementGroup {
                            heading: "user".into(),
                            entries: vec![ManagementEntry {
                                id: "cleanup".into(),
                                source: None,
                                label: "cleanup".into(),
                                detail: vec!["Runs first.".into()],
                                enabled: Some(true),
                                states: Vec::new(),
                                actions: vec![ActionKind::Toggle],
                                remove_verb: Default::default(),
                                document: None,
                                run: None,
                            }],
                        }],
                        ..ManagementView::default()
                    }),
                    note: None,
                },
            });
            Prompt::Management(Box::new(panel))
        };
        let lines = || {
            let snapshot = ViewSnapshot {
                session_id: "s1".into(),
                projection_id: 3,
                focus: "b-7c".into(),
                lines: vec![
                    LineView {
                        id: "main".into(),
                        parent: None,
                        purpose: String::new(),
                        state: LineState::Main,
                        cut: None,
                    },
                    LineView {
                        id: "b-7c".into(),
                        parent: Some("main".into()),
                        purpose: "parser race".into(),
                        state: LineState::Working,
                        cut: None,
                    },
                ],
                ..ViewSnapshot::default()
            };
            let mut panel = LinesPanel::new(&snapshot);
            let key = |code| {
                crate::ui::terminal::KeyEvent::new(code, crate::ui::terminal::Modifiers::NONE)
            };
            panel.handle_key(key(KeyCode::Char('x')));
            Prompt::Lines(Box::new(panel))
        };
        let isolation = || {
            let mut panel = crate::ui::isolation_panel::IsolationPanel::new();
            panel.set_snapshot(Ok(ExecutionSnapshot {
                revision: "r1".into(),
                sandbox: false,
                network: false,
            }));
            Prompt::Isolation(Box::new(panel))
        };
        let render = |prompt: &Prompt, app: &App, width: usize, rows: usize| match prompt {
            Prompt::Sessions(panel) => panel.render(width, rows, &app.palette),
            Prompt::Management(panel) => panel.render(width, rows, &app.palette),
            Prompt::Lines(panel) => panel.render(width, rows, &app.palette, &app.markdown),
            Prompt::Isolation(panel) => panel.render(width, rows, &app.palette),
            _ => unreachable!(),
        };

        // The composer's rows, from the first row after the dock's blank
        // line to its bottom border.
        let composer_rows = |frame: &crate::ui::render::screen::Frame| {
            let live: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
            let start = frame.dock + 1;
            let end = (start..live.len())
                .find(|&row| live[row].trim_start().starts_with('╰'))
                .unwrap_or(live.len() - 1);
            end + 1 - start
        };

        let sweep = |fullscreen: bool,
                     open: &dyn Fn() -> Prompt,
                     title: &str,
                     selected: &str,
                     keys: &str,
                     notes: bool| {
            let mut drawn = Drawn::new((40, 8), None, true);
            drawn.app.fullscreen = fullscreen;
            let max_panel_rows = if fullscreen {
                OVERLAY_MAX_ROWS
            } else {
                live_panel_rows(usize::MAX, 0, 0)
            };
            let mut expected_rows = None;
            let mut reached = false;
            let mut budgets = Vec::new();
            for height in 8..=40u16 {
                let size = (40, height);
                // The composer's longest draft: the shortest terminals leave
                // the panel no rows at all.
                drawn.app.composer.set_draft("l1\nl2\nl3\nl4\nl5\nl6\nl7");
                drawn.app.prompt = Some(open());
                let frame = drawn.app.frame(size);
                let budget = if fullscreen {
                    overlay_panel_rows(size.1 as usize, composer_rows(&frame))
                } else {
                    live_panel_rows(
                        crate::ui::render::live_row_budget(
                            size.1 as usize,
                            crate::ui::render::screen::MIN_HISTORY_ROWS as usize,
                        ),
                        composer_rows(&frame),
                        1,
                    )
                };
                budgets.push(budget);
                let live: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
                let mut top = 0;
                let panel: Vec<String> = if fullscreen {
                    let overlay = frame
                        .overlay
                        .as_ref()
                        .unwrap_or_else(|| panic!("no overlay for {title}"));
                    overlay.rows.iter().map(|row| strip_escapes(row)).collect()
                } else {
                    match live
                        .iter()
                        .rposition(|row| row.contains(&format!("╭─ {title}")))
                    {
                        Some(panel_top) => {
                            top = panel_top;
                            live[top..live.len() - 1].to_vec()
                        }
                        None => Vec::new(),
                    }
                };
                assert!(
                    panel.len() <= budget,
                    "{title} {size:?} fullscreen={fullscreen}: {} rows drawn of {budget}: {panel:#?}",
                    panel.len()
                );
                assert_eq!(
                    panel.is_empty(),
                    budget < 2,
                    "{title} {size:?} fullscreen={fullscreen}: a {budget}-row budget drew {} rows: {live:#?}",
                    panel.len()
                );
                if expected_rows.is_none() && !panel.is_empty() {
                    expected_rows = Some(
                        render(
                            &open(),
                            &drawn.app,
                            visible_width(&panel[0]),
                            max_panel_rows,
                        )
                        .len(),
                    );
                }
                let text = panel.join("\n");
                if !panel.is_empty() {
                    assert!(
                        text.contains(keys),
                        "{title} {size:?} fullscreen={fullscreen}: the closing key: {text}"
                    );
                }
                if fullscreen {
                    let dock = frame.live.len() - frame.dock;
                    let overlay = frame.overlay.as_ref().unwrap().rows.len();
                    assert!(
                        overlay + dock <= size.1 as usize,
                        "{title} {size:?} fullscreen: {overlay} rows and a {dock}-row dock: {text}"
                    );
                } else {
                    assert!(
                        frame.live.len() <= size.1 as usize,
                        "{title} {size:?}: {} live rows: {text}",
                        frame.live.len()
                    );
                }
                if panel.len() >= 3 {
                    assert!(
                        panel.iter().any(|row| row.contains(selected)
                            && !row.contains("End line")
                            && !row.contains("y end it")),
                        "{title} {size:?} fullscreen={fullscreen}: the selection: {text}"
                    );
                }
                if notes {
                    // The first three list rows stay while the notes still
                    // show a line; the notes give up lines before they do.
                    let entries = [
                        "No network",
                        "Network allowed",
                        "Save",
                        "Reset to defaults",
                        "Reload",
                    ];
                    let rows = panel
                        .iter()
                        .filter(|row| {
                            (row.contains("Off") && row.contains("selected"))
                                || entries.iter().any(|entry| row.contains(entry))
                        })
                        .count();
                    let note = panel.iter().any(|row| row.contains("Applies to"));
                    assert!(
                        !note || rows >= 3,
                        "{title} {size:?} fullscreen={fullscreen}: {rows} list rows beside a note: {text}"
                    );
                }
                if matches!(
                    drawn.app.prompt,
                    Some(Prompt::Sessions(_) | Prompt::Lines(_))
                ) {
                    // The cursor sits on the `> query` row, or nowhere once
                    // the panel gave the row up.
                    let query = panel
                        .iter()
                        .position(|row| row.trim_start().starts_with("│ > "));
                    let caret = if fullscreen {
                        frame.overlay.as_ref().unwrap().cursor
                    } else {
                        frame.cursor
                    };
                    assert_eq!(
                        caret.map(|(row, _)| row as usize),
                        query.map(|row| row + top),
                        "{title} {size:?} fullscreen={fullscreen}: the query cursor: {text}"
                    );
                }
                let bytes = if fullscreen {
                    FullScreen::new(size, true).render(&frame, size, &drawn.app.palette, &drawn.app)
                } else {
                    Screen::new(size, true).render(&frame, size)
                };
                let mut emu =
                    crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                emu.feed(&bytes);
                let screen = emu.rows_text().join("\n").replace('\0', "");
                let screen_rows: Vec<&str> = screen.lines().collect();
                for row in live.iter().skip(frame.dock + 1).take(composer_rows(&frame)) {
                    let row = row.trim_end();
                    assert!(
                        screen_rows.iter().any(|line| line.trim_end() == row),
                        "{title} {size:?} fullscreen={fullscreen}: the composer row {row:?} is cut: {screen}"
                    );
                }
                if !panel.is_empty() {
                    assert!(
                        screen.contains(keys),
                        "{title} {size:?} fullscreen={fullscreen}: the keys: {screen}"
                    );
                }
                if expected_rows.is_some_and(|rows| panel.len() == rows) {
                    reached = true;
                    break;
                }
            }
            for budget in [0, 1, 2] {
                assert!(
                    budgets.contains(&budget),
                    "{title} fullscreen={fullscreen}: never gave the panel {budget} rows: {budgets:?}"
                );
            }
            assert!(
                reached,
                "{title} fullscreen={fullscreen}: never reached {expected_rows:?} rows"
            );
        };

        for fullscreen in [false, true] {
            sweep(
                fullscreen,
                &sessions,
                "Sessions",
                "item0",
                "Esc close",
                false,
            );
            sweep(
                fullscreen,
                &workflow,
                "Workflow",
                "cleanup",
                "Esc close",
                false,
            );
            sweep(fullscreen, &lines, "Lines", "b-7c", "y end it", false);
            sweep(
                fullscreen,
                &isolation,
                "Process isolation",
                "Off",
                "Esc close",
                true,
            );
        }
    }

    /// At the sizes a short screen leaves it, the management panel keeps the
    /// composer, the selected entry and the key hint, in both layouts.
    #[test]
    fn short_management_panels_keep_the_composer_and_key_hint() {
        use crate::model::session::{
            ActionKind, Area, ManagementAction, ManagementEntry, ManagementGroup, ManagementReply,
            ManagementRequest, ManagementResult, ManagementView,
        };
        let view = || ManagementView {
            actions: vec![ActionKind::Reload, ActionKind::Add],
            groups: vec![ManagementGroup {
                heading: "user".into(),
                entries: vec![ManagementEntry {
                    id: "review-and-fix".into(),
                    source: None,
                    label: "review-and-fix".into(),
                    detail: vec!["Runs the review first.".into()],
                    enabled: Some(true),
                    states: Vec::new(),
                    actions: vec![
                        ActionKind::Toggle,
                        ActionKind::Edit,
                        ActionKind::Run,
                        ActionKind::Inspect,
                    ],
                    remove_verb: Default::default(),
                    document: None,
                    run: None,
                }],
            }],
            ..ManagementView::default()
        };
        for fullscreen in [false, true] {
            for size in [(32, 8), (40, 10), (32, 12)] {
                let mut drawn = Drawn::new(size, None, true);
                drawn.app.fullscreen = fullscreen;
                drawn.app.composer.set_draft("draft");
                let mut panel = ManagementPanel::new(Area::Workflow, None);
                panel.set_result(&ManagementReply {
                    request: ManagementRequest {
                        area: Area::Workflow,
                        action: ManagementAction::List,
                    },
                    result: ManagementResult::View {
                        view: Box::new(view()),
                        note: None,
                    },
                });
                drawn.app.prompt = Some(Prompt::Management(Box::new(panel)));
                let frame = drawn.app.frame(size);
                let bytes = if fullscreen {
                    FullScreen::new(size, true).render(&frame, size, &drawn.app.palette, &drawn.app)
                } else {
                    Screen::new(size, true).render(&frame, size)
                };
                let mut emu =
                    crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                emu.feed(&bytes);
                let text = emu.rows_text().join("\n").replace('\0', "");
                assert!(
                    text.contains("draft"),
                    "{size:?} fullscreen={fullscreen}: the composer: {text}"
                );
                assert!(
                    text.contains("review"),
                    "{size:?} fullscreen={fullscreen}: the selected entry: {text}"
                );
                assert!(
                    text.contains("Esc close"),
                    "{size:?} fullscreen={fullscreen}: the keys: {text}"
                );
                if size.0 >= 40 {
                    assert!(
                        text.contains("Enter toggle"),
                        "{size:?} fullscreen={fullscreen}: the action keys: {text}"
                    );
                }
            }
        }
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
                        manual: "dima update".into(),
                    };
                    drawn.app.apply(Update::Upgrade(offer.clone()));
                    assert!(shown(&mut drawn).contains(&format!("› {}", t("Skip this version"))));
                    drawn.key(KeyCode::Tab);
                    assert!(shown(&mut drawn).contains(&format!("› {}", t("Close"))));
                    drawn.key(KeyCode::Tab);
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
                    assert!(details.contains("dimaupdate"));
                    drawn.size = (80, 24);
                    assert!(shown(&mut drawn).contains(&offer.current));
                    drawn.size = size;
                    drawn.key(KeyCode::Enter);
                    assert!(
                        sent.borrow().is_empty(),
                        "the dialog sends nothing to install"
                    );
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
                        manual: "dima update".into(),
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
            for (locale, hint) in [
                (i18n::Locale::En, "Ask anything... · Ctrl+J newline"),
                (i18n::Locale::ZhCn, "输入你的问题… · Ctrl+J 换行"),
            ] {
                i18n::set(locale);
                let frame = drawn.app.frame(drawn.size);
                let bytes = if fullscreen {
                    FullScreen::new(drawn.size, true).render(
                        &frame,
                        drawn.size,
                        &drawn.app.palette,
                        &drawn.app,
                    )
                } else {
                    Screen::new(drawn.size, true).render(&frame, drawn.size)
                };
                let mut emu = crate::ui::render::testterm::Emu::new(80, 24);
                emu.feed(&bytes);
                assert!(
                    emu.row(emu.cy).replace('\0', "").contains(hint),
                    "fullscreen={fullscreen}: {:?}",
                    emu.rows_text()
                );
            }
            i18n::set(i18n::Locale::En);
            typed(&mut drawn.app, "first line");
            drawn.app.handle(InputEvent::Key(KeyEvent::new(
                KeyCode::Char('j'),
                Modifiers::CTRL,
            )));
            typed(&mut drawn.app, "second line");
            assert_eq!(drawn.app.composer.text(), "first line\nsecond line");
            assert!(
                sent.borrow().is_empty(),
                "newline must not submit the draft"
            );
            assert!(
                !drawn
                    .app
                    .frame(drawn.size)
                    .live
                    .join("\n")
                    .contains("Ctrl+J"),
                "typing replaces the placeholder"
            );
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

    /// A short terminal cannot show the whole live region. The rows cut from
    /// its top must not carry the composer's caret down onto the status row.
    #[test]
    fn caret_stays_in_the_composer_of_a_short_terminal_with_a_pending_tail() {
        let size = (80, 5);
        let mut drawn = Drawn::new(size, None, true);
        show(&mut drawn, vec![message(true)]);
        let live = drawn.app.frame(size).live.len();
        assert!(
            live > size.1 as usize,
            "the live region must be taller than the terminal: {live} rows"
        );
        let rows = drawn.emu.rows_text();
        let caret = rows.get(drawn.emu.cy).map_or("", String::as_str);
        assert!(
            caret.contains(t(COMPOSER_PLACEHOLDER)),
            "caret on {caret:?} of {rows:?}"
        );
    }

    #[test]
    fn composer_tracks_git_switches_in_repositories_and_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        for fullscreen in [false, true] {
            let mut app = App::new(
                Box::new(Idle),
                MarkdownRenderer::new(&theme),
                theme.palette.clone(),
                root.path().join("home"),
                root.path().to_string_lossy().into_owned(),
            );
            app.fullscreen = fullscreen;
            let frame = app.frame((160, 30));
            let cwd = crate::ui::tilde(&root.path().to_string_lossy());
            let row = frame
                .live
                .iter()
                .find(|row| row.contains(&cwd))
                .expect("the composer names the workspace it was given, not the process's");
            assert!(
                !row.contains(&format!("{cwd} (")),
                "a workspace without Git must show only its path: {row}"
            );
        }
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

    /// The reported flow: in fullscreen, a linked worktree, a running turn.
    /// The label must survive 80 columns by degrading, and the border must
    /// keep its width whatever form it takes.
    #[test]
    fn a_running_turn_in_a_worktree_keeps_the_workspace_label_at_eighty_columns() {
        use crate::ui::render::panel::visible_width;

        i18n::set(i18n::Locale::En);
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let workspace = "~/Projects/repo/dim.worktrees/T-4440f2";
        let branch = "rex-team/T-4440f2";
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/tmp/T-4440f2".into(),
        );
        app.workspace = workspace.to_owned();
        app.branch = Some(branch.to_owned());
        app.fullscreen = true;
        // A running turn: the run status, the elapsed clock and the
        // interrupt hint, which is the dock the reported screenshot shows.
        app.snapshot = Some(ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            main_running: true,
            focus: "main".into(),
            ..ViewSnapshot::default()
        });
        app.run_started = Some(Instant::now());
        let border = |app: &mut App, cols| {
            let size = (cols, 30);
            let frame = app.frame(size);
            let row = frame
                .live
                .iter()
                .rev()
                .find(|row| row.contains('╰'))
                .cloned()
                .unwrap_or_default();
            let mut screen = FullScreen::new(size, true);
            let bytes = screen.render(&frame, size, &app.palette, app);
            let mut emu = crate::ui::render::testterm::Emu::new(cols as usize, 30);
            emu.feed(&bytes);
            let painted = emu
                .rows_text()
                .iter()
                .rev()
                .find(|row| row.contains('╰'))
                .cloned()
                .unwrap_or_default();
            (row, painted)
        };
        for cols in [80u16, 100, 120, 160] {
            let (row, painted) = border(&mut app, cols);
            // The frame is the margin plus `outer`, two cells short of the
            // terminal; the label must not change that.
            for text in [&row, &painted] {
                assert_eq!(
                    visible_width(text),
                    usize::from(cols) - 2,
                    "{cols}: the border must keep its width: {text:?}"
                );
            }
        }
        // At 80 columns the dock leaves too few cells for the whole label:
        // the tail of the path and the branch survive.
        let (row, _) = border(&mut app, 80);
        assert!(
            row.contains(&format!("orktrees/T-4440f2 ({branch})")),
            "80: the path's tail and the branch must survive: {row:?}"
        );
        assert!(
            !row.contains(workspace),
            "80: the whole path must not fit beside this dock: {row:?}"
        );
        // The Chinese dock is narrower than the English one, so it keeps a
        // longer tail at the same width.
        i18n::set(i18n::Locale::ZhCn);
        let (zh, _) = border(&mut app, 80);
        i18n::set(i18n::Locale::En);
        let (en, _) = border(&mut app, 80);
        assert!(
            visible_width(&zh) == visible_width(&en),
            "both docks must fill the same border: {zh:?} / {en:?}"
        );
        assert!(
            zh.contains("dim.worktrees") && !en.contains("dim.worktrees"),
            "the narrower dock keeps more of the path: {zh:?} / {en:?}"
        );
        // A branch too long beside any usable tail gives way to the path.
        app.branch = Some("rex-team/T-4440f2-and-a-much-longer-name".into());
        let (row, _) = border(&mut app, 80);
        assert!(
            row.contains("worktrees/T-4440f2") && !row.contains("(rex-team"),
            "80: the branch gives way when its tail is too short: {row:?}"
        );
    }

    /// A launch with an explicit workspace names that workspace, not the
    /// process's directory, and the unread badge measures against the same
    /// budget as the place so it cannot push the label out of the border.
    #[test]
    fn the_label_names_the_sessions_workspace_and_the_badge_shares_its_budget() {
        use crate::ui::render::panel::{strip_escapes, visible_width};

        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().to_path_buf(),
            "/tmp/the-launch-directory".into(),
        );
        app.workspace = "~/Projects/repo/dim.worktrees/T-4440f2".into();
        app.branch = Some("rex-team/T-4440f2".into());
        app.fullscreen = false;
        let bottom = |app: &mut App, cols| {
            let frame = app.frame((cols, 30));
            let rows: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
            rows.iter()
                .rev()
                .find(|row| row.contains('╰'))
                .cloned()
                .unwrap_or_default()
        };
        let row = bottom(&mut app, 160);
        assert!(
            row.contains("~/Projects/repo/dim.worktrees/T-4440f2 (rex-team/T-4440f2)"),
            "the label follows the Session's workspace: {row:?}"
        );
        assert!(
            !row.contains("the-launch-directory"),
            "the process's directory must not reach the border: {row:?}"
        );

        let mut snapshot = app.snapshot.clone().unwrap_or_default();
        snapshot.unread_lines.insert("b-7c".into());
        app.snapshot = Some(snapshot);
        for cols in [160u16, 80, 40, 24] {
            let row = bottom(&mut app, cols);
            let width = visible_width(&row);
            assert_eq!(width, usize::from(cols) - 2, "{cols}: {row:?}");
            assert!(
                row.contains("╰") && row.contains("╯"),
                "{cols}: the border stays closed: {row:?}"
            );
        }
        // The place survives beside the badge where the room allows both.
        let row = bottom(&mut app, 160);
        assert!(row.contains("Unread (1)"), "{row:?}");
        assert!(
            row.contains("T-4440f2"),
            "the place keeps its tail: {row:?}"
        );
    }

    /// The reported flow: launched in workspace A, the client attaches a
    /// Session that lives in workspace B (an explicit `--cwd`, a `/resume`
    /// into another worktree). The border must name B, its branch included,
    /// and going Home must put A back. `@` file completion is not the
    /// Session's: interactive local paths keep the selected `--cwd` as their
    /// base, so it offers A's files throughout, and that is asserted here so
    /// the border cannot quietly take the mention root with it.
    #[test]
    fn a_session_attached_elsewhere_moves_the_border_but_not_the_mention_base() {
        use crate::ui::render::panel::{strip_escapes, visible_width};

        let root = tempfile::tempdir().unwrap();
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
        // Long enough that the narrow title's tail cannot hold it whole under
        // any platform's temp root, and short enough that the wide title
        // still names the whole path.
        const LONG: &str = "a-checkout-longer-than-the-narrow-titles-tail-can-hold";
        let launch = root.path().join(LONG).join("A");
        let other = root.path().join("B");
        for (path, branch) in [(&launch, "launch-line"), (&other, "session-line")] {
            std::fs::create_dir_all(path).unwrap();
            git(path, &["init", "-q", "-b", branch]);
            git(
                path,
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
        }
        let launch_workspace = crate::ui::tilde(&launch.to_string_lossy());
        let session_workspace = crate::ui::tilde(&other.to_string_lossy());
        // Each workspace holds a file of its own, so an `@` candidate names
        // the workspace it was found in.
        std::fs::write(launch.join("launch-note.txt"), "").unwrap();
        std::fs::write(other.join("session-note.txt"), "").unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            launch.to_string_lossy().into_owned(),
        );
        let border = |app: &mut App, cols: u16| {
            let frame = app.frame((cols, 30));
            let rows: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
            let row = rows
                .iter()
                .rev()
                .find(|row| row.contains('╰'))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                visible_width(&row),
                usize::from(cols) - 2,
                "{cols}: the border keeps its width: {row:?}"
            );
            row
        };
        // The `@` picker for `@note`: the panel's title row and the file
        // names it offers, read from one frame once the walk reports that it
        // has finished.
        let picker = |app: &mut App, cols: u16| -> (String, Vec<String>) {
            let names = ["launch-note.txt", "session-note.txt"];
            typed(app, "@note");
            let started = Instant::now();
            let rows = loop {
                let frame = app.frame((cols, 30));
                let rows: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
                if !app.composer.mention_searching() {
                    break rows;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "the `@` walk did not finish"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            app.composer.clear();
            // The title row is where the panel writes its heading: the row
            // carrying the `Files` title, whatever its box draws above it.
            let title = rows
                .iter()
                .find(|row| row.contains("Files"))
                .cloned()
                .expect("the picker draws its title");
            let offered = names
                .iter()
                .filter(|name| rows.iter().any(|row| row.contains(*name)))
                .map(|name| (*name).to_owned())
                .collect();
            (title, offered)
        };

        // Launched in A: Home names A and its branch, and `@` offers A's
        // files, the base pasted and dropped paths also use. The picker's
        // title says nothing extra while the border names that base too.
        let row = border(&mut app, 160);
        assert!(
            row.contains(&format!("{launch_workspace} (launch-line)")),
            "Home names the launch workspace: {row:?}"
        );
        let (title, offered) = picker(&mut app, 160);
        assert_eq!(
            offered,
            ["launch-note.txt"],
            "Home offers the launch workspace's files"
        );
        assert!(
            title.contains("Files") && !title.contains(&launch_workspace),
            "Home's border already names the base: {title:?}"
        );

        // A Session in B is attached, as `/resume` into a sibling worktree
        // does: the daemon reports B and the border moves with it.
        app.apply(Update::Switched {
            session_id: "s-b".into(),
            kind: SwitchKind::Resumed,
            cwd: Some(other.to_string_lossy().into_owned()),
        });
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s-b".into(),
            focus: "main".into(),
            ..ViewSnapshot::default()
        })));
        let row = border(&mut app, 160);
        assert!(
            row.contains(&format!("{session_workspace} (session-line)")),
            "the border names the Session's workspace and its branch: {row:?}"
        );
        assert!(
            !row.contains(&launch_workspace),
            "the launch workspace must not stand in for it: {row:?}"
        );
        let (title, offered) = picker(&mut app, 160);
        assert_eq!(
            offered,
            ["launch-note.txt"],
            "the mention picker keeps the local `--cwd` while the border names the Session's workspace"
        );
        assert!(
            title.contains(&format!("Files · {launch_workspace}")),
            "the picker names the base the border no longer does: {title:?}"
        );
        assert!(
            !title.contains(&session_workspace),
            "the picker names the base, not the border's workspace: {title:?}"
        );
        // Too narrow for the whole path, the title keeps its tail, as the
        // border's does, so the checkout it ends in is still readable. The
        // title leaves 54 path cells at 80 columns, and the fixture's path
        // outgrows them under any platform's temp root.
        const NARROW: u16 = 80;
        use std::path::MAIN_SEPARATOR;
        let (narrow, _) = picker(&mut app, NARROW);
        let named = narrow
            .split("Files · ")
            .nth(1)
            .unwrap_or_default()
            .split("esc close")
            .next()
            .unwrap_or_default()
            .trim_end();
        assert!(
            named.starts_with('…'),
            "a narrow title cuts the path's front: {narrow:?}"
        );
        assert!(
            named.ends_with(&format!("{MAIN_SEPARATOR}A")),
            "a narrow title keeps the checkout it ends in: {narrow:?}"
        );
        assert!(
            !narrow.contains(LONG),
            "a narrow title keeps the tail, not the whole path: {narrow:?}"
        );

        // The daemon may spell the same directory another way. A trailing
        // separator changes nothing, so the picker stays plain: the border
        // already names that base.
        let spelled = format!("{}{MAIN_SEPARATOR}", launch.to_string_lossy());
        app.apply(Update::Switched {
            session_id: "s-a".into(),
            kind: SwitchKind::Resumed,
            cwd: Some(spelled),
        });
        let row = border(&mut app, 160);
        assert!(
            row.contains(&launch_workspace) && row.contains("(launch-line)"),
            "the border names that base and its branch: {row:?}"
        );
        let (title, offered) = picker(&mut app, 160);
        assert_eq!(
            offered,
            ["launch-note.txt"],
            "the picker keeps the launch workspace's files"
        );
        assert!(
            !title.contains(&launch_workspace),
            "the same directory in another spelling is not another base: {title:?}"
        );

        // `/new` goes Home: the Session is gone and the launch workspace is
        // what the next Session will work in.
        app.apply(Update::Switched {
            session_id: String::new(),
            kind: SwitchKind::New,
            cwd: None,
        });
        let row = border(&mut app, 160);
        assert!(
            row.contains(&format!("{launch_workspace} (launch-line)")),
            "Home moves the border back to the launch workspace: {row:?}"
        );
        let (title, offered) = picker(&mut app, 160);
        assert_eq!(
            offered,
            ["launch-note.txt"],
            "Home keeps the mention picker on the launch workspace"
        );
        assert!(
            !title.contains(&launch_workspace),
            "Home's border names the base again, so the picker drops it: {title:?}"
        );
    }

    /// The workspace label steps down through four forms in the border
    /// instead of vanishing, and the border keeps its width through all of
    /// them: the whole label, the whole label with the path's front cut while
    /// its tail is still worth reading, the path alone, then the path alone
    /// cut. Each workspace is attached the way `/resume` attaches one, so the
    /// path and the branch are the daemon's answer rather than a field a test
    /// filled in. The border gives a right label `cols - 9` cells: the margin,
    /// the two border cells, and the `─ ` and ` ─` around it.
    #[test]
    fn the_workspace_label_steps_down_in_four_forms_in_the_border() {
        use crate::ui::render::panel::{strip_escapes, visible_width};

        /// The cells the border keeps for itself around a right label.
        const BORDER: usize = 9;
        /// The narrowest path tail worth putting a branch beside.
        const TAIL: usize = 8;

        let root = tempfile::tempdir().unwrap();
        let repository = |name: &str, branch: &str| {
            let path = root.path().join(name);
            std::fs::create_dir(&path).unwrap();
            let git = |args: &[&str]| {
                let output = std::process::Command::new("git")
                    .current_dir(&path)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&["init", "-q", "-b", branch]);
            git(&[
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
            ]);
            crate::ui::tilde(&path.to_string_lossy())
        };

        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            root.path().join("launch").to_string_lossy().into_owned(),
        );
        // The row the border draws the workspace in: the label sits between
        // the `─ ` and ` ─` the border keeps around it.
        let label_row = |app: &mut App, cells: usize| {
            let cols = (cells + BORDER) as u16;
            let frame = app.frame((cols, 30));
            let rows: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
            let row = rows
                .iter()
                .rev()
                .find(|row| row.contains('╰'))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                visible_width(&row),
                usize::from(cols) - 2,
                "{cols}: the border keeps its width whatever the label: {row:?}"
            );
            row
        };
        let label = |app: &mut App, cells: usize| {
            let row = label_row(app, cells);
            let label = row
                .split("─ ")
                .nth(1)
                .and_then(|rest| rest.split(" ─").next())
                .unwrap_or_default()
                .to_owned();
            assert!(
                visible_width(&label) <= cells,
                "{cells}: the label stays inside its cells: {label:?}"
            );
            label
        };
        let attach = |app: &mut App, id: &str, cwd: &str| {
            app.apply(Update::Switched {
                session_id: id.into(),
                kind: SwitchKind::Resumed,
                cwd: Some(cwd.into()),
            });
        };

        // A branch wider than the path, so the path-alone rung is reachable:
        // it takes over once the tail beside the branch would be too short.
        let path = root.path().join("checkout").to_string_lossy().into_owned();
        let branch = format!("team/{}", "x".repeat(width_of(&crate::ui::tilde(&path))));
        let path = repository("checkout", &branch);
        let suffix = format!(" ({branch})");
        let whole = format!("{path}{suffix}");
        assert!(
            width_of(&suffix) + TAIL > width_of(&path),
            "this repository shows the path-alone rung"
        );
        attach(&mut app, "s-a", &path);

        // The whole label, at the last column that holds it.
        assert_eq!(label(&mut app, width_of(&whole)), whole);
        // One column short: the path's front is cut and the branch stays.
        let cut = label(&mut app, width_of(&whole) - 1);
        assert!(cut.ends_with(&suffix), "the branch stays: {cut:?}");
        assert!(cut.starts_with('…'), "the cut is marked: {cut:?}");
        // A tail of exactly eight cells is still worth reading beside it.
        let tail = label(&mut app, width_of(&suffix) + TAIL);
        assert!(
            tail.ends_with(&suffix),
            "an eight-cell tail keeps it: {tail:?}"
        );
        // Seven is not: the branch gives way, and the path is all there is.
        let without = label(&mut app, width_of(&suffix) + TAIL - 1);
        assert!(
            !without.contains(&branch),
            "the branch gives way: {without:?}"
        );
        // The path alone, whole at its own width, then cut from the front.
        assert_eq!(
            label(&mut app, width_of(&path)),
            path,
            "the whole path stays when the branch gives way"
        );
        let cut = label(&mut app, width_of(&path) - 1);
        assert!(
            cut.starts_with('…') && cut != path,
            "the path is cut: {cut:?}"
        );
        // No form fits: with a cell left the label is the cut mark alone,
        // and only a border with no cells at all drops it.
        assert_eq!(label(&mut app, 1), "\u{2026}");
        assert!(label(&mut app, 0).is_empty());

        // The budget is in cells, not characters: a CJK path spends two cells
        // on a character, so the same columns hold fewer of them.
        let ascii = repository("ascii", "main");
        let cjk = repository("项目", "main");
        assert!(
            width_of(&cjk) > cjk.chars().count(),
            "{cjk:?} must spend two cells on a character"
        );
        let tail_of = |app: &mut App, path: &str| {
            attach(app, "s-b", path);
            let tail = label(app, width_of(" (main)") + TAIL);
            assert!(tail.ends_with(" (main)"), "{tail:?}");
            tail
        };
        let ascii_tail = tail_of(&mut app, &ascii);
        let cjk_tail = tail_of(&mut app, &cjk);
        assert_eq!(
            width_of(&ascii_tail),
            width_of(&cjk_tail),
            "both tails spend the same cells: {ascii_tail:?} / {cjk_tail:?}"
        );
        assert!(
            cjk_tail.chars().count() < ascii_tail.chars().count(),
            "the CJK tail holds fewer characters at the same width: \
             {cjk_tail:?} / {ascii_tail:?}"
        );
        assert!(
            cjk_tail.starts_with('…'),
            "the CJK tail is cut, not dropped: {cjk_tail:?}"
        );
    }

    /// A shown line outranks the workspace in the border: where the whole
    /// place fits it is drawn whole, one column short of that it keeps `⑂`
    /// alone, and where even that does not fit the border drops the label
    /// rather than falling back to a shortened path.
    #[test]
    fn a_shown_line_keeps_its_forms_in_the_border_and_comes_before_the_workspace() {
        use crate::ui::render::panel::{strip_escapes, visible_width};

        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/w".into(),
        );
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            focus: "b-7c".into(),
            lines: vec![LineView {
                id: "main".into(),
                parent: None,
                purpose: String::new(),
                state: LineState::Main,
                cut: None,
            }],
            ..ViewSnapshot::default()
        })));
        let border = |app: &mut App, cols: u16| {
            let frame = app.frame((cols, 30));
            let rows: Vec<String> = frame.live.iter().map(|row| strip_escapes(row)).collect();
            let row = rows
                .iter()
                .rev()
                .find(|row| row.contains('╰'))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                visible_width(&row),
                usize::from(cols) - 2,
                "{cols}: the border keeps its width: {row:?}"
            );
            row
        };
        // `/w · ⑂ b-7c` is 11 cells and `⑂ b-7c` is 6, so the border's nine
        // fixed cells put the whole place at 20 columns and `⑂` alone at 15:
        // one column short of the line's own width drops it.
        let row = border(&mut app, 20);
        assert!(row.contains("/w · ⑂ b-7c"), "{row:?}");
        let row = border(&mut app, 19);
        assert!(
            row.contains("⑂ b-7c") && !row.contains("/w"),
            "a column short, the line keeps itself and the path gives way: {row:?}"
        );
        let row = border(&mut app, 15);
        assert!(
            row.contains("⑂ b-7c") && !row.contains("/w"),
            "the `⑂` form survives to its own width: {row:?}"
        );
        let row = border(&mut app, 14);
        assert!(
            !row.contains('⑂'),
            "below the line's own form the border drops it: {row:?}"
        );
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
    fn legacy_tool_preferences_survive_until_the_three_state_setting_is_saved() {
        for (legacy, initial, changed) in [
            (false, "collapsed", "summary"),
            (true, "expanded", "collapsed"),
        ] {
            let mut drawn = Drawn::new((100, 40), None, true);
            assert_eq!(
                crate::ui::config::write(
                    drawn._home.path(),
                    LEGACY_TOOL_DETAILS_KEY,
                    legacy.into()
                ),
                None
            );
            for expected in [initial, changed] {
                let theme = test_theme();
                drawn.app = App::new(
                    Box::new(Idle),
                    MarkdownRenderer::new(&theme),
                    theme.palette,
                    drawn._home.path().to_path_buf(),
                    "/work".into(),
                );
                drawn.app.open_settings(Setting::ToolDetails);
                let frame = drawn.app.frame(drawn.size);
                let settings = crate::ui::render::panel::strip_escapes(&frame.live.join("\n"));
                assert!(
                    settings
                        .lines()
                        .any(|line| line.contains("Tool details") && line.contains(expected)),
                    "{settings}"
                );
                if expected == initial {
                    press(&mut drawn.app, KeyCode::Right);
                    assert_eq!(
                        crate::ui::config::read(drawn._home.path(), TOOL_DETAILS_KEY),
                        Some(changed.into())
                    );
                }
            }
        }
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

    /// An unfinished command task named `label`.
    fn background_task(id: &str, label: &str) -> crate::model::view::TaskView {
        crate::model::view::TaskView {
            id: id.into(),
            branch_id: "main".into(),
            kind: crate::model::view::TaskKind::Command(TaskTarget {
                branch_id: "main".into(),
                task_id: id.into(),
            }),
            label: label.into(),
            started_at: 0,
            finished: None,
        }
    }

    /// A task the live stream ended with `outcome`.
    fn finished(
        mut task: crate::model::view::TaskView,
        outcome: TaskOutcome,
    ) -> crate::model::view::TaskView {
        task.finished = Some(crate::model::view::TaskFinish {
            at: 1,
            status: match outcome {
                TaskOutcome::Completed => "Completed",
                TaskOutcome::Failed => "Failed",
                TaskOutcome::Stopped => "Cancelled",
            },
            outcome,
            replayed: false,
            text: String::new(),
            exit_code: None,
        });
        task
    }

    /// A task that ended at `at_ms`, Unix milliseconds.
    fn ended_at(
        mut task: crate::model::view::TaskView,
        at_ms: u64,
    ) -> crate::model::view::TaskView {
        task.finished = Some(crate::model::view::TaskFinish {
            at: at_ms,
            status: "Completed",
            outcome: TaskOutcome::Completed,
            replayed: false,
            text: String::new(),
            exit_code: None,
        });
        task
    }

    #[test]
    fn the_task_line_names_running_tasks_and_fades_an_ended_one() {
        use crate::ui::render::panel::strip_escapes;
        let p = &test_theme().palette;
        let now = 1_000;
        let running = background_task("t1", "cargo build");
        let mut running = running;
        running.started_at = (now - 72) * 1000;
        let line = task_line(shown_tasks(&[running.clone()], now), now, 60, p)
            .expect("a running task draws a line");
        let plain = strip_escapes(&line);
        assert_eq!(plain, "◐ 1 · cargo build 1m12s", "{plain}");

        let mut second = background_task("t2", "cargo test");
        second.started_at = (now - 34) * 1000;
        let line = task_line(
            shown_tasks(&[running.clone(), second.clone()], now),
            now,
            60,
            p,
        )
        .expect("two tasks draw one line");
        let plain = strip_escapes(&line);
        assert_eq!(plain, "◐ 2 · cargo build 1m12s · cargo test 34s", "{plain}");

        let ended = ended_at(running, now * 1000);
        let both = [ended.clone(), second.clone()];
        let shown = shown_tasks(&both, now);
        let plain = strip_escapes(&task_line(shown.clone(), now, 60, p).expect("a line"));
        assert!(plain.starts_with("◐ 1 · ✓ cargo build 1m12s"), "{plain}");
        assert!(plain.contains("cargo test 34s"), "{plain}");

        // An ended task that has not faded yet is the only one the row
        // shows, so it carries no count.
        let ended_now = |task, outcome| {
            let mut task = finished(task, outcome);
            task.finished.as_mut().expect("a finish").at = now * 1000;
            task
        };
        let mut failed = ended_now(background_task("t3", "cargo build"), TaskOutcome::Failed);
        failed.started_at = (now - 72) * 1000;
        let plain =
            strip_escapes(&task_line(shown_tasks(&[failed], now), now, 60, p).expect("a line"));
        assert!(plain.starts_with("✗ cargo build 1m12s"), "{plain}");

        let mut stopped = ended_now(background_task("t4", "cargo test"), TaskOutcome::Stopped);
        stopped.started_at = (now - 12) * 1000;
        let plain =
            strip_escapes(&task_line(shown_tasks(&[stopped], now), now, 60, p).expect("a line"));
        assert_eq!(plain, "⊘ cargo test 12s", "{plain}");

        // Five seconds after the end the ended task leaves; nothing else runs.
        let later = now + TASK_FADE;
        assert!(
            shown_tasks(&[ended], later).is_empty(),
            "an ended task fades out"
        );
        assert!(task_line(Vec::new(), later, 60, p).is_none());
    }

    /// With nothing running the count has nothing to say, so the row starts
    /// at the first entry's mark and carries no leading separator; a narrow
    /// row still names that task.
    #[test]
    fn a_row_of_ended_tasks_starts_at_the_first_mark_without_a_count() {
        use crate::ui::render::panel::{strip_escapes, visible_width};
        let p = &test_theme().palette;
        let now = 1_000;
        let ended_now = |task, outcome| {
            let mut task = finished(task, outcome);
            task.finished.as_mut().expect("a finish").at = now * 1000;
            task
        };
        let mut first = ended_now(
            background_task("t1", "cargo build --all-targets --locked"),
            TaskOutcome::Completed,
        );
        first.started_at = (now - 72) * 1000;
        let mut second = ended_now(
            background_task("t2", "cargo test --workspace --locked"),
            TaskOutcome::Stopped,
        );
        second.started_at = (now - 34) * 1000;

        let tasks = [first.clone(), second.clone()];
        let plain =
            strip_escapes(&task_line(shown_tasks(&tasks, now), now, 100, p).expect("a line"));
        assert_eq!(
            plain,
            "✓ cargo build --all-targets --locked 1m12s · ⊘ cargo test --workspace --locked 34s",
            "{plain}"
        );

        for width in [20, 30, 40, 100] {
            let line = strip_escapes(
                &task_line(shown_tasks(&[first.clone()], now), now, width, p).expect("a line"),
            );
            assert!(!line.starts_with(" ·"), "{width}: {line:?}");
            assert!(line.starts_with("✓ "), "{width}: {line:?}");
            assert!(visible_width(&line) <= width, "{width}: {line:?}");
            assert!(line.contains("cargo"), "{width}: {line:?}");
        }
    }

    #[test]
    fn a_narrow_task_line_cuts_labels_and_still_names_the_first_task() {
        use crate::ui::render::panel::{strip_escapes, visible_width};
        let p = &test_theme().palette;
        let now = 100;
        let mut first = background_task("t1", "cargo build --all-targets --locked");
        first.started_at = 0;
        let mut second = background_task("t2", "cargo test --workspace --locked");
        second.started_at = 0;
        for width in [20, 30, 40, 120] {
            let line = task_line(
                shown_tasks(&[first.clone(), second.clone()], now),
                now,
                width,
                p,
            )
            .expect("a line");
            assert!(
                visible_width(&line) <= width,
                "{width}: too wide: {:?}",
                strip_escapes(&line)
            );
            let plain = strip_escapes(&line);
            assert!(plain.contains("cargo"), "{width}: {plain:?}");
        }
    }

    #[test]
    fn the_dock_draws_the_task_line_above_the_composer_and_enter_opens_the_tasks() {
        use crate::model::session::Command;
        use crate::ui::render::panel::strip_escapes;
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let theme = test_theme();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let mut task = background_task("t1", "cargo build");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        task.started_at = (now - 72) * 1000;
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            focus: "main".into(),
            tasks: vec![task.clone()],
            ..ViewSnapshot::default()
        })));
        for fullscreen in [false, true] {
            app.fullscreen = fullscreen;
            let rows: Vec<String> = app
                .frame((80, 30))
                .live
                .iter()
                .map(|row| strip_escapes(row))
                .collect();
            let line = rows
                .iter()
                .position(|row| row.contains("cargo build 1m12s"))
                .unwrap_or_else(|| panic!("{fullscreen}: no task line in {rows:?}"));
            let composer = rows
                .iter()
                .position(|row| row.contains('╭'))
                .expect("the composer's top border");
            assert!(line < composer, "{fullscreen}: the line sits above it");
        }

        app.fullscreen = false;
        app.handle_input(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
            KeyCode::Enter,
            Modifiers::NONE,
        )));
        assert!(
            matches!(app.prompt, Some(Prompt::Tasks(_))),
            "Enter on the line opens /tasks"
        );
        assert!(
            sent.borrow()
                .iter()
                .any(|command| matches!(command, Command::TaskHistory { .. })),
            "the panel reads the Session's task history"
        );
    }

    /// The row's end is the pulse's doing: an ended task keeps its place while
    /// it fades, and the tick that finds it gone asks for the frame that
    /// erases the row, so the composer never keeps a row nothing draws into.
    #[test]
    fn the_pulse_asks_for_the_frame_that_erases_an_ended_task_row() {
        let theme = test_theme();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let snapshot = |tasks| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                tasks,
                ..ViewSnapshot::default()
            }))
        };
        let fading = ended_at(background_task("t1", "cargo build"), (now - 1) * 1000);
        app.apply(snapshot(vec![fading]));
        assert!(app.task_line_shown(), "an ended task stays while it fades");
        assert!(app.tick(), "the pulse repaints the fading row");
        assert!(app.task_row, "the frame it asked for drew the row");

        // The end is older than the fade: nothing draws the row, and the
        // pulse still asks for the frame that erases what it last drew. The
        // pulse runs once a second, so the recorded second is moved back the
        // way the clock would.
        let gone = ended_at(
            background_task("t2", "cargo test"),
            (now - TASK_FADE) * 1000,
        );
        app.apply(snapshot(vec![gone]));
        assert!(!app.task_line_shown(), "an ended task leaves");
        app.task_second -= 1;
        assert!(app.tick(), "the pulse asks for the erasing frame");
        assert!(!app.task_row, "and then stops asking");
    }

    /// A task whose end a read of recorded history applied, as the attach
    /// read and every history page do.
    fn replayed(
        mut task: crate::model::view::TaskView,
        outcome: TaskOutcome,
    ) -> crate::model::view::TaskView {
        task = finished(task, outcome);
        if let Some(finish) = &mut task.finished {
            finish.replayed = true;
        }
        task
    }

    #[test]
    fn each_background_task_is_reported_ended_once() {
        use crate::ui::notify::Event;
        let theme = test_theme();
        let home = tempfile::tempdir().unwrap();
        let build = |notify: bool| {
            let mut app = App::new(
                Box::new(Idle),
                MarkdownRenderer::new(&theme),
                theme.palette.clone(),
                home.path().to_path_buf(),
                "/work".into(),
            );
            app.notify = notify;
            app
        };
        let snapshot = |tasks: Vec<crate::model::view::TaskView>| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                tasks,
                ..ViewSnapshot::default()
            }))
        };
        let mut app = build(true);
        app.apply(snapshot(vec![background_task("t1", "cargo build")]));
        assert!(app.notifications.is_empty());

        let ended = vec![finished(
            background_task("t1", "cargo build"),
            TaskOutcome::Failed,
        )];
        app.apply(snapshot(ended.clone()));
        assert_eq!(
            app.notifications,
            vec![Event::BackgroundFinished {
                label: "cargo build".into(),
                outcome: TaskOutcome::Failed,
            }]
        );

        app.notifications.clear();
        app.apply(snapshot(ended));
        assert!(app.notifications.is_empty(), "{:?}", app.notifications);

        let mut quiet = build(false);
        quiet.apply(snapshot(vec![background_task("t2", "cargo test")]));
        quiet.apply(snapshot(vec![finished(
            background_task("t2", "cargo test"),
            TaskOutcome::Completed,
        )]));
        assert!(quiet.notifications.is_empty(), "the switch is respected");
    }

    /// A finish a read applied is history: the attach read and every task
    /// history page replay the ends a Session already recorded, and none of
    /// them is news. Only an end the live stream carries after attach is,
    /// whatever snapshot happens to show it first.
    #[test]
    fn a_replayed_end_is_history_and_a_live_one_is_news() {
        use crate::ui::notify::Event;
        let theme = test_theme();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let snapshot = |tasks: Vec<crate::model::view::TaskView>| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                tasks,
                ..ViewSnapshot::default()
            }))
        };

        // The worker publishes an empty or loading snapshot before the
        // delayed history reply lands.
        app.apply(snapshot(Vec::new()));
        assert!(app.notifications.is_empty());

        app.apply(snapshot(vec![replayed(
            background_task("t1", "already done"),
            TaskOutcome::Completed,
        )]));
        assert!(
            app.notifications.is_empty(),
            "a restored end is history: {:?}",
            app.notifications
        );

        app.apply(snapshot(vec![
            replayed(
                background_task("t1", "already done"),
                TaskOutcome::Completed,
            ),
            finished(background_task("t2", "live now"), TaskOutcome::Stopped),
        ]));
        assert_eq!(
            app.notifications,
            vec![Event::BackgroundFinished {
                label: "live now".into(),
                outcome: TaskOutcome::Stopped,
            }],
            "a stop the live stream carried is one notification"
        );
    }

    /// Switching away from a Session and back re-reads its history: those
    /// ends are replayed and raise nothing, while a task that ends after the
    /// return still does.
    #[test]
    fn returning_to_a_session_reports_none_of_its_ended_tasks_again() {
        use crate::model::session::SwitchKind;
        use crate::ui::notify::Event;
        let theme = test_theme();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let snapshot = |session: &str, tasks| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: session.into(),
                tasks,
                ..ViewSnapshot::default()
            }))
        };
        let ended = replayed(background_task("t1", "cargo build"), TaskOutcome::Completed);

        app.apply(snapshot("s1", vec![background_task("t1", "cargo build")]));
        app.apply(snapshot(
            "s1",
            vec![finished(
                background_task("t1", "cargo build"),
                TaskOutcome::Completed,
            )],
        ));
        assert_eq!(
            app.notifications,
            vec![Event::BackgroundFinished {
                label: "cargo build".into(),
                outcome: TaskOutcome::Completed,
            }]
        );

        app.notifications.clear();
        app.apply(Update::Switched {
            session_id: "s2".into(),
            kind: SwitchKind::Resumed,
            cwd: None,
        });
        app.apply(snapshot(
            "s2",
            vec![replayed(
                background_task("u1", "cargo test"),
                TaskOutcome::Completed,
            )],
        ));
        assert!(
            app.notifications.is_empty(),
            "another Session's history is not news: {:?}",
            app.notifications
        );

        app.apply(Update::Switched {
            session_id: "s1".into(),
            kind: SwitchKind::Resumed,
            cwd: None,
        });
        app.apply(snapshot("s1", vec![ended.clone()]));
        assert!(
            app.notifications.is_empty(),
            "coming back replays its ends instead of reporting them: {:?}",
            app.notifications
        );

        app.apply(snapshot(
            "s1",
            vec![
                ended,
                finished(background_task("t2", "cargo run"), TaskOutcome::Failed),
            ],
        ));
        assert_eq!(
            app.notifications,
            vec![Event::BackgroundFinished {
                label: "cargo run".into(),
                outcome: TaskOutcome::Failed,
            }],
            "a task that ends after the return still reports"
        );
    }

    #[test]
    fn the_background_count_survives_a_running_turn() {
        use crate::ui::render::panel::strip_escapes;
        let theme = test_theme();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Box::new(Idle),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            tasks: vec![
                background_task("t1", "cargo build"),
                background_task("t2", "cargo test"),
            ],
            ..ViewSnapshot::default()
        })));
        let line = strip_escapes(&app.status_line(160));
        assert!(line.contains("esc interrupt"), "{line}");
        assert!(line.contains("2 in background · /tasks"), "{line}");

        let docked = app
            .dock_status(Some("· working…  1s"))
            .expect("a status corner");
        let docked = strip_escapes(&docked);
        assert!(docked.contains("esc interrupt"), "{docked}");
        assert!(docked.contains("2 in background · /tasks"), "{docked}");

        let mut no_tasks = ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            ..ViewSnapshot::default()
        };
        app.apply(Update::Snapshot(Box::new(no_tasks.clone())));
        let line = strip_escapes(&app.status_line(160));
        assert!(line.contains("esc interrupt"), "{line}");
        assert!(!line.contains("in background"), "{line}");

        no_tasks.tasks = vec![background_task("t3", "cargo fmt")];
        app.apply(Update::Snapshot(Box::new(no_tasks)));
        assert!(
            strip_escapes(&app.status_line(160)).contains("1 in background · /tasks"),
            "a new task counts again"
        );
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
            assert!(settings.contains("Default send mode"), "{settings}");
            assert!(!settings.contains("File access"), "{settings}");
            assert!(!settings.contains("Tool approval"), "{settings}");
            assert!(!settings.contains("Execution"), "{settings}");
            assert!(!settings.contains("Model"), "{settings}");
            drawn.key(KeyCode::End);
            let settings = screen(&mut drawn);
            assert!(settings.contains("Process isolation"), "{settings}");
            assert!(settings.contains("Permission"), "{settings}");
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
    fn the_offer_truncates_to_the_composer_and_hides_below_twenty_columns() {
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent)),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        app.apply(Update::SuggestionOffer {
            turn_id: "turn".into(),
            text: "run the tests and commit the change".into(),
        });
        let live = |app: &mut App, cols: u16, rows: u16| {
            crate::ui::render::panel::strip_escapes(&app.frame((cols, rows)).live.join("\n"))
        };
        assert!(live(&mut app, 80, 24).contains("run the tests and commit the change"));
        assert!(live(&mut app, 30, 24).contains("run the tests and co…"));
        let narrow = live(&mut app, 19, 24);
        assert!(!narrow.contains("run the tests"), "{narrow}");
        assert!(narrow.contains("Ask anythi"), "{narrow}");
    }

    #[test]
    fn the_prompt_suggestion_row_shows_the_daemons_value_and_writes_the_flip() {
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
        app.apply(Update::SuggestionMethods(true));
        app.run_command("settings", "");
        assert!(matches!(
            sent.borrow().first(),
            Some(Command::ReadSuggestionSetting)
        ));
        app.apply(Update::SuggestionSetting(Ok(SuggestionSetting {
            revision: "r1".into(),
            document: dim_agent_capabilities::prompt_suggestion::PromptSuggestionSettings {
                enabled: false,
                max_chars: 80,
            },
        })));
        let row = |app: &mut App| {
            let live =
                crate::ui::render::panel::strip_escapes(&app.frame((100, 40)).live.join("\n"));
            let line = live
                .lines()
                .find(|line| line.contains("Prompt suggestion"))
                .expect("the switch's row")
                .to_owned();
            line.split("Prompt suggestion")
                .nth(1)
                .expect("the row's value")
                .trim_start()
                .to_owned()
        };
        assert!(row(&mut app).starts_with("off"));
        for _ in 0..9 {
            app.handle_input(key(KeyCode::Down));
        }
        app.handle_input(key(KeyCode::Enter));
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::SaveSuggestionSetting {
                expected_revision,
                enabled: true,
                max_chars: 80,
            }) if expected_revision == "r1"
        ));
        app.apply(Update::SuggestionSetting(Ok(SuggestionSetting {
            revision: "r2".into(),
            document: dim_agent_capabilities::prompt_suggestion::PromptSuggestionSettings {
                enabled: true,
                max_chars: 80,
            },
        })));
        assert!(row(&mut app).starts_with("on"));
    }

    /// The prompt rows show each text's origin, and Enter and `r` reach the
    /// editor and the reset without a new slash command.
    #[test]
    fn settings_prompt_rows_show_origin_and_edit_or_reset() {
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
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        app.apply(Update::PromptSettings {
            result: Ok(PromptSetting {
                revision: "r1".into(),
                document: serde_json::json!({"summary": "S"}),
            }),
            field: None,
            request: None,
        });
        app.run_command("settings", "");
        let live = strip_escapes(&app.frame((100, 40)).live.join("\n"));
        for row in ["Base prompt", "Compaction prompt", "Skills instructions"] {
            assert!(live.contains(row), "{row} is missing: {live}");
        }
        assert!(live.contains("custom"), "{live}");
        for _ in 0..9 {
            app.handle_input(key(KeyCode::Down));
        }
        assert!(matches!(
            app.handle_input(key(KeyCode::Enter)),
            Outcome::EditPrompt {
                field: PromptField::Base,
                ..
            }
        ));
        app.handle_input(key(KeyCode::Down));
        app.handle_input(key(KeyCode::Char('r')));
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::ResetPromptSetting {
                field: PromptField::Summary,
                ..
            })
        ));
    }

    /// A read reply never settles a prompt write; only the write's own reply
    /// may remove its draft.
    #[test]
    fn a_read_or_unrelated_reply_leaves_a_pending_prompt_alone() {
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent)),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        let draft = editor::Draft::open_text(home.path(), "base", "md", "draft").unwrap();
        let path = draft.path().to_path_buf();
        app.pending_prompt = Some(PendingPrompt {
            request: 7,
            field: PromptField::Base,
            draft: Some(draft),
        });
        app.apply(Update::PromptSettings {
            result: Ok(PromptSetting {
                revision: "r2".into(),
                document: serde_json::json!({}),
            }),
            field: None,
            request: None,
        });
        assert!(app.pending_prompt.is_some(), "a read settled the write");
        assert!(path.exists(), "a read removed the draft");
        app.apply(Update::PromptSettings {
            result: Ok(PromptSetting {
                revision: "r3".into(),
                document: serde_json::json!({}),
            }),
            field: Some(PromptField::Base),
            request: Some(6),
        });
        assert!(app.pending_prompt.is_some(), "another write settled it");
        assert!(path.exists(), "another write removed the draft");
    }

    /// A reset occupies the slot, so an edit cannot start until its reply.
    #[test]
    fn a_reset_is_serialized_with_a_later_edit() {
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
        app.prompt_setting = Some(PromptSetting {
            revision: "r1".into(),
            document: serde_json::json!({"summary": "S"}),
        });
        let _ = app.apply_effect(Effect::ResetPrompt(PromptField::Summary));
        assert!(
            app.pending_prompt.is_some(),
            "reset did not occupy the slot"
        );
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::ResetPromptSetting {
                field: PromptField::Summary,
                ..
            })
        ));
        assert!(matches!(
            app.apply_effect(Effect::EditPrompt(PromptField::Summary)),
            Outcome::Continue
        ));
    }

    /// The skills warning is visible while the row is selected, before Enter
    /// opens the editor.
    #[test]
    fn settings_skills_row_shows_the_warning_before_enter() {
        use crate::ui::render::panel::strip_escapes;

        let mut drawn = Drawn::new((100, 40), None, true);
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        drawn.app.apply(Update::PromptSettings {
            result: Ok(PromptSetting {
                revision: "r1".into(),
                document: serde_json::json!({}),
            }),
            field: None,
            request: None,
        });
        drawn.app.run_command("settings", "");
        for _ in 0..11 {
            drawn.app.handle_input(key(KeyCode::Down));
        }
        let live = strip_escapes(&drawn.app.frame((100, 40)).live.join("\n"));
        assert!(live.contains("skill tool and list stay"), "{live}");
    }

    /// A read that fails shows that the source is unavailable rather than a
    /// value the client invented.
    #[test]
    fn a_failed_suggestion_read_shows_no_value() {
        let home = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent)),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            home.path().to_path_buf(),
            "/work".into(),
        );
        app.apply(Update::SuggestionMethods(true));
        app.run_command("settings", "");
        app.apply(Update::SuggestionSetting(Err(
            "prompt suggestion settings: missing field `maxChars`".into(),
        )));
        let live = crate::ui::render::panel::strip_escapes(&app.frame((100, 40)).live.join("\n"));
        let row = live
            .lines()
            .find(|line| line.contains("Prompt suggestion"))
            .expect("the switch's row");
        assert!(
            row.contains(t("Not available")),
            "the row shows a value: {row}"
        );
    }

    #[test]
    fn process_isolation_select_and_save() {
        use crate::model::session::ExecutionSnapshot;
        for (command, via_permissions) in [
            ("settings", false),
            ("permissions", false),
            ("settings", true),
        ] {
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
            let key =
                |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
            app.run_command(command, "");
            if command == "settings" {
                app.handle_input(key(KeyCode::End));
                if via_permissions {
                    app.handle_input(key(KeyCode::Up));
                }
            } else {
                for _ in 0..4 {
                    app.handle_input(key(KeyCode::Down));
                }
            }
            app.handle_input(key(KeyCode::Enter));
            if via_permissions {
                for _ in 0..4 {
                    app.handle_input(key(KeyCode::Down));
                }
                app.handle_input(key(KeyCode::Enter));
            }
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
            app.apply(Update::Isolation(Ok(ExecutionSnapshot {
                revision: "r2".into(),
                sandbox: true,
                network: false,
            })));
            app.handle_input(key(KeyCode::Esc));
            let rendered = app.frame((112, 40));
            let frame = rendered
                .overlay
                .as_ref()
                .map_or(&rendered.live, |overlay| &overlay.rows)
                .join("\n");
            let parent = if command == "permissions" || via_permissions {
                "Command rules"
            } else {
                "Settings"
            };
            assert!(frame.contains(parent), "{frame}");
            if via_permissions {
                app.handle_input(key(KeyCode::Esc));
                assert!(matches!(app.prompt, Some(Prompt::Settings(_))));
            }
        }
    }

    #[test]
    fn permissions_focus_tracks_hints_and_survives_policy_updates_and_submenus() {
        use dim_agent_types::{PermissionPolicy, PermissionPreset};

        for fullscreen in [false, true] {
            for size in [(100, 32), (40, 24), (32, 12)] {
                let action_hint = |text: &str, action: &str| {
                    let hint = if size.1 >= 24 { action } else { "Esc close" };
                    assert!(text.contains(hint), "{hint} is missing: {text}");
                };
                for from_settings in [false, true] {
                    let mut drawn = Drawn::new(size, None, true);
                    drawn.app.fullscreen = fullscreen;
                    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                    drawn.app.host = Box::new(Recorder(sent.clone()));
                    if from_settings {
                        drawn.app.open_settings(Setting::Permission);
                        drawn.key(KeyCode::Enter);
                    } else {
                        typed(&mut drawn.app, "/permissions");
                        drawn.key(KeyCode::Enter);
                    }
                    let render = |drawn: &mut Drawn| {
                        let frame = drawn.app.frame(size);
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
                        let mut emu =
                            crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                        emu.feed(&bytes);
                        emu.rows_text()
                            .join("\n")
                            .replace('│', " ")
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    let initial = render(&mut drawn);
                    assert!(initial.contains("(*) Workspace write"), "{initial}");
                    action_hint(&initial, "Enter keep mode");
                    if size.0 == 100 {
                        assert!(initial.contains("Permission mode"));
                        assert!(initial.contains("Related settings"));
                    }
                    assert!(!initial.contains("This session"));
                    assert!(!initial.contains("All sessions"));
                    if size.1 >= 24 {
                        assert!(initial.contains("Current session; remembered for new sessions."));
                    }
                    drawn.key(KeyCode::Home);
                    drawn.key(KeyCode::Up);
                    let read_only = render(&mut drawn);
                    action_hint(&read_only, "Enter use mode");
                    if size.1 >= 24 {
                        assert!(read_only.contains("Writes and commands"));
                        assert!(read_only.contains("denied;"));
                        assert!(read_only.contains("external reads may"));
                        assert!(!read_only.contains("Workspace writes allowed;"));
                        assert_eq!(read_only.matches("Hint:").count(), 1);
                    }
                    drawn.key(KeyCode::Tab);
                    action_hint(&render(&mut drawn), "Enter keep mode");
                    drawn.key(KeyCode::Down);
                    let full = render(&mut drawn);
                    action_hint(&full, "Enter use mode");
                    drawn.app.apply(Update::Permissions(Ok(PermissionPolicy {
                        preset: PermissionPreset::FullAccess,
                        ..Default::default()
                    })));
                    let refreshed = render(&mut drawn);
                    assert!(refreshed.contains("(*) Full access"), "{refreshed}");
                    action_hint(&refreshed, "Enter keep mode");
                    if size.1 >= 24 {
                        assert!(refreshed.contains("Skips preset approval;"));
                        assert!(!refreshed.contains("Writes and commands denied;"));
                    }
                    drawn.key(KeyCode::Down);
                    let rules = render(&mut drawn);
                    assert!(rules.contains("Command rules"), "{rules}");
                    action_hint(&rules, "Enter open");
                    if size.1 >= 24 {
                        assert!(rules.contains("User-wide command rules."));
                        assert!(!rules.contains("Skips preset approval;"));
                    }
                    drawn.app.apply(Update::Permissions(Ok(PermissionPolicy {
                        preset: PermissionPreset::WorkspaceWrite,
                        ..Default::default()
                    })));
                    let refreshed = render(&mut drawn);
                    action_hint(&refreshed, "Enter open");
                    if size.1 >= 24 {
                        assert!(refreshed.contains("(*) Workspace write"));
                        assert!(refreshed.contains("User-wide command rules."));
                    }
                    drawn.key(KeyCode::Enter);
                    assert!(matches!(
                        sent.borrow().last(),
                        Some(Command::Management(ManagementRequest {
                            area: Area::Permissions,
                            action: ManagementAction::List,
                        }))
                    ));
                    drawn.app.apply(Update::Permissions(Ok(PermissionPolicy {
                        preset: PermissionPreset::ReadOnly,
                        ..Default::default()
                    })));
                    drawn.key(KeyCode::Esc);
                    let returned = render(&mut drawn);
                    action_hint(&returned, "Enter open");
                    drawn.key(KeyCode::Enter);
                    assert!(matches!(drawn.app.prompt, Some(Prompt::Management(_))));
                    drawn.key(KeyCode::Esc);
                    drawn.key(KeyCode::End);
                    drawn.key(KeyCode::BackTab);
                    drawn.key(KeyCode::Tab);
                    let isolation = render(&mut drawn);
                    assert!(isolation.contains("Process isolation"), "{isolation}");
                    action_hint(&isolation, "Enter open");
                    if size.1 >= 24 {
                        assert!(isolation.contains("Opens Settings;"));
                        assert!(isolation.contains("user-wide,"));
                        assert!(!isolation.contains("User-wide command rules."));
                    }
                    drawn.key(KeyCode::Enter);
                    assert!(matches!(sent.borrow().last(), Some(Command::ReadIsolation)));
                    drawn.key(KeyCode::Esc);
                    let returned = render(&mut drawn);
                    action_hint(&returned, "Enter open");
                    drawn.key(KeyCode::Enter);
                    assert!(matches!(drawn.app.prompt, Some(Prompt::Isolation(_))));
                    drawn.key(KeyCode::Esc);
                    drawn.key(KeyCode::Esc);
                    if from_settings {
                        assert!(matches!(drawn.app.prompt, Some(Prompt::Settings(_))));
                    } else {
                        assert!(drawn.app.prompt.is_none());
                    }
                    assert!(!sent.borrow().iter().any(|command| matches!(
                        command,
                        Command::SetPermission(_) | Command::SaveIsolation { .. }
                    )));
                }
            }
        }
    }

    #[test]
    fn a_permission_choice_applies_the_preset_and_keeps_it_for_new_sessions() {
        use dim_agent_types::{PermissionPolicy, PermissionPreset};
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
        app.apply(Update::Permissions(Ok(PermissionPolicy {
            preset: PermissionPreset::ReadOnly,
            allowed_tools: vec!["read".into()],
            disallowed_tools: vec!["blocked".into()],
        })));
        app.run_command("permissions", "");
        let rendered = app.frame((112, 40));
        let frame = crate::ui::render::panel::strip_escapes(
            &rendered
                .overlay
                .as_ref()
                .map_or(&rendered.live, |overlay| &overlay.rows)
                .join("\n"),
        );
        for line in [
            "Read only",
            "Workspace write",
            "Full access",
            "Permission mode",
            "Related settings",
            "Writes and commands denied; external reads may ask.",
        ] {
            assert!(frame.contains(line), "{line} is missing: {frame}");
        }
        assert!(!frame.contains("File access"), "{frame}");
        app.handle_input(key(KeyCode::Down));
        app.handle_input(key(KeyCode::Enter));
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::SetPermission(
                crate::permission::PresetSelection::Preset(PermissionPreset::WorkspaceWrite)
            ))
        ));
        app.apply(Update::PermissionSelected(PermissionPreset::WorkspaceWrite));
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.path().join("config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved, serde_json::json!({"permission":"workspace-write"}));
        assert_eq!(app.permission_policy.disallowed_tools, ["blocked"]);
    }

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
        assert!(app.place_label(80).contains("new line from main"));
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
        assert!(
            !matches!(app.prompt, Some(Prompt::Fork(_))),
            "a fork copies main only"
        );
        app.apply(snapshot("main"));
        app.ask_fork(4, String::new(), None);
        assert!(
            app.frame((80, 30))
                .live
                .join("\n")
                .contains("after the last turn")
        );
        app.handle_input(key(KeyCode::Char('n')));
        app.ask_fork(4, "try RwLock".into(), Some("try RwLock".into()));
        assert!(
            app.frame((80, 30))
                .live
                .join("\n")
                .contains("before “try RwLock”"),
            "the question names the message"
        );
        app.handle_input(key(KeyCode::Char('n')));
        assert!(app.prompt.is_none());
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
            cwd: None,
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
                failure: None,
                reportable: false,
                row_id: text.into(),
                provisional_id: None,
                kind,
                text: text.into(),
                display: None,
                streaming: false,
                elapsed_ms: None,
                tone: None,
                call_id: None,
                tool_lookup_done: false,
                tool_name: tool.map(Into::into),
                tool_input: tool.map(|_| serde_json::json!({"path": "cache.rs"})),
                batch: tool.map(|_| "q1".into()),
                parent_call_id: None,
                pending: tool.is_some(),
                parked: false,
                result_text: None,
                result_is_error: false,
                diff: None,
                exit_code: None,
                member: None,
                cut: None,
                line: None,
                parent_row: None,
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
            Detail::Expanded,
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
            optional: false,
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
            optional: false,
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
        let now = Instant::now() + Duration::from_millis(1050);
        assert!(app.tick_at(now), "an acquiring row is read again");
        assert_eq!(last(&sent), "list", "the rows are read once a second");
        assert_eq!(sent.borrow().len(), count + 1);
        assert!(
            !app.tick_at(now + Duration::from_millis(150)),
            "a read inside the second waits"
        );
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
                        failure: None,
                        reportable: false,
                        row_id: (*id).into(),
                        provisional_id: None,
                        kind: RowKind::AssistantMessage,
                        text: (*text).into(),
                        display: None,
                        streaming: false,
                        elapsed_ms: None,
                        tone: None,
                        call_id: None,
                        tool_lookup_done: false,
                        tool_name: None,
                        tool_input: None,
                        batch: None,
                        parent_call_id: None,
                        pending: false,
                        parked: false,
                        result_text: None,
                        result_is_error: false,
                        diff: None,
                        exit_code: None,
                        member: None,
                        cut: None,
                        line: None,
                        parent_row: None,
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
                        .map(|row| row.trim())
                        .collect::<Vec<_>>()
                        .join("\n");
                    assert!(
                        !selected.contains(['│', '|']),
                        "Native selection must not include dialog borders: {selected}"
                    );
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
    fn an_unread_reply_points_to_its_line_in_both_tui_modes() {
        use crate::ui::render::panel::strip_escapes;

        for fullscreen in [false, true] {
            let mut drawn = Drawn::new((80, 24), None, true);
            drawn.app.fullscreen = fullscreen;
            let mut snapshot = ViewSnapshot {
                session_id: "s1".into(),
                history_complete: true,
                focus: "main".into(),
                lines: vec![
                    LineView {
                        id: "main".into(),
                        parent: None,
                        purpose: String::new(),
                        state: LineState::Main,
                        cut: None,
                    },
                    LineView {
                        id: "b-7c".into(),
                        parent: Some("main".into()),
                        purpose: "parser race".into(),
                        state: LineState::Waiting,
                        cut: Some(crate::model::view::LineCut {
                            sequence: 1,
                            messages: 1,
                        }),
                    },
                ],
                ..ViewSnapshot::default()
            };
            drawn
                .app
                .apply(Update::Snapshot(Box::new(snapshot.clone())));
            drawn.app.composer.set_draft("keep typing");
            snapshot.unread_lines.insert("b-7c".into());
            drawn
                .app
                .apply(Update::Snapshot(Box::new(snapshot.clone())));
            let frame = drawn.app.frame((80, 24));
            assert!(frame.live.join("\n").contains("Unread (1) · /lines"));
            assert_eq!(drawn.app.composer.text(), "keep typing");
            let narrow = drawn.app.frame((24, 24)).live.join("\n");
            assert!(narrow.contains("/lines (1)"), "{narrow}");
            let compact = drawn.app.frame((16, 24)).live.join("\n");
            assert!(compact.contains("/lines"), "{compact}");
            assert!(!compact.contains("/lines (1)"), "{compact}");

            drawn.app.run_command("lines", "");
            let frame = drawn.app.frame((80, 24));
            let rows = frame
                .overlay
                .as_ref()
                .map_or(&frame.live, |panel| &panel.rows);
            let lines = strip_escapes(&rows.join("\n"));
            assert!(lines.contains("* parser race"), "{lines}");
            assert!(lines.contains("Unread · after message 1"), "{lines}");
            let Some(Prompt::Lines(panel)) = &mut drawn.app.prompt else {
                panic!("/lines opens the line list");
            };
            assert!(matches!(
                panel.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
                Effect::FocusLine(line) if line == "b-7c"
            ));

            drawn.app.prompt = None;
            snapshot.focus = "b-7c".into();
            snapshot.unread_lines.clear();
            drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
            assert!(
                !drawn
                    .app
                    .frame((80, 24))
                    .live
                    .join("\n")
                    .contains("Unread (1) · /lines")
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

    /// What the main loop writes for a queued notification, not the bytes a
    /// form is built from: a permission request that appears while the
    /// terminal sits behind a multiplexer reaches the write the loop performs
    /// in the shape that one carries, with the bare `BEL` after it that still
    /// rings where the sequence itself is dropped.
    #[test]
    fn a_notification_reaches_the_terminal_through_the_write_path_it_takes() {
        use crate::model::view::ApprovalView;
        use crate::ui::notify::{Event as Notification, Multiplexer, Terminal as NotifyTerminal};

        let approval = |request: &str| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                approvals: vec![ApprovalView {
                    member: None,
                    request_id: request.into(),
                    branch_id: "main".into(),
                    reason: "shell command".into(),
                    tool_name: "exec".into(),
                    tool_input: serde_json::Value::Null,
                    always: None,
                }],
                ..ViewSnapshot::default()
            }))
        };
        let terminal = |multiplexer, term_program: &str, term: &str| NotifyTerminal {
            multiplexer,
            term_program: term_program.into(),
            term: term.into(),
            ..NotifyTerminal::default()
        };
        let mut drawn = Drawn::new((80, 24), None, true);
        drawn.app.notify = true;
        let cases = [
            (
                terminal(Multiplexer::Tmux, "iTerm.app", "tmux-256color"),
                "\x1bPtmux;\x1b\x1b]9;DIM: Permission needed: exec\x07\x1b\\\x07",
                "tmux: the wrapped form and a bare BEL after it",
            ),
            (
                terminal(Multiplexer::Screen, "iTerm.app", "screen-256color"),
                "\x1b]9;DIM: Permission needed: exec\x07\x07",
                "screen with a BEL-terminated form: the form as it is, then a BEL",
            ),
            (
                terminal(Multiplexer::Screen, "", "xterm-kitty"),
                "\x07",
                "screen with kitty's ST-terminated form: the bell alone",
            ),
            (
                terminal(Multiplexer::None, "iTerm.app", "xterm-256color"),
                "\x1b]9;DIM: Permission needed: exec\x07",
                "no multiplexer: the form alone",
            ),
        ];
        for (index, (terminal, expected, what)) in cases.into_iter().enumerate() {
            drawn.app.apply(approval(&format!("approval-{index}")));
            assert_eq!(
                drawn.app.notifications,
                vec![Notification::Permission("exec".into())],
                "{what}"
            );
            let mut written = String::new();
            write_notifications(&mut drawn.app.notifications, &terminal, |bytes| {
                written.push_str(std::str::from_utf8(bytes).expect("the form is text"));
                Ok(())
            })
            .unwrap();
            assert_eq!(written, expected, "{what}");
            assert_eq!(
                drawn.app.notifications,
                Vec::<Notification>::new(),
                "{what}: sent once"
            );
            write_notifications(&mut drawn.app.notifications, &terminal, |bytes| {
                panic!("nothing is owed after the write: {bytes:?}")
            })
            .unwrap();
        }

        // A background task's end takes the same write: the task runs on the
        // first snapshot, ends on the next, and the form is the one this
        // terminal understands.
        let snapshot = |tasks| {
            Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                tasks,
                ..ViewSnapshot::default()
            }))
        };
        let task = background_task("t1", "cargo build");
        drawn.app.apply(snapshot(vec![task.clone()]));
        drawn
            .app
            .apply(snapshot(vec![finished(task, TaskOutcome::Completed)]));
        let mut written = String::new();
        write_notifications(
            &mut drawn.app.notifications,
            &terminal(Multiplexer::Tmux, "iTerm.app", "tmux-256color"),
            |bytes| {
                written.push_str(std::str::from_utf8(bytes).expect("the form is text"));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            written,
            "\x1bPtmux;\x1b\x1b]9;DIM: Background task finished: cargo build\x07\x1b\\\x07",
            "a background end: the wrapped form and a bare BEL after it"
        );
    }

    /// The reviewer's case on a real 80x24 frame: thirty replacements keep the
    /// target line and the choices on screen, and the keys reach the last entry,
    /// in both renderers.
    #[test]
    fn a_long_edit_approval_fits_an_80x24_frame_and_reaches_its_last_entry() {
        use crate::model::view::ApprovalView;

        let size = (80, 24);
        let edits: Vec<_> = (1..=30)
            .map(|n| serde_json::json!({"oldText": format!("old {n}"), "newText": format!("new {n}")}))
            .collect();
        for fullscreen in [false, true] {
            let mut drawn = Drawn::new(size, None, true);
            drawn.app.fullscreen = fullscreen;
            drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s1".into(),
                approvals: vec![ApprovalView {
                    member: None,
                    request_id: "approval-1".into(),
                    branch_id: "main".into(),
                    reason: "source: tool_rule".into(),
                    tool_name: "edit".into(),
                    tool_input: serde_json::json!({"path": "src/lib.rs", "edits": edits}),
                    always: None,
                }],
                ..ViewSnapshot::default()
            })));
            let screen_text = |drawn: &mut Drawn| -> String {
                if fullscreen {
                    let frame = drawn.app.frame(size);
                    let mut full = FullScreen::new(size, true);
                    let bytes = full.render(&frame, size, &drawn.app.palette, &drawn.app);
                    let mut emu = crate::ui::render::testterm::Emu::new(80, 24);
                    emu.feed(&bytes);
                    emu.rows_text().join("\n")
                } else {
                    drawn.draw();
                    drawn.emu.rows_text().join("\n")
                }
            };
            let first = screen_text(&mut drawn);
            for shown in [
                "# source: tool_rule",
                "$ edit 30 replacements in src/lib.rs",
                "1 · old 1 → new 1",
                "Allow once",
            ] {
                assert!(first.contains(shown), "{fullscreen}: {shown:?} in {first}");
            }
            for _ in 0..30 {
                let _ = drawn
                    .app
                    .handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
                        KeyCode::Down,
                        Modifiers::NONE,
                    )));
            }
            let last = screen_text(&mut drawn);
            for shown in ["30 · old 30 → new 30", "# source: tool_rule", "Allow once"] {
                assert!(
                    last.contains(shown),
                    "{fullscreen}: after scrolling, {shown:?} in {last}"
                );
            }
        }
    }

    #[test]
    fn the_window_topic_stays_mains_message_while_a_line_is_shown() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows: vec![std::sync::Arc::new(ViewRow {
                failure: None,
                reportable: false,
                row_id: "r1".into(),
                provisional_id: None,
                kind: RowKind::UserMessage,
                text: "line work".into(),
                display: None,
                streaming: false,
                elapsed_ms: None,
                tone: None,
                call_id: None,
                tool_lookup_done: false,
                tool_name: None,
                tool_input: None,
                batch: None,
                parent_call_id: None,
                pending: false,
                parked: false,
                result_text: None,
                result_is_error: false,
                diff: None,
                exit_code: None,
                member: None,
                cut: None,
                line: None,
                parent_row: None,
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

    /// The palette lists the Session's enabled, unshadowed Skills, and a row
    /// completes its marker without sending.
    #[test]
    fn the_palette_lists_enabled_unshadowed_skills() {
        use crate::management::skills::Entry;
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        let (mut app, sent, _home) = session_with_queue(false, &[]);
        let entry = |name: &str, enabled: bool, shadowed_by: Option<&str>| Entry {
            id: format!("user:{name}"),
            source: Some("user".into()),
            name: name.into(),
            description: format!("About {name}"),
            file_path: format!("/home/.agents/skills/{name}/SKILL.md"),
            enabled,
            shadowed_by: shadowed_by.map(str::to_owned),
        };
        app.apply(Update::Skills(vec![
            entry("commit", true, None),
            entry("deploy", false, None),
            entry("shadowed", true, Some("user:shadowed")),
            entry("two words", true, None),
        ]));
        typed(&mut app, "/comm");
        assert!(matches!(app.handle(key(KeyCode::Enter)), Outcome::Continue));
        assert_eq!(app.composer.text(), "$skill: commit ");
        assert!(
            sent.borrow().is_empty(),
            "the marker is a draft, not a message"
        );
        app.composer.clear();
        typed(&mut app, "/dep");
        app.handle(key(KeyCode::Tab));
        assert_eq!(
            app.composer.text(),
            "/dep",
            "a disabled Skill is not offered"
        );
        app.composer.clear();
        typed(&mut app, "/sha");
        app.handle(key(KeyCode::Tab));
        assert_eq!(
            app.composer.text(),
            "/sha",
            "a shadowed Skill is not offered"
        );
        app.composer.clear();
        typed(&mut app, "/two");
        app.handle(key(KeyCode::Tab));
        assert_eq!(
            app.composer.text(),
            "/two",
            "a name with whitespace cannot be referenced and is not offered"
        );
    }

    /// A resumed session of `count` user messages `ask N`, each cut at `N`.
    fn resume_asks(drawn: &mut Drawn, count: usize) {
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        let rows = (0..count)
            .map(|n| {
                std::sync::Arc::new(ViewRow {
                    failure: None,
                    reportable: false,
                    row_id: format!("u{n}"),
                    provisional_id: None,
                    kind: RowKind::UserMessage,
                    text: format!("ask {n}"),
                    display: None,
                    streaming: false,
                    elapsed_ms: None,
                    tone: None,
                    call_id: None,
                    tool_lookup_done: false,
                    tool_name: None,
                    tool_input: None,
                    batch: None,
                    parent_call_id: None,
                    pending: false,
                    parked: false,
                    result_text: None,
                    result_is_error: false,
                    diff: None,
                    exit_code: None,
                    member: None,
                    cut: Some(n as u64),
                    line: None,
                    parent_row: None,
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
            failure: None,
            reportable: false,
            display: None,
            row_id: "u1".into(),
            provisional_id: None,
            kind: RowKind::UserMessage,
            text: "find the leak".into(),
            streaming: false,
            elapsed_ms: None,
            tone: None,
            call_id: None,
            tool_lookup_done: false,
            tool_name: None,
            tool_input: None,
            batch: None,
            parent_call_id: None,
            pending,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
            exit_code: None,
            member: None,
            cut: None,
            line: None,
            parent_row: None,
            compaction: None,
        })
    }

    /// The status row a cancelled turn settles after a stop.
    fn stopped() -> std::sync::Arc<ViewRow> {
        std::sync::Arc::new(ViewRow {
            failure: None,
            reportable: false,
            display: None,
            row_id: "stopped-1".into(),
            provisional_id: None,
            kind: RowKind::Status,
            text: "stopped".into(),
            streaming: false,
            elapsed_ms: None,
            tone: None,
            call_id: None,
            tool_lookup_done: false,
            tool_name: None,
            tool_input: None,
            batch: None,
            parent_call_id: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
            exit_code: None,
            member: None,
            cut: None,
            line: None,
            parent_row: None,
            compaction: None,
        })
    }

    /// The `dim-hooks` row a message carrying hook context appends after
    /// itself, naming `u1` as its parent.
    fn hook() -> std::sync::Arc<ViewRow> {
        let mut row = (*message(false)).clone();
        row.row_id = "hook-1".into();
        row.kind = RowKind::HookContext;
        row.text = "read src/parser.rs".into();
        row.pending = false;
        row.cut = None;
        row.parent_row = Some("u1".into());
        std::sync::Arc::new(row)
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
    fn fullscreen_tools_keep_each_level_independent_across_live_settle_and_rebuild() {
        use crate::ui::render::panel::strip_escapes;

        let call = |request: &str, id: &str, parent: Option<&str>, name: &str, output: &str| {
            let mut row = (*message(false)).clone();
            row.row_id = serde_json::to_string(&(request, id)).unwrap();
            row.kind = RowKind::ToolCall;
            row.text.clear();
            row.call_id = Some(id.into());
            row.batch = Some(request.into());
            row.parent_call_id = parent.map(str::to_owned);
            row.tool_name = Some(name.into());
            row.tool_input = Some(serde_json::json!({"command": format!("echo {id}")}));
            row.result_text = Some(output.into());
            std::sync::Arc::new(row)
        };
        let text = |app: &mut App, cols: usize| {
            let frame = app.frame((cols as u16, 40));
            let rows = (0..app.len()).flat_map(|index| app.rows(index, cols));
            strip_escapes(&rows.chain(frame.live).collect::<Vec<_>>().join("\n"))
        };
        let toggle = |app: &mut App, label: &str, cols: usize| {
            let frame = app.frame((cols as u16, 40));
            let target = (0..app.len())
                .find_map(|index| {
                    app.rows(index, cols)
                        .iter()
                        .enumerate()
                        .find_map(|(row, line)| {
                            let block = Some(app.dropped() + index);
                            (strip_escapes(line).contains(label)
                                && app.disclosure(block, cols, row).is_some())
                            .then_some((block, row))
                        })
                })
                .or_else(|| {
                    frame.live.iter().enumerate().find_map(|(row, line)| {
                        (strip_escapes(line).contains(label)
                            && app.disclosure(None, cols, row).is_some())
                        .then_some((None, row))
                    })
                })
                .unwrap_or_else(|| panic!("no header {label}: {}", text(app, cols)));
            assert!(app.click(target.0, target.1, cols));
        };
        let mut drawn = attached();
        drawn.app.set_fullscreen(true);
        let mut rows = vec![
            call("r1", "a", None, "exec", "alpha output"),
            call("r1", "b", None, "exec", "beta output"),
        ];
        std::sync::Arc::make_mut(&mut rows[0]).pending = true;
        std::sync::Arc::make_mut(&mut rows[0]).result_text = None;
        show(&mut drawn, rows.clone());
        let mut running = drawn.app.snapshot.as_ref().unwrap().clone();
        running.running = true;
        drawn.app.apply(Update::Snapshot(Box::new(running)));
        let before = text(&mut drawn.app, 80);
        assert!(drawn.app.tick_at(drawn.app.run_started.unwrap() + PULSE));
        let pulsing = text(&mut drawn.app, 80);
        assert_ne!(before, pulsing);
        assert!(pulsing.contains("• Running 2 commands"), "{pulsing}");
        toggle(&mut drawn.app, "Running 2 commands", 80);
        assert!(!text(&mut drawn.app, 80).contains("beta output"));
        toggle(&mut drawn.app, "echo a", 80);
        assert!(text(&mut drawn.app, 80).contains("Waiting for result"));
        assert!(!text(&mut drawn.app, 80).contains("beta output"));
        toggle(&mut drawn.app, "echo b", 80);
        toggle(&mut drawn.app, "echo b", 80);
        toggle(&mut drawn.app, "Running 2 commands", 80);
        toggle(&mut drawn.app, "Running 2 commands", 80);
        assert!(text(&mut drawn.app, 80).contains("Waiting for result"));
        assert!(text(&mut drawn.app, 80).contains("Running…"));
        std::sync::Arc::make_mut(&mut rows[0]).result_text = Some("partial output".into());
        show(&mut drawn, rows.clone());
        assert!(text(&mut drawn.app, 80).contains("partial output"));
        assert!(!text(&mut drawn.app, 80).contains("Waiting for result"));
        std::sync::Arc::make_mut(&mut rows[0]).pending = false;
        std::sync::Arc::make_mut(&mut rows[0]).result_text = Some("alpha output".into());
        show(&mut drawn, rows.clone());
        assert!(text(&mut drawn.app, 80).contains("alpha output"));
        assert!(!text(&mut drawn.app, 80).contains("beta output"));
        drawn.app.run_command("tools", "");
        drawn.app.run_command("tools", "");
        assert!(text(&mut drawn.app, 35).contains("alpha output"));
        assert!(!text(&mut drawn.app, 35).contains("beta output"));
        drawn.app.redraw_window();
        assert!(text(&mut drawn.app, 35).contains("alpha output"));
        assert!(!text(&mut drawn.app, 35).contains("beta output"));

        rows.extend([
            call("r2", "a", None, "exec", "another request"),
            call("r2", "b", None, "exec", "second request sibling"),
        ]);
        show(&mut drawn, rows.clone());
        assert!(text(&mut drawn.app, 80).contains("second request sibling"));
        let snapshot = drawn.app.snapshot.as_ref().unwrap().clone();
        for (session, branch) in [("s1", "line"), ("s2", "main")] {
            let mut other = snapshot.clone();
            other.session_id = session.into();
            other.focus = branch.into();
            if drawn.app.session_id != session {
                drawn
                    .app
                    .switch_session(session.into(), SwitchKind::Resumed);
            }
            drawn.app.apply(Update::Snapshot(Box::new(other)));
            assert!(text(&mut drawn.app, 80).contains("beta output"));
        }
        drawn.app.switch_session("s1".into(), SwitchKind::Resumed);
        drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
        assert!(!text(&mut drawn.app, 80).contains("beta output"));

        drawn.app.tool_detail = Detail::Summary;
        let tree = vec![
            call("tree", "parent-a", None, "codemode", ""),
            call("tree", "parent-b", None, "codemode", ""),
            call("tree", "one", Some("parent-a"), "exec", "first child"),
            call("tree", "nested", Some("parent-a"), "codemode", ""),
            call("tree", "deep", Some("nested"), "exec", "deep output"),
            call("tree", "two", Some("parent-b"), "exec", "second child"),
        ];
        show(&mut drawn, tree);
        let visible = text(&mut drawn.app, 80);
        assert!(visible.contains("echo deep"), "{visible}");
        assert!(!visible.contains("deep output"), "{visible}");
        toggle(&mut drawn.app, "echo deep", 80);
        toggle(&mut drawn.app, "Ran code · 2 tool calls", 80);
        let visible = text(&mut drawn.app, 80);
        assert!(!visible.contains("echo one"), "{visible}");
        assert!(visible.contains("echo two"), "{visible}");
        toggle(&mut drawn.app, "Ran code · 2 tool calls", 80);
        assert!(text(&mut drawn.app, 80).contains("deep output"));
        assert!(!text(&mut drawn.app, 80).contains("second child"));

        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        drawn.app.host = Box::new(Recorder(sent.clone()));
        let mut orphaned = vec![
            call("lazy", "a", None, "alpha-tool", "alpha result"),
            call("lazy", "b", None, "beta-tool", "beta result"),
        ];
        for row in &mut orphaned {
            std::sync::Arc::make_mut(row).tool_input = None;
        }
        show(&mut drawn, orphaned.clone());
        assert!(
            sent.borrow().is_empty(),
            "closed call details do not fetch origins"
        );
        toggle(&mut drawn.app, "Alpha-tool", 80);
        assert!(matches!(sent.borrow().as_slice(), [Command::ToolOrigin {
            request_id, call_id, ..
        }] if request_id == "lazy" && call_id == "a"));
        let resolved = std::sync::Arc::make_mut(&mut orphaned[0]);
        resolved.tool_input = Some(serde_json::json!({"text": "loaded alpha argument"}));
        resolved.tool_lookup_done = true;
        sent.borrow_mut().clear();
        show(&mut drawn, orphaned);
        let visible = text(&mut drawn.app, 80);
        assert!(visible.contains("loaded alpha argument"), "{visible}");
        assert!(
            !visible.contains("beta result"),
            "origin arrival preserves sibling disclosure"
        );
        assert!(
            sent.borrow().is_empty(),
            "origin arrival does not fetch closed siblings"
        );
    }

    #[test]
    fn fullscreen_command_previews_preserve_background_handoff_status() {
        use crate::ui::render::panel::strip_escapes;

        for background in [false, true] {
            for command in ["server --listen", "server --listen\necho ready"] {
                let mut drawn = attached();
                drawn.app.set_fullscreen(true);
                drawn.app.tool_detail = Detail::Expanded;
                let mut row = (*message(false)).clone();
                row.kind = RowKind::ToolCall;
                row.row_id = "command".into();
                row.call_id = Some("exec-call".into());
                row.batch = Some("exec-request".into());
                row.tool_name = Some("exec".into());
                row.tool_input = Some(serde_json::json!({
                    "command": command,
                    "runInBackground": background,
                }));
                row.result_text = Some("Execution completed".into());
                show(&mut drawn, vec![row.into()]);
                let frame = drawn.app.frame((96, 40));
                let rows: Vec<_> = (0..drawn.app.len())
                    .flat_map(|index| drawn.app.rows(index, 96))
                    .chain(frame.live)
                    .collect();
                let rendered = strip_escapes(&rows.join("\n"));
                let header = rendered
                    .lines()
                    .find(|line| line.contains("$ server"))
                    .unwrap();
                assert_eq!(header.contains("(detached)"), background, "{rendered}");
                assert!(!header.contains("echo ready"), "{rendered}");
                assert!(rendered.contains("Ran 1 command"), "{rendered}");
                assert_eq!(
                    rendered.contains("Execution completed"),
                    !background,
                    "{rendered}"
                );
                if command.contains('\n') {
                    assert!(rendered.contains("echo ready"), "{rendered}");
                }
            }
        }
    }

    #[test]
    fn fullscreen_file_tools_show_recorded_comparisons_not_proposed_or_current_content() {
        use crate::ui::render::panel::strip_escapes;

        for (name, diff, expected) in [
            (
                "edit",
                Some(
                    "@@ -41,2 +41,2 @@\n-old value\n+new value\n context\n@@ -50 +50 @@\n-before\n+after\n",
                ),
                "Edit",
            ),
            (
                "write",
                Some("--- /dev/null\n+++ new.rs\n@@ -0,0 +1,2 @@\n+first line\n+second line\n"),
                "Create",
            ),
            (
                "write",
                Some("--- existing.rs\n+++ existing.rs\n@@ -1 +1 @@\n-old value\n+new value\n"),
                "Write",
            ),
            (
                "write",
                Some("--- existing.rs\n+++ existing.rs\n@@ -0,0 +1 @@\n+first line\n"),
                "Write",
            ),
            (
                "write",
                Some("--- existing.rs\n+++ existing.rs\n"),
                "No content changes.",
            ),
            (
                "write",
                None,
                "Before/after comparison is unavailable for this record.",
            ),
        ] {
            for state in ["done", "running", "failed"] {
                let mut drawn = attached();
                drawn.app.set_fullscreen(true);
                drawn.app.tool_detail = Detail::Expanded;
                let mut row = (*message(false)).clone();
                row.kind = RowKind::ToolCall;
                row.row_id = "file-operation".into();
                row.call_id = Some("file-call".into());
                row.batch = Some("file-request".into());
                row.tool_name = Some(name.into());
                row.tool_input = Some(
                    serde_json::json!({"path":"src/file.rs", "content":"do not fabricate this input", "oldText":"not recorded old", "newText":"not recorded new"}),
                );
                row.result_text = Some(
                    if state == "failed" {
                        "revision conflict"
                    } else {
                        "completed"
                    }
                    .into(),
                );
                row.result_is_error = state == "failed";
                row.pending = state == "running";
                row.diff = diff.map(str::to_owned);
                show(&mut drawn, vec![row.into()]);
                let frame = drawn.app.frame((96, 40));
                let rows: Vec<_> = (0..drawn.app.len())
                    .flat_map(|index| drawn.app.rows(index, 96))
                    .chain(frame.live)
                    .collect();
                let rendered = strip_escapes(&rows.join("\n"));
                assert!(!rendered.contains("fabricate"), "{rendered}");
                assert!(!rendered.contains("not recorded"), "{rendered}");
                if state == "done" {
                    assert!(rendered.contains(expected), "{rendered}");
                    if name == "edit" {
                        assert!(rendered.contains("41     −  old value"), "{rendered}");
                        assert!(rendered.contains("    41 +  new value"), "{rendered}");
                        assert!(rendered.contains("7 unchanged lines"), "{rendered}");
                    }
                } else {
                    assert!(!rendered.contains("Changes"), "{rendered}");
                    assert!(!rendered.contains("old value"), "{rendered}");
                    assert!(
                        rendered.contains(if state == "failed" {
                            "revision conflict"
                        } else {
                            "Waiting for result"
                        }),
                        "{rendered}"
                    );
                }
                for (index, lines) in rows.iter().enumerate() {
                    if strip_escapes(lines).contains("old value") {
                        assert!(
                            drawn
                                .app
                                .disclosure(Some(drawn.app.dropped()), 96, index)
                                .is_none()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fullscreen_codemode_headers_match_state_children_and_available_details() {
        use crate::ui::render::panel::strip_escapes;

        let call = |id: &str, parent: Option<&str>, name: &str, state: &str| {
            let mut row = (*message(false)).clone();
            row.kind = RowKind::ToolCall;
            row.row_id = id.into();
            row.call_id = Some(id.into());
            row.batch = Some("code-request".into());
            row.parent_call_id = parent.map(str::to_owned);
            row.tool_name = Some(name.into());
            row.tool_input = Some(serde_json::json!({"command":"echo child"}));
            row.pending = state == "running";
            row.result_is_error = state == "failed";
            row.result_text = (state != "running").then(|| "code result".into());
            std::sync::Arc::new(row)
        };
        let visible_rows = |app: &mut App| {
            let frame = app.frame((96, 40));
            let mut rows = Vec::new();
            for index in 0..app.len() {
                for (row, line) in app.rows(index, 96).into_iter().enumerate() {
                    rows.push((Some(app.dropped() + index), row, line));
                }
            }
            rows.extend(
                frame
                    .live
                    .into_iter()
                    .enumerate()
                    .map(|(row, line)| (None, row, line)),
            );
            rows
        };
        for state in ["done", "running", "failed"] {
            let mut drawn = attached();
            drawn.app.set_fullscreen(true);
            show(&mut drawn, vec![call("program-a", None, "codemode", state)]);
            let rows = visible_rows(&mut drawn.app);
            let (block, row) = rows
                .iter()
                .find_map(|(block, row, line)| {
                    let line = strip_escapes(line);
                    (line.contains("Ran code") || line.contains("Running code"))
                        .then_some((*block, *row))
                })
                .unwrap();
            assert_eq!(
                drawn.app.disclosure(block, 96, row).is_some(),
                state != "done"
            );
            assert_eq!(drawn.app.click(block, row, 96), state != "done");
            if state != "done" {
                let rendered = strip_escapes(
                    &visible_rows(&mut drawn.app)
                        .into_iter()
                        .map(|(_, _, line)| line)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                assert!(
                    rendered.contains(if state == "running" {
                        "Waiting for result"
                    } else {
                        "code result"
                    }),
                    "{rendered}"
                );
            }
        }
        for state in ["done", "running"] {
            for children in [0, 1, 2] {
                let mut drawn = attached();
                drawn.app.set_fullscreen(true);
                drawn.app.tool_detail = Detail::Expanded;
                let mut rows = vec![
                    call("program-a", None, "codemode", state),
                    call("program-b", None, "codemode", "done"),
                ];
                for child in 0..children {
                    rows.push(call(
                        &format!("child-{child}"),
                        Some("program-a"),
                        "exec",
                        "done",
                    ));
                }
                show(&mut drawn, rows);
                let rows = visible_rows(&mut drawn.app);
                let header = rows.iter().find_map(|(block, row, line)| {
                    drawn
                        .app
                        .disclosure(*block, 96, *row)
                        .filter(|key| key.contains("call:program-a"))
                        .map(|_| strip_escapes(line))
                });
                if children == 0 && state == "done" {
                    assert!(header.is_none());
                } else {
                    let header = header.unwrap();
                    assert!(
                        header.contains(if state == "running" {
                            "Running code"
                        } else {
                            "Ran code"
                        }),
                        "{header}"
                    );
                    if children > 0 {
                        assert!(
                            header.contains(if children == 1 {
                                "1 tool call"
                            } else {
                                "2 tool calls"
                            }),
                            "{header}"
                        );
                        assert!(!header.contains("1 tool calls"), "{header}");
                    } else {
                        assert!(!header.contains("tool calls"), "{header}");
                    }
                }
            }
        }
    }

    #[test]
    fn expanded_thinking_streams_into_native_history_without_repeating_on_completion() {
        let paragraphs = (1..=45)
            .map(|n| format!("Step{n:02}: inspect the lock before waiting."))
            .collect::<Vec<_>>();
        for synchronized in [false, true] {
            for separator in ["\n\n", " "] {
                let mut drawn = Drawn::new((72, 20), Some(0), synchronized);
                drawn.app.thinking_detail = Detail::Expanded;
                let mut thought = (*message(false)).clone();
                thought.kind = RowKind::Thinking;
                thought.streaming = true;
                for end in 1..=paragraphs.len() {
                    thought.row_id = if end < 10 {
                        "progress"
                    } else {
                        "thinking-event"
                    }
                    .into();
                    thought.provisional_id = (end >= 10).then(|| "progress".into());
                    thought.text = paragraphs[..end].join(separator);
                    show(&mut drawn, vec![std::sync::Arc::new(thought.clone())]);
                    if end == 20 {
                        drawn.size = (45, 20);
                        drawn.emu = crate::ui::render::testterm::Emu::new(45, 20);
                        let rows = drawn.app.reflow_history(45);
                        let frame = drawn.app.frame(drawn.size);
                        let wanted = drawn.screen.wanted_for(drawn.size, frame.live.len());
                        let mut bytes = drawn.screen.rewrite(drawn.size, &rows, wanted);
                        bytes.extend(drawn.screen.render(&frame, drawn.size));
                        drawn.emu.feed(&bytes);
                    }
                    let transcript = drawn.transcript();
                    for n in 1..=end {
                        assert_eq!(
                            transcript.matches(&format!("Step{n:02}:")).count(),
                            1,
                            "{transcript}"
                        );
                    }
                }
                assert!(
                    drawn
                        .emu
                        .scrollback
                        .iter()
                        .any(|row| row.contains("Step01:"))
                );
                let before = drawn.transcript();
                drawn.draw();
                assert_eq!(drawn.transcript(), before);
                let anchor = drawn.anchor();
                thought.streaming = false;
                thought.elapsed_ms = Some(2700);
                show(&mut drawn, vec![std::sync::Arc::new(thought)]);
                let transcript = drawn.transcript();
                for n in 1..=45 {
                    assert_eq!(
                        transcript.matches(&format!("Step{n:02}:")).count(),
                        1,
                        "{transcript}"
                    );
                }
                assert!(transcript.contains("Thought for 2.7s"), "{transcript}");
                assert_eq!(transcript.matches("◆ Thought").count(), 2, "{transcript}");
                assert!(drawn.transcript().contains("Step45:"));
                assert_eq!(drawn.anchor(), anchor);
            }
        }
    }

    #[test]
    fn fullscreen_file_totals_require_all_completed_comparisons() {
        use crate::ui::render::panel::strip_escapes;

        for complete in [false, true] {
            let mut drawn = attached();
            drawn.app.set_fullscreen(true);
            drawn.app.tool_detail = Detail::Expanded;
            let rows = ["first.rs", "second.rs"]
                .into_iter()
                .enumerate()
                .map(|(index, path)| {
                    let mut row = (*message(false)).clone();
                    row.kind = RowKind::ToolCall;
                    row.row_id = path.into();
                    row.call_id = Some(path.into());
                    row.batch = Some("write-request".into());
                    row.tool_name = Some("write".into());
                    row.tool_input = Some(serde_json::json!({"path":path}));
                    row.result_text = Some("Wrote file".into());
                    row.diff = (complete || index == 0)
                        .then(|| "@@ -1,1 +1,2 @@\n-old\n+new\n+extra".into());
                    std::sync::Arc::new(row)
                })
                .collect();
            show(&mut drawn, rows);
            let _ = drawn.app.frame((96, 40));
            let rows = drawn.app.reflow_history(96);
            let rendered = strip_escapes(&rows.join("\n"));
            let header = rendered
                .lines()
                .find(|line| line.contains("Changed 2 files"))
                .unwrap();
            assert_eq!(header.contains("(+4 -2)"), complete, "{rendered}");
            assert_eq!(header.contains('('), complete, "{rendered}");
            assert!(rendered.contains("Write first.rs +2 −1"), "{rendered}");
            assert_eq!(
                rendered.contains("comparison is unavailable"),
                !complete,
                "{rendered}"
            );
        }
    }

    #[test]
    fn thinking_preserves_history_when_its_identity_changes_while_collapsed() {
        use crate::projection::Projection;
        use serde_json::json;

        for finish_while_collapsed in [false, true] {
            let mut drawn = Drawn::new((72, 20), Some(0), false);
            drawn.app.thinking_detail = Detail::Expanded;
            let mut projection = Projection::default();
            let text = (1..=30)
                .map(|n| format!("Step{n:02}: inspect the lock before waiting."))
                .collect::<Vec<_>>()
                .join("\n\n");
            projection.progress(
                &serde_json::from_value(json!({
                    "sessionId": "s1", "branchId": "main", "requestId": "r1",
                    "text": "", "textOffset": 0, "thinking": text, "thinkingOffset": 0,
                }))
                .unwrap(),
            );
            show(&mut drawn, projection.snapshot().rows);
            assert!(
                drawn
                    .emu
                    .scrollback
                    .iter()
                    .any(|row| row.contains("Step01:"))
            );

            let toggle = |drawn: &mut Drawn| {
                drawn.app.open_settings(Setting::ThinkingDetails);
                press(&mut drawn.app, KeyCode::Char(' '));
                press(&mut drawn.app, KeyCode::Esc);
                assert!(drawn.app.take_reflow_request());
                let rows = drawn.app.reflow_history(drawn.size.0 as usize);
                let frame = drawn.app.frame(drawn.size);
                let wanted = drawn.screen.wanted_for(drawn.size, frame.live.len());
                drawn.emu = crate::ui::render::testterm::Emu::new(72, 20);
                let mut bytes = drawn.screen.rewrite(drawn.size, &rows, wanted);
                bytes.extend(drawn.screen.render(&frame, drawn.size));
                drawn.emu.feed(&bytes);
            };
            toggle(&mut drawn);
            projection.apply(
                "main",
                &serde_json::from_value(json!({
                    "id": "thought-event", "sequence": 1, "create_at": 1000,
                    "type": "step", "name": "thinking",
                    "data": {"turn_id": "t1", "request_id": "r1",
                        "delta": format!("{text}\n\nStep31: saved after collapsing.")},
                }))
                .unwrap(),
            );
            show(&mut drawn, projection.snapshot().rows);
            let finish: crate::wire::Event = serde_json::from_value(json!({
                "sequence": 2, "create_at": 4500, "type": "step", "name": "text",
                "data": {"turn_id": "t1", "request_id": "r1", "delta": "Answer."},
            }))
            .unwrap();
            if finish_while_collapsed {
                projection.apply("main", &finish);
                show(&mut drawn, projection.snapshot().rows);
            }
            toggle(&mut drawn);
            let assert_body = |drawn: &Drawn| {
                let transcript = drawn.transcript();
                for n in 1..=31 {
                    assert_eq!(
                        transcript.matches(&format!("Step{n:02}:")).count(),
                        1,
                        "finish_while_collapsed={finish_while_collapsed}: {transcript}"
                    );
                }
            };
            assert_body(&drawn);
            if !finish_while_collapsed {
                projection.apply("main", &finish);
                show(&mut drawn, projection.snapshot().rows);
                assert_body(&drawn);
                assert!(drawn.transcript().contains("Thought for 3.5s"));
            }
        }
    }

    #[test]
    fn thinking_commits_settlement_when_collapsed_before_the_completion_frame() {
        use crate::projection::Projection;
        use serde_json::json;

        for draw_completion in [false, true] {
            let mut drawn = Drawn::new((72, 20), Some(0), false);
            drawn.app.thinking_detail = Detail::Expanded;
            let mut projection = Projection::default();
            let text = (1..=20)
                .map(|n| format!("Step{n:02}: inspect the lock before waiting."))
                .collect::<Vec<_>>()
                .join("\n\n");
            projection.apply(
                "main",
                &serde_json::from_value(json!({
                    "id": "thought", "sequence": 1, "create_at": 1000,
                    "type": "step", "name": "thinking",
                    "data": {"turn_id": "t1", "request_id": "r1", "delta": text},
                }))
                .unwrap(),
            );
            show(&mut drawn, projection.snapshot().rows);
            projection.apply(
                "main",
                &serde_json::from_value(json!({
                    "sequence": 2, "create_at": 2500, "type": "step", "name": "text",
                    "data": {"turn_id": "t1", "request_id": "r1", "delta": "Answer."},
                }))
                .unwrap(),
            );
            let mut snapshot = projection.snapshot();
            snapshot.session_id = "s1".into();
            snapshot.focus = "main".into();
            snapshot.history_complete = true;
            drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
            if draw_completion {
                drawn.draw();
            }
            drawn.app.open_settings(Setting::ThinkingDetails);
            press(&mut drawn.app, KeyCode::Char(' '));
            press(&mut drawn.app, KeyCode::Esc);
            assert!(drawn.app.take_reflow_request());
            let rows = drawn.app.reflow_history(72);
            let frame = drawn.app.frame(drawn.size);
            let wanted = drawn.screen.wanted_for(drawn.size, frame.live.len());
            drawn.emu = crate::ui::render::testterm::Emu::new(72, 20);
            let mut bytes = drawn.screen.rewrite(drawn.size, &rows, wanted);
            bytes.extend(drawn.screen.render(&frame, drawn.size));
            drawn.emu.feed(&bytes);
            for _ in 0..2 {
                let transcript = drawn.transcript();
                assert_eq!(
                    transcript.matches("Thought for 1.5s").count(),
                    1,
                    "draw_completion={draw_completion}: {transcript}"
                );
                drawn.draw();
            }
        }
    }

    #[test]
    fn thinking_commits_opening_rows_after_an_earlier_answer_finishes_revealing() {
        use crate::projection::Projection;
        use serde_json::json;

        for finish_while_held in [false, true] {
            let mut drawn = Drawn::new((72, 20), Some(0), false);
            drawn.app.thinking_detail = Detail::Expanded;
            let mut projection = Projection::default();
            let answer = format!(
                "AnswerFirst: {} AnswerLast.",
                "waiting for output. ".repeat(80)
            );
            projection.apply(
                "main",
                &serde_json::from_value(json!({
                    "sequence": 1, "create_at": 1, "type": "step", "name": "text",
                    "data": {"turn_id": "t0", "request_id": "r0", "delta": answer},
                }))
                .unwrap(),
            );
            show(&mut drawn, projection.snapshot().rows);
            projection.apply(
                "main",
                &serde_json::from_value(json!({
                    "sequence": 2, "create_at": 2, "type": "step", "name": "model-output-end",
                    "data": {"turn_id": "t0", "request_id": "r0", "status": "completed",
                        "finish_reason": "end_turn"},
                }))
                .unwrap(),
            );
            let text = (1..=20)
                .map(|n| format!("Step{n:02}: inspect the lock before waiting."))
                .collect::<Vec<_>>()
                .join("\n\n");
            projection.apply(
                "main",
                &serde_json::from_value(json!({
                    "id": "thought", "sequence": 3, "create_at": 1000,
                    "type": "step", "name": "thinking",
                    "data": {"turn_id": "t1", "request_id": "r1", "delta": text},
                }))
                .unwrap(),
            );
            show(&mut drawn, projection.snapshot().rows);
            drawn.draw();
            let finish: crate::wire::Event = serde_json::from_value(json!({
                "sequence": 4, "create_at": 2500, "type": "step", "name": "text",
                "data": {"turn_id": "t1", "request_id": "r1", "delta": "AnswerNext."},
            }))
            .unwrap();
            if finish_while_held {
                projection.apply("main", &finish);
                show(&mut drawn, projection.snapshot().rows);
            }
            assert!(!drawn.transcript().contains("Step01:"));
            let (pace, shown) = drawn.app.revealed.get_mut("assistant-r0").unwrap();
            *shown = pace.advance(Instant::now() + Duration::from_secs(1), answer.len(), true);
            drawn.draw();
            let assert_body = |drawn: &Drawn| {
                let transcript = drawn.transcript();
                for n in 1..=20 {
                    assert_eq!(
                        transcript.matches(&format!("Step{n:02}:")).count(),
                        1,
                        "finish_while_held={finish_while_held}: {transcript}"
                    );
                }
                assert!(
                    transcript.find("AnswerLast.").unwrap() < transcript.find("Step01:").unwrap()
                );
            };
            assert_body(&drawn);
            if !finish_while_held {
                projection.apply("main", &finish);
                show(&mut drawn, projection.snapshot().rows);
            }
            assert_body(&drawn);
            assert_eq!(drawn.transcript().matches("Thought for 1.5s").count(), 1);
        }
    }

    #[test]
    fn thinking_keeps_every_character_across_wrapping_and_detail_changes() {
        for text in [
            "Inspect **the locking boundary** before waiting. ".repeat(20),
            "先检查**锁的边界**，等待任务前释放资源。\n\n继续检查下一步！".repeat(20),
            format!("prefix {} suffix", "abcdefghij".repeat(80)),
        ] {
            let mut drawn = Drawn::new((34, 20), Some(0), false);
            drawn.app.thinking_detail = Detail::Expanded;
            let mut thought = (*message(false)).clone();
            thought.kind = RowKind::Thinking;
            thought.streaming = true;
            for end in text.char_indices().map(|(at, c)| at + c.len_utf8()) {
                thought.text = text[..end].into();
                show(&mut drawn, vec![std::sync::Arc::new(thought.clone())]);
            }
            let body = |transcript: String| {
                transcript
                    .lines()
                    .filter_map(|row| row.trim_start().strip_prefix('▎'))
                    .filter(|row| !row.contains('◆'))
                    .flat_map(str::chars)
                    .filter(|c| !c.is_whitespace() && *c != '\0')
                    .collect::<String>()
            };
            let expected: String = text
                .replace("**", "")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            assert_eq!(body(drawn.transcript()), expected);
            for expanded in [false, true] {
                drawn.app.thinking_detail = if expanded {
                    Detail::Expanded
                } else {
                    Detail::Collapsed
                };
                let rows = drawn.app.reflow_history(drawn.size.0 as usize);
                let frame = drawn.app.frame(drawn.size);
                let wanted = drawn.screen.wanted_for(drawn.size, frame.live.len());
                drawn.emu = crate::ui::render::testterm::Emu::new(34, 20);
                let mut bytes = drawn.screen.rewrite(drawn.size, &rows, wanted);
                bytes.extend(drawn.screen.render(&frame, drawn.size));
                drawn.emu.feed(&bytes);
                if expanded {
                    assert_eq!(body(drawn.transcript()), expected);
                } else {
                    assert!(drawn.transcript().contains("Thinking…"));
                    assert!(body(drawn.transcript()).is_empty());
                }
            }
            thought.streaming = false;
            thought.elapsed_ms = Some(1500);
            show(&mut drawn, vec![std::sync::Arc::new(thought)]);
            assert_eq!(body(drawn.transcript()), expected);
        }
    }

    #[test]
    fn settings_keep_tool_and_thinking_details_independent() {
        use crate::ui::render::panel::strip_escapes;

        for fullscreen in [false, true] {
            for streaming in [false, true] {
                let mut drawn = Drawn::new((100, 40), None, true);
                drawn.app.fullscreen = fullscreen;
                let mut thought = (*message(false)).clone();
                thought.row_id = "thought".into();
                thought.kind = RowKind::Thinking;
                thought.text =
                    "Inspect the lock lifetime. The current guard remains held while the asynchronous operation waits for the worker, so the next caller cannot acquire it.\n\nMove the guard outside the await.".into();
                thought.streaming = streaming;
                let mut tool = (*message(false)).clone();
                tool.row_id = "tool".into();
                tool.kind = RowKind::ToolCall;
                tool.text.clear();
                tool.tool_name = Some("exec".into());
                tool.tool_input = Some(serde_json::json!({"command": "cargo check"}));
                tool.result_text = (!streaming).then(|| "Build succeeded".into());
                tool.pending = streaming;
                let mut compacted = (*message(false)).clone();
                compacted.row_id = "compacted".into();
                compacted.kind = RowKind::Compaction;
                compacted.text.clear();
                compacted.compaction = Some(crate::model::view::CompactionView {
                    state: CompactionState::Completed,
                    text: "The build configuration was checked.".into(),
                    detail: None,
                    insufficient: Vec::new(),
                });
                let rows = vec![
                    message(false),
                    compacted.into(),
                    thought.into(),
                    tool.into(),
                ];
                let snapshot = ViewSnapshot {
                    session_id: "s1".into(),
                    rows: rows.clone(),
                    focus: "main".into(),
                    history_complete: true,
                    side: Some(SideLine {
                        id: "side".into(),
                        purpose: "Check the build".into(),
                        state: LineState::Working,
                        rows,
                    }),
                    ..ViewSnapshot::default()
                };
                drawn
                    .app
                    .apply(Update::Snapshot(Box::new(snapshot.clone())));
                let frame = drawn.app.frame(drawn.size);
                let defaults = strip_escapes(&frame.commits.join("\n"))
                    + &strip_escapes(&frame.live.join("\n"));
                assert!(
                    !defaults.contains("Inspect the lock lifetime."),
                    "{defaults}"
                );
                assert!(!defaults.contains("cargo check"), "{defaults}");
                drawn.app.open_settings(Setting::ThinkingDetails);
                press(&mut drawn.app, KeyCode::Char(' '));
                press(&mut drawn.app, KeyCode::Esc);
                let theme = test_theme();
                drawn.app = App::new(
                    Box::new(Idle),
                    MarkdownRenderer::new(&theme),
                    theme.palette,
                    drawn._home.path().to_path_buf(),
                    "/work".into(),
                );
                drawn.app.fullscreen = fullscreen;
                drawn
                    .app
                    .apply(Update::Snapshot(Box::new(snapshot.clone())));
                let _ = drawn.app.frame(drawn.size);
                let expanded = strip_escapes(&drawn.app.reflow_history(100).join("\n"))
                    + &strip_escapes(&drawn.app.frame(drawn.size).live.join("\n"));
                assert_eq!(
                    expanded.matches("Inspect the lock lifetime.").count(),
                    2,
                    "{expanded}"
                );
                assert!(!expanded.contains("cargo check"), "{expanded}");

                drawn.app.open_settings(Setting::ThinkingDetails);
                press(&mut drawn.app, KeyCode::Char(' '));
                press(&mut drawn.app, KeyCode::Esc);
                drawn.app.run_command("tools", "");
                let collapsed = strip_escapes(&drawn.app.reflow_history(100).join("\n"))
                    + &strip_escapes(&drawn.app.frame(drawn.size).live.join("\n"));
                assert!(
                    !collapsed.contains("Inspect the lock lifetime."),
                    "{collapsed}"
                );
                assert!(collapsed.contains("cargo check"), "{collapsed}");

                let theme = test_theme();
                let mut reopened = App::new(
                    Box::new(Idle),
                    MarkdownRenderer::new(&theme),
                    theme.palette,
                    drawn._home.path().to_path_buf(),
                    "/work".into(),
                );
                reopened.fullscreen = fullscreen;
                reopened.apply(Update::Snapshot(Box::new(snapshot)));
                let frame = reopened.frame(drawn.size);
                let restored = strip_escapes(&frame.commits.join("\n"))
                    + &strip_escapes(&frame.live.join("\n"));
                assert!(
                    !restored.contains("Inspect the lock lifetime."),
                    "{restored}"
                );
                assert!(restored.contains("cargo check"), "{restored}");
                reopened.open_settings(Setting::ToolDetails);
                let frame = reopened.frame(drawn.size);
                let settings = strip_escapes(&frame.live.join("\n"))
                    + &strip_escapes(
                        &frame
                            .overlay
                            .map_or_else(Vec::new, |overlay| overlay.rows)
                            .join("\n"),
                    );
                assert!(
                    settings
                        .lines()
                        .any(|line| line.contains("Tool details") && line.contains("summary")),
                    "{settings}"
                );
                assert!(settings.lines().any(|line| line.contains("Thinking details") && line.contains("collapsed")), "{settings}");
                press(&mut reopened, KeyCode::Enter);
                press(&mut reopened, KeyCode::Esc);
                let full = strip_escapes(&reopened.reflow_history(100).join("\n"))
                    + &strip_escapes(&reopened.frame(drawn.size).live.join("\n"));
                assert_eq!(full.contains("Build succeeded"), !streaming, "{full}");
                reopened.run_command("tools", "");
                let folded = strip_escapes(&reopened.reflow_history(100).join("\n"))
                    + &strip_escapes(&reopened.frame(drawn.size).live.join("\n"));
                assert!(!folded.contains("cargo check"), "{folded}");
                assert!(!folded.contains("Inspect the lock lifetime."), "{folded}");
                assert!(
                    full.contains("The build configuration was checked."),
                    "{full}"
                );
                for text in [&expanded, &collapsed, &folded] {
                    assert!(
                        !text.contains("The build configuration was checked."),
                        "{text}"
                    );
                    assert!(text.contains("Context compacted"), "{text}");
                    assert_eq!(text.contains("Context compacted ▸"), fullscreen, "{text}");
                }
                assert_eq!(full.contains("Context compacted ▾"), fullscreen, "{full}");
                for text in [&expanded, &collapsed, &full, &folded] {
                    assert_eq!(text.contains(['▸', '▾']), fullscreen, "{text}");
                }
                reopened.set_fullscreen(!fullscreen);
                let frame = reopened.frame((44, 40));
                let switched = strip_escapes(&reopened.reflow_history(44).join("\n"))
                    + &strip_escapes(&frame.live.join("\n"));
                assert!(
                    !switched.contains("The build configuration was checked."),
                    "switching modes restores the collapsed compaction preset: {switched}"
                );
                assert_eq!(
                    switched.contains("Context compacted ▸"),
                    !fullscreen,
                    "{switched}"
                );
                if fullscreen {
                    for detail in [Detail::Collapsed, Detail::Summary, Detail::Expanded] {
                        reopened.set_fullscreen(true);
                        let _ = reopened.frame((44, 40));
                        let block = reopened.dropped
                            + reopened
                                .history
                                .iter()
                                .position(|block| matches!(block, Block::Compaction { .. }))
                                .unwrap();
                        assert!(reopened.click(Some(block), 1, 44));
                        let toggled = strip_escapes(&reopened.reflow_history(44).join("\n"));
                        assert_eq!(
                            toggled.contains("The build configuration was checked."),
                            detail != Detail::Expanded,
                            "{toggled}"
                        );
                        reopened.set_fullscreen(false);
                        let _ = reopened.frame((44, 40));
                        let restored = strip_escapes(&reopened.reflow_history(44).join("\n"));
                        assert_eq!(
                            restored.contains("The build configuration was checked."),
                            detail == Detail::Expanded,
                            "{restored}"
                        );
                        assert!(!restored.contains(['▸', '▾']), "{restored}");
                        reopened.run_command("tools", "");
                    }
                }
            }
        }
    }

    #[test]
    fn a_message_waits_in_the_live_tail_until_the_model_consumes_it() {
        let mut drawn = attached();
        show(&mut drawn, vec![message(true), hook()]);
        let text = drawn.transcript();
        assert!(text.contains("find the leak"), "a pending message is drawn");
        assert!(
            text.find("find the leak") < text.find("Hook context · 1 line"),
            "its hook's context follows it in the tail: {text}"
        );
        assert!(
            !drawn
                .app
                .history
                .iter()
                .any(|block| matches!(block, Block::User { .. } | Block::Hook { .. })),
            "neither the message nor its hook context is committed while unread"
        );
        show(&mut drawn, vec![message(false), hook()]);
        let user = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::User { text, .. } if text == "find the leak"))
            .expect("a consumed message settles into scrollback");
        let hook = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::Hook { .. }))
            .expect("its hook context commits with it");
        assert!(
            user < hook,
            "the message commits first and its hook row right after it"
        );
    }

    #[test]
    fn scrollback_settles_a_message_and_its_hook_context_together() {
        let mut drawn = attached();
        show(&mut drawn, vec![message(true), hook(), stopped()]);
        let user = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::User { .. }))
            .expect("the message is committed");
        let hook = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::Hook { .. }))
            .expect("its hook context is committed with it");
        let stop = drawn
            .app
            .history
            .iter()
            .position(|block| matches!(block, Block::Status { text, .. } if text == "stopped"))
            .expect("the stop is committed");
        assert!(
            user < hook && hook < stop,
            "a later settled row commits the message, then its hook row, then itself"
        );
        let text = drawn.transcript();
        assert!(
            text.find("find the leak") < text.find("Hook context · 1 line"),
            "{text}"
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
    fn timeline_dialogs_keep_actions_drafts_and_history_in_both_modes() {
        use crate::ui::render::panel::visible_width;

        for locale in [i18n::Locale::En, i18n::Locale::ZhCn] {
            i18n::set(locale);
            for fullscreen in [false, true] {
                for size in [(32, 12), (48, 24), (80, 24), (120, 40)] {
                    let mut drawn = Drawn::new(size, None, true);
                    drawn.app.fullscreen = fullscreen;
                    resume_asks(&mut drawn, 12);
                    drawn.app.snapshot.as_mut().unwrap().ended =
                        Some((45, crate::model::view::TurnEnd::Completed));
                    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                    drawn.app.host = Box::new(Recorder(sent.clone()));
                    drawn.app.composer.set_draft("unfinished draft");
                    let mut full = FullScreen::new(size, true);
                    let mut screen = Screen::new(size, true);
                    let mut emu =
                        crate::ui::render::testterm::Emu::new(size.0 as usize, size.1 as usize);
                    if fullscreen {
                        let frame = drawn.app.frame(size);
                        let bytes = full.render(&frame, size, &drawn.app.palette, &drawn.app);
                        emu.feed(&bytes);
                        full.scroll_to_top();
                    }
                    let mut shown = |app: &mut App| {
                        let frame = app.frame(size);
                        let rows = frame.overlay.as_ref().map_or(&frame.live, |o| &o.rows);
                        assert!(rows.iter().all(|row| visible_width(row) <= size.0 as usize));
                        if size.0 == 32 && matches!(app.prompt, Some(Prompt::Fork(_))) {
                            let lines: Vec<_> = rows
                                .iter()
                                .map(|row| crate::ui::render::panel::strip_escapes(row))
                                .collect();
                            for line in lines
                                .iter()
                                .skip_while(|line| !line.trim().starts_with('╭'))
                                .skip(1)
                                .take_while(|line| !line.trim().starts_with('╰'))
                            {
                                let text = line.trim().trim_matches('│').trim();
                                assert!(
                                    !text.is_empty() && text != "…",
                                    "compact fork dialog has no empty or ellipsis-only rows: {lines:?}"
                                );
                            }
                        }
                        let bytes = if fullscreen {
                            full.render(&frame, size, &app.palette, app)
                        } else {
                            screen.render(&frame, size)
                        };
                        emu.feed(&bytes);
                        emu.rows_text()
                            .join("\n")
                            .replace(['\0', '│'], "")
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    let before = shown(&mut drawn.app);
                    drawn.app.run_command("fork", "");
                    let text = shown(&mut drawn.app);
                    for label in [t("Fork session"), t("1. Create fork"), t("2. Cancel")] {
                        assert!(
                            text.contains(label),
                            "{size:?} fullscreen={fullscreen}: {text}"
                        );
                    }
                    drawn.key(KeyCode::Char('x'));
                    assert!(shown(&mut drawn.app).contains(t("2. Cancel")));
                    drawn.key(KeyCode::Enter);
                    assert!(sent.borrow().is_empty(), "Enter defaults to cancel");
                    let after = shown(&mut drawn.app);
                    assert!(after.contains("unfinished draft"));
                    if fullscreen {
                        assert_eq!(after, before, "cancelling restores the scrolled transcript");
                    }
                    drawn.app.run_command("timeline", "");
                    drawn.key(KeyCode::Home);
                    drawn.key(KeyCode::Down);
                    drawn.key(KeyCode::Enter);
                    let text = shown(&mut drawn.app);
                    assert!(text.contains(t("Timeline actions")), "{text}");
                    assert!(text.contains(t("Jump to message")), "{text}");
                    drawn.key(KeyCode::End);
                    let text = shown(&mut drawn.app);
                    assert!(text.contains(t("Fork session")), "{text}");
                    assert!(text.contains(t("Before this message")), "{text}");
                    assert!(text.contains("#2"), "the selected message is named: {text}");
                    drawn.key(KeyCode::Enter);
                    assert!(shown(&mut drawn.app).contains("ask 1"));
                    drawn.key(KeyCode::Esc);
                    let text = shown(&mut drawn.app);
                    assert!(text.contains(t("Timeline actions")), "{text}");
                    assert!(text.contains(t("Fork session")), "{text}");
                    drawn.key(KeyCode::Esc);
                    drawn.key(KeyCode::Enter);
                    drawn.key(KeyCode::Char('2'));
                    assert_eq!(drawn.app.composer.text(), "ask 1");
                    let text = shown(&mut drawn.app);
                    assert!(!text.contains(t("Timeline actions")), "{text}");
                    drawn.app.run_command("timeline", "");
                    drawn.key(KeyCode::Home);
                    drawn.key(KeyCode::Down);
                    drawn.key(KeyCode::Enter);
                    drawn.key(KeyCode::Char('5'));
                    shown(&mut drawn.app);
                    drawn.key(KeyCode::Up);
                    drawn.key(KeyCode::Enter);
                    shown(&mut drawn.app);
                    assert!(matches!(
                        sent.borrow().as_slice(),
                        [Command::Fork { at: 1, .. }]
                    ));
                    assert!(
                        emu.scrollback
                            .iter()
                            .all(|row| !row.contains(t("1. Create fork"))
                                && !row.contains(t("Timeline actions"))),
                        "dialogs must not become terminal history"
                    );
                }
            }
        }
        i18n::set(i18n::Locale::En);
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
        drawn.key(KeyCode::Enter);
        assert!(drawn.app.prompt.is_none(), "a jump closes the panel");
        let text = drawn.transcript();
        let page = text.find("Earlier history").expect("the page is drawn");
        assert!(text[page..].contains("ask 2"), "{text}");
        assert!(!text[page..].contains("ask 1\n"), "{text}");
        drawn.app.run_command("timeline", "");
        drawn.key(KeyCode::Enter);
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
                    cwd: None,
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
        let before = sent.borrow().len();
        let _ = drawn
            .app
            .handle(InputEvent::Key(crate::ui::terminal::KeyEvent::new(
                KeyCode::Left,
                Modifiers::SHIFT,
            )));
        assert!(drawn.app.composer.is_empty());
        assert_eq!(sent.borrow().len(), before);
        assert!(drawn.app.notices.last().unwrap().contains("stay queued"));
        assert_eq!(
            drawn.app.snapshot.as_ref().unwrap().queued[0].id,
            "queued-image"
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

    /// Editing a message that loaded a Skill prefills the operator's own text,
    /// so resending does not carry the block and load the Skill twice.
    #[test]
    fn editing_a_message_that_loaded_a_skill_prefills_the_operators_text_only() {
        let mut drawn = Drawn::new((80, 40), None, true);
        let mut projection = crate::projection::Projection::default();
        let event: crate::wire::Event = serde_json::from_value(serde_json::json!({
            "id": "input-1", "sequence": 1, "create_at": 1,
            "type": "input", "name": "message",
            "data": {"user": {"content": [
                {"type": "text", "text": "check the build"},
                {"type": "text", "text": "<dim-skill name=\"checkin\">\nSkill: checkin\nDescription: D\nFile: /x/SKILL.md\n\nInstructions:\nBODY\n</dim-skill>"},
            ]}, "mode": "queue"},
        }))
        .unwrap();
        projection.apply("main", &event);
        let applied: crate::wire::Event = serde_json::from_value(serde_json::json!({
            "id": "applied-1", "sequence": 2, "create_at": 2,
            "type": "input", "name": "applied", "data": {"input_id": "input-1"},
        }))
        .unwrap();
        projection.apply("main", &applied);
        let mut snapshot = projection.snapshot();
        snapshot.session_id = "s1".into();
        snapshot.focus = "main".into();
        snapshot.history_complete = true;
        drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
        drawn.draw();
        drawn.app.run_command("timeline", "");
        drawn.key(KeyCode::Up);
        drawn.key(KeyCode::Char('e'));
        assert_eq!(drawn.app.composer.text(), "check the build");
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
            cwd: None,
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
            cwd: None,
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
        let hint = tf(QUEUE_HINT, &[("key", "Ctrl+S")]);
        let full = queue_strip(&three, 3, 80, p, &hint);
        assert_eq!(queue_strip(&three[..1], 3, 80, p, &hint).len(), full.len());
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
            false,
        );
        assert_eq!(settled.len(), live.len());
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

    #[test]
    fn effort_picker_preserves_pending_questions_and_rejects_stale_model_choices() {
        for fullscreen in [false, true] {
            let (mut app, sent, _home) = asked();
            app.fullscreen = fullscreen;
            app.apply(Update::Ready(Configuration {
                provider_id: "relay".into(),
                model_id: "reasoner".into(),
                model_preferences: None,
                capability_snapshot: None,
            }));
            press(&mut app, KeyCode::Char(' '));
            let answer = selection(&app);
            for (result, message) in [
                (
                    Err("Reasoning not supported".into()),
                    "Reasoning not supported",
                ),
                (
                    Ok(crate::configure::EffortChoices {
                        model: "relay / reasoner".into(),
                        current: None,
                        choices: Vec::new(),
                    }),
                    "does not advertise",
                ),
            ] {
                let before = sent.borrow().len();
                typed(&mut app, "/effort");
                press(&mut app, KeyCode::Enter);
                assert!(
                    app.frame((36, 16))
                        .live
                        .join("\n")
                        .contains(t("Loading reasoning efforts…"))
                );
                app.apply(Update::EffortChoices(result));
                press(&mut app, KeyCode::Enter);
                assert!(app.frame((36, 16)).live.join("\n").contains(message));
                assert_eq!(sent.borrow().len(), before + 1);
                press(&mut app, KeyCode::Esc);
                assert_eq!(selection(&app), answer);
            }

            for stale in [false, true] {
                typed(&mut app, "/effort");
                press(&mut app, KeyCode::Enter);
                app.apply(Update::EffortChoices(Ok(crate::configure::EffortChoices {
                    model: "relay / reasoner".into(),
                    current: Some("high".into()),
                    choices: vec!["low".into(), "high".into(), "none".into()],
                })));
                let frame = app.frame((36, 16));
                assert!(
                    frame.overlay.is_none(),
                    "effort uses the inline AskUser location"
                );
                assert!(frame.live.join("\n").contains("› 2 (•) high"));
                assert!(frame.live.iter().all(|row| width_of(row) <= 36));
                if stale {
                    app.apply(Update::Ready(Configuration {
                        provider_id: "relay".into(),
                        model_id: "other".into(),
                        model_preferences: None,
                        capability_snapshot: None,
                    }));
                }
                let before = sent.borrow().len();
                press(&mut app, KeyCode::Down);
                press(&mut app, KeyCode::Enter);
                assert_eq!(selection(&app), answer);
                if stale {
                    assert_eq!(sent.borrow().len(), before);
                    assert!(app.notice.as_ref().unwrap().0.contains("/effort"));
                } else {
                    assert!(
                        matches!(sent.borrow().last(), Some(Command::Configure(text)) if text == "/effort none")
                    );
                }
            }
            assert!(!sent.borrow().iter().any(|command| matches!(
                command,
                Command::AskAnswers { .. }
                    | Command::AskCancel { .. }
                    | Command::SelectModel { .. }
            )));
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
                    held: true,
                    recallable: true,
                })
                .collect(),
            ..ViewSnapshot::default()
        })));
        (app, sent, home)
    }

    #[test]
    fn queue_display_keeps_applied_inputs_only_in_the_transcript() {
        use crate::ui::render::panel::strip_escapes;

        for locale in [i18n::Locale::En, i18n::Locale::ZhCn] {
            i18n::set(locale);
            for fullscreen in [false, true] {
                for width in [44, 100] {
                    let (mut app, _sent, _home) = session_with_queue(
                        true,
                        &[("applied", "find the leak"), ("held", "then add a test")],
                    );
                    app.fullscreen = fullscreen;
                    let mut snapshot = app.snapshot.as_ref().unwrap().clone();
                    snapshot.rows = vec![message(true)];
                    snapshot.queued[0].held = false;
                    snapshot.queued[0].recallable = false;
                    let mut draw = |snapshot: &ViewSnapshot| {
                        app.apply(Update::Snapshot(Box::new(snapshot.clone())));
                        let frame = app.frame((width, 30));
                        let rendered = app.reflow_history(width as usize).join("\n")
                            + "\n"
                            + &frame.live.join("\n");
                        strip_escapes(&rendered)
                    };
                    let mixed = draw(&snapshot);
                    assert!(
                        mixed.contains(&tf("Queue ({count})", &[("count", "1")])),
                        "{mixed}"
                    );
                    assert_eq!(mixed.matches("find the leak").count(), 1, "{mixed}");
                    assert!(mixed.contains("then add a test"), "{mixed}");
                    snapshot.queued.pop();
                    let applied = draw(&snapshot);
                    assert!(!applied.contains(t("Queue")), "{applied}");
                    assert!(!applied.contains(t("Unread")), "{applied}");
                    assert_eq!(applied.matches("find the leak").count(), 1, "{applied}");
                    snapshot.running = false;
                    snapshot.main_running = false;
                    snapshot.rows.push(stopped());
                    let idle = draw(&snapshot);
                    assert!(!idle.contains(t("Queue")), "{idle}");
                    assert_eq!(idle.matches("find the leak").count(), 1, "{idle}");
                    assert!(!idle.contains(t("Unread")), "{idle}");
                    snapshot.running = true;
                    snapshot.main_running = true;
                    let resumed = draw(&snapshot);
                    assert!(!resumed.contains(t("Unread")), "{resumed}");
                    snapshot.running = false;
                    snapshot.main_running = false;
                    snapshot.queued.clear();
                    snapshot.rows[0] = message(false);
                    let consumed = draw(&snapshot);
                    assert!(!consumed.contains(t("Unread")), "{consumed}");
                    assert_eq!(consumed.matches("find the leak").count(), 1, "{consumed}");
                }
            }
        }
        i18n::set(i18n::Locale::En);
    }

    #[test]
    fn choosing_goal_completes_before_dispatch_in_both_modes() {
        for fullscreen in [false, true] {
            for query in ["/go", "/goal"] {
                for selection in [KeyCode::Enter, KeyCode::Tab] {
                    for (objective, expected) in [
                        ("", crate::goal::Request::Show),
                        (
                            "fix the failing build",
                            crate::goal::Request::Objective("fix the failing build".into()),
                        ),
                    ] {
                        let (mut app, sent, _home) = session_with_queue(false, &[]);
                        app.fullscreen = fullscreen;
                        typed(&mut app, query);
                        press(&mut app, selection);
                        assert!(sent.borrow().is_empty(), "selection must not dispatch");
                        assert_eq!(app.composer.text(), "/goal ");
                        assert!(!app.composer.palette_open());
                        typed(&mut app, objective);
                        press(&mut app, KeyCode::Enter);
                        assert!(matches!(
                            sent.borrow().as_slice(),
                            [Command::Goal(request)] if *request == expected
                        ));
                        assert!(app.composer.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn every_loop_form_completes_then_dispatches_its_request_in_both_modes() {
        for fullscreen in [false, true] {
            for (words, expected) in [
                ("", crate::loop_view::Request::Show),
                (
                    "check the deploy",
                    crate::loop_view::Request::Start {
                        prompt: "check the deploy".into(),
                        every_seconds: None,
                    },
                ),
                (
                    "5m check the deploy",
                    crate::loop_view::Request::Start {
                        prompt: "check the deploy".into(),
                        every_seconds: Some(300),
                    },
                ),
                ("stop", crate::loop_view::Request::Stop(None)),
                ("7f2", crate::loop_view::Request::Stop(Some("7f2".into()))),
            ] {
                let (mut app, sent, _home) = session_with_queue(false, &[]);
                app.fullscreen = fullscreen;
                typed(&mut app, "/loop");
                // Selection completes the command and does not dispatch it.
                press(&mut app, KeyCode::Enter);
                assert_eq!(app.composer.text(), "/loop ");
                if words == "7f2" {
                    typed(&mut app, "stop ");
                }
                typed(&mut app, words);
                press(&mut app, KeyCode::Enter);
                assert!(
                    matches!(sent.borrow().as_slice(), [Command::Loop(request)] if *request == expected),
                    "{words:?} dispatches {expected:?}"
                );
                assert!(app.composer.is_empty());
            }
        }
    }

    /// An argument the command needs is refused when it is missing, so a bare
    /// `/rename` or `/branch` never reaches its handler with an empty value.
    /// Only `/loop` runs on its own, and its argument is declared optional.
    #[test]
    fn a_command_that_needs_its_argument_refuses_a_bare_submission() {
        for (command, needed) in [
            ("/rename", "/rename needs <title>"),
            ("/branch", "/branch needs <prompt>"),
        ] {
            let (mut app, sent, _home) = session_with_queue(false, &[]);
            // The palette completes the command and its argument hint first;
            // the submission that matters is the second Enter.
            typed(&mut app, command);
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Enter);
            assert!(
                sent.borrow().is_empty(),
                "{command} alone dispatches nothing"
            );
            // A refused submission lands in the status notice, not the
            // transcript (`tui.rs:3171`, `:1781-1783`).
            let frame = app.frame((100, 16)).live.join("\n");
            assert!(
                frame.contains(needed),
                "{command} alone prints {needed}: {frame}"
            );
        }
    }

    /// A second `stop` word is a usage error, which leaves the composer alone
    /// and prints the usage rather than dispatching.
    #[test]
    fn an_unreadable_loop_form_prints_usage_and_dispatches_nothing() {
        let (mut app, sent, _home) = session_with_queue(false, &[]);
        typed(&mut app, "/loop stop two words");
        press(&mut app, KeyCode::Enter);
        assert!(sent.borrow().is_empty(), "no command is dispatched");
        assert!(
            app.reports.iter().any(
                |block| matches!(block, Block::Status { text, .. } if text.contains("Usage: /loop"))
            ),
            "the usage is printed"
        );
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

    /// Tab in the palette only completes, so repeating it never runs a
    /// command and never exits, in either layout.
    #[test]
    fn repeated_tab_in_the_palette_only_completes() {
        let key = |code| InputEvent::Key(crate::ui::terminal::KeyEvent::new(code, Modifiers::NONE));
        for fullscreen in [false, true] {
            let (mut app, sent, _home) = session_with_queue(false, &[]);
            app.fullscreen = fullscreen;
            assert!(matches!(
                app.handle(key(KeyCode::Char('/'))),
                Outcome::Continue
            ));
            for _ in 0..3 {
                assert!(matches!(app.handle(key(KeyCode::Tab)), Outcome::Continue));
                assert_eq!(app.composer.text(), "/new");
            }
            assert!(sent.borrow().is_empty(), "Tab ran nothing");

            let (mut app, sent, _home) = session_with_queue(false, &[]);
            app.fullscreen = fullscreen;
            typed(&mut app, "/ke");
            assert!(matches!(app.handle(key(KeyCode::Tab)), Outcome::Continue));
            assert_eq!(app.composer.text(), "/keys");
            assert!(sent.borrow().is_empty(), "the key bindings did not open");
        }
    }

    #[test]
    fn default_send_mode_switches_draft_keys_and_survives_reopen() {
        let ctrl_s = InputEvent::Key(KeyEvent::new(KeyCode::Char('s'), Modifiers::CTRL));
        for fullscreen in [false, true] {
            let (mut app, sent, home) = session_with_queue(true, &[("a", "held")]);
            app.fullscreen = fullscreen;
            typed(&mut app, "queue");
            app.handle(InputEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                Modifiers::ALT,
            )));
            typed(&mut app, "me");
            assert!(sent.borrow().is_empty(), "Alt+Enter must insert a newline");
            press(&mut app, KeyCode::Enter);
            typed(&mut app, "steer me");
            app.handle(ctrl_s.clone());
            assert_eq!(
                describe(&sent.take()),
                ["submit:queue:queue\nme", "submit:steer:steer me"]
            );

            app.open_settings(Setting::DefaultSendMode);
            press(&mut app, KeyCode::Char(' '));
            press(&mut app, KeyCode::Esc);
            assert!(
                app.frame((110, 30))
                    .live
                    .join("\n")
                    .contains("empty draft: Enter steer")
            );
            app.run_command("keys", "");
            assert!(matches!(
                app.reports.last(),
                Some(Block::Status { text, .. }) if text.contains("Enter steer · Ctrl+S queue")
            ));
            app.handle(ctrl_s.clone());
            assert!(
                sent.borrow().is_empty(),
                "Ctrl+S needs a draft in steer mode"
            );
            press(&mut app, KeyCode::Enter);
            typed(&mut app, "steer by default");
            press(&mut app, KeyCode::Enter);
            typed(&mut app, "queue this one");
            app.handle(ctrl_s.clone());
            assert_eq!(
                describe(&sent.take()),
                [
                    "steer:a",
                    "submit:steer:steer by default",
                    "submit:queue:queue this one"
                ]
            );

            let theme = test_theme();
            let mut reopened = App::new(
                Box::new(Recorder(sent.clone())),
                MarkdownRenderer::new(&theme),
                theme.palette,
                home.path().to_path_buf(),
                "/work".into(),
            );
            reopened.fullscreen = fullscreen;
            typed(&mut reopened, "idle");
            press(&mut reopened, KeyCode::Enter);
            reopened.apply(Update::Snapshot(Box::new(app.snapshot.take().unwrap())));
            typed(&mut reopened, "running");
            press(&mut reopened, KeyCode::Enter);
            assert_eq!(
                describe(&sent.take()),
                ["submit:queue:idle", "submit:steer:running"]
            );
        }
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
        app.apply(Update::Withdrawn(dim_agent_types::UserInput {
            content: vec![dim_agent_types::ContentBlock::Text {
                text: "first".into(),
            }],
        }));
        app.apply(Update::Withdrawn(dim_agent_types::UserInput {
            content: vec![dim_agent_types::ContentBlock::Text {
                text: "second".into(),
            }],
        }));
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
        app.apply(Update::Withdrawn(dim_agent_types::UserInput {
            content: vec![dim_agent_types::ContentBlock::Text {
                text: "second".into(),
            }],
        }));
        assert_eq!(app.composer.text(), "second");
    }

    /// A withdrawn message returns as the operator's own words: a block the
    /// runtime appended after them stays out of the draft, and a later
    /// operator block that merely opens a `<dim-` name comes back.
    #[test]
    fn a_withdrawal_restores_the_operators_blocks_not_the_appended_context() {
        let (mut app, _sent, _home) = session_with_queue(true, &[]);
        app.apply(Update::Withdrawn(dim_agent_types::UserInput {
            content: vec![
                dim_agent_types::ContentBlock::Text {
                    text: "Why does the parser stall?".into(),
                },
                dim_agent_types::ContentBlock::Text {
                    text: "<dim-hooks kind=\"prompt-submit\">read src/parser.rs</dim-hooks>".into(),
                },
                dim_agent_types::ContentBlock::Text {
                    text:
                        "<dim-typed-by-the-operator>a second paragraph</dim-typed-by-the-operator>"
                            .into(),
                },
            ],
        }));
        assert_eq!(
            app.composer.text(),
            "Why does the parser stall?\n<dim-typed-by-the-operator>a second paragraph</dim-typed-by-the-operator>"
        );
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

    fn open_test_report(app: &mut App) -> u64 {
        press(app, KeyCode::Char('/'));
        typed(app, "report");
        press(app, KeyCode::Enter);
        match &app.prompt {
            Some(Prompt::Report(report)) => report.id,
            _ => panic!("expected report panel"),
        }
    }

    fn prepared_report(report_id: &str, plan_id: &str) -> crate::report::Prepared {
        serde_json::from_value(serde_json::json!({
            "reportId": report_id,
            "plan": {
                "planId": plan_id, "generation": {"kind": "local"},
                "evidence": {"timeline":{"recent":[],"truncated":false}}, "instructionsStatus": "none"
            }
        }))
        .unwrap()
    }

    #[test]
    fn report_targets_the_shown_session_during_a_run_and_keeps_the_home_fallback() {
        use crate::report::{Action, Captured, Operation, Reply, Request, Target};

        for fullscreen in [false, true] {
            let (mut app, sent, _home) = asked();
            app.fullscreen = fullscreen;
            app.apply(Update::Switched {
                session_id: String::new(),
                kind: SwitchKind::Resumed,
                cwd: None,
            });
            app.run_command("report", "");
            assert_eq!(
                app.notice.as_ref().map(|(text, _)| text.as_str()),
                Some(t("No session to report."))
            );
            app.apply(Update::Reportable {
                error: Captured::observed(
                    Operation::SessionRead,
                    Some("session_closed"),
                    "Could not read conversation state".into(),
                ),
                show_error: true,
            });
            open_test_report(&mut app);
            assert!(matches!(
                sent.borrow().last(),
                Some(Command::Report(Request {
                    action: Action::Prepare(Target::Error(_)),
                    ..
                }))
            ));
            press(&mut app, KeyCode::Esc);

            app.apply(Update::Switched {
                session_id: "s2".into(),
                kind: SwitchKind::Resumed,
                cwd: Some("/work".into()),
            });
            app.apply(Update::Snapshot(Box::new(ViewSnapshot {
                session_id: "s2".into(),
                running: true,
                ..ViewSnapshot::default()
            })));
            assert!(app.running());
            let panel = open_test_report(&mut app);
            assert!(
                matches!(sent.borrow().last(), Some(Command::Report(Request { action: Action::Prepare(Target::Session(session)), .. })) if session == "s2")
            );
            app.apply(Update::ReportReply {
                panel,
                reply: Reply::Prepared(Some("s2".into()), prepared_report("r2", "p1")),
            });
            let Some(Prompt::Report(report)) = &app.prompt else {
                panic!("expected report panel")
            };
            let screen = report.render(90, 20, &app.palette).join("\n");
            assert!(screen.contains(t("Report this session")));
            assert!(screen.contains(t("Evidence · No recent session activity")));
            press(&mut app, KeyCode::Esc);

            app.apply(Update::Switched {
                session_id: String::new(),
                kind: SwitchKind::Resumed,
                cwd: None,
            });
            app.run_command("report", "");
            assert_eq!(
                app.notice.as_ref().map(|(text, _)| text.as_str()),
                Some(t("No session to report."))
            );
        }
    }

    #[test]
    fn report_stays_open_on_snapshot_and_restores_the_pending_answer_on_close() {
        use crate::report::{Action, Reply, Request};

        for fullscreen in [false, true] {
            let (mut app, sent, _home) = asked();
            app.fullscreen = fullscreen;
            app.session_id = "s1".into();
            press(&mut app, KeyCode::Char(' '));
            let answer = selection(&app).expect("selected answer");
            let panel = open_test_report(&mut app);
            let mut snapshot = app.snapshot.clone().unwrap();
            snapshot.history_complete = true;
            app.apply(Update::Snapshot(Box::new(snapshot)));
            assert!(matches!(app.prompt, Some(Prompt::Report(_))));
            app.apply(Update::ReportReply {
                panel,
                reply: Reply::Prepared(Some("s1".into()), prepared_report("r1", "p1")),
            });
            press(&mut app, KeyCode::Esc);
            assert_eq!(selection(&app), Some(answer));
            assert!(matches!(
                sent.borrow().last(),
                Some(Command::Report(Request { action: Action::Cancel { draft, generation: false }, .. }))
                    if draft.report_id == "r1"
            ));
        }
    }

    #[test]
    fn report_replans_after_a_failed_generate_and_preserves_options_after_expiry() {
        use crate::report::{Action, Reply, Request, Target};

        let (mut app, sent, _home) = asked();
        app.session_id = "s1".into();
        let panel = open_test_report(&mut app);
        app.apply(Update::ReportReply {
            panel,
            reply: Reply::Prepared(Some("s1".into()), prepared_report("r1", "p1")),
        });
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        app.apply(Update::ReportReply {
            panel,
            reply: Reply::Plan(prepared_report("r1", "p2").plan),
        });
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        typed(&mut app, "omit timeline");
        press(&mut app, KeyCode::Enter);
        let mut plan = prepared_report("r1", "p3").plan;
        plan.evidence.timeline = None;
        plan.evidence.unavailable = Some(crate::report::EvidenceUnavailable::Omitted);
        app.apply(Update::ReportReply {
            panel,
            reply: Reply::Plan(plan),
        });
        let Some(Prompt::Report(report)) = &app.prompt else {
            panic!("expected report panel");
        };
        assert!(
            report
                .render(90, 20, &app.palette)
                .join("\n")
                .contains(t("Evidence · Timeline omitted"))
        );
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::Report(Request { action: Action::Generate(draft), .. }))
                if draft.plan_id == "p3"
        ));
        app.apply(Update::ReportReply {
            panel,
            reply: Reply::Failed {
                message: "Generation cancelled".into(),
                draft_missing: false,
            },
        });
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::Report(Request { action: Action::Plan { draft, redact: false, instructions }, .. }))
                if draft.report_id == "r1" && instructions == "omit timeline"
        ));
        app.apply(Update::ReportReply {
            panel,
            reply: Reply::Failed {
                message: "Report not found".into(),
                draft_missing: true,
            },
        });
        app.apply(Update::Switched {
            session_id: "s2".into(),
            kind: SwitchKind::Resumed,
            cwd: Some("/work".into()),
        });
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::Report(Request { action: Action::Prepare(Target::Session(session)), .. }))
                if session == "s1"
        ));
        app.apply(Update::ReportReply {
            panel,
            reply: Reply::Prepared(Some("s1".into()), prepared_report("r2", "p4")),
        });
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::Report(Request { action: Action::Plan { draft, redact: false, instructions }, .. }))
                if draft.report_id == "r2" && draft.session.as_deref() == Some("s1") && instructions == "omit timeline"
        ));
    }

    #[test]
    fn report_saves_only_bounded_zip_bytes_after_disconnect() {
        use crate::report::{Archive, GeneratedReport, Reply, Sharing};
        use base64::{Engine, engine::general_purpose::STANDARD};

        let valid = b"PK\x03\x04fixture".to_vec();
        let mut oversized = valid.clone();
        oversized.resize(64 * 1024 + 1, 0);
        for (bytes, should_save) in [
            (b"invalid".to_vec(), false),
            (oversized, false),
            (valid, true),
        ] {
            let (mut app, _sent, home) = asked();
            app.session_id = "s1".into();
            app.cwd_key = home.path().display().to_string();
            let panel = open_test_report(&mut app);
            app.apply(Update::ReportReply {
                panel,
                reply: Reply::Generated(GeneratedReport {
                    title: "Test report".into(),
                    archive: Archive {
                        base64: STANDARD.encode(&bytes),
                    },
                    sharing: Sharing {
                        github_issue_url: "https://github.com/another/repo/issues/new".into(),
                        discord_url: String::new(),
                        description: String::new(),
                    },
                }),
            });
            app.apply(Update::Failed("Disconnected".into()));
            press(&mut app, KeyCode::Enter);
            let Some(Prompt::Report(report)) = &app.prompt else {
                panic!("expected report panel");
            };
            assert_eq!(report.saved_path().is_some(), should_save);
            if let Some(path) = report.saved_path() {
                assert_eq!(path.parent(), Some(home.path()));
                assert!(
                    path.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("dim-report-")
                );
                assert_eq!(
                    path.extension().and_then(std::ffi::OsStr::to_str),
                    Some("zip")
                );
                assert_eq!(std::fs::read(path).unwrap(), bytes);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
                press(&mut app, KeyCode::Down);
                press(&mut app, KeyCode::Enter);
                let Some(Prompt::Report(report)) = &app.prompt else {
                    panic!("expected report panel");
                };
                assert!(
                    report
                        .render(90, 20, &app.palette)
                        .join("\n")
                        .contains(t("Could not open the share destination"))
                );
            } else {
                assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
            }
        }
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
                        source: None,
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
                seed: None,
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
            seed: None,
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
                action: ManagementAction::Run {
                    id: "job-1".into(),
                    source: None,
                    document: None,
                    seed: None,
                },
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
            RemoveVerb,
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
                            source: None,
                            label: "commit".into(),
                            detail: Vec::new(),
                            enabled: Some(true),
                            states: Vec::new(),
                            actions: vec![ActionKind::Toggle],
                            remove_verb: RemoveVerb::Remove,
                            document: None,
                            run: None,
                        }],
                    }],
                    ..ManagementView::default()
                }),
                note: None,
            },
        });
        app.prompt = Some(Prompt::Management(Box::new(panel)));
        let request = ManagementRequest {
            area: Area::Skills,
            action: ManagementAction::Toggle {
                id: "user:commit".into(),
                source: None,
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

    /// A reply for another Provider's models leaves this panel as it is: the
    /// panel manages one whole area, Provider scope included, and the notice
    /// still reports what the action came to.
    #[test]
    fn a_reply_for_another_providers_models_leaves_the_panel_as_it_is() {
        use crate::model::session::{
            ActionKind, ManagementEntry, ManagementGroup, ManagementReply, ManagementView,
            RemoveVerb,
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
        let row = |id: &str, label: &str| ManagementEntry {
            id: id.into(),
            source: None,
            label: label.into(),
            detail: Vec::new(),
            enabled: Some(true),
            states: Vec::new(),
            actions: vec![ActionKind::Toggle],
            remove_verb: RemoveVerb::Remove,
            document: None,
            run: None,
        };
        let area = Area::Models {
            provider_id: "a".into(),
        };
        let mut panel = ManagementPanel::new(area.clone(), None);
        panel.set_result(&ManagementReply {
            request: ManagementRequest {
                area: area.clone(),
                action: ManagementAction::List,
            },
            result: ManagementResult::View {
                view: Box::new(ManagementView {
                    groups: vec![ManagementGroup {
                        heading: "catalog".into(),
                        entries: vec![row("flash", "Flash")],
                    }],
                    ..ManagementView::default()
                }),
                note: None,
            },
        });
        app.prompt = Some(Prompt::Management(Box::new(panel)));
        app.apply(Update::Management(ManagementReply {
            request: ManagementRequest {
                area: Area::Models {
                    provider_id: "b".into(),
                },
                action: ManagementAction::Toggle {
                    id: "other".into(),
                    source: None,
                    enabled: false,
                },
            },
            result: ManagementResult::View {
                view: Box::new(ManagementView {
                    groups: vec![ManagementGroup {
                        heading: "catalog".into(),
                        entries: vec![row("other", "Other")],
                    }],
                    ..ManagementView::default()
                }),
                note: Some("Disabled other.".into()),
            },
        }));
        let shown = match &app.prompt {
            Some(Prompt::Management(panel)) => panel.render(80, 20, &theme.palette).join(" "),
            _ => panic!("the panel for the first Provider stays open"),
        };
        assert!(shown.contains("Flash"), "{shown}");
        assert!(!shown.contains("Other"), "{shown}");
        let (text, _) = app.notice.clone().unwrap();
        assert_eq!(text, "Disabled other.");
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
                action: ManagementAction::Run {
                    id: "job-1".into(),
                    source: None,
                    document: None,
                    seed: None,
                },
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
            seed: None,
        });
        let path = app
            .management_edits
            .last()
            .unwrap()
            .draft
            .path()
            .display()
            .to_string();
        app.prompt = Some(Prompt::Management(Box::new(ManagementPanel::new(
            Area::Skills,
            None,
        ))));
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
                seed: None,
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
            app.prompt = Some(Prompt::Management(Box::new(ManagementPanel::new(
                Area::Scheduled,
                None,
            ))));
            let path = draft(&mut app);
            app.apply(create_reply(result));
            assert!(path.exists(), "a create keeps its file: {path:?}");
            let kept = app.management_edits.first().expect("the link stays");
            assert_eq!(kept.draft.path(), path.as_path());
        }
    }

    /// A run's draft is the operation's input: the editor's text is sent
    /// with the run, a success removes the file, and a refused run keeps it,
    /// for its own kind only.
    #[cfg(unix)]
    #[test]
    fn a_run_draft_is_sent_with_the_run_and_reopened_only_by_its_own_kind() {
        use crate::model::session::{ManagementReply, ManagementView};
        use std::os::unix::process::ExitStatusExt;
        fn run_reply(result: ManagementResult) -> Update {
            Update::Management(ManagementReply {
                request: ManagementRequest {
                    area: Area::Skills,
                    action: ManagementAction::Run {
                        id: "job-1".into(),
                        source: None,
                        document: Some(serde_json::json!({"target": "edited"})),
                        seed: Some(serde_json::json!({"target": "src"})),
                    },
                },
                result,
            })
        }
        let root = tempfile::tempdir().unwrap();
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            root.path().join("home"),
            "/w".into(),
        );
        let draft =
            || editor::Draft::open(root.path(), Area::Skills, "{\"target\":\"edited\"}").unwrap();
        let written = |app: &mut App, draft: editor::Draft| {
            let path = draft.path().to_path_buf();
            app.prompt = Some(Prompt::Management(Box::new(ManagementPanel::new(
                Area::Skills,
                None,
            ))));
            app.write_draft(
                ManagementDraft {
                    area: Area::Skills,
                    target: WriteTarget::Run {
                        id: "job-1".into(),
                        source: None,
                    },
                    draft,
                    seed: Some(serde_json::json!({"target": "src"})),
                },
                false,
                "vi".into(),
                Ok(std::process::ExitStatus::from_raw(0)),
            );
            path
        };
        // The text the editor left is the run's document.
        let path = written(&mut app, draft());
        assert!(matches!(
            sent.borrow().last(),
            Some(Command::Management(ManagementRequest {
                area: Area::Skills,
                action: ManagementAction::Run {
                    id,
                    source,
                    document: Some(document),
                    seed: Some(seed),
                },
            })) if id == "job-1"
                && *document == serde_json::json!({"target": "edited"})
                && *seed == serde_json::json!({"target": "src"})
        ));
        app.apply(run_reply(ManagementResult::View {
            view: Box::new(ManagementView::default()),
            note: None,
        }));
        assert!(!path.exists(), "a written run removes its draft");
        assert!(app.management_edits.is_empty());
        // A refusal keeps the file and the link; Ctrl+T reopens it, Ctrl+E
        // never does.
        let path = written(&mut app, draft());
        app.apply(run_reply(ManagementResult::Refused(
            ManagementRefusal::Refused("nope".into()),
        )));
        assert!(path.exists(), "a refused run keeps its draft");
        let text = match &app.prompt {
            Some(Prompt::Management(panel)) => panel.render(100, 20, &app.palette).join(" "),
            _ => panic!("the panel stays open"),
        };
        assert!(text.contains("nope"), "{text}");
        assert!(text.contains("Your text is kept in"), "{text}");
        assert!(
            app.kept_draft(
                &Area::Skills,
                &WriteTarget::Entry {
                    id: "job-1".into(),
                    source: None,
                    revision: "1".into(),
                },
            )
            .is_none(),
            "an edit never reopens a run's draft"
        );
        let (target, kept, seed) = app
            .kept_draft(
                &Area::Skills,
                &WriteTarget::Run {
                    id: "job-1".into(),
                    source: None,
                },
            )
            .expect("the run key reopens the draft");
        assert_eq!(
            target,
            WriteTarget::Run {
                id: "job-1".into(),
                source: None,
            }
        );
        assert_eq!(
            seed,
            Some(serde_json::json!({"target": "src"})),
            "the kept draft keeps the seed it was opened with, not the edited text"
        );
        assert_eq!(kept.path(), path.as_path());
    }

    #[test]
    fn compaction_disclosure_shares_body_releases_it_and_keeps_attachment_identity() {
        use crate::model::view::CompactionDetailState;
        for fullscreen in [false, true] {
            let commands = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut drawn = Drawn::new((100, 32), None, true);
            drawn.app.host = Box::new(Recorder(commands.clone()));
            drawn.app.fullscreen = fullscreen;
            drawn.app.tool_detail = Detail::Collapsed;
            let mut projection = crate::projection::Projection::default();
            let event = serde_json::from_value(serde_json::json!({"id":"compact-7","sequence":7,"create_at":1,"type":"display","name":"compaction","data":{"strategy":"summary"}})).unwrap();
            projection.apply("main", &event);
            let mut snapshot = projection.snapshot();
            snapshot.session_id = "source".into();
            snapshot.focus = "main".into();
            snapshot.history_complete = true;
            drawn
                .app
                .apply(Update::Snapshot(Box::new(snapshot.clone())));
            drawn.draw();
            assert!(commands.borrow().iter().all(|command| !matches!(command, Command::CompactionDetails { targets, .. } if !targets.is_empty())));
            let block = drawn.app.dropped
                + drawn
                    .app
                    .history
                    .iter()
                    .position(|block| matches!(block, Block::Compaction { .. }))
                    .unwrap();
            assert!(drawn.app.click(Some(block), 1, 100));
            assert!(
                matches!(commands.borrow().last(), Some(Command::CompactionDetails { targets, .. }) if targets.len() == 1 && targets.first().unwrap().session_id == "source")
            );
            let body: std::sync::Arc<str> = "Loaded compaction body".into();
            let weak = std::sync::Arc::downgrade(&body);
            let mut loaded = snapshot.clone();
            let view = std::sync::Arc::make_mut(&mut loaded.rows[0])
                .compaction
                .as_mut()
                .unwrap();
            view.text = body.clone();
            view.detail.as_mut().unwrap().state = CompactionDetailState::Loaded;
            drawn.app.apply(Update::Snapshot(Box::new(loaded)));
            drawn.draw();
            assert!(drawn_text(&mut drawn).contains("Loaded compaction body"));
            assert!(drawn.app.history.iter().any(|block| matches!(block, Block::Compaction { text, .. } if std::sync::Arc::ptr_eq(text, &body))));
            drop(body);
            if fullscreen {
                drawn.app.set_fullscreen(false);
            } else {
                assert!(drawn.app.click(Some(block), 1, 100));
            }
            assert!(
                matches!(commands.borrow().last(), Some(Command::CompactionDetails { targets, .. }) if targets.is_empty())
            );
            drawn
                .app
                .apply(Update::Snapshot(Box::new(snapshot.clone())));
            drawn.draw();
            assert!(
                weak.upgrade().is_none(),
                "closed detail has no retained shared body"
            );
            assert!(drawn.app.click(Some(block), 1, 100));
            drawn.app.switch_session("fork".into(), SwitchKind::Resumed);
            snapshot.session_id = "fork".into();
            snapshot.projection_id = 1;
            drawn.app.apply(Update::Snapshot(Box::new(snapshot)));
            drawn.draw();
            drawn.app.toggle_tool_details();
            drawn.app.toggle_tool_details();
            assert!(
                matches!(commands.borrow().last(), Some(Command::CompactionDetails { projection_id: 1, targets }) if targets.len() == 1 && targets.first().unwrap().session_id == "fork")
            );
            drawn.app.tool_detail = Detail::Collapsed;
            let mut task = background_task("member-task", "Member");
            task.kind = crate::model::view::TaskKind::Member(
                crate::model::view::MemberTarget::Session("member".into()),
            );
            let mut main = drawn.app.snapshot.as_ref().unwrap().clone();
            main.tasks.push(task.clone());
            drawn.app.apply(Update::Snapshot(Box::new(main.clone())));
            let mut panel = TasksPanel::new(&main);
            panel.open_card(task);
            panel.member(
                "member",
                Box::new(projection.snapshot()),
                crate::model::view::MemberConversationState::Live,
                None,
            );
            drawn.app.prompt = Some(Prompt::Tasks(panel));
            commands.borrow_mut().clear();
            drawn.draw();
            assert!(commands.borrow().iter().any(|command| matches!(command,Command::CompactionDetails {targets,..} if targets.iter().any(|target| target.session_id=="member"))), "an expanded member detail requests its body while global disclosure is collapsed");
        }
    }

    fn drawn_text(drawn: &mut Drawn) -> String {
        use crate::ui::render::panel::strip_escapes;
        strip_escapes(&drawn.app.reflow_history(100).join("\n"))
            + &strip_escapes(&drawn.app.frame(drawn.size).live.join("\n"))
            + &strip_escapes(&drawn.emu.rows_text().join("\n"))
    }

    fn retry_at(offset_ms: i64) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        now.saturating_add_signed(offset_ms)
    }

    #[test]
    fn the_working_line_shows_a_running_retry() {
        use crate::model::view::RetryView;
        use dim_agent_types::ModelFailureKind;
        let mut drawn = Drawn::new((160, 40), None, true);
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            focus: "main".into(),
            history_complete: true,
            retry: Some(RetryView {
                attempt: 3,
                max_attempts: 8,
                retry_at_ms: retry_at(60_000),
                kind: ModelFailureKind::ServerError,
                reason: Some("The server is overloaded".into()),
            }),
            ..ViewSnapshot::default()
        })));
        drawn.draw();
        let shown = drawn_text(&mut drawn);
        assert!(
            shown.contains("Retrying 3/8 in 60 s · The provider reported a server error: The server is overloaded · Esc stops"),
            "{shown}"
        );

        // The countdown never goes below zero.
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            focus: "main".into(),
            history_complete: true,
            retry: Some(RetryView {
                attempt: 2,
                max_attempts: 8,
                retry_at_ms: 0,
                kind: ModelFailureKind::Network,
                reason: None,
            }),
            ..ViewSnapshot::default()
        })));
        drawn.draw();
        let shown = drawn_text(&mut drawn);
        assert!(
            shown.contains(
                "Waiting for the provider · Could not reach the provider, or the connection dropped · Esc stops"
            ),
            "{shown}"
        );

        // Clearing the retry brings the working line back.
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            running: true,
            focus: "main".into(),
            history_complete: true,
            ..ViewSnapshot::default()
        })));
        drawn.draw();
        let shown = drawn_text(&mut drawn);
        assert!(!shown.contains("Retrying"), "{shown}");
        assert!(shown.contains("working…"), "{shown}");
    }

    #[test]
    fn an_error_rows_details_join_the_tools_cycle() {
        use crate::model::view::FailureDetail;
        use dim_agent_types::ModelFailureKind;
        let mut error = (*message(false)).clone();
        error.row_id = "error-1".into();
        error.kind = RowKind::Error;
        error.text = "The provider reported a server error: The server is overloaded. Try again later or choose another model.".into();
        error.tone = Some(crate::model::view::RowTone::Danger);
        error.failure = Some(FailureDetail {
            kind: ModelFailureKind::ServerError,
            status: Some(503),
            provider_code: Some("overloaded".into()),
            request_id: Some("req-provider".into()),
            attempts: 3,
            waited_ms: 4200,
            retry_after_ms: Some(60_000),
            cause: vec!["upstream 503".into()],
        });
        let mut drawn = attached();
        show(&mut drawn, vec![error.into()]);
        let collapsed = drawn_text(&mut drawn);
        assert!(!collapsed.contains("kind: server_error"), "{collapsed}");
        drawn.app.run_command("tools", "");
        drawn.draw();
        let expanded = drawn_text(&mut drawn);
        for line in [
            "kind: server_error",
            "status: 503",
            "provider code: overloaded",
            "request id: req-provider",
            "tried 3 times over 4.2 s",
            "the provider asked to wait 60 s",
            "cause:",
            "upstream 503",
        ] {
            assert!(expanded.contains(line), "{line:?} missing: {expanded}");
        }
    }

    /// A Session the daemon refuses for its protocol version is refused again
    /// by every run of this client, so the failure block and the Enter notice
    /// offer no command to run it.
    #[test]
    fn a_refused_protocol_version_offers_no_resume_command() {
        for (resumable, offered) in [(true, true), (false, false)] {
            let mut drawn = Drawn::new((100, 40), None, true);
            drawn.app.switch_session("s1".into(), SwitchKind::Resumed);
            drawn.app.apply(Update::StartupFailed {
                message: "Session was created by protocol version 5; this connection speaks 4"
                    .into(),
                resumable,
            });
            drawn.draw();
            let shown = drawn_text(&mut drawn);
            assert_eq!(shown.contains("Exit and run:"), offered, "{shown}");
            assert_eq!(shown.contains("dimcode --resume s1"), offered, "{shown}");
            drawn.key(KeyCode::Enter);
            let notice = drawn_text(&mut drawn);
            assert_eq!(
                notice.contains("Exit and resume this Session."),
                offered,
                "{notice}"
            );
            if !offered {
                assert!(notice.contains("Cannot open this Session"), "{notice}");
            }
        }
    }

    /// #1474: `/resume` in a repository with several worktrees must draw one
    /// page. The panel opens on the exact launch workspace and asks for its
    /// first page; the git probe answers later with the repository's
    /// worktrees, and that answer may not clear a page already drawn.
    ///
    /// The git read itself is covered where it lives
    /// (`session_host::tests::the_git_read_answers_one_worktree_several_and_nothing_outside_git`);
    /// this drives the client's state sequence, which is what the operator
    /// sees, and injects the set that read publishes.
    #[test]
    fn resume_draws_one_page_in_a_repository_with_several_worktrees() {
        use crate::model::session::{SessionItem, SessionsScope};

        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("main");
        let sibling = root.path().join("fix");
        std::fs::create_dir_all(&launch).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let set = vec![
            launch.to_string_lossy().into_owned(),
            sibling.to_string_lossy().into_owned(),
        ];

        let item = |id: &str, title: &str, cwd: &std::path::Path| SessionItem {
            id: id.into(),
            title: Some(title.into()),
            preview: None,
            cwd: cwd.to_string_lossy().into_owned(),
            updated_at: u64::MAX,
            model: "p/m".into(),
            origin: None,
        };
        let here = item("s-here", "Fix the failing test", &launch);
        let also = item("s-also", "Retry the flaky test", &launch);
        let there = item("s-there", "Update the cache", &sibling);

        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut drawn = Drawn::new((100, 30), None, true);
        drawn.app.host = Box::new(Recorder(sent.clone()));

        // What the operator sees, in reading order: the panel's title, its
        // loading line, and its rows.
        const MARKS: [&str; 6] = [
            "Sessions · This workspace",
            "Sessions · This repository",
            "Loading sessions...",
            "Fix the failing test",
            "Retry the flaky test",
            "Update the cache",
        ];
        let states = |drawn: &mut Drawn| -> Vec<&'static str> {
            drawn.draw();
            let text = drawn.emu.rows_text().join("\n");
            MARKS
                .iter()
                .copied()
                .filter(|mark| text.contains(mark))
                .collect()
        };
        // The first-page reads asked for since the last call: their id and
        // their scope, which is `None` for a cursor-less first page.
        let reads = |sent: &std::rc::Rc<std::cell::RefCell<Vec<Command>>>| {
            let mut commands = sent.borrow_mut();
            let reads: Vec<(String, SessionsScope)> = commands
                .iter()
                .filter_map(|command| match command {
                    Command::ListSessions {
                        id, scope, cursor, ..
                    } => {
                        assert!(cursor.is_none(), "a first page carries no cursor");
                        Some((id.clone(), scope.clone()))
                    }
                    _ => None,
                })
                .collect();
            commands.clear();
            reads
        };
        // Pressing Enter resumes the selected row: the id it picks.
        let pressed = |drawn: &mut Drawn, sent: &std::rc::Rc<std::cell::RefCell<Vec<Command>>>| {
            sent.borrow_mut().clear();
            drawn.key(KeyCode::Enter);
            let picked = sent.borrow_mut().iter().find_map(|command| match command {
                Command::Resume(id) => Some(id.clone()),
                _ => None,
            });
            sent.borrow_mut().clear();
            picked.expect("Enter resumes the selected row")
        };

        let mut sequence: Vec<(&str, Vec<&'static str>)> = Vec::new();
        for open in 1..=2 {
            typed(&mut drawn.app, "/resume");
            drawn.key(KeyCode::Enter);
            sequence.push(("opened", states(&mut drawn)));

            let asked = reads(&sent);
            assert_eq!(
                asked.len(),
                1,
                "open {open}: one read is asked for at open: {asked:?}"
            );
            assert!(
                matches!(asked[0].1, SessionsScope::Local(None)),
                "open {open}: the opening read lists the exact launch workspace: {asked:?}"
            );
            drawn.app.apply(Update::Sessions {
                id: asked[0].0.clone(),
                paged: true,
                result: Ok((vec![here.clone(), also.clone()], None)),
            });
            sequence.push(("first page", states(&mut drawn)));

            // The operator moves to the second row before git answers, and the
            // wider page lists that row third: the selection must stay on it,
            // not on the same index.
            drawn.key(KeyCode::Down);
            drawn.app.apply(Update::RepositoryWorkspaces(set.clone()));
            let answered = states(&mut drawn);
            assert!(
                !answered.contains(&"Loading sessions..."),
                "open {open}: the git answer does not clear the drawn page: {answered:?}"
            );
            sequence.push(("git answer", answered));

            drawn.app.tick();
            let widened = reads(&sent);
            assert!(
                matches!(
                    widened.as_slice(),
                    [(_, SessionsScope::Local(Some(asked)))] if *asked == set
                ),
                "open {open}: the wider scope reads its first page: {widened:?}"
            );
            drawn.app.apply(Update::Sessions {
                id: widened[0].0.clone(),
                paged: true,
                result: Ok((vec![here.clone(), there.clone(), also.clone()], None)),
            });
            sequence.push(("wider page", states(&mut drawn)));
            assert_eq!(
                pressed(&mut drawn, &sent),
                "s-also",
                "open {open}: the selection stays on its row, now the third"
            );
        }

        for open in 1..=2 {
            let state = |label: &str| -> &Vec<&'static str> {
                sequence
                    .iter()
                    .filter_map(|(recorded, states)| (*recorded == label).then_some(states))
                    .nth(open - 1)
                    .unwrap_or_else(|| {
                        panic!("open {open} {label} was not recorded: {sequence:#?}")
                    })
            };
            assert_eq!(
                state("opened"),
                &["Sessions · This workspace", "Loading sessions..."],
                "open {open} opens on the launch workspace"
            );
            assert_eq!(
                state("wider page"),
                &[
                    "Sessions · This repository",
                    "Fix the failing test",
                    "Retry the flaky test",
                    "Update the cache",
                ],
                "open {open} ends on the repository's page without a second loading line"
            );
        }
    }

    /// `count` message rows `u0..u{count-1}`, the way a loaded window holds them.
    fn window_rows(count: usize) -> Vec<std::sync::Arc<ViewRow>> {
        (0..count)
            .map(|n| {
                std::sync::Arc::new(ViewRow {
                    row_id: format!("u{n}"),
                    provisional_id: None,
                    kind: RowKind::UserMessage,
                    display: None,
                    text: format!("ask {n}"),
                    streaming: false,
                    elapsed_ms: None,
                    tone: None,
                    call_id: None,
                    tool_lookup_done: false,
                    tool_name: None,
                    tool_input: None,
                    batch: None,
                    parent_call_id: None,
                    pending: false,
                    parked: false,
                    result_text: None,
                    result_is_error: false,
                    diff: None,
                    exit_code: None,
                    member: None,
                    cut: Some(n as u64),
                    line: None,
                    parent_row: None,
                    compaction: None,
                    failure: None,
                    reportable: false,
                })
            })
            .collect()
    }

    /// Older rows `old0..old{count-1}`, distinct from `window_rows`.
    fn older_rows(count: usize) -> Vec<std::sync::Arc<ViewRow>> {
        (0..count)
            .map(|n| {
                let mut row = (*window_rows(1)[0]).clone();
                row.row_id = format!("old{n}");
                row.text = format!("older {n}");
                std::sync::Arc::new(row)
            })
            .collect()
    }

    /// A fullscreen app showing one window of rows, armed, its floor at 0.
    fn fullscreen_app() -> (App, std::rc::Rc<std::cell::RefCell<Vec<Command>>>) {
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            std::path::PathBuf::from("/tmp"),
            "/work".into(),
        );
        app.fullscreen = true;
        app.snapshot = Some(ViewSnapshot {
            session_id: "s1".into(),
            rows: window_rows(HISTORY_WINDOW),
            focus: "main".into(),
            history_complete: true,
            ..ViewSnapshot::default()
        });
        app.window_floor = 0;
        app.history_floor = 0;
        (app, sent)
    }

    /// Publishes `rows` as `s1`'s snapshot. The first call arms the window
    /// over the newest `HISTORY_WINDOW` messages, as a resume does.
    fn show_window(drawn: &mut Drawn, rows: Vec<std::sync::Arc<ViewRow>>) {
        // The host keeps publishing `earlier` until the read ends, so the
        // fixture carries it from one snapshot to the next.
        let earlier = drawn
            .app
            .snapshot
            .as_ref()
            .map_or(Earlier::AtStart, |snapshot| snapshot.earlier);
        drawn.app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            earlier,
            ..ViewSnapshot::default()
        })));
    }

    #[test]
    fn follow_floors_keeps_each_floor_on_its_row_through_prepend_append_and_trim() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.fullscreen = true;
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        // Two windows of rows; the next frame arms the floor on the first
        // message of the newest window, hiding one window's worth above it.
        show_window(&mut drawn, window_rows(2 * HISTORY_WINDOW));
        drawn.draw();
        assert_eq!(drawn.app.window_floor, HISTORY_WINDOW);
        assert_eq!(drawn.app.history_floor, HISTORY_WINDOW);
        // A prepend of one window of older rows keeps the anchored row where
        // it was relative to the rows that follow it.
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW));
        show_window(&mut drawn, rows);
        assert_eq!(drawn.app.window_floor, 2 * HISTORY_WINDOW);
        assert_eq!(drawn.app.history_floor, 2 * HISTORY_WINDOW);
        // An append moves no floor.
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW + 1));
        show_window(&mut drawn, rows);
        assert_eq!(drawn.app.window_floor, 2 * HISTORY_WINDOW);
        // A front trim removes rows above the anchored one only: it keeps its
        // identity and moves down by the number removed.
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW + 1));
        let trimmed = HISTORY_WINDOW / 2;
        show_window(&mut drawn, rows[trimmed..].to_vec());
        assert_eq!(drawn.app.window_floor, 2 * HISTORY_WINDOW - trimmed);
        assert_eq!(drawn.app.history_floor, 2 * HISTORY_WINDOW - trimmed);
        // A floor of 0 names the first row: a prepend of N rows moves it to N.
        show_window(&mut drawn, window_rows(2 * HISTORY_WINDOW));
        drawn.app.window_floor = 0;
        drawn.app.history_floor = 0;
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW));
        show_window(&mut drawn, rows);
        assert_eq!(drawn.app.window_floor, HISTORY_WINDOW);
        assert_eq!(drawn.app.history_floor, HISTORY_WINDOW);
    }

    #[test]
    fn follow_floors_leaves_an_unarmed_window_alone() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.fullscreen = true;
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW));
        show_window(&mut drawn, rows);
        assert!(drawn.app.awaiting_window, "no frame has armed the window");
        assert_eq!(drawn.app.window_floor, 0, "an unarmed floor stays put");
        assert_eq!(drawn.app.history_floor, 0);
    }

    #[test]
    fn follow_floors_lands_on_the_first_surviving_row_when_the_floor_row_is_gone() {
        let mut drawn = Drawn::new((80, 40), None, true);
        drawn.app.fullscreen = true;
        drawn
            .app
            .switch_session("s1".into(), crate::model::session::SwitchKind::Resumed);
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW));
        show_window(&mut drawn, rows);
        drawn.draw();
        let floor = drawn.app.window_floor;
        assert_eq!(
            floor,
            2 * HISTORY_WINDOW,
            "three windows of messages hide two of them"
        );
        // A trim removes the floor row and the rows after it, up to old3.
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(2 * HISTORY_WINDOW));
        // rows[floor + 3] is the first row that survives the trim.
        show_window(&mut drawn, rows[floor + 3..].to_vec());
        assert_eq!(
            drawn.app.window_floor, 0,
            "the first surviving row after the gone one takes over"
        );
        assert_eq!(
            drawn.app.snapshot.as_ref().unwrap().rows[0].row_id,
            rows[floor + 3].row_id
        );
    }

    #[test]
    fn show_earlier_requests_unread_history_and_does_nothing_at_the_start() {
        let theme = test_theme();
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut app = App::new(
            Box::new(Recorder(sent.clone())),
            MarkdownRenderer::new(&theme),
            theme.palette.clone(),
            std::path::PathBuf::from("/tmp"),
            "/work".into(),
        );
        app.fullscreen = true;
        app.snapshot = Some(ViewSnapshot {
            session_id: "s1".into(),
            rows: window_rows(2 * HISTORY_WINDOW),
            focus: "main".into(),
            history_complete: true,
            earlier: Earlier::Unread,
            ..ViewSnapshot::default()
        });
        // Every loaded row is drawn, so the request is the only way forward.
        app.window_floor = 0;
        app.show_earlier();
        assert_eq!(sent.borrow().len(), 1, "an unread prefix is requested");
        assert!(matches!(sent.borrow()[0], Command::Earlier));
        assert!(app.reveal_pending, "the arriving page is to be revealed");
        // A live-read snapshot keeps the request; it is still in flight.
        app.snapshot.as_mut().unwrap().earlier = Earlier::Reading;
        app.apply_earlier(None);
        assert!(
            app.reveal_pending,
            "a live snapshot must not drop the reveal"
        );
        // `Reading` asks for nothing new.
        sent.borrow_mut().clear();
        app.show_earlier();
        assert!(sent.borrow().is_empty(), "a read in flight is not repeated");
        assert!(app.reveal_pending);
        // At the start there is nothing to show and nothing is sent.
        app.reveal_pending = false;
        sent.borrow_mut().clear();
        app.snapshot.as_mut().unwrap().earlier = Earlier::AtStart;
        let floor = app.window_floor;
        app.show_earlier();
        assert!(sent.borrow().is_empty(), "AtStart sends nothing");
        assert!(!app.reveal_pending, "AtStart reveals nothing");
        assert_eq!(app.window_floor, floor, "AtStart does not move the floor");
    }

    #[test]
    fn a_pending_reveal_survives_a_live_snapshot_and_shows_the_page_that_lands() {
        let (mut app, sent) = fullscreen_app();
        app.snapshot.as_mut().unwrap().earlier = Earlier::Unread;
        app.show_earlier();
        assert!(matches!(sent.borrow().as_slice(), [Command::Earlier]));
        assert!(app.reveal_pending);
        // A live turn appends one row while the read is in flight.
        let mut rows = window_rows(HISTORY_WINDOW);
        let mut live = (*window_rows(1)[0]).clone();
        live.row_id = "live".into();
        live.text = "streamed".into();
        rows.push(std::sync::Arc::new(live));
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            earlier: Earlier::Reading,
            ..ViewSnapshot::default()
        })));
        assert!(app.reveal_pending, "a live snapshot keeps the reveal");
        assert_eq!(app.window_floor, 0, "a floor of 0 follows its row");
        // The page lands above the drawn rows.
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(HISTORY_WINDOW));
        let mut live = (*window_rows(1)[0]).clone();
        live.row_id = "live".into();
        rows.push(std::sync::Arc::new(live));
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            earlier: Earlier::Unread,
            ..ViewSnapshot::default()
        })));
        assert!(!app.reveal_pending, "the landed page was revealed");
        assert_eq!(
            app.window_floor, 0,
            "the page is drawn, nothing left hidden"
        );
        assert_eq!(
            app.landing.as_ref().map(|(id, _)| id.as_str()),
            Some("u0"),
            "the oldest drawn message stays at the top of the view"
        );
    }

    #[test]
    fn a_page_longer_than_one_window_leaves_the_rest_behind_the_load_more_row() {
        let (mut app, sent) = fullscreen_app();
        app.snapshot.as_mut().unwrap().earlier = Earlier::Unread;
        app.show_earlier();
        assert_eq!(sent.borrow().len(), 1);
        // A page of one window plus ten messages lands.
        let mut rows = older_rows(HISTORY_WINDOW + 10);
        rows.extend(window_rows(HISTORY_WINDOW));
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            earlier: Earlier::Unread,
            ..ViewSnapshot::default()
        })));
        assert!(!app.reveal_pending);
        assert_eq!(app.hidden_messages(), 10, "one window of the page is drawn");
        assert_eq!(
            app.landing.as_ref().map(|(id, _)| id.as_str()),
            Some("u0"),
            "the oldest drawn message stays at the top of the view"
        );
        // The next action draws the rest without reading again.
        app.show_earlier();
        assert_eq!(sent.borrow().len(), 1, "no second read is requested");
        assert_eq!(app.hidden_messages(), 0, "the rest of the page is drawn");
        assert_eq!(app.window_floor, 0);
    }

    #[test]
    fn a_line_jump_to_a_message_that_earlier_read_prints_it() {
        let (mut app, _sent) = fullscreen_app();
        app.fullscreen = false;
        // `/earlier` prepends a page of rows the scrollback has not printed.
        let mut rows = older_rows(HISTORY_WINDOW);
        rows.extend(window_rows(HISTORY_WINDOW));
        app.apply(Update::Snapshot(Box::new(ViewSnapshot {
            session_id: "s1".into(),
            rows,
            focus: "main".into(),
            history_complete: true,
            ..ViewSnapshot::default()
        })));
        assert_eq!(
            app.history_floor, HISTORY_WINDOW,
            "the scrollback floor followed its row below the prepended page"
        );
        app.jump_to(0);
        assert_eq!(
            app.history_request,
            Some((0, HISTORY_WINDOW)),
            "the jump schedules the page instead of saying it is already shown"
        );
    }
}
