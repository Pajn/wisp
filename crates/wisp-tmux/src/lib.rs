use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use thiserror::Error;

pub trait TmuxClient {
    fn capabilities(&self) -> Result<TmuxCapabilities, TmuxError>;
    fn current_context(&self) -> Result<TmuxContext, TmuxError>;
    fn list_sessions(&self) -> Result<Vec<TmuxSession>, TmuxError>;
    fn list_windows(&self) -> Result<Vec<TmuxWindow>, TmuxError>;
    fn capture_pane(&self, target: &str) -> Result<String, TmuxError>;
    fn snapshot(&self, query_windows: bool) -> Result<TmuxSnapshot, TmuxError>;
    fn ensure_session(&self, session_name: &str, directory: &Path) -> Result<(), TmuxError>;
    fn switch_or_attach_session(&self, session_name: &str) -> Result<(), TmuxError>;
    fn create_or_switch_session(
        &self,
        session_name: &str,
        directory: &Path,
    ) -> Result<(), TmuxError>;
    fn open_popup(&self, command: &PopupCommand, options: &PopupOptions) -> Result<(), TmuxError>;
    fn open_sidebar_pane(&self, spec: &SidebarPaneSpec) -> Result<(), TmuxError>;
    fn close_sidebar_pane(&self, target: Option<&str>) -> Result<(), TmuxError>;
    fn update_status_line(&self, line: usize, content: &str) -> Result<(), TmuxError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSnapshot {
    pub context: TmuxContext,
    pub capabilities: TmuxCapabilities,
    pub sessions: Vec<TmuxSession>,
    pub windows: Vec<TmuxWindow>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TmuxContext {
    pub client_tty: Option<String>,
    pub session_name: Option<String>,
    pub window_index: Option<u32>,
    pub window_name: Option<String>,
    pub pane_id: Option<String>,
    pub inside_tmux: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCapabilities {
    pub version: TmuxVersion,
    pub supports_popup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    pub name: String,
    pub attached: bool,
    pub windows: usize,
    pub current: bool,
    pub last_activity: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxWindow {
    pub session_name: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupSpec {
    pub command: PopupCommand,
    pub options: PopupOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupOptions {
    pub width: PopupDimension,
    pub height: PopupDimension,
    pub title: Option<String>,
}

impl Default for PopupOptions {
    fn default() -> Self {
        Self {
            width: PopupDimension::Percent(80),
            height: PopupDimension::Percent(85),
            title: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupDimension {
    Percent(u8),
    Cells(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSide {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarPaneSpec {
    pub target: Option<String>,
    pub side: SidebarSide,
    pub width: u16,
    pub command: PopupCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventStrategy {
    PollingFallback,
    ControlMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxCommand {
    EnsureSession {
        session_name: String,
        directory: PathBuf,
    },
    SwitchOrAttachSession {
        session_name: String,
    },
    CreateOrSwitchSession {
        session_name: String,
        directory: PathBuf,
    },
    KillPane {
        target: Option<String>,
    },
    UpdateStatusLine {
        line: usize,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxEvent {
    SnapshotLoaded(TmuxSnapshot),
    SessionAdded(TmuxSession),
    SessionRemoved(String),
    SessionUpdated(TmuxSession),
    FocusChanged {
        client_id: String,
        session_name: String,
        window_id: String,
        pane_id: Option<String>,
    },
}

impl PopupDimension {
    #[must_use]
    pub fn format(&self) -> String {
        match self {
            Self::Percent(value) => format!("{value}%"),
            Self::Cells(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TmuxVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: Option<u8>,
}

impl TmuxVersion {
    #[must_use]
    pub fn supports_popup(self) -> bool {
        self.major > 3 || (self.major == 3 && self.minor >= 2)
    }
}

impl FromStr for TmuxVersion {
    type Err = TmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let version = value
            .strip_prefix("tmux ")
            .ok_or_else(|| TmuxError::Parse {
                context: "tmux version",
                message: format!("unexpected version string `{value}`"),
            })?;

        let digits = version
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect::<String>();
        let mut parts = digits.split('.');
        let major = parts
            .next()
            .ok_or_else(|| TmuxError::Parse {
                context: "tmux version",
                message: "missing major version".to_string(),
            })?
            .parse::<u8>()
            .map_err(|_| TmuxError::Parse {
                context: "tmux version",
                message: format!("invalid major version in `{value}`"),
            })?;
        let minor = parts
            .next()
            .unwrap_or("0")
            .parse::<u8>()
            .map_err(|_| TmuxError::Parse {
                context: "tmux version",
                message: format!("invalid minor version in `{value}`"),
            })?;
        let patch = match parts.next() {
            Some(raw_patch) => Some(raw_patch.parse::<u8>().map_err(|_| TmuxError::Parse {
                context: "tmux version",
                message: format!("invalid patch version in `{value}`"),
            })?),
            None => None,
        };

        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

#[derive(Debug, Error)]
pub enum TmuxError {
    #[error("tmux is unavailable: {message}")]
    Unavailable { message: String },
    #[error("tmux command failed: {command:?} (status {status:?}): {stderr}")]
    CommandFailed {
        command: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    #[error("failed to parse {context}: {message}")]
    Parse {
        context: &'static str,
        message: String,
    },
    #[error("popup mode is unavailable on tmux {version}")]
    PopupUnavailable { version: String },
}

#[derive(Debug, Clone)]
pub struct CommandTmuxClient {
    binary: PathBuf,
    socket_name: Option<String>,
    config_file: Option<PathBuf>,
    inside_tmux: bool,
}

impl Default for CommandTmuxClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandTmuxClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("tmux"),
            socket_name: None,
            config_file: None,
            inside_tmux: env::var_os("TMUX").is_some(),
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    #[must_use]
    pub fn with_socket_name(mut self, socket_name: impl Into<String>) -> Self {
        self.socket_name = Some(socket_name.into());
        self
    }

    #[must_use]
    pub fn with_config_file(mut self, config_file: impl Into<PathBuf>) -> Self {
        self.config_file = Some(config_file.into());
        self
    }

    #[must_use]
    pub fn with_inside_tmux(mut self, inside_tmux: bool) -> Self {
        self.inside_tmux = inside_tmux;
        self
    }

    fn run_tmux(&self, args: Vec<String>) -> Result<String, TmuxError> {
        let command_line = self.command_line(&args);
        let mut command = Command::new(&self.binary);
        if let Some(socket_name) = &self.socket_name {
            command.arg("-L").arg(socket_name);
        }
        if let Some(config_file) = &self.config_file {
            command.arg("-f").arg(config_file);
        }
        command.args(&args);

        let output = command.output().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                TmuxError::Unavailable {
                    message: source.to_string(),
                }
            } else {
                TmuxError::CommandFailed {
                    command: command_line.clone(),
                    status: None,
                    stderr: source.to_string(),
                }
            }
        })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(TmuxError::CommandFailed {
                command: command_line,
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    fn command_line(&self, args: &[String]) -> Vec<String> {
        let mut command = vec![self.binary.display().to_string()];
        if let Some(socket_name) = &self.socket_name {
            command.push("-L".to_string());
            command.push(socket_name.clone());
        }
        if let Some(config_file) = &self.config_file {
            command.push("-f".to_string());
            command.push(config_file.display().to_string());
        }
        command.extend(args.iter().cloned());
        command
    }
}

impl TmuxClient for CommandTmuxClient {
    fn capabilities(&self) -> Result<TmuxCapabilities, TmuxError> {
        let output = self.run_tmux(vec!["-V".to_string()])?;
        let version = output.parse::<TmuxVersion>()?;

        Ok(TmuxCapabilities {
            version,
            supports_popup: version.supports_popup(),
        })
    }

    fn current_context(&self) -> Result<TmuxContext, TmuxError> {
        let output = match self.run_tmux(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "#{client_tty}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_id}"
                .to_string(),
        ]) {
            Ok(output) => output,
            Err(TmuxError::CommandFailed { stderr, .. })
                if is_no_current_client_error(&stderr) || is_no_server_error(&stderr) =>
            {
                return Ok(TmuxContext {
                    inside_tmux: self.inside_tmux,
                    ..TmuxContext::default()
                });
            }
            Err(error) => return Err(error),
        };

        let mut fields = output.split('\t');
        let client_tty = empty_to_none(fields.next());
        let session_name = empty_to_none(fields.next());
        let window_index = fields.next().and_then(|field| field.parse::<u32>().ok());
        let window_name = empty_to_none(fields.next());
        let pane_id = empty_to_none(fields.next());

        Ok(TmuxContext {
            client_tty,
            session_name,
            window_index,
            window_name,
            pane_id,
            inside_tmux: self.inside_tmux,
        })
    }

    fn list_sessions(&self) -> Result<Vec<TmuxSession>, TmuxError> {
        let output = match self.run_tmux(vec![
            "list-sessions".to_string(),
            "-F".to_string(),
            "#{session_name}\t#{session_attached}\t#{session_windows}\t#{session_activity}"
                .to_string(),
        ]) {
            Ok(output) => output,
            Err(TmuxError::CommandFailed { stderr, .. }) if is_no_server_error(&stderr) => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let context = self.current_context()?;

        parse_sessions(&output, context.session_name.as_deref())
    }

    fn list_windows(&self) -> Result<Vec<TmuxWindow>, TmuxError> {
        let output = match self.run_tmux(vec![
            "list-windows".to_string(),
            "-a".to_string(),
            "-F".to_string(),
            "#{session_name}\t#{window_index}\t#{window_name}\t#{window_active}".to_string(),
        ]) {
            Ok(output) => output,
            Err(TmuxError::CommandFailed { stderr, .. }) if is_no_server_error(&stderr) => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };

        parse_windows(&output)
    }

    fn capture_pane(&self, target: &str) -> Result<String, TmuxError> {
        self.run_tmux(vec![
            "capture-pane".to_string(),
            "-p".to_string(),
            "-e".to_string(),
            "-t".to_string(),
            target.to_string(),
        ])
    }

    fn snapshot(&self, query_windows: bool) -> Result<TmuxSnapshot, TmuxError> {
        let capabilities = self.capabilities()?;
        let context = self.current_context()?;
        let sessions = self.list_sessions()?;
        let windows = if query_windows {
            self.list_windows()?
        } else {
            Vec::new()
        };

        Ok(TmuxSnapshot {
            context,
            capabilities,
            sessions,
            windows,
        })
    }

    fn ensure_session(&self, session_name: &str, directory: &Path) -> Result<(), TmuxError> {
        match self.run_tmux(vec![
            "new-session".to_string(),
            "-Ad".to_string(),
            "-s".to_string(),
            session_name.to_string(),
            "-c".to_string(),
            directory.display().to_string(),
        ]) {
            Ok(_) => Ok(()),
            Err(TmuxError::CommandFailed { stderr, .. })
                if stderr.contains("duplicate session") =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn switch_or_attach_session(&self, session_name: &str) -> Result<(), TmuxError> {
        self.run_tmux(focus_session_command(session_name, self.inside_tmux))
            .map(|_| ())
    }

    fn create_or_switch_session(
        &self,
        session_name: &str,
        directory: &Path,
    ) -> Result<(), TmuxError> {
        self.ensure_session(session_name, directory)?;
        self.switch_or_attach_session(session_name)
    }

    fn open_popup(&self, command: &PopupCommand, options: &PopupOptions) -> Result<(), TmuxError> {
        let capabilities = self.capabilities()?;
        if !capabilities.supports_popup {
            return Err(TmuxError::PopupUnavailable {
                version: format!(
                    "{}.{}",
                    capabilities.version.major, capabilities.version.minor
                ),
            });
        }

        let mut args = vec![
            "display-popup".to_string(),
            "-E".to_string(),
            "-w".to_string(),
            options.width.format(),
            "-h".to_string(),
            options.height.format(),
        ];
        if let Some(title) = &options.title {
            args.push("-T".to_string());
            args.push(title.clone());
        }
        args.push(format_popup_command(command));

        self.run_tmux(args).map(|_| ())
    }

    fn open_sidebar_pane(&self, spec: &SidebarPaneSpec) -> Result<(), TmuxError> {
        self.run_tmux(sidebar_pane_command(spec)).map(|_| ())
    }

    fn close_sidebar_pane(&self, target: Option<&str>) -> Result<(), TmuxError> {
        let mut args = vec!["kill-pane".to_string()];
        if let Some(target) = target {
            args.push("-t".to_string());
            args.push(target.to_string());
        }
        self.run_tmux(args).map(|_| ())
    }

    fn update_status_line(&self, line: usize, content: &str) -> Result<(), TmuxError> {
        self.run_tmux(status_line_command(line, content))
            .map(|_| ())
    }
}

#[must_use]
pub fn focus_session_command(session_name: &str, inside_tmux: bool) -> Vec<String> {
    if inside_tmux {
        vec![
            "switch-client".to_string(),
            "-t".to_string(),
            session_name.to_string(),
        ]
    } else {
        vec![
            "attach-session".to_string(),
            "-t".to_string(),
            session_name.to_string(),
        ]
    }
}

#[must_use]
pub fn format_popup_command(command: &PopupCommand) -> String {
    let mut parts = Vec::with_capacity(1 + command.args.len());
    parts.push(shell_escape(&command.program.display().to_string()));
    parts.extend(command.args.iter().map(|arg| shell_escape(arg)));
    parts.join(" ")
}

#[must_use]
pub fn sidebar_pane_command(spec: &SidebarPaneSpec) -> Vec<String> {
    let mut args = vec![
        "split-window".to_string(),
        "-d".to_string(),
        "-h".to_string(),
    ];
    if matches!(spec.side, SidebarSide::Left) {
        args.push("-b".to_string());
    }
    if let Some(target) = &spec.target {
        args.push("-t".to_string());
        args.push(target.clone());
    }
    args.push("-l".to_string());
    args.push(spec.width.to_string());
    args.push(format_popup_command(&spec.command));
    args
}

#[must_use]
pub fn status_line_command(line: usize, content: &str) -> Vec<String> {
    let slot = line.saturating_sub(1);
    vec![
        "set-option".to_string(),
        "-gq".to_string(),
        format!("status-format[{slot}]"),
        content.to_string(),
    ]
}

pub trait TmuxBackend {
    fn event_strategy(&self) -> EventStrategy;
    fn snapshot(&self) -> Result<TmuxSnapshot, TmuxError>;
    fn poll_events(&mut self) -> Result<Vec<TmuxEvent>, TmuxError>;
    fn send(&self, command: TmuxCommand) -> Result<(), TmuxError>;
    fn open_popup(&self, spec: &PopupSpec) -> Result<(), TmuxError>;
    fn open_sidebar_pane(&self, spec: &SidebarPaneSpec) -> Result<(), TmuxError>;
    fn close_sidebar_pane(&self, target: Option<&str>) -> Result<(), TmuxError>;
    fn update_status_line(&self, line: usize, content: &str) -> Result<(), TmuxError>;
}

#[derive(Debug, Clone)]
pub struct PollingTmuxBackend {
    client: CommandTmuxClient,
    query_windows: bool,
    previous_snapshot: Option<TmuxSnapshot>,
}

impl PollingTmuxBackend {
    #[must_use]
    pub fn new(client: CommandTmuxClient) -> Self {
        Self {
            client,
            query_windows: true,
            previous_snapshot: None,
        }
    }

    #[must_use]
    pub fn with_windows(mut self, query_windows: bool) -> Self {
        self.query_windows = query_windows;
        self
    }
}

impl TmuxBackend for PollingTmuxBackend {
    fn event_strategy(&self) -> EventStrategy {
        EventStrategy::PollingFallback
    }

    fn snapshot(&self) -> Result<TmuxSnapshot, TmuxError> {
        self.client.snapshot(self.query_windows)
    }

    fn poll_events(&mut self) -> Result<Vec<TmuxEvent>, TmuxError> {
        let snapshot = self.snapshot()?;
        let events = match &self.previous_snapshot {
            Some(previous) => diff_snapshots(previous, &snapshot),
            None => vec![TmuxEvent::SnapshotLoaded(snapshot.clone())],
        };
        self.previous_snapshot = Some(snapshot);
        Ok(events)
    }

    fn send(&self, command: TmuxCommand) -> Result<(), TmuxError> {
        match command {
            TmuxCommand::EnsureSession {
                session_name,
                directory,
            } => self.client.ensure_session(&session_name, &directory),
            TmuxCommand::SwitchOrAttachSession { session_name } => {
                self.client.switch_or_attach_session(&session_name)
            }
            TmuxCommand::CreateOrSwitchSession {
                session_name,
                directory,
            } => self
                .client
                .create_or_switch_session(&session_name, &directory),
            TmuxCommand::KillPane { target } => self.client.close_sidebar_pane(target.as_deref()),
            TmuxCommand::UpdateStatusLine { line, content } => {
                self.client.update_status_line(line, &content)
            }
        }
    }

    fn open_popup(&self, spec: &PopupSpec) -> Result<(), TmuxError> {
        self.client.open_popup(&spec.command, &spec.options)
    }

    fn open_sidebar_pane(&self, spec: &SidebarPaneSpec) -> Result<(), TmuxError> {
        self.client.open_sidebar_pane(spec)
    }

    fn close_sidebar_pane(&self, target: Option<&str>) -> Result<(), TmuxError> {
        self.client.close_sidebar_pane(target)
    }

    fn update_status_line(&self, line: usize, content: &str) -> Result<(), TmuxError> {
        self.client.update_status_line(line, content)
    }
}

#[must_use]
pub fn diff_snapshots(previous: &TmuxSnapshot, next: &TmuxSnapshot) -> Vec<TmuxEvent> {
    let mut events = Vec::new();

    for next_session in &next.sessions {
        match previous
            .sessions
            .iter()
            .find(|session| session.name == next_session.name)
        {
            None => events.push(TmuxEvent::SessionAdded(next_session.clone())),
            Some(previous_session) if previous_session != next_session => {
                events.push(TmuxEvent::SessionUpdated(next_session.clone()));
            }
            Some(_) => {}
        }
    }

    for previous_session in &previous.sessions {
        if next
            .sessions
            .iter()
            .all(|session| session.name != previous_session.name)
        {
            events.push(TmuxEvent::SessionRemoved(previous_session.name.clone()));
        }
    }

    if (previous.context.session_name != next.context.session_name
        || previous.context.window_index != next.context.window_index
        || previous.context.pane_id != next.context.pane_id)
        && let (Some(session_name), Some(window_index)) =
            (next.context.session_name.clone(), next.context.window_index)
    {
        events.push(TmuxEvent::FocusChanged {
            client_id: next
                .context
                .client_tty
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            window_id: format!("{session_name}:{window_index}"),
            session_name,
            pane_id: next.context.pane_id.clone(),
        });
    }

    events
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn parse_sessions(
    output: &str,
    current_session: Option<&str>,
) -> Result<Vec<TmuxSession>, TmuxError> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            let name = required_field(fields.next(), "session name", line)?;
            let attached =
                parse_numeric_bool(required_field(fields.next(), "session attached", line)?)?;
            let windows = required_field(fields.next(), "session windows", line)?
                .parse::<usize>()
                .map_err(|_| TmuxError::Parse {
                    context: "tmux sessions",
                    message: format!("invalid window count in `{line}`"),
                })?;
            let last_activity = empty_to_none(fields.next())
                .map(|raw| {
                    raw.parse::<u64>().map_err(|_| TmuxError::Parse {
                        context: "tmux sessions",
                        message: format!("invalid session activity in `{line}`"),
                    })
                })
                .transpose()?;

            Ok(TmuxSession {
                current: current_session.is_some_and(|current| current == name),
                name: name.to_string(),
                attached,
                windows,
                last_activity,
            })
        })
        .collect()
}

fn parse_windows(output: &str) -> Result<Vec<TmuxWindow>, TmuxError> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            let session_name = required_field(fields.next(), "window session", line)?;
            let index = required_field(fields.next(), "window index", line)?
                .parse::<u32>()
                .map_err(|_| TmuxError::Parse {
                    context: "tmux windows",
                    message: format!("invalid window index in `{line}`"),
                })?;
            let name = required_field(fields.next(), "window name", line)?;
            let active = parse_numeric_bool(required_field(fields.next(), "window active", line)?)?;

            Ok(TmuxWindow {
                session_name: session_name.to_string(),
                index,
                name: name.to_string(),
                active,
            })
        })
        .collect()
}

fn parse_numeric_bool(value: &str) -> Result<bool, TmuxError> {
    value
        .parse::<u8>()
        .map(|parsed| parsed > 0)
        .map_err(|_| TmuxError::Parse {
            context: "tmux output",
            message: format!("expected numeric boolean, got `{value}`"),
        })
}

fn required_field<'line>(
    value: Option<&'line str>,
    field: &'static str,
    line: &'line str,
) -> Result<&'line str, TmuxError> {
    value.ok_or_else(|| TmuxError::Parse {
        context: "tmux output",
        message: format!("missing {field} in `{line}`"),
    })
}

fn empty_to_none(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(ToOwned::to_owned)
}

fn is_no_current_client_error(stderr: &str) -> bool {
    stderr.contains("no current client") || stderr.contains("no current target")
}

fn is_no_server_error(stderr: &str) -> bool {
    stderr.contains("no server running")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        EventStrategy, PollingTmuxBackend, PopupCommand, PopupDimension, SidebarPaneSpec,
        SidebarSide, TmuxBackend, TmuxContext, TmuxEvent, TmuxSession, TmuxSnapshot, TmuxVersion,
        TmuxWindow, diff_snapshots, focus_session_command, format_popup_command,
        sidebar_pane_command, status_line_command,
    };

    #[test]
    fn parses_tmux_versions_with_suffixes() {
        let version = "tmux 3.6a"
            .parse::<TmuxVersion>()
            .expect("version should parse");

        assert_eq!(version.major, 3);
        assert_eq!(version.minor, 6);
        assert!(version.supports_popup());
    }

    #[test]
    fn selects_attach_or_switch_command_by_context() {
        assert_eq!(
            focus_session_command("work", false),
            vec!["attach-session", "-t", "work"]
        );
        assert_eq!(
            focus_session_command("work", true),
            vec!["switch-client", "-t", "work"]
        );
    }

    #[test]
    fn shell_quotes_popup_commands() {
        let command = PopupCommand {
            program: PathBuf::from("/tmp/wisp"),
            args: vec!["popup".to_string(), "quote's test".to_string()],
        };

        assert_eq!(
            format_popup_command(&command),
            "'/tmp/wisp' 'popup' 'quote'\"'\"'s test'"
        );
    }

    #[test]
    fn formats_popup_dimensions() {
        assert_eq!(PopupDimension::Percent(80).format(), "80%");
        assert_eq!(PopupDimension::Cells(40).format(), "40");
    }

    #[test]
    fn builds_sidebar_pane_commands() {
        let command = PopupCommand {
            program: PathBuf::from("/tmp/wisp"),
            args: vec!["sidebar".to_string()],
        };

        let args = sidebar_pane_command(&SidebarPaneSpec {
            target: Some("alpha:1".to_string()),
            side: SidebarSide::Left,
            width: 36,
            command,
        });

        assert_eq!(
            args,
            vec![
                "split-window",
                "-d",
                "-h",
                "-b",
                "-t",
                "alpha:1",
                "-l",
                "36",
                "'/tmp/wisp' 'sidebar'",
            ]
        );
    }

    #[test]
    fn builds_status_line_option_updates() {
        assert_eq!(
            status_line_command(2, "Wisp  main"),
            vec!["set-option", "-gq", "status-format[1]", "Wisp  main"]
        );
    }

    #[test]
    fn diffs_snapshots_into_events() {
        let previous = TmuxSnapshot {
            context: TmuxContext::default(),
            capabilities: crate::TmuxCapabilities {
                version: TmuxVersion {
                    major: 3,
                    minor: 6,
                    patch: None,
                },
                supports_popup: true,
            },
            sessions: vec![TmuxSession {
                name: "alpha".to_string(),
                attached: false,
                windows: 1,
                current: false,
                last_activity: Some(1),
            }],
            windows: Vec::new(),
        };
        let next = TmuxSnapshot {
            context: TmuxContext {
                client_tty: Some("tty1".to_string()),
                session_name: Some("beta".to_string()),
                window_index: Some(1),
                window_name: Some("shell".to_string()),
                pane_id: Some("%1".to_string()),
                inside_tmux: true,
            },
            capabilities: previous.capabilities.clone(),
            sessions: vec![
                TmuxSession {
                    name: "alpha".to_string(),
                    attached: true,
                    windows: 2,
                    current: false,
                    last_activity: Some(2),
                },
                TmuxSession {
                    name: "beta".to_string(),
                    attached: false,
                    windows: 1,
                    current: true,
                    last_activity: Some(3),
                },
            ],
            windows: vec![TmuxWindow {
                session_name: "beta".to_string(),
                index: 1,
                name: "shell".to_string(),
                active: true,
            }],
        };

        let events = diff_snapshots(&previous, &next);

        assert!(events.iter().any(
            |event| matches!(event, TmuxEvent::SessionAdded(session) if session.name == "beta")
        ));
        assert!(events.iter().any(
            |event| matches!(event, TmuxEvent::SessionUpdated(session) if session.name == "alpha")
        ));
        assert!(events.iter().any(|event| matches!(event, TmuxEvent::FocusChanged { session_name, .. } if session_name == "beta")));
    }

    #[test]
    fn polling_backend_reports_polling_strategy() {
        let backend = PollingTmuxBackend::new(crate::CommandTmuxClient::new());

        assert_eq!(backend.event_strategy(), EventStrategy::PollingFallback);
    }
}
