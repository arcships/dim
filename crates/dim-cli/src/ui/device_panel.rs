use super::{
    i18n::{t, tf},
    line_input::LineInput,
    list_panel::{Event, Item, ListPanel},
    prompts::Effect,
    render::panel::bordered,
    terminal::{KeyCode, KeyEvent},
    theme::Palette,
};
use crate::devices::{Device, Devices, Directories, Prepared, Session, Target};
use std::{future::Future, sync::Arc};
use tokio::sync::oneshot;

enum Page {
    Computers,
    Sessions(Vec<Session>),
    Directories,
}
enum Field {
    Address,
    Token(String),
    Path,
}
enum Reply {
    Known(Vec<Device>),
    Found(Vec<Device>),
    Sessions(Arc<Target>, Vec<Session>),
    Dirs(Directories),
    Opened(Box<Prepared>),
}
struct Pending {
    answer: oneshot::Receiver<crate::Result<Reply>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) struct DevicePanel {
    devices: Devices,
    current: Arc<Target>,
    target: Option<Arc<Target>>,
    list: ListPanel,
    page: Page,
    field: Option<(Field, LineInput)>,
    remember: bool,
    resume_current: bool,
    pending: Option<Pending>,
    pairing: Option<String>,
    directory: Option<Directories>,
    prepared: Option<Prepared>,
    local_return: Option<(String, Option<String>)>,
    reconnect: Option<(String, Option<String>)>,
    error: String,
}

impl DevicePanel {
    pub fn new(
        devices: Devices,
        current: Arc<Target>,
        local_return: Option<(String, Option<String>)>,
        reconnect: Option<(String, Option<String>)>,
    ) -> Self {
        let mut p = Self {
            devices,
            current,
            target: None,
            list: ListPanel::new(
                "Computers",
                t("Enter select · Esc back"),
                "Finding computers…",
                "No computers",
                "No matches",
                false,
            )
            .with_inline_rows()
            .without_filter(),
            page: Page::Computers,
            field: None,
            remember: true,
            resume_current: false,
            pending: None,
            pairing: None,
            directory: None,
            prepared: None,
            local_return,
            reconnect,
            error: String::new(),
        };
        p.computers();
        p
    }
    fn request(&mut self, future: impl Future<Output = crate::Result<Reply>> + Send + 'static) {
        self.error.clear();
        let (tx, answer) = oneshot::channel();
        let task = self.devices.runtime.spawn(async move {
            let _ = tx.send(future.await);
        });
        self.pending = Some(Pending { answer, task });
    }
    fn computers(&mut self) {
        self.pending = None;
        self.page = Page::Computers;
        self.resume_current = false;
        self.target = None;
        self.pairing = None;
        self.field = None;
        self.list.set_title(t("Computers"));
        self.found(vec![]);
        let devices = self.devices.clone();
        self.request(async move { Ok(Reply::Known(devices.list(false).await?)) });
    }
    fn found(&mut self, found: Vec<Device>) {
        let mut items = Vec::new();
        if self.reconnect.is_some() && self.current.address.is_some() {
            items.push(Item::new(
                "reconnect",
                tf("Reconnect {name}", &[("name", &self.current.name)]),
                t("Resume this view"),
            ));
        }
        items.push(Item::new(
            "local",
            t("This computer"),
            if self.current.id == "local" {
                t("Current")
            } else {
                ""
            },
        ));
        for device in found {
            if let Some(addr) = device.addresses.first() {
                items.push(Item::new(
                    format!("remote:{addr}"),
                    crate::sanitize::safe_line(&device.name),
                    format!(
                        "{}  {addr}",
                        if device.verified {
                            t("Remembered")
                        } else {
                            t("Token required")
                        }
                    ),
                ));
            }
        }
        items.push(Item::new(
            "address",
            t("Add by address…"),
            t("LAN or Tailscale"),
        ));
        let selected = self.list.selected_id();
        self.list.set_items(Ok(items), selected.as_deref());
    }
    fn connect(&mut self, address: Option<String>, token: String) {
        let runtime = self.devices.clone();
        let remember = self.remember;
        self.pairing = address.clone();
        self.list.set_hint(t("Connecting…").into());
        let local_return = self.local_return.clone();
        let reopen = self
            .resume_current
            .then(|| self.reconnect.clone())
            .flatten();
        let current = self.current.id.clone();
        self.request(async move {
            let target = runtime.connect(address.clone(), token, remember).await?;
            if let Some((cwd, id)) = reopen {
                if target.id != current {
                    return Err("The selected computer identity changed".into());
                }
                return Ok(Reply::Opened(Box::new(target.open(cwd, id).await?)));
            }
            if address.is_none()
                && let Some((cwd, id)) = local_return
            {
                return Ok(Reply::Opened(Box::new(target.open(cwd, id).await?)));
            }
            let rows = target.sessions().await?;
            Ok(Reply::Sessions(target, rows))
        });
    }
    pub fn current_sessions(&mut self) {
        let target = self.current.clone();
        self.pairing = None;
        self.list.set_hint(t("Connecting…").into());
        self.request(async move {
            let rows = target.sessions().await?;
            Ok(Reply::Sessions(target, rows))
        });
    }
    fn directories(&mut self, path: Option<String>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        self.list.set_hint(t("Connecting…").into());
        self.request(async move { Ok(Reply::Dirs(target.directories(path).await?)) });
    }
    fn open(&mut self, cwd: String, session: Option<String>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        self.list.set_hint(t("Connecting…").into());
        self.request(async move {
            let cwd = if session.is_none() {
                target
                    .directories(Some(cwd))
                    .await?
                    .path
                    .ok_or(t("Choose a directory"))?
            } else {
                cwd
            };
            Ok(Reply::Opened(Box::new(target.open(cwd, session).await?)))
        });
    }
    pub fn focus(&mut self, view: Prepared, line: String) {
        self.request(async move { Ok(Reply::Opened(Box::new(view.focus(line).await?))) });
    }

    pub fn poll(&mut self) -> bool {
        let Some(pending) = &mut self.pending else {
            return false;
        };
        let reply = match pending.answer.try_recv() {
            Ok(r) => r,
            Err(oneshot::error::TryRecvError::Empty) => return false,
            Err(_) => Err("Connection request ended".into()),
        };
        self.pending = None;
        self.list.set_hint(t("Enter select · Esc back").into());
        match reply {
            Ok(Reply::Known(rows)) => {
                self.found(rows);
                let devices = self.devices.clone();
                self.request(async move { Ok(Reply::Found(devices.list(true).await?)) });
            }
            Ok(Reply::Found(rows)) => self.found(rows),
            Ok(Reply::Sessions(target, rows)) => {
                self.pairing = None;
                self.list.set_title(format!(
                    "{} · {}",
                    if target.address.is_some() {
                        &target.name
                    } else {
                        t("This computer")
                    },
                    t("Active sessions")
                ));
                let mut items: Vec<_> = rows
                    .iter()
                    .map(|s| {
                        Item::new(
                            format!("session:{}", s.session_id),
                            crate::sanitize::safe_line(s.title.as_deref().unwrap_or(&s.session_id)),
                            format!(
                                "{}  {}",
                                crate::sanitize::safe_line(&s.cwd),
                                if s.running {
                                    t("Running")
                                } else {
                                    t("Attached")
                                }
                            ),
                        )
                    })
                    .collect();
                items.push(Item::new("new", t("New session…"), t("Choose a directory")));
                self.list.set_items(Ok(items), None);
                self.page = Page::Sessions(rows);
                self.target = Some(target);
            }
            Ok(Reply::Dirs(dir)) => {
                self.list.set_title(crate::sanitize::safe_line(
                    dir.path.as_deref().unwrap_or(t("Drives")),
                ));
                let mut items = Vec::new();
                if let Some(p) = &dir.parent {
                    items.push(Item::new(format!("dir:{p}"), "../", t("Parent")));
                }
                for child in &dir.directories {
                    items.push(Item::new(
                        format!("dir:{}", child.path),
                        crate::sanitize::safe_line(&child.name),
                        "",
                    ));
                }
                if dir.path.is_some() {
                    items.push(Item::new("use", t("Start a session here"), ""))
                }
                items.push(Item::new("path", t("Enter a path…"), ""));
                self.list.set_items(Ok(items), None);
                self.page = Page::Directories;
                self.directory = Some(dir);
            }
            Ok(Reply::Opened(view)) => self.prepared = Some(*view),
            Err(err) => {
                let msg = crate::sanitize::safe_line(&err.to_string());
                if let Some(address) = self.pairing.take()
                    && err.is::<crate::devices::Pairing>()
                {
                    self.field = Some((Field::Token(address), LineInput::new("")));
                }
                self.error = msg;
                self.list.set_hint(self.error.clone());
            }
        }
        true
    }
    pub fn take_open(&mut self) -> Option<Prepared> {
        self.prepared.take()
    }
    pub fn key(&mut self, key: KeyEvent) -> Effect {
        if let Some((field, input)) = &mut self.field {
            match key.code {
                KeyCode::Esc => {
                    self.field = None;
                    self.error.clear();
                    self.pairing = None;
                    if matches!(self.page, Page::Computers) {
                        self.computers();
                    }
                }
                KeyCode::Tab if matches!(field, Field::Token(_)) => self.remember = !self.remember,
                KeyCode::Enter => {
                    let (field, input) = self.field.take().unwrap();
                    let text = input.text().trim().to_owned();
                    if !text.is_empty() {
                        match field {
                            Field::Address => self.connect(Some(text), String::new()),
                            Field::Token(a) => self.connect(Some(a), text),
                            Field::Path => self.directories(Some(text)),
                        }
                    }
                }
                _ => {
                    input.handle_key(key);
                }
            }
            return Effect::None;
        }
        if key.code == KeyCode::Esc {
            self.pending = None;
            match self.page {
                Page::Computers => return Effect::Close,
                Page::Sessions(_) => self.computers(),
                Page::Directories => {
                    let target = self.target.as_ref().unwrap().clone();
                    self.request(async move {
                        let rows = target.sessions().await?;
                        Ok(Reply::Sessions(target, rows))
                    });
                }
            }
            return Effect::None;
        }
        if let Event::Pick { id, .. } = self.list.handle_key(key) {
            self.resume_current = id == "reconnect";
            match id.as_str() {
                "local" => self.connect(None, String::new()),
                "reconnect" => {
                    self.connect(self.current.address.clone(), String::new());
                }
                "address" => {
                    self.pending = None;
                    self.field = Some((Field::Address, LineInput::new("")));
                }
                "new" => self.directories(None),
                "path" => self.field = Some((Field::Path, LineInput::new(""))),
                "use" => {
                    if let Some(path) = self.directory.as_ref().and_then(|d| d.path.clone()) {
                        self.open(path, None);
                    }
                }
                _ => {
                    if let Some(a) = id.strip_prefix("remote:") {
                        self.connect(Some(a.into()), String::new());
                    } else if let Some(id) = id.strip_prefix("session:")
                        && let Page::Sessions(sessions) = &self.page
                        && let Some(session) = sessions.iter().find(|s| s.session_id == id)
                    {
                        self.open(session.cwd.clone(), Some(id.into()));
                    } else if let Some(p) = id.strip_prefix("dir:") {
                        self.directories((!p.is_empty()).then(|| p.to_owned()));
                    }
                }
            }
        }
        Effect::None
    }
    pub fn paste(&mut self, text: &str) {
        if let Some((_, input)) = &mut self.field {
            input.paste(text)
        } else {
            self.list.paste(text)
        }
    }
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        let Some((field, input)) = &self.field else {
            return self.list.render(width, rows, p);
        };
        let (title, shown) = match field {
            Field::Address => (t("Computer address"), input.text().into()),
            Field::Path => (t("Directory path"), input.text().into()),
            Field::Token(_) => (t("Sharing token"), "*".repeat(input.text().chars().count())),
        };
        let mut lines = vec![crate::sanitize::safe_line(&shown)];
        if matches!(field, Field::Token(_)) {
            lines.push(format!(
                "[{}] {}   {}",
                if self.remember { "x" } else { " " },
                t("Remember on this computer"),
                t("Tab toggle")
            ));
        }
        if !self.error.is_empty() {
            lines.push(self.error.clone());
        }
        bordered(
            &lines,
            width,
            None,
            &p.border,
            Some(title),
            Some(t("Enter confirm · Esc back")),
        )
    }
}
