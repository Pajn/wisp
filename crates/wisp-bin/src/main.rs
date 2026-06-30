use std::{
    collections::BTreeMap,
    collections::VecDeque,
    env,
    error::Error,
    fs,
    io::stdout,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use argh::{FromArgValue, FromArgs};
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::widgets::Clear;
use ratatui::{Terminal, backend::CrosstermBackend};
use wisp_app::{BackendSnapshot, CandidateSources, build_domain_state};
#[cfg(feature = "embers")]
use wisp_config::Dimension;
use wisp_config::{
    BackendKind, CliOverrides, KeyAction, LoadOptions, ResolvedConfig, SessionSortMode, load_config,
};
use wisp_core::{
    DomainState, GitBranchStatus, GitBranchSync, PickerMode, PreviewContent, PreviewKey,
    PreviewRequest, SessionListItem, SessionListSortMode, derive_session_list,
    derive_session_list_with_worktrees, derive_status_items, sanitize_session_name,
    sort_session_list_items,
};
#[cfg(feature = "embers")]
use wisp_embers::{EmbersClient, EmbersJoinPlacement};
use wisp_fuzzy::{MatchItem, Matcher, SimpleMatcher};
use wisp_kindra::{CommandKindraProvider, KindraProvider};
use wisp_preview::{
    ActivePanePreviewProvider, FilesystemPreviewProvider, PreviewProvider,
    SessionDetailsPreviewProvider,
};
use wisp_status::{StatusFormatOptions, StatusRenderMode, render_status_line};
use wisp_tmux::{
    CommandTmuxClient, PollingTmuxBackend, PopupCommand, PopupOptions, PopupSpec, SidebarPaneSpec,
    SidebarSide, TmuxBackend, TmuxCapabilities, TmuxClient, TmuxError, format_popup_command,
};
use wisp_ui::{KeyBindings, SurfaceKind, SurfaceModel, UiIntent, render_surface, translate_key};
use wisp_zoxide::{CommandZoxideProvider, ZoxideProvider};

mod git;

const PREVIEW_REFRESH_DEBOUNCE: Duration = Duration::from_millis(400);
const DEFAULT_CLIENT_ID: &str = "default";
const SIDEBAR_PANE_TITLE: &str = "Wisp Sidebar";
const SIDEBAR_PANE_WIDTH: u16 = 36;
#[cfg(feature = "embers")]
const EMBERS_SURFACE_ENV: &str = "WISP_EMBERS_SURFACE";
#[cfg(feature = "embers")]
const EMBERS_SIDEBAR_PANE_SURFACE: &str = "sidebar-pane";
const STATUSLINE_REFRESH_HOOKS: &[&str] = &[
    "client-session-changed[200]",
    "session-created[200]",
    "session-closed[200]",
    "session-renamed[200]",
];
const STATUSLINE_REFRESH_COMMAND: &str = "refresh-client -S";

struct CombinedPreviewProvider<'a> {
    session: &'a dyn PreviewProvider,
    filesystem: &'a FilesystemPreviewProvider,
}

impl PreviewProvider for CombinedPreviewProvider<'_> {
    fn can_preview(&self, request: &PreviewRequest) -> bool {
        matches!(
            request,
            PreviewRequest::Directory { .. }
                | PreviewRequest::File { .. }
                | PreviewRequest::Metadata { .. }
                | PreviewRequest::SessionSummary { .. }
        )
    }

    fn generate(
        &self,
        request: &PreviewRequest,
    ) -> Result<PreviewContent, wisp_preview::PreviewError> {
        match request {
            PreviewRequest::Directory { .. }
            | PreviewRequest::File { .. }
            | PreviewRequest::Metadata { .. } => self.filesystem.generate(request),
            PreviewRequest::SessionSummary { .. } => self.session.generate(request),
        }
    }
}

struct TerminalTeardown {
    active: bool,
}

impl TerminalTeardown {
    fn active() -> Self {
        Self { active: true }
    }

    fn restore(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<(), Box<dyn Error>> {
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for TerminalTeardown {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen);
            let _ = execute!(stdout(), crossterm::cursor::Show);
        }
    }
}

#[derive(Clone)]
enum RuntimeBackend {
    Tmux,
    #[cfg(feature = "embers")]
    Embers(Arc<EmbersClient>),
}

#[cfg(feature = "embers")]
struct EmbersPanePreviewProvider {
    embers: Arc<EmbersClient>,
    max_lines: usize,
}

#[cfg(feature = "embers")]
impl EmbersPanePreviewProvider {
    fn new(embers: Arc<EmbersClient>) -> Self {
        Self {
            embers,
            max_lines: 40,
        }
    }
}

#[cfg(feature = "embers")]
impl PreviewProvider for EmbersPanePreviewProvider {
    fn can_preview(&self, request: &PreviewRequest) -> bool {
        matches!(request, PreviewRequest::SessionSummary { .. })
    }

    fn generate(
        &self,
        request: &PreviewRequest,
    ) -> Result<PreviewContent, wisp_preview::PreviewError> {
        let PreviewRequest::SessionSummary { session_name, .. } = request else {
            return Err(wisp_preview::PreviewError::Unsupported);
        };
        let captured = self
            .embers
            .capture_session_preview(session_name, self.max_lines)
            .map_err(|error| wisp_preview::PreviewError::SessionCapture {
                session_name: session_name.clone(),
                message: error.to_string(),
            })?;
        Ok(PreviewContent::from_text_tail(
            format!("Pane {session_name}"),
            captured,
            self.max_lines,
        ))
    }
}

#[cfg(feature = "embers")]
impl RuntimeBackend {
    fn poll_updates(&self) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Tmux => Ok(false),
            #[cfg(feature = "embers")]
            Self::Embers(client) => client.poll_updates().map_err(|error| error.into()),
        }
    }
}

#[derive(Debug, FromArgs, PartialEq)]
/// Wisp is a tmux navigation workspace.
///
/// Run `wisp help` to list available commands.
#[argh(help_triggers("-h", "--help", "help"))]
struct Cli {
    #[argh(subcommand)]
    command: Command,
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand)]
enum Command {
    Doctor(DoctorCommand),
    PrintConfig(PrintConfigCommand),
    Fullscreen(FullscreenCommand),
    Popup(PopupCommandCli),
    SidebarPopup(SidebarPopupCommand),
    SidebarPane(SidebarPaneCommand),
    Statusline(StatuslineGroupCommand),
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "doctor")]
/// Print runtime diagnostics for tmux and zoxide.
struct DoctorCommand {}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "print-config")]
/// Print the resolved configuration.
struct PrintConfigCommand {}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "fullscreen")]
/// Open the main picker fullscreen in tmux.
pub struct FullscreenCommand {
    #[argh(switch, short = 'w', long = "worktree")]
    /// start in worktree mode, showing only worktrees from the current repo
    pub worktree: bool,
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "popup")]
/// Open the main picker in a tmux popup.
pub struct PopupCommandCli {
    #[argh(switch, short = 'w', long = "worktree")]
    /// start in worktree mode, showing only worktrees from the current repo
    pub worktree: bool,
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "sidebar-popup")]
/// Open the sidebar picker in a tmux popup.
struct SidebarPopupCommand {}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "sidebar-pane")]
/// Open the sidebar picker in a persistent tmux pane.
struct SidebarPaneCommand {}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "statusline")]
/// Manage the tmux statusline integration.
struct StatuslineGroupCommand {
    #[argh(subcommand)]
    command: StatuslineSubcommand,
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand)]
enum StatuslineSubcommand {
    Install(StatuslineInstallCommand),
    Render(StatuslineRenderCommand),
    Uninstall(StatuslineUninstallCommand),
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "install")]
/// Install the Wisp tmux statusline.
struct StatuslineInstallCommand {
    /// tmux status row to install into.
    #[argh(option)]
    line: Option<usize>,
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "render")]
/// Render the Wisp tmux statusline.
struct StatuslineRenderCommand {
    /// force passive rendering.
    #[argh(switch)]
    force_passive: bool,

    /// force clickable rendering.
    #[argh(switch)]
    force_clickable: bool,
}

#[derive(Debug, FromArgs, PartialEq)]
#[argh(subcommand, name = "uninstall")]
/// Remove the Wisp tmux statusline.
struct StatuslineUninstallCommand {
    /// tmux status row to remove from.
    #[argh(option)]
    line: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiMode {
    Picker(PickerMode),
    SidebarCompact,
    SidebarExpanded,
}

impl FromArgValue for UiMode {
    fn from_arg_value(value: &str) -> Result<Self, String> {
        match value {
            "picker" => Ok(Self::Picker(PickerMode::AllSessions)),
            "picker-worktree" => Ok(Self::Picker(PickerMode::Worktree)),
            "sidebar-compact" => Ok(Self::SidebarCompact),
            "sidebar-expanded" => Ok(Self::SidebarExpanded),
            other => Err(format!(
                "expected one of \"picker\", \"picker-worktree\", \"sidebar-compact\", or \"sidebar-expanded\", got `{other}`"
            )),
        }
    }
}

impl UiMode {
    fn surface_kind(self) -> SurfaceKind {
        match self {
            Self::Picker(_) => SurfaceKind::Picker,
            Self::SidebarCompact => SurfaceKind::SidebarCompact,
            Self::SidebarExpanded => SurfaceKind::SidebarExpanded,
        }
    }

    fn picker_mode(self) -> PickerMode {
        match self {
            Self::Picker(mode) => mode,
            Self::SidebarCompact | Self::SidebarExpanded => PickerMode::AllSessions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewMode {
    Pane,
    Details,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputMode {
    Filter,
    Rename {
        session_id: String,
        filter_query: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitWorkItem {
    session_id: String,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitStatusUpdate {
    session_id: String,
    sync: GitBranchSync,
    dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarUiState {
    query: String,
    selected_session_id: Option<String>,
    kind: SurfaceKind,
    sort_mode: Option<SessionSortMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarRuntime {
    session_name: String,
    target: SidebarTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarTarget {
    Tmux {
        home_window_index: u32,
        pane_id: Option<String>,
    },
    #[cfg(feature = "embers")]
    Embers { buffer_id: String },
}

impl SidebarRuntime {
    fn session_name(&self) -> &str {
        &self.session_name
    }

    fn tmux_pane_id(&self) -> Option<&str> {
        match &self.target {
            SidebarTarget::Tmux { pane_id, .. } => pane_id.as_deref(),
            #[cfg(feature = "embers")]
            SidebarTarget::Embers { .. } => None,
        }
    }

    #[cfg(feature = "embers")]
    fn embers_buffer_id(&self) -> Option<&str> {
        match &self.target {
            SidebarTarget::Tmux { .. } => None,
            SidebarTarget::Embers { buffer_id } => Some(buffer_id.as_str()),
        }
    }

    fn rename_session(&mut self, new_name: String) {
        self.session_name = new_name;
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wisp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match parse_cli() {
        Ok(cli) => execute_cli(cli),
        Err(early_exit) => {
            if early_exit.status.is_ok() {
                print!("{}", early_exit.output);
                Ok(())
            } else {
                Err(early_exit.output.into())
            }
        }
    }
}

enum ParsedCli {
    Public(Cli),
    Ui(UiMode),
}

fn ui_help_early_exit(command_name: &str) -> argh::EarlyExit {
    argh::EarlyExit {
        output: format!(
            "Usage: {command_name} ui <mode>\n\n    Internal helper for launching a specific surface.\n\n    Modes:\n      picker             Open the picker surface.\n      picker-worktree    Open the picker surface in worktree mode.\n      sidebar-compact    Open the compact sidebar surface.\n      sidebar-expanded   Open the expanded sidebar surface.\n"
        ),
        status: Ok(()),
    }
}

fn parse_cli_args(args: &[String]) -> Result<ParsedCli, argh::EarlyExit> {
    let command_name = command_name(&args[0]);
    let mut cli_args = args.iter().skip(1).cloned().collect::<Vec<_>>();

    if matches!(cli_args.first().map(String::as_str), Some("ui")) {
        match cli_args.get(1).map(String::as_str) {
            None => return Err(ui_help_early_exit(&command_name)),
            Some("-h") | Some("--help") | Some("help") => {
                return Err(ui_help_early_exit(&command_name));
            }
            Some(mode) => {
                if cli_args.len() > 2 {
                    return Err(ui_parse_error(
                        "ui accepts exactly one surface mode: picker, picker-worktree, sidebar-compact, or sidebar-expanded",
                    ));
                }

                let mode = UiMode::from_arg_value(mode).map_err(ui_parse_error)?;
                return Ok(ParsedCli::Ui(mode));
            }
        }
    }

    if cli_args.first().is_some_and(|arg| arg == "status-line")
        && let Some(command) = cli_args.first_mut()
    {
        *command = "statusline".to_string();
    }

    if cli_args.is_empty() {
        cli_args.push("help".to_string());
    }

    let command_name = [command_name.as_str()];
    let cli_args = cli_args.iter().map(String::as_str).collect::<Vec<_>>();
    Cli::from_args(&command_name, &cli_args).map(ParsedCli::Public)
}

fn parse_cli() -> Result<ParsedCli, argh::EarlyExit> {
    let args = env::args().collect::<Vec<_>>();
    parse_cli_args(&args)
}

fn command_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or("wisp")
        .to_string()
}

fn ui_parse_error(message: impl Into<String>) -> argh::EarlyExit {
    argh::EarlyExit {
        output: format!("{}\n", message.into()),
        status: Err(()),
    }
}

fn execute_cli(cli: ParsedCli) -> Result<(), Box<dyn Error>> {
    match cli {
        ParsedCli::Ui(mode) => {
            let config = load_runtime_config()?;
            let backend = load_runtime_backend(&config)?;
            run_surface(mode.surface_kind(), &config, mode.picker_mode(), &backend)
        }
        ParsedCli::Public(cli) => match cli.command {
            Command::Doctor(_) => doctor(),
            Command::PrintConfig(_) => {
                let config = load_runtime_config()?;
                println!("{config:#?}");
                Ok(())
            }
            Command::Fullscreen(fullscreen_cmd) => {
                let config = load_runtime_config()?;
                let backend = load_runtime_backend(&config)?;
                let mode = if fullscreen_cmd.worktree {
                    PickerMode::Worktree
                } else {
                    PickerMode::AllSessions
                };
                run_surface(SurfaceKind::Picker, &config, mode, &backend)
            }
            Command::Popup(popup_cmd) => {
                let config = load_runtime_config()?;
                let backend = load_runtime_backend(&config)?;
                let mode = if popup_cmd.worktree {
                    PickerMode::Worktree
                } else {
                    PickerMode::AllSessions
                };
                open_popup_or_run_inline(SurfaceKind::Picker, &config, mode, &backend)
            }
            Command::SidebarPopup(_) => {
                let config = load_runtime_config()?;
                let backend = load_runtime_backend(&config)?;
                open_sidebar_popup_or_run_inline(&config, &backend)
            }
            Command::SidebarPane(_) => {
                let config = load_runtime_config()?;
                let backend = load_runtime_backend(&config)?;
                open_sidebar_pane(&backend)
            }
            Command::Statusline(statusline) => {
                validate_statusline_flags(&statusline)?;
                let config = load_runtime_config()?;
                if selected_backend_kind(&config) == BackendKind::Embers {
                    return Err(embers_unsupported("wisp statusline"));
                }
                let backend = load_runtime_backend(&config)?;
                run_statusline_group(&config, statusline, &backend)
            }
        },
    }
}

fn load_runtime_config() -> Result<ResolvedConfig, Box<dyn Error>> {
    load_config(&LoadOptions {
        cli_overrides: CliOverrides::default(),
        ..LoadOptions::default()
    })
    .map_err(|error| Box::new(error) as Box<dyn Error>)
}

fn selected_backend_kind(config: &ResolvedConfig) -> BackendKind {
    match config.backend.kind {
        BackendKind::Auto => {
            // Mirror load_runtime_backend: a socket hint only resolves to Embers
            // if it actually connects, otherwise Auto falls back to tmux. This
            // keeps the doctor/statusline view consistent with the real runtime.
            #[cfg(feature = "embers")]
            if let Some(socket_path) = resolve_embers_socket_path(config)
                && EmbersClient::connect(socket_path).is_ok()
            {
                return BackendKind::Embers;
            }
            BackendKind::Tmux
        }
        kind => kind,
    }
}

#[cfg(feature = "embers")]
fn resolve_embers_socket_path(config: &ResolvedConfig) -> Option<PathBuf> {
    config
        .embers
        .socket_path
        .clone()
        .or_else(|| env::var_os("EMBERS_SOCKET").map(PathBuf::from))
}

fn load_runtime_backend(config: &ResolvedConfig) -> Result<RuntimeBackend, Box<dyn Error>> {
    match config.backend.kind {
        BackendKind::Tmux => Ok(RuntimeBackend::Tmux),
        #[cfg(feature = "embers")]
        BackendKind::Embers => {
            let socket_path = resolve_embers_socket_path(config).ok_or_else(|| {
                "embers backend selected, but no socket path was configured".to_string()
            })?;
            Ok(RuntimeBackend::Embers(Arc::new(EmbersClient::connect(
                socket_path,
            )?)))
        }
        #[cfg(not(feature = "embers"))]
        BackendKind::Embers => Err(embers_feature_disabled()),
        #[cfg(feature = "embers")]
        BackendKind::Auto => match resolve_embers_socket_path(config) {
            // In Auto mode a configured/leftover socket path is a hint, not a
            // commitment: if the embers server is unreachable (e.g. a stale
            // EMBERS_SOCKET pointing at a dead server), fall back to tmux rather
            // than failing every command.
            Some(socket_path) => match EmbersClient::connect(socket_path) {
                Ok(client) => Ok(RuntimeBackend::Embers(Arc::new(client))),
                Err(error) => {
                    eprintln!(
                        "wisp: embers socket present but connection failed ({error}); falling back to tmux"
                    );
                    Ok(RuntimeBackend::Tmux)
                }
            },
            None => Ok(RuntimeBackend::Tmux),
        },
        #[cfg(not(feature = "embers"))]
        BackendKind::Auto => Ok(RuntimeBackend::Tmux),
    }
}

#[cfg(not(feature = "embers"))]
fn embers_feature_disabled() -> Box<dyn Error> {
    "embers backend support was not compiled in; rebuild with `--features embers`".into()
}

fn embers_unsupported(feature: &str) -> Box<dyn Error> {
    format!("{feature} is not supported on the embers backend yet").into()
}

fn doctor() -> Result<(), Box<dyn Error>> {
    let config = load_runtime_config()?;
    let backend_kind = selected_backend_kind(&config);
    let tmux = CommandTmuxClient::new();
    let zoxide = CommandZoxideProvider::new();

    println!("wisp doctor");
    println!();
    println!(
        "backend: {}",
        match backend_kind {
            BackendKind::Tmux => "tmux",
            BackendKind::Embers => "embers",
            BackendKind::Auto => unreachable!("auto backend should resolve before doctor output"),
        }
    );
    // Always report tmux capabilities: tmux is a useful diagnostic regardless of
    // the selected backend (e.g. confirming the environment is sane when embers
    // is misconfigured).
    match tmux.capabilities() {
        Ok(capabilities) => {
            println!(
                "tmux: {}.{} (popup: {}, status clicks: {}, mouse: {})",
                capabilities.version.major,
                capabilities.version.minor,
                capabilities.supports_popup,
                capabilities.supports_status_mouse_ranges,
                capabilities.mouse_enabled
            );
        }
        Err(error) => println!("tmux: unavailable ({error})"),
    }
    match backend_kind {
        BackendKind::Tmux => {}
        BackendKind::Embers => {
            #[cfg(feature = "embers")]
            match resolve_embers_socket_path(&config) {
                // Actually probe the socket rather than trusting that a path is
                // configured: the server may be down or the socket stale.
                Some(socket_path) => match EmbersClient::connect(socket_path.clone()) {
                    Ok(_) => println!("embers: available ({})", socket_path.display()),
                    Err(error) => {
                        println!("embers: unavailable ({}: {error})", socket_path.display())
                    }
                },
                None => println!("embers: socket not configured"),
            }
            #[cfg(not(feature = "embers"))]
            println!("embers: support not compiled in");
        }
        BackendKind::Auto => unreachable!("auto backend should resolve before doctor output"),
    }

    match zoxide.load_entries(5) {
        Ok(entries) => println!("zoxide: available ({} sample entries)", entries.len()),
        Err(error) => println!("zoxide: unavailable ({error})"),
    }

    println!(
        "event strategy: {}",
        match backend_kind {
            BackendKind::Tmux => "PollingFallback",
            #[cfg(feature = "embers")]
            BackendKind::Embers => "SubscriptionStream",
            #[cfg(not(feature = "embers"))]
            BackendKind::Embers => "Disabled",
            BackendKind::Auto => unreachable!("auto backend should resolve before doctor output"),
        }
    );
    Ok(())
}

fn validate_statusline_flags(statusline: &StatuslineGroupCommand) -> Result<(), Box<dyn Error>> {
    match &statusline.command {
        StatuslineSubcommand::Install(args) => {
            if args.line == Some(0) {
                return Err("statusline line must be >= 1".into());
            }
        }
        StatuslineSubcommand::Uninstall(args) => {
            if args.line == Some(0) {
                return Err("statusline line must be >= 1".into());
            }
        }
        StatuslineSubcommand::Render(args) => {
            if args.force_passive && args.force_clickable {
                return Err(
                    "statusline render accepts only one of --force-passive or --force-clickable"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

fn run_statusline_group(
    config: &ResolvedConfig,
    command: StatuslineGroupCommand,
    backend: &RuntimeBackend,
) -> Result<(), Box<dyn Error>> {
    if !matches!(backend, RuntimeBackend::Tmux) {
        return Err(embers_unsupported("wisp statusline"));
    }
    match command.command {
        StatuslineSubcommand::Install(args) => install_statusline(config, args.line),
        StatuslineSubcommand::Render(args) => {
            let force = if args.force_passive {
                Some(StatusRenderMode::Passive)
            } else if args.force_clickable {
                Some(StatusRenderMode::Clickable)
            } else {
                None
            };
            render_statusline(config, force)
        }
        StatuslineSubcommand::Uninstall(args) => uninstall_statusline(config, args.line),
    }
}

fn render_statusline(
    config: &ResolvedConfig,
    force: Option<StatusRenderMode>,
) -> Result<(), Box<dyn Error>> {
    let loaded = load_domain_state(&RuntimeBackend::Tmux)?;
    let items = derive_status_items(&loaded.state, Some(loaded.client_id.as_str()));
    let tmux = CommandTmuxClient::new();
    let capabilities = tmux.capabilities()?;
    let rendered = render_status_line(
        &items,
        &status_format_options(config),
        force.unwrap_or_else(|| statusline_mode(config, &capabilities)),
    );
    println!("{}", rendered.text);
    Ok(())
}

fn install_statusline(
    config: &ResolvedConfig,
    line_override: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    let tmux = CommandTmuxClient::new();
    let capabilities = tmux.capabilities()?;
    let line = line_override.unwrap_or(config.status.line);
    if line > 1 && !capabilities.supports_multi_status_lines {
        return Err(format!(
            "tmux {}.{} does not support multi-line status rows",
            capabilities.version.major, capabilities.version.minor
        )
        .into());
    }

    let current_lines = tmux.status_line_count()?;
    if current_lines < line {
        tmux.set_status_line_count(line)?;
    }

    let content = statusline_command_expression(env::current_exe()?);
    tmux.update_status_line(line, &content)?;
    install_statusline_refresh_hooks(&tmux)?;
    tmux.refresh_client_status()?;
    println!("Installed Wisp statusline on row {line}.");
    Ok(())
}

fn uninstall_statusline(
    config: &ResolvedConfig,
    line_override: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    let tmux = CommandTmuxClient::new();
    let line = line_override.unwrap_or(config.status.line);
    tmux.clear_status_line(line)?;
    uninstall_statusline_refresh_hooks(&tmux)?;
    tmux.refresh_client_status()?;
    println!("Removed Wisp statusline from row {line}.");
    Ok(())
}

fn install_statusline_refresh_hooks(tmux: &impl TmuxClient) -> Result<(), TmuxError> {
    for hook in STATUSLINE_REFRESH_HOOKS {
        tmux.set_hook(hook, STATUSLINE_REFRESH_COMMAND)?;
    }
    Ok(())
}

fn uninstall_statusline_refresh_hooks(tmux: &impl TmuxClient) -> Result<(), TmuxError> {
    for hook in STATUSLINE_REFRESH_HOOKS {
        tmux.clear_hook(hook)?;
    }
    Ok(())
}

fn status_format_options(config: &ResolvedConfig) -> StatusFormatOptions {
    StatusFormatOptions {
        icon: config.status.icon.clone(),
        max_sessions: config.status.max_sessions,
        show_previous: config.status.show_previous,
        show_counts: false,
    }
}

fn statusline_mode(config: &ResolvedConfig, capabilities: &TmuxCapabilities) -> StatusRenderMode {
    if config.status.interactive
        && capabilities.supports_status_mouse_ranges
        && capabilities.mouse_enabled
    {
        StatusRenderMode::Clickable
    } else {
        StatusRenderMode::Passive
    }
}

fn statusline_command_expression(program: PathBuf) -> String {
    let command = PopupCommand {
        program,
        args: vec!["statusline".to_string(), "render".to_string()],
    };
    format!("#({})", format_popup_command(&command))
}

fn apply_session_sort(items: &mut [SessionListItem], sort_mode: SessionSortMode) {
    let sort_mode = match sort_mode {
        SessionSortMode::Recent => SessionListSortMode::Recent,
        SessionSortMode::Alphabetical => SessionListSortMode::Alphabetical,
    };
    sort_session_list_items(items, sort_mode);
}

fn current_session_id(items: &[SessionListItem]) -> Option<&str> {
    items
        .iter()
        .find(|item| item.is_current)
        .map(|item| item.session_id.as_str())
}

fn selected_index_for_session(
    items: &[SessionListItem],
    query: &str,
    session_id: Option<&str>,
) -> Option<usize> {
    let session_id = session_id?;
    filter_items(items, query)
        .iter()
        .position(|item| item.session_id == session_id)
}

/// Backend-agnostic session operations the picker's activation logic needs,
/// letting the tmux and embers backends share a single implementation.
///
/// Implemented for `&T: TmuxClient` and for `Arc<EmbersClient>`; those are
/// disjoint type shapes (a reference vs. a nominal type), so the two impls do
/// not overlap and existing tmux call sites keep passing `&tmux` unchanged.
trait SessionLauncher {
    fn create_or_switch(&self, name: &str, directory: &Path) -> Result<(), Box<dyn Error>>;
    fn existing_session_names(&self) -> Result<Vec<String>, Box<dyn Error>>;
    fn switch_to(&self, session_id: &str) -> Result<(), Box<dyn Error>>;
}

impl<T: TmuxClient> SessionLauncher for &T {
    fn create_or_switch(&self, name: &str, directory: &Path) -> Result<(), Box<dyn Error>> {
        self.create_or_switch_session(name, directory)?;
        Ok(())
    }

    fn existing_session_names(&self) -> Result<Vec<String>, Box<dyn Error>> {
        // `list_sessions` already maps the "no server / no sessions" case to an
        // empty list, so propagating here only surfaces genuine tmux errors
        // (matching the embers implementation) rather than hiding them.
        Ok(self
            .list_sessions()?
            .into_iter()
            .map(|session| session.name)
            .collect())
    }

    fn switch_to(&self, session_id: &str) -> Result<(), Box<dyn Error>> {
        self.switch_or_attach_session(session_id)?;
        Ok(())
    }
}

#[cfg(feature = "embers")]
impl SessionLauncher for Arc<EmbersClient> {
    fn create_or_switch(&self, name: &str, directory: &Path) -> Result<(), Box<dyn Error>> {
        self.create_or_switch_session(name, directory)?;
        Ok(())
    }

    fn existing_session_names(&self) -> Result<Vec<String>, Box<dyn Error>> {
        Ok(self.list_session_names()?)
    }

    fn switch_to(&self, session_id: &str) -> Result<(), Box<dyn Error>> {
        self.switch_session(session_id)?;
        Ok(())
    }
}

fn create_session_from_query(
    launcher: impl SessionLauncher,
    zoxide: &impl ZoxideProvider,
    query: &str,
    fallback_directory: &Path,
) -> Result<bool, Box<dyn Error>> {
    let session_name = query.trim();
    if session_name.is_empty() {
        return Ok(false);
    }

    let directory = zoxide
        .query_directory(session_name)?
        .map(|entry| entry.path)
        .unwrap_or_else(|| fallback_directory.to_path_buf());
    launcher.create_or_switch(session_name, &directory)?;
    Ok(true)
}

fn create_session_with_basename(
    launcher: impl SessionLauncher,
    basename: &str,
    path: &Path,
) -> Result<bool, Box<dyn Error>> {
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // The session list is only used to deduplicate the basename. A transient
    // listing failure should not abort session creation, so fall back to an empty
    // list (treat as no collision) rather than propagating the error.
    let existing_sessions = launcher.existing_session_names().unwrap_or_default();
    let session_name = if existing_sessions.iter().any(|name| name == basename) {
        format!(
            "{}-{:08x}",
            sanitize_session_name(&canonical_path),
            stable_path_hash(&canonical_path) as u32
        )
    } else {
        basename.to_string()
    };
    launcher.create_or_switch(&session_name, &canonical_path)?;
    Ok(true)
}

fn session_items_for_picker_mode(
    state: &DomainState,
    client_id: Option<&str>,
    picker_mode: PickerMode,
) -> Vec<SessionListItem> {
    if picker_mode == PickerMode::Worktree {
        if let Some(repo_root) = git::worktree_repo_root(state, client_id) {
            let worktrees = git::git_worktree_list(&repo_root);
            derive_session_list_with_worktrees(state, client_id, &worktrees)
        } else {
            vec![picker_info_item("not in a git repository")]
        }
    } else {
        derive_session_list(state, client_id)
    }
}

fn rebuild_session_items_for_picker_mode(
    state: &DomainState,
    client_id: Option<&str>,
    picker_mode: PickerMode,
    session_sort: SessionSortMode,
) -> (
    Vec<SessionListItem>,
    VecDeque<GitWorkItem>,
    mpsc::Receiver<GitStatusUpdate>,
) {
    let mut session_items = session_items_for_picker_mode(state, client_id, picker_mode);
    let pending_branch_names = if picker_mode == PickerMode::Worktree {
        VecDeque::new()
    } else {
        git_work_items(state)
    };
    let status_work_items = if picker_mode == PickerMode::Worktree {
        git_work_items_for_worktree_items(&session_items)
            .into_iter()
            .collect()
    } else {
        pending_branch_names.iter().cloned().collect()
    };
    apply_session_sort(&mut session_items, session_sort);
    let branch_status_updates = spawn_git_status_workers(status_work_items);
    (session_items, pending_branch_names, branch_status_updates)
}

/// Apply a freshly loaded [`LoadedDomainState`] to the picker's working state:
/// update the active client id, rebuild the session items for the current picker
/// mode, refresh the details-preview provider, and drop any deferred
/// branch-status updates. Shared by every `run_surface` handler that reloads
/// backend state (embers poll, rename, close, worktree toggle) so the reload
/// contract lives in one place. Callers still own follow-up concerns that differ
/// per handler (which session to reselect, preview reset).
#[allow(clippy::too_many_arguments)]
fn apply_reloaded_state(
    reloaded_state: LoadedDomainState,
    picker_mode: PickerMode,
    session_sort: SessionSortMode,
    active_client_id: &mut String,
    session_items: &mut Vec<SessionListItem>,
    pending_branch_names: &mut VecDeque<GitWorkItem>,
    branch_status_updates: &mut mpsc::Receiver<GitStatusUpdate>,
    details_preview_provider: &mut SessionDetailsPreviewProvider,
    deferred_branch_status: &mut BTreeMap<String, GitStatusUpdate>,
) {
    *active_client_id = reloaded_state.client_id;
    (
        *session_items,
        *pending_branch_names,
        *branch_status_updates,
    ) = rebuild_session_items_for_picker_mode(
        &reloaded_state.state,
        Some(active_client_id.as_str()),
        picker_mode,
        session_sort,
    );
    details_preview_provider.state = reloaded_state.state;
    deferred_branch_status.clear();
}

/// A worktree-mode repository that has Kindra temporary worktrees configured,
/// together with the trunk branch new temp worktrees should branch off of.
#[derive(Debug, Clone)]
struct KindraTempContext {
    repo_root: PathBuf,
    trunk: String,
}

/// Derives a git branch name from the picker query for a new temp worktree.
///
/// Whitespace runs collapse to single dashes and surrounding dashes are trimmed,
/// keeping slashes so users can still type `feature/foo`. Returns `None` when the
/// query is empty or would produce a name git would reject (e.g. `fix: crash`,
/// `foo..bar`, or `foo.lock`), so the invalid row never reaches `kin wt temp`.
fn kindra_temp_branch_name(query: &str) -> Option<String> {
    let collapsed = query.split_whitespace().collect::<Vec<_>>().join("-");
    let trimmed = collapsed.trim_matches('-');
    if trimmed.is_empty() || !is_valid_git_branch_name(trimmed) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Returns whether `name` is a valid git branch name per `git check-ref-format`.
///
/// Covers the subset of rules a normalized picker slug can still violate: a stray
/// `:`/`~`/`^` from the query, `..`, a leading/trailing dot, or a `.lock` suffix.
fn is_valid_git_branch_name(name: &str) -> bool {
    if name.is_empty() || name == "@" {
        return false;
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return false;
    }
    if name.starts_with('.') || name.ends_with('.') {
        return false;
    }
    if name.contains("..") || name.contains("@{") {
        return false;
    }
    if name.chars().any(|c| {
        c.is_ascii_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    }) {
        return false;
    }
    name.split('/').all(|component| {
        !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
    })
}

#[allow(clippy::too_many_arguments)]
fn activate_filter_selection(
    launcher: impl SessionLauncher,
    zoxide: &impl ZoxideProvider,
    kindra: &impl KindraProvider,
    filtered: &[SessionListItem],
    selected: usize,
    query: &str,
    fallback_directory: &Path,
    force_create_from_query: bool,
) -> Result<bool, Box<dyn Error>> {
    if force_create_from_query || filtered.get(selected).is_none() {
        return create_session_from_query(launcher, zoxide, query, fallback_directory);
    }

    if let Some(item) = filtered.get(selected) {
        match item.kind {
            wisp_core::SessionListItemKind::Info => return Ok(false),
            wisp_core::SessionListItemKind::Worktree => {
                if let Some(worktree_path) = &item.worktree_path {
                    let basename = worktree_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("worktree");
                    return create_session_with_basename(launcher, basename, worktree_path);
                }
                return Ok(false);
            }
            wisp_core::SessionListItemKind::Zoxide => {
                if let Some(path) = &item.worktree_path {
                    let folder_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("zoxide");
                    if !folder_name.is_empty() {
                        return create_session_with_basename(launcher, folder_name, path);
                    }
                }
                return Ok(false);
            }
            wisp_core::SessionListItemKind::CreateTempWorktree => {
                // `worktree_path` carries the repo root to run `kin` in and
                // `worktree_branch` the new branch name derived from the query.
                if let (Some(repo_root), Some(branch)) =
                    (&item.worktree_path, &item.worktree_branch)
                {
                    let trunk = git::trunk_branch(repo_root);
                    let worktree_path = kindra.create_temp_worktree(repo_root, branch, &trunk)?;
                    let basename = worktree_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(branch.as_str());
                    return create_session_with_basename(launcher, basename, &worktree_path);
                }
                return Ok(false);
            }
            _ => {}
        }

        launcher.switch_to(&item.session_id)?;
        return Ok(true);
    }

    Ok(false)
}

#[cfg(feature = "embers")]
fn surface_command_args(kind: SurfaceKind, mode: PickerMode) -> Vec<String> {
    match kind {
        SurfaceKind::Picker => vec![
            "ui".to_string(),
            match mode {
                PickerMode::AllSessions => "picker".to_string(),
                PickerMode::Worktree => "picker-worktree".to_string(),
            },
        ],
        SurfaceKind::SidebarCompact => vec!["ui".to_string(), "sidebar-compact".to_string()],
        SurfaceKind::SidebarExpanded => vec!["ui".to_string(), "sidebar-expanded".to_string()],
    }
}

#[cfg(feature = "embers")]
fn surface_title(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Picker => "Wisp Picker",
        SurfaceKind::SidebarCompact | SurfaceKind::SidebarExpanded => SIDEBAR_PANE_TITLE,
    }
}

#[cfg(feature = "embers")]
fn embers_sidebar_env(persistent: bool) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if persistent {
        env.insert(
            EMBERS_SURFACE_ENV.to_string(),
            EMBERS_SIDEBAR_PANE_SURFACE.to_string(),
        );
    }
    env
}

#[cfg(feature = "embers")]
fn resolve_dimension_to_cells(dimension: &Dimension, max: u16) -> u16 {
    match dimension {
        Dimension::Percent(percent) => {
            ((u32::from(max) * u32::from(*percent)) / 100).clamp(1, u32::from(max.max(1))) as u16
        }
        Dimension::Cells(cells) => (*cells).clamp(1, max.max(1)),
    }
}

#[cfg(feature = "embers")]
fn create_embers_floating_for_buffer(
    client: &EmbersClient,
    buffer_id: &str,
    title: Option<&str>,
    width: u16,
    height: u16,
    focus: bool,
    close_on_empty: bool,
) -> Result<(), Box<dyn Error>> {
    match client.create_floating_for_buffer_in_current_session(
        buffer_id,
        title,
        width,
        height,
        focus,
        close_on_empty,
    ) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = client.kill_buffer(buffer_id);
            Err(Box::new(error))
        }
    }
}

#[cfg(feature = "embers")]
fn join_embers_buffer_to_current_session_root(
    client: &EmbersClient,
    buffer_id: &str,
    placement: EmbersJoinPlacement,
    leading_size: Option<u16>,
    focus: bool,
) -> Result<String, Box<dyn Error>> {
    match client.join_buffer_to_current_session_root(buffer_id, placement, leading_size, focus) {
        Ok(session_name) => Ok(session_name),
        Err(error) => {
            let _ = client.kill_buffer(buffer_id);
            Err(Box::new(error))
        }
    }
}

fn open_popup_or_run_inline(
    kind: SurfaceKind,
    config: &ResolvedConfig,
    mode: PickerMode,
    backend: &RuntimeBackend,
) -> Result<(), Box<dyn Error>> {
    match backend {
        RuntimeBackend::Tmux => {
            let tmux_backend = PollingTmuxBackend::new(CommandTmuxClient::new());
            let command = PopupCommand {
                program: env::current_exe()?,
                args: vec![
                    "ui".to_string(),
                    match mode {
                        PickerMode::AllSessions => "picker".to_string(),
                        PickerMode::Worktree => "picker-worktree".to_string(),
                    },
                ],
            };
            match tmux_backend.open_popup(&PopupSpec {
                command,
                options: PopupOptions::default(),
            }) {
                Ok(()) => Ok(()),
                Err(TmuxError::PopupUnavailable { .. }) | Err(TmuxError::CommandFailed { .. }) => {
                    run_surface(kind, config, mode, backend)
                }
                Err(error) => Err(Box::new(error)),
            }
        }
        #[cfg(feature = "embers")]
        RuntimeBackend::Embers(client) => {
            let (viewport_cols, viewport_rows) = client
                .current_session_viewport_size()?
                .ok_or_else(|| "embers popup requires an active embers session".to_string())?;
            let command = surface_command_args(kind, mode);
            let cwd = env::current_dir()?;
            let width = resolve_dimension_to_cells(&config.tmux.popup_width, viewport_cols);
            let height = resolve_dimension_to_cells(&config.tmux.popup_height, viewport_rows);
            let buffer_id = client.create_buffer(
                &command,
                surface_title(kind),
                Some(&cwd),
                &BTreeMap::new(),
            )?;
            create_embers_floating_for_buffer(
                client,
                &buffer_id,
                Some(surface_title(kind)),
                width,
                height,
                true,
                true,
            )?;
            Ok(())
        }
    }
}

fn open_sidebar_popup_or_run_inline(
    config: &ResolvedConfig,
    backend: &RuntimeBackend,
) -> Result<(), Box<dyn Error>> {
    match backend {
        RuntimeBackend::Tmux => {
            let tmux_backend = PollingTmuxBackend::new(CommandTmuxClient::new());
            let command = PopupCommand {
                program: env::current_exe()?,
                args: vec!["ui".to_string(), "sidebar-compact".to_string()],
            };
            match tmux_backend.open_popup(&PopupSpec {
                command,
                options: PopupOptions {
                    width: wisp_tmux::PopupDimension::Percent(35),
                    height: wisp_tmux::PopupDimension::Percent(85),
                    title: Some("Wisp Sidebar".to_string()),
                },
            }) {
                Ok(()) => Ok(()),
                Err(TmuxError::PopupUnavailable { .. }) | Err(TmuxError::CommandFailed { .. }) => {
                    run_surface(
                        SurfaceKind::SidebarCompact,
                        config,
                        PickerMode::AllSessions,
                        backend,
                    )
                }
                Err(error) => Err(Box::new(error)),
            }
        }
        #[cfg(feature = "embers")]
        RuntimeBackend::Embers(client) => {
            let (viewport_cols, viewport_rows) =
                client.current_session_viewport_size()?.ok_or_else(|| {
                    "embers sidebar-popup requires an active embers session".to_string()
                })?;
            let command =
                surface_command_args(SurfaceKind::SidebarCompact, PickerMode::AllSessions);
            let cwd = env::current_dir()?;
            let buffer_id = client.create_buffer(
                &command,
                surface_title(SurfaceKind::SidebarCompact),
                Some(&cwd),
                &BTreeMap::new(),
            )?;
            create_embers_floating_for_buffer(
                client,
                &buffer_id,
                Some(surface_title(SurfaceKind::SidebarCompact)),
                resolve_dimension_to_cells(&Dimension::Percent(35), viewport_cols),
                resolve_dimension_to_cells(&Dimension::Percent(85), viewport_rows),
                true,
                true,
            )?;
            Ok(())
        }
    }
}

fn open_sidebar_pane(backend: &RuntimeBackend) -> Result<(), Box<dyn Error>> {
    match backend {
        RuntimeBackend::Tmux => {
            let tmux = CommandTmuxClient::new();
            reconcile_sidebar_for_current_context(
                &tmux,
                &sidebar_surface_command(env::current_exe()?),
                None,
            )?;
            Ok(())
        }
        #[cfg(feature = "embers")]
        RuntimeBackend::Embers(client) => {
            let command =
                surface_command_args(SurfaceKind::SidebarCompact, PickerMode::AllSessions);
            let cwd = env::current_dir()?;
            let buffer_id = client.create_buffer(
                &command,
                SIDEBAR_PANE_TITLE,
                Some(&cwd),
                &embers_sidebar_env(true),
            )?;
            join_embers_buffer_to_current_session_root(
                client,
                &buffer_id,
                EmbersJoinPlacement::Left,
                Some(SIDEBAR_PANE_WIDTH),
                true,
            )?;
            Ok(())
        }
    }
}

fn run_surface(
    kind: SurfaceKind,
    config: &ResolvedConfig,
    mode: PickerMode,
    backend: &RuntimeBackend,
) -> Result<(), Box<dyn Error>> {
    let zoxide_entries = load_zoxide_entries();
    let loaded_state = load_domain_state_with_zoxide(backend, zoxide_entries.clone())?;
    let mut active_client_id = loaded_state.client_id;
    let state = loaded_state.state;
    let mut session_sort = config.ui.session_sort;
    let (mut session_items, mut pending_branch_names, mut branch_status_updates) =
        rebuild_session_items_for_picker_mode(
            &state,
            Some(active_client_id.as_str()),
            mode,
            session_sort,
        );
    let mut pane_preview_provider = ActivePanePreviewProvider::new(CommandTmuxClient::new());
    #[cfg(feature = "embers")]
    let mut embers_preview_provider = match backend {
        RuntimeBackend::Tmux => None,
        RuntimeBackend::Embers(client) => Some(EmbersPanePreviewProvider::new(client.clone())),
    };
    let mut details_preview_provider = SessionDetailsPreviewProvider {
        state: state.clone(),
    };
    let filesystem_preview_provider = FilesystemPreviewProvider::default();
    let tmux = CommandTmuxClient::new();
    let zoxide = CommandZoxideProvider::new();
    let kindra = CommandKindraProvider::new();
    let current_directory = env::current_dir()?;
    let sidebar_command = sidebar_surface_command(env::current_exe()?);
    let mut sidebar_runtime = match backend {
        RuntimeBackend::Tmux => tmux_sidebar_runtime(&tmux, kind)?,
        #[cfg(feature = "embers")]
        RuntimeBackend::Embers(client) => embers_sidebar_runtime(client, kind)?,
    };
    let saved_sidebar_state = match &sidebar_runtime {
        Some(runtime) => load_sidebar_ui_state(runtime.session_name())?,
        None => None,
    };
    if let Some(saved_state) = &saved_sidebar_state
        && let Some(saved_sort_mode) = saved_state.sort_mode
    {
        session_sort = saved_sort_mode;
        (session_items, pending_branch_names, branch_status_updates) =
            rebuild_session_items_for_picker_mode(
                &state,
                Some(active_client_id.as_str()),
                mode,
                session_sort,
            );
    }
    let mut query = saved_sidebar_state
        .as_ref()
        .map(|state| state.query.clone())
        .unwrap_or_default();
    let mut selected = saved_sidebar_state
        .as_ref()
        .and_then(|state| {
            selected_index_for_session(&session_items, &query, state.selected_session_id.as_deref())
        })
        .or_else(|| {
            selected_index_for_session(&session_items, &query, current_session_id(&session_items))
        })
        .unwrap_or(0);
    let mut show_help = true;
    let bindings = picker_bindings(config);
    let mut input_mode = InputMode::Filter;
    let mut preview_enabled = matches!(kind, SurfaceKind::Picker);
    let mut preview_mode = PreviewMode::Pane;
    let mut preview = preview_enabled.then_some(Vec::new());
    let mut preview_session_id = None;
    let mut preview_refreshed_at: Option<Instant> = None;
    let mut surface_kind = saved_sidebar_state
        .as_ref()
        .map(|state| state.kind)
        .unwrap_or(kind);
    let mut picker_mode = mode;
    let mut first_frame = true;
    let mut deferred_branch_status = BTreeMap::new();
    let mut last_zoxide_query: String = String::new();
    let mut last_zoxide_match: Option<wisp_zoxide::DirectoryEntry> = None;
    // Cache of the worktree-mode repo root we last probed for Kindra temp-worktree
    // support, plus the resolved trunk. Detection shells out to git/kin, so we only
    // recompute it when the repo root under the cursor changes.
    let mut last_kindra_repo_root: Option<Option<PathBuf>> = None;
    let mut kindra_temp_context: Option<KindraTempContext> = None;

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut teardown = TerminalTeardown::active();
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = loop {
        pane_preview_provider.max_lines = preview_line_budget(&terminal, show_help)?;
        #[cfg(feature = "embers")]
        if let Some(provider) = embers_preview_provider.as_mut() {
            provider.max_lines = preview_line_budget(&terminal, show_help)?;
        }

        if let Some(runtime) = &mut sidebar_runtime {
            #[cfg(feature = "embers")]
            let requires_handoff = sidebar_requires_handoff(
                &tmux,
                match backend {
                    RuntimeBackend::Tmux => None,
                    RuntimeBackend::Embers(client) => Some(client.as_ref()),
                },
                runtime,
            )?;
            #[cfg(not(feature = "embers"))]
            let requires_handoff = sidebar_requires_handoff(&tmux, runtime)?;

            if !requires_handoff {
                // Stay on the current surface.
            } else {
                persist_sidebar_ui_state(
                    runtime,
                    &session_items,
                    &query,
                    surface_kind,
                    session_sort,
                    selected,
                )?;
                match (&runtime.target, backend) {
                    (SidebarTarget::Tmux { pane_id, .. }, RuntimeBackend::Tmux) => {
                        reconcile_sidebar_for_current_context(
                            &tmux,
                            &sidebar_command,
                            pane_id.as_deref(),
                        )?;
                        break Ok(());
                    }
                    #[cfg(feature = "embers")]
                    (SidebarTarget::Embers { buffer_id }, RuntimeBackend::Embers(client)) => {
                        let new_session_name = client.join_buffer_to_current_session_root(
                            buffer_id,
                            EmbersJoinPlacement::Left,
                            Some(SIDEBAR_PANE_WIDTH),
                            true,
                        )?;
                        runtime.rename_session(new_session_name);
                        let reloaded = reload_sidebar_runtime_state(SidebarReloadContext {
                            config,
                            backend,
                            picker_mode,
                            default_kind: kind,
                            runtime_session_name: runtime.session_name(),
                            session_sort: &mut session_sort,
                            query: &mut query,
                            selected: &mut selected,
                            surface_kind: &mut surface_kind,
                        })?;
                        (
                            details_preview_provider.state,
                            session_items,
                            pending_branch_names,
                            branch_status_updates,
                        ) = reloaded;
                        deferred_branch_status.clear();
                        preview_session_id = None;
                        preview_refreshed_at = None;
                        if preview_enabled {
                            preview = Some(Vec::new());
                        }
                        continue;
                    }
                    #[cfg(feature = "embers")]
                    _ => {}
                }
            }
        }

        let mut filtered = match input_mode {
            InputMode::Filter => filter_items(&session_items, &query),
            InputMode::Rename { .. } => session_items.clone(),
        };
        if matches!(input_mode, InputMode::Filter)
            && !query.trim().is_empty()
            && picker_mode == PickerMode::AllSessions
        {
            let query_trimmed = query.trim();
            if query_trimmed != last_zoxide_query.trim() {
                last_zoxide_query = query_trimmed.to_string();
                last_zoxide_match = zoxide.query_directory(&query).ok().flatten();
            }
            if let Some(zoxide_match) = &last_zoxide_match {
                let path_display = zoxide_match.path.display().to_string();
                let basename = zoxide_match
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                filtered.push(SessionListItem {
                    session_id: format!("zoxide:{}", zoxide_match.path.display()),
                    label: basename,
                    kind: wisp_core::SessionListItemKind::Zoxide,
                    is_current: false,
                    is_previous: false,
                    last_activity: None,
                    attached: false,
                    attention: wisp_core::AttentionBadge::None,
                    attention_count: 0,
                    active_window_label: None,
                    path_hint: Some(path_display),
                    command_hint: None,
                    git_branch: None,
                    worktree_path: Some(zoxide_match.path.clone()),
                    worktree_branch: None,
                });
            }
        } else if query.trim().is_empty() {
            last_zoxide_query.clear();
            last_zoxide_match = None;
        }
        if matches!(input_mode, InputMode::Filter) && picker_mode == PickerMode::Worktree {
            let repo_root = git::worktree_repo_root(
                &details_preview_provider.state,
                Some(active_client_id.as_str()),
            );
            if last_kindra_repo_root.as_ref() != Some(&repo_root) {
                last_kindra_repo_root = Some(repo_root.clone());
                kindra_temp_context = repo_root
                    .filter(|root| kindra.temp_worktrees_configured(root))
                    .map(|root| {
                        let trunk = git::trunk_branch(&root);
                        KindraTempContext {
                            repo_root: root,
                            trunk,
                        }
                    });
            }
            if let Some(context) = &kindra_temp_context
                && let Some(branch) = kindra_temp_branch_name(&query)
            {
                filtered.push(SessionListItem {
                    session_id: format!("kindra-temp:{branch}"),
                    label: branch.clone(),
                    kind: wisp_core::SessionListItemKind::CreateTempWorktree,
                    is_current: false,
                    is_previous: false,
                    last_activity: None,
                    attached: false,
                    attention: wisp_core::AttentionBadge::None,
                    attention_count: 0,
                    active_window_label: None,
                    path_hint: Some(format!("new temp worktree from {}", context.trunk)),
                    command_hint: None,
                    git_branch: None,
                    worktree_path: Some(context.repo_root.clone()),
                    worktree_branch: Some(branch),
                });
            }
        }
        if selected >= filtered.len() {
            selected = filtered.len().saturating_sub(1);
        }
        let selected_item = filtered.get(selected);
        let should_refresh_preview = !first_frame
            && preview_enabled
            && selected_item.is_some()
            && match (
                selected_item,
                preview_session_id.as_deref(),
                preview_refreshed_at,
            ) {
                (Some(item), Some(previous_session_id), Some(refreshed_at))
                    if previous_session_id == item.session_id
                        && preview_mode == PreviewMode::Pane =>
                {
                    refreshed_at.elapsed() >= PREVIEW_REFRESH_DEBOUNCE
                }
                (Some(item), Some(previous_session_id), _)
                    if previous_session_id == item.session_id =>
                {
                    false
                }
                (Some(_), _, _) => true,
                (None, _, _) => false,
            };

        if !preview_enabled {
            preview = None;
            preview_session_id = None;
            preview_refreshed_at = None;
        } else if should_refresh_preview && let Some(item) = selected_item {
            let session_preview_provider: &dyn PreviewProvider = match preview_mode {
                #[cfg(feature = "embers")]
                PreviewMode::Pane => match (&embers_preview_provider, backend) {
                    (_, RuntimeBackend::Tmux) => &pane_preview_provider,
                    (Some(provider), RuntimeBackend::Embers(_)) => provider,
                    (None, RuntimeBackend::Embers(_)) => &details_preview_provider,
                },
                #[cfg(not(feature = "embers"))]
                PreviewMode::Pane => &pane_preview_provider,
                PreviewMode::Details => &details_preview_provider,
            };
            let combined_provider = CombinedPreviewProvider {
                session: session_preview_provider,
                filesystem: &filesystem_preview_provider,
            };
            preview = Some(generate_preview(
                &combined_provider as &dyn PreviewProvider,
                item,
            ));
            preview_session_id = Some(item.session_id.clone());
            preview_refreshed_at = Some(Instant::now());
        } else if selected_item.is_none() {
            preview = Some(Vec::new());
            preview_session_id = None;
            preview_refreshed_at = None;
        }

        let model = SurfaceModel {
            title: match (&surface_kind, &input_mode) {
                (SurfaceKind::Picker, InputMode::Rename { .. }) => "Rename Session".to_string(),
                (SurfaceKind::Picker, InputMode::Filter) => "Wisp Picker".to_string(),
                (SurfaceKind::SidebarCompact, _) => "Wisp Sidebar".to_string(),
                (SurfaceKind::SidebarExpanded, _) => "Wisp Sidebar+".to_string(),
            },
            query: query.clone(),
            items: filtered.clone(),
            selected,
            show_help,
            preview: preview.clone(),
            kind: surface_kind,
            bindings: bindings.clone(),
            mode: picker_mode,
        };

        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Clear, area);
            render_surface(area, frame.buffer_mut(), &model);
        })?;

        if first_frame {
            first_frame = false;
            preview_session_id = None;
            preview_refreshed_at = None;
            continue;
        }

        while let Ok(update) = branch_status_updates.try_recv() {
            if has_branch_name(&session_items, &update.session_id) {
                update_branch_status(
                    &mut session_items,
                    &update.session_id,
                    update.sync,
                    update.dirty,
                );
            } else {
                deferred_branch_status.insert(update.session_id.clone(), update);
            }
        }

        if let Some(work_item) = pending_branch_names.pop_front() {
            if let Some(branch_name) = git::branch_name_for_directory(&work_item.path) {
                update_branch_name(&mut session_items, &work_item.session_id, branch_name);
                if let Some(update) = deferred_branch_status.remove(&work_item.session_id) {
                    update_branch_status(
                        &mut session_items,
                        &update.session_id,
                        update.sync,
                        update.dirty,
                    );
                }
            }
            continue;
        }

        #[cfg(feature = "embers")]
        if matches!(backend, RuntimeBackend::Embers(_)) && backend.poll_updates()? {
            let selected_session_id = filtered.get(selected).map(|item| item.session_id.clone());
            let reloaded_state = load_domain_state_with_zoxide(backend, zoxide_entries.clone())?;
            apply_reloaded_state(
                reloaded_state,
                picker_mode,
                session_sort,
                &mut active_client_id,
                &mut session_items,
                &mut pending_branch_names,
                &mut branch_status_updates,
                &mut details_preview_provider,
                &mut deferred_branch_status,
            );
            selected =
                selected_index_for_session(&session_items, &query, selected_session_id.as_deref())
                    .or_else(|| {
                        selected_index_for_session(
                            &session_items,
                            &query,
                            current_session_id(&session_items),
                        )
                    })
                    .unwrap_or(0);
            preview_session_id = None;
            preview_refreshed_at = None;
            if preview_enabled {
                preview = Some(Vec::new());
            }
            continue;
        }

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && let Some(intent) = translate_key(key, &bindings)
        {
            match intent {
                UiIntent::SelectNext => {
                    if matches!(input_mode, InputMode::Filter) && !filtered.is_empty() {
                        selected = (selected + 1).min(filtered.len() - 1);
                    }
                }
                UiIntent::SelectPrev => {
                    if matches!(input_mode, InputMode::Filter) {
                        selected = selected.saturating_sub(1);
                    }
                }
                UiIntent::FilterChanged(fragment) => {
                    query.push_str(&fragment);
                    if matches!(input_mode, InputMode::Filter) {
                        selected = 0;
                    }
                }
                UiIntent::Backspace => {
                    query.pop();
                    if matches!(input_mode, InputMode::Filter) {
                        selected = 0;
                    }
                }
                UiIntent::ToggleCompactSidebar => {
                    if matches!(input_mode, InputMode::Filter) {
                        surface_kind = match surface_kind {
                            SurfaceKind::SidebarCompact => SurfaceKind::SidebarExpanded,
                            SurfaceKind::SidebarExpanded => SurfaceKind::SidebarCompact,
                            other => other,
                        };
                    }
                }
                UiIntent::TogglePreview => {
                    if matches!(input_mode, InputMode::Filter) {
                        preview_enabled = !preview_enabled;
                        preview = preview_enabled.then_some(Vec::new());
                        preview_session_id = None;
                        preview_refreshed_at = None;
                    }
                }
                UiIntent::ToggleDetails => {
                    if matches!(input_mode, InputMode::Filter) {
                        preview_mode = match preview_mode {
                            PreviewMode::Pane => PreviewMode::Details,
                            PreviewMode::Details => PreviewMode::Pane,
                        };
                        preview_session_id = None;
                        preview_refreshed_at = None;
                    }
                }
                UiIntent::ToggleSort => {
                    if matches!(input_mode, InputMode::Filter) {
                        let selected_session_id =
                            filtered.get(selected).map(|item| item.session_id.clone());
                        session_sort = match session_sort {
                            SessionSortMode::Recent => SessionSortMode::Alphabetical,
                            SessionSortMode::Alphabetical => SessionSortMode::Recent,
                        };
                        apply_session_sort(&mut session_items, session_sort);
                        selected = selected_index_for_session(
                            &session_items,
                            &query,
                            selected_session_id.as_deref(),
                        )
                        .or_else(|| {
                            selected_index_for_session(
                                &session_items,
                                &query,
                                current_session_id(&session_items),
                            )
                        })
                        .unwrap_or(0);
                        preview_session_id = None;
                        preview_refreshed_at = None;
                    }
                }
                activate_intent @ (UiIntent::ActivateSelected
                | UiIntent::CreateSessionFromQuery) => match &input_mode {
                    InputMode::Filter => {
                        if let Some(runtime) = &sidebar_runtime {
                            persist_sidebar_ui_state(
                                runtime,
                                &session_items,
                                &query,
                                surface_kind,
                                session_sort,
                                selected,
                            )?;
                        }
                        let activated = match backend {
                            RuntimeBackend::Tmux => activate_filter_selection(
                                &tmux,
                                &zoxide,
                                &kindra,
                                &filtered,
                                selected,
                                &query,
                                &current_directory,
                                matches!(activate_intent, UiIntent::CreateSessionFromQuery),
                            )?,
                            #[cfg(feature = "embers")]
                            RuntimeBackend::Embers(client) => activate_filter_selection(
                                Arc::clone(client),
                                &zoxide,
                                &kindra,
                                &filtered,
                                selected,
                                &query,
                                &current_directory,
                                matches!(activate_intent, UiIntent::CreateSessionFromQuery),
                            )?,
                        };
                        if activated {
                            match (sidebar_runtime.as_mut(), backend) {
                                (Some(runtime), RuntimeBackend::Tmux) => {
                                    reconcile_sidebar_for_current_context(
                                        &tmux,
                                        &sidebar_command,
                                        runtime.tmux_pane_id(),
                                    )?;
                                    break Ok(());
                                }
                                #[cfg(feature = "embers")]
                                (Some(runtime), RuntimeBackend::Embers(client))
                                    if runtime.embers_buffer_id().is_some() =>
                                {
                                    let buffer_id = runtime
                                        .embers_buffer_id()
                                        .expect("checked embers sidebar buffer exists")
                                        .to_string();
                                    let new_session_name = client
                                        .join_buffer_to_current_session_root(
                                            &buffer_id,
                                            EmbersJoinPlacement::Left,
                                            Some(SIDEBAR_PANE_WIDTH),
                                            true,
                                        )?;
                                    runtime.rename_session(new_session_name);
                                    let reloaded =
                                        reload_sidebar_runtime_state(SidebarReloadContext {
                                            config,
                                            backend,
                                            picker_mode,
                                            default_kind: kind,
                                            runtime_session_name: runtime.session_name(),
                                            session_sort: &mut session_sort,
                                            query: &mut query,
                                            selected: &mut selected,
                                            surface_kind: &mut surface_kind,
                                        })?;
                                    (
                                        details_preview_provider.state,
                                        session_items,
                                        pending_branch_names,
                                        branch_status_updates,
                                    ) = reloaded;
                                    deferred_branch_status.clear();
                                    preview_session_id = None;
                                    preview_refreshed_at = None;
                                    if preview_enabled {
                                        preview = Some(Vec::new());
                                    }
                                    continue;
                                }
                                _ => break Ok(()),
                            }
                        }
                    }
                    InputMode::Rename {
                        session_id,
                        filter_query,
                    } => {
                        let session_id = session_id.clone();
                        let filter_query = filter_query.clone();
                        let new_name = query.trim().to_string();
                        if new_name.is_empty() || new_name == session_id {
                            query = filter_query.clone();
                            input_mode = InputMode::Filter;
                            preview_session_id = None;
                            preview_refreshed_at = None;
                            continue;
                        }

                        match backend {
                            RuntimeBackend::Tmux => tmux.rename_session(&session_id, &new_name)?,
                            #[cfg(feature = "embers")]
                            RuntimeBackend::Embers(client) => {
                                client.rename_session(&session_id, &new_name)?
                            }
                        }
                        let reloaded_state = load_domain_state(backend)?;
                        apply_reloaded_state(
                            reloaded_state,
                            picker_mode,
                            session_sort,
                            &mut active_client_id,
                            &mut session_items,
                            &mut pending_branch_names,
                            &mut branch_status_updates,
                            &mut details_preview_provider,
                            &mut deferred_branch_status,
                        );
                        query = filter_query.clone();
                        input_mode = InputMode::Filter;
                        selected =
                            selected_index_for_session(&session_items, &query, Some(&new_name))
                                .unwrap_or(selected);
                        preview_session_id = None;
                        preview_refreshed_at = None;
                        if preview_enabled {
                            preview = Some(Vec::new());
                        }
                        if let Some(runtime) = sidebar_runtime.as_mut()
                            && runtime.session_name() == session_id
                        {
                            clear_sidebar_ui_state(runtime.session_name())?;
                            runtime.rename_session(new_name);
                        }
                    }
                },
                UiIntent::RenameSession => {
                    if matches!(input_mode, InputMode::Filter)
                        && let Some(item) = filtered.get(selected)
                        && matches!(
                            item.kind,
                            wisp_core::SessionListItemKind::Session
                                | wisp_core::SessionListItemKind::WorktreeSession
                        )
                    {
                        input_mode = InputMode::Rename {
                            session_id: item.session_id.clone(),
                            filter_query: query.clone(),
                        };
                        query = item.session_id.clone();
                        preview_session_id = None;
                        preview_refreshed_at = None;
                    }
                }
                UiIntent::CloseSession => {
                    if matches!(input_mode, InputMode::Filter)
                        && let Some(item) = filtered.get(selected)
                        && matches!(
                            item.kind,
                            wisp_core::SessionListItemKind::Session
                                | wisp_core::SessionListItemKind::WorktreeSession
                        )
                    {
                        let session_id = item.session_id.clone();
                        match backend {
                            RuntimeBackend::Tmux => tmux.kill_session(&session_id)?,
                            #[cfg(feature = "embers")]
                            RuntimeBackend::Embers(client) => client.kill_session(&session_id)?,
                        }
                        let reloaded_state = load_domain_state(backend)?;
                        apply_reloaded_state(
                            reloaded_state,
                            picker_mode,
                            session_sort,
                            &mut active_client_id,
                            &mut session_items,
                            &mut pending_branch_names,
                            &mut branch_status_updates,
                            &mut details_preview_provider,
                            &mut deferred_branch_status,
                        );
                        preview_session_id = None;
                        preview_refreshed_at = None;
                        if preview_enabled {
                            preview = Some(Vec::new());
                        }
                    }
                }
                UiIntent::Close => match &input_mode {
                    InputMode::Filter => {
                        if let Some(runtime) = &sidebar_runtime {
                            match (&runtime.target, backend) {
                                (SidebarTarget::Tmux { pane_id, .. }, RuntimeBackend::Tmux) => {
                                    disable_sidebar_for_session(
                                        &tmux,
                                        runtime.session_name(),
                                        pane_id.as_deref(),
                                    )?;
                                }
                                #[cfg(feature = "embers")]
                                (
                                    SidebarTarget::Embers { buffer_id },
                                    RuntimeBackend::Embers(client),
                                ) => {
                                    client.detach_buffer(buffer_id)?;
                                }
                                #[cfg(feature = "embers")]
                                _ => {}
                            }
                            clear_sidebar_ui_state(runtime.session_name())?;
                        }
                        break Ok(());
                    }
                    InputMode::Rename { filter_query, .. } => {
                        query = filter_query.clone();
                        input_mode = InputMode::Filter;
                        preview_session_id = None;
                        preview_refreshed_at = None;
                    }
                },
                UiIntent::ToggleWorktreeMode => {
                    if matches!(input_mode, InputMode::Filter) {
                        picker_mode = match picker_mode {
                            PickerMode::AllSessions => PickerMode::Worktree,
                            PickerMode::Worktree => PickerMode::AllSessions,
                        };

                        let reloaded_state = load_domain_state(backend)?;
                        apply_reloaded_state(
                            reloaded_state,
                            picker_mode,
                            session_sort,
                            &mut active_client_id,
                            &mut session_items,
                            &mut pending_branch_names,
                            &mut branch_status_updates,
                            &mut details_preview_provider,
                            &mut deferred_branch_status,
                        );
                        selected = 0;
                        preview_session_id = None;
                        preview_refreshed_at = None;
                    }
                }
            }

            if let Some(runtime) = &sidebar_runtime
                && matches!(input_mode, InputMode::Filter)
            {
                persist_sidebar_ui_state(
                    runtime,
                    &session_items,
                    &query,
                    surface_kind,
                    session_sort,
                    selected,
                )?;
            }
        }
        show_help = matches!(surface_kind, SurfaceKind::Picker);
    };

    teardown.restore(&mut terminal)?;
    result
}

fn picker_bindings(config: &ResolvedConfig) -> KeyBindings {
    KeyBindings {
        down: ui_intent_for_action(config.actions.down),
        up: ui_intent_for_action(config.actions.up),
        ctrl_j: ui_intent_for_action(config.actions.ctrl_j),
        ctrl_k: ui_intent_for_action(config.actions.ctrl_k),
        enter: ui_intent_for_action(config.actions.enter),
        shift_enter: ui_intent_for_action(config.actions.shift_enter),
        backspace: ui_intent_for_action(config.actions.backspace),
        ctrl_r: ui_intent_for_action(config.actions.ctrl_r),
        ctrl_s: ui_intent_for_action(config.actions.ctrl_s),
        ctrl_x: ui_intent_for_action(config.actions.ctrl_x),
        ctrl_p: ui_intent_for_action(config.actions.ctrl_p),
        ctrl_d: ui_intent_for_action(config.actions.ctrl_d),
        ctrl_m: ui_intent_for_action(config.actions.ctrl_m),
        esc: ui_intent_for_action(config.actions.esc),
        ctrl_c: ui_intent_for_action(config.actions.ctrl_c),
        ctrl_w: ui_intent_for_action(config.actions.ctrl_w),
    }
}

fn ui_intent_for_action(action: KeyAction) -> UiIntent {
    match action {
        KeyAction::MoveDown => UiIntent::SelectNext,
        KeyAction::MoveUp => UiIntent::SelectPrev,
        KeyAction::Open => UiIntent::ActivateSelected,
        KeyAction::CreateSessionFromQuery => UiIntent::CreateSessionFromQuery,
        KeyAction::Backspace => UiIntent::Backspace,
        KeyAction::RenameSession => UiIntent::RenameSession,
        KeyAction::ToggleSort => UiIntent::ToggleSort,
        KeyAction::CloseSession => UiIntent::CloseSession,
        KeyAction::TogglePreview => UiIntent::TogglePreview,
        KeyAction::ToggleDetails => UiIntent::ToggleDetails,
        KeyAction::ToggleCompactSidebar => UiIntent::ToggleCompactSidebar,
        KeyAction::Close => UiIntent::Close,
        KeyAction::ToggleWorktreeMode => UiIntent::ToggleWorktreeMode,
    }
}

fn preview_line_budget(
    terminal: &Terminal<CrosstermBackend<std::io::Stdout>>,
    show_help: bool,
) -> Result<usize, Box<dyn Error>> {
    let area = terminal.size()?;
    let reserved_rows = if show_help { 8 } else { 6 };
    Ok(usize::from(area.height.saturating_sub(reserved_rows)).max(1))
}

fn generate_preview(provider: &dyn PreviewProvider, item: &SessionListItem) -> Vec<String> {
    match item.kind {
        wisp_core::SessionListItemKind::Info => vec![item.label.clone()],
        wisp_core::SessionListItemKind::Worktree => {
            // For worktrees without sessions, show directory listing or "not an active session"
            if let Some(path) = &item.worktree_path {
                let request = PreviewRequest::Directory {
                    key: PreviewKey::Directory(path.clone()),
                    path: path.clone(),
                };
                if provider.can_preview(&request) {
                    provider
                        .generate(&request)
                        .map(|content| content.body)
                        .unwrap_or_else(|_| vec!["not an active session".to_string()])
                } else {
                    vec!["preview not supported".to_string()]
                }
            } else {
                vec!["not an active session".to_string()]
            }
        }
        wisp_core::SessionListItemKind::WorktreeSession => {
            // For sessions in worktrees, show the session preview (tmux capture)
            provider
                .generate(&PreviewRequest::SessionSummary {
                    key: PreviewKey::Session(item.session_id.clone()),
                    session_name: item.session_id.clone(),
                })
                .map(|content| content.body)
                .unwrap_or_else(|error| vec![error.to_string()])
        }
        wisp_core::SessionListItemKind::CreateTempWorktree => {
            let mut lines = vec![format!("Create temporary worktree '{}'", item.label)];
            if let Some(hint) = &item.path_hint {
                lines.push(hint.clone());
            }
            lines
        }
        wisp_core::SessionListItemKind::Zoxide => {
            // For zoxide matches, show directory preview using worktree_path
            if let Some(path) = &item.worktree_path {
                let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
                let request = PreviewRequest::Directory {
                    key: PreviewKey::Directory(canonical_path.clone()),
                    path: canonical_path,
                };
                if provider.can_preview(&request) {
                    provider
                        .generate(&request)
                        .map(|content| content.body)
                        .unwrap_or_else(|_| vec!["directory not found".to_string()])
                } else {
                    vec!["preview not supported".to_string()]
                }
            } else {
                vec!["no path available".to_string()]
            }
        }
        _ => {
            // For regular sessions
            provider
                .generate(&PreviewRequest::SessionSummary {
                    key: PreviewKey::Session(item.session_id.clone()),
                    session_name: item.session_id.clone(),
                })
                .map(|content| content.body)
                .unwrap_or_else(|error| vec![error.to_string()])
        }
    }
}

fn sidebar_surface_command(program: PathBuf) -> PopupCommand {
    PopupCommand {
        program,
        args: vec!["ui".to_string(), "sidebar-compact".to_string()],
    }
}

fn tmux_sidebar_runtime(
    tmux: &impl TmuxClient,
    kind: SurfaceKind,
) -> Result<Option<SidebarRuntime>, Box<dyn Error>> {
    if !matches!(
        kind,
        SurfaceKind::SidebarCompact | SurfaceKind::SidebarExpanded
    ) {
        return Ok(None);
    }

    let context = tmux.current_context()?;
    let session_name = context.session_name.ok_or_else(|| TmuxError::Unavailable {
        message: "sidebar UI must run inside tmux".to_string(),
    })?;
    let home_window_index = context.window_index.ok_or_else(|| TmuxError::Unavailable {
        message: "sidebar UI requires an active tmux window".to_string(),
    })?;

    Ok(Some(SidebarRuntime {
        session_name,
        target: SidebarTarget::Tmux {
            home_window_index,
            pane_id: env::var("TMUX_PANE").ok(),
        },
    }))
}

#[cfg(feature = "embers")]
fn embers_sidebar_runtime(
    embers: &EmbersClient,
    kind: SurfaceKind,
) -> Result<Option<SidebarRuntime>, Box<dyn Error>> {
    if !matches!(
        kind,
        SurfaceKind::SidebarCompact | SurfaceKind::SidebarExpanded
    ) || env::var(EMBERS_SURFACE_ENV).ok().as_deref() != Some(EMBERS_SIDEBAR_PANE_SURFACE)
    {
        return Ok(None);
    }

    let session_name = embers
        .current_session_name()?
        .ok_or_else(|| "embers sidebar requires an active session".to_string())?;
    let buffer_id = embers
        .focused_buffer_id()?
        .ok_or_else(|| "embers sidebar could not resolve its focused buffer".to_string())?;
    Ok(Some(SidebarRuntime {
        session_name,
        target: SidebarTarget::Embers { buffer_id },
    }))
}

fn sidebar_requires_handoff(
    tmux: &impl TmuxClient,
    #[cfg(feature = "embers")] embers: Option<&EmbersClient>,
    runtime: &SidebarRuntime,
) -> Result<bool, Box<dyn Error>> {
    match &runtime.target {
        SidebarTarget::Tmux {
            home_window_index, ..
        } => {
            let context = tmux.current_context()?;
            Ok(
                context.session_name.as_deref() != Some(runtime.session_name())
                    || context.window_index != Some(*home_window_index),
            )
        }
        #[cfg(feature = "embers")]
        SidebarTarget::Embers { .. } => {
            let Some(embers) = embers else {
                return Ok(false);
            };
            Ok(embers
                .current_session_name()?
                .is_some_and(|session_name| session_name != runtime.session_name()))
        }
    }
}

fn reconcile_sidebar_for_current_context(
    tmux: &impl TmuxClient,
    command: &PopupCommand,
    exclude_pane: Option<&str>,
) -> Result<(), TmuxError> {
    let context = tmux.current_context()?;
    let session_name = context.session_name.ok_or_else(|| TmuxError::Unavailable {
        message: "sidebar-pane must run inside tmux".to_string(),
    })?;
    let window_index = context.window_index.ok_or_else(|| TmuxError::Unavailable {
        message: "sidebar-pane requires an active tmux window".to_string(),
    })?;
    reconcile_sidebar_for_window(tmux, &session_name, window_index, command, exclude_pane)
}

fn reconcile_sidebar_for_window(
    tmux: &impl TmuxClient,
    session_name: &str,
    active_window_index: u32,
    command: &PopupCommand,
    exclude_pane: Option<&str>,
) -> Result<(), TmuxError> {
    let mut sidebars = tmux
        .list_panes(None)?
        .into_iter()
        .filter(|pane| pane.session_name == session_name && pane.title == SIDEBAR_PANE_TITLE)
        .collect::<Vec<_>>();

    let keep_index = sidebars
        .iter()
        .position(|pane| {
            pane.window_index == active_window_index
                && exclude_pane.is_some_and(|exclude| pane.pane_id == exclude)
        })
        .or_else(|| {
            sidebars
                .iter()
                .position(|pane| pane.window_index == active_window_index && pane.active)
        })
        .or_else(|| {
            sidebars
                .iter()
                .position(|pane| pane.window_index == active_window_index)
        });

    let keep_pane_id = match keep_index {
        Some(index) => sidebars[index].pane_id.clone(),
        None => tmux.open_sidebar_pane(&SidebarPaneSpec {
            target: Some(format!("{session_name}:{active_window_index}")),
            side: SidebarSide::Left,
            width: SIDEBAR_PANE_WIDTH,
            title: Some(SIDEBAR_PANE_TITLE.to_string()),
            command: command.clone(),
        })?,
    };

    tmux.resize_pane_width(&keep_pane_id, SIDEBAR_PANE_WIDTH)?;

    for pane in sidebars.drain(..) {
        if pane.pane_id != keep_pane_id && Some(pane.pane_id.as_str()) != exclude_pane {
            tmux.close_sidebar_pane(Some(&pane.pane_id))?;
        }
    }

    tmux.select_pane(&keep_pane_id)?;
    Ok(())
}

fn disable_sidebar_for_session(
    tmux: &impl TmuxClient,
    session_name: &str,
    exclude_pane: Option<&str>,
) -> Result<(), TmuxError> {
    for pane in tmux
        .list_panes(None)?
        .into_iter()
        .filter(|pane| pane.session_name == session_name && pane.title == SIDEBAR_PANE_TITLE)
    {
        if Some(pane.pane_id.as_str()) != exclude_pane {
            tmux.close_sidebar_pane(Some(&pane.pane_id))?;
        }
    }

    Ok(())
}

fn sidebar_state_dir() -> PathBuf {
    env::var_os("WISP_SIDEBAR_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("wisp-sidebar"))
}

fn sidebar_state_path(session_name: &str) -> PathBuf {
    let sanitized = session_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    sidebar_state_dir().join(format!("{sanitized}.state"))
}

fn load_sidebar_ui_state(session_name: &str) -> Result<Option<SidebarUiState>, Box<dyn Error>> {
    let path = sidebar_state_path(session_name);
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Box::new(error)),
    };

    let mut parts = raw.splitn(4, '\n');
    let kind = match parts.next().unwrap_or_default().trim() {
        "compact" => SurfaceKind::SidebarCompact,
        "expanded" => SurfaceKind::SidebarExpanded,
        value => {
            return Err(format!("invalid sidebar state kind `{value}`").into());
        }
    };
    let next = parts.next().unwrap_or_default();
    let (sort_mode, selected_session_id, query) = match next.trim() {
        "recent" | "alphabetical" => {
            let sort_mode = next.parse::<SessionSortMode>()?;
            let selected_session_id = match parts.next() {
                Some(value) if !value.trim().is_empty() => Some(value.to_string()),
                _ => None,
            };
            let query = parts.next().unwrap_or_default().to_string();
            (Some(sort_mode), selected_session_id, query)
        }
        _ => {
            let selected_session_id = if !next.trim().is_empty() {
                Some(next.to_string())
            } else {
                None
            };
            let query = parts.next().unwrap_or_default().to_string();
            (None, selected_session_id, query)
        }
    };

    Ok(Some(SidebarUiState {
        query,
        selected_session_id,
        kind,
        sort_mode,
    }))
}

fn persist_sidebar_ui_state(
    runtime: &SidebarRuntime,
    session_items: &[SessionListItem],
    query: &str,
    kind: SurfaceKind,
    sort_mode: SessionSortMode,
    selected: usize,
) -> Result<(), Box<dyn Error>> {
    let filtered = filter_items(session_items, query);
    let state = SidebarUiState {
        query: query.to_string(),
        selected_session_id: filtered.get(selected).map(|item| item.session_id.clone()),
        kind: match kind {
            SurfaceKind::SidebarExpanded => SurfaceKind::SidebarExpanded,
            _ => SurfaceKind::SidebarCompact,
        },
        sort_mode: Some(sort_mode),
    };
    let path = sidebar_state_path(runtime.session_name());
    let directory = path
        .parent()
        .ok_or_else(|| format!("missing parent directory for `{}`", path.display()))?;
    fs::create_dir_all(directory)?;

    let kind_token = match state.kind {
        SurfaceKind::SidebarExpanded => "expanded",
        SurfaceKind::SidebarCompact => "compact",
        SurfaceKind::Picker => "compact",
    };
    let payload = format!(
        "{kind_token}\n{}\n{}\n{}",
        match state.sort_mode.unwrap_or(SessionSortMode::Recent) {
            SessionSortMode::Recent => "recent",
            SessionSortMode::Alphabetical => "alphabetical",
        },
        state.selected_session_id.as_deref().unwrap_or_default(),
        state.query
    );
    let temporary_path = path.with_extension("tmp");
    fs::write(&temporary_path, payload)?;
    fs::rename(temporary_path, path)?;
    Ok(())
}

fn clear_sidebar_ui_state(session_name: &str) -> Result<(), Box<dyn Error>> {
    match fs::remove_file(sidebar_state_path(session_name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}

fn load_zoxide_entries() -> Vec<wisp_zoxide::DirectoryEntry> {
    CommandZoxideProvider::new()
        .load_entries(500)
        .unwrap_or_default()
}

struct LoadedDomainState {
    state: DomainState,
    client_id: String,
}

fn load_domain_state(backend: &RuntimeBackend) -> Result<LoadedDomainState, Box<dyn Error>> {
    load_domain_state_with_zoxide(backend, load_zoxide_entries())
}

/// Build domain state from a fresh backend snapshot but a caller-supplied set of
/// zoxide entries. Reused by the embers update loop so a `zoxide query`
/// subprocess is not spawned on every server event — only the backend snapshot
/// is refreshed, while the (rarely changing) zoxide list is carried over.
fn load_domain_state_with_zoxide(
    backend: &RuntimeBackend,
    zoxide: Vec<wisp_zoxide::DirectoryEntry>,
) -> Result<LoadedDomainState, Box<dyn Error>> {
    let snapshot = match backend {
        RuntimeBackend::Tmux => {
            let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
            BackendSnapshot::Tmux(backend.snapshot()?)
        }
        #[cfg(feature = "embers")]
        RuntimeBackend::Embers(client) => BackendSnapshot::Embers(client.snapshot()?),
    };
    let client_id = snapshot_client_id(&snapshot);
    let state = build_domain_state(&CandidateSources {
        backend: snapshot,
        zoxide,
    });
    Ok(LoadedDomainState { state, client_id })
}

fn snapshot_client_id(snapshot: &BackendSnapshot) -> String {
    match snapshot {
        BackendSnapshot::Tmux(_) => DEFAULT_CLIENT_ID.to_string(),
        #[cfg(feature = "embers")]
        BackendSnapshot::Embers(snapshot) => snapshot
            .context
            .client_id
            .clone()
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string()),
    }
}

#[cfg(feature = "embers")]
type SidebarReload = (
    DomainState,
    Vec<SessionListItem>,
    VecDeque<GitWorkItem>,
    mpsc::Receiver<GitStatusUpdate>,
);

#[cfg(feature = "embers")]
struct SidebarReloadContext<'a> {
    config: &'a ResolvedConfig,
    backend: &'a RuntimeBackend,
    picker_mode: PickerMode,
    default_kind: SurfaceKind,
    runtime_session_name: &'a str,
    session_sort: &'a mut SessionSortMode,
    query: &'a mut String,
    selected: &'a mut usize,
    surface_kind: &'a mut SurfaceKind,
}

#[cfg(feature = "embers")]
fn reload_sidebar_runtime_state(
    context: SidebarReloadContext<'_>,
) -> Result<SidebarReload, Box<dyn Error>> {
    let SidebarReloadContext {
        config,
        backend,
        picker_mode,
        default_kind,
        runtime_session_name,
        session_sort,
        query,
        selected,
        surface_kind,
    } = context;

    let reloaded_state = load_domain_state(backend)?;
    *session_sort = config.ui.session_sort;
    let saved_sidebar_state = load_sidebar_ui_state(runtime_session_name)?;
    if let Some(saved_state) = &saved_sidebar_state
        && let Some(saved_sort_mode) = saved_state.sort_mode
    {
        *session_sort = saved_sort_mode;
    }
    let (session_items, pending_branch_names, branch_status_updates) =
        rebuild_session_items_for_picker_mode(
            &reloaded_state.state,
            Some(reloaded_state.client_id.as_str()),
            picker_mode,
            *session_sort,
        );
    *query = saved_sidebar_state
        .as_ref()
        .map(|state| state.query.clone())
        .unwrap_or_default();
    *surface_kind = saved_sidebar_state
        .as_ref()
        .map(|state| state.kind)
        .unwrap_or(default_kind);
    *selected = saved_sidebar_state
        .as_ref()
        .and_then(|state| {
            selected_index_for_session(&session_items, query, state.selected_session_id.as_deref())
        })
        .or_else(|| {
            selected_index_for_session(&session_items, query, current_session_id(&session_items))
        })
        .unwrap_or(0);
    Ok((
        reloaded_state.state,
        session_items,
        pending_branch_names,
        branch_status_updates,
    ))
}

fn git_work_items(state: &DomainState) -> VecDeque<GitWorkItem> {
    state
        .sessions
        .iter()
        .filter_map(|(session_id, session)| {
            let active_window = session
                .windows
                .values()
                .find(|window| window.active)
                .or_else(|| session.windows.values().next())?;
            let path = active_window.current_path.clone()?;
            Some(GitWorkItem {
                session_id: session_id.clone(),
                path,
            })
        })
        .collect()
}

fn git_work_items_for_worktree_items(items: &[SessionListItem]) -> VecDeque<GitWorkItem> {
    items
        .iter()
        .filter_map(|item| {
            matches!(
                item.kind,
                wisp_core::SessionListItemKind::Worktree
                    | wisp_core::SessionListItemKind::WorktreeSession
            )
            .then(|| item.worktree_path.as_ref())
            .flatten()
            .map(|path| GitWorkItem {
                session_id: item.session_id.clone(),
                path: path.clone(),
            })
        })
        .collect()
}

fn picker_info_item(message: &str) -> SessionListItem {
    SessionListItem {
        session_id: format!("info:{message}"),
        label: message.to_string(),
        kind: wisp_core::SessionListItemKind::Info,
        is_current: false,
        is_previous: false,
        last_activity: None,
        attached: false,
        attention: wisp_core::AttentionBadge::None,
        attention_count: 0,
        active_window_label: None,
        path_hint: None,
        command_hint: None,
        git_branch: None,
        worktree_path: None,
        worktree_branch: None,
    }
}

fn spawn_git_status_workers(work_items: Vec<GitWorkItem>) -> mpsc::Receiver<GitStatusUpdate> {
    let (sender, receiver) = mpsc::channel();
    let queue = Arc::new(Mutex::new(VecDeque::from(work_items)));
    let worker_count = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .max(1);

    for worker_index in 0..worker_count {
        let sender = sender.clone();
        let queue = Arc::clone(&queue);
        thread::spawn(move || {
            let mut reported_poison = false;
            loop {
                let work_item = match queue.lock() {
                    Ok(mut queue) => queue.pop_front(),
                    Err(poison) => {
                        if !reported_poison {
                            eprintln!(
                                "wisp: git status worker {worker_index} recovered from a poisoned work queue; continuing"
                            );
                            reported_poison = true;
                        }
                        poison.into_inner().pop_front()
                    }
                };
                let Some(work_item) = work_item else {
                    break;
                };

                if let Some((sync, dirty)) = git::branch_status_for_directory(&work_item.path) {
                    let _ = sender.send(GitStatusUpdate {
                        session_id: work_item.session_id,
                        sync,
                        dirty,
                    });
                }
            }
        });
    }

    drop(sender);
    receiver
}

fn stable_path_hash(path: &Path) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    path.to_string_lossy()
        .as_bytes()
        .iter()
        .fold(FNV_OFFSET_BASIS, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        })
}

fn update_branch_name(items: &mut [SessionListItem], session_id: &str, branch_name: String) {
    if let Some(item) = items.iter_mut().find(|item| item.session_id == session_id) {
        item.git_branch = Some(GitBranchStatus {
            name: branch_name,
            sync: GitBranchSync::Unknown,
            dirty: false,
        });
    }
}

fn has_branch_name(items: &[SessionListItem], session_id: &str) -> bool {
    items.iter().any(|item| {
        item.session_id == session_id
            && item
                .git_branch
                .as_ref()
                .is_some_and(|branch| !branch.name.is_empty())
    })
}

fn update_branch_status(
    items: &mut [SessionListItem],
    session_id: &str,
    sync: GitBranchSync,
    dirty: bool,
) {
    if let Some(branch) = items
        .iter_mut()
        .find(|item| item.session_id == session_id)
        .and_then(|item| item.git_branch.as_mut())
    {
        branch.sync = sync;
        branch.dirty = dirty;
    }
}

fn filter_items(items: &[SessionListItem], query: &str) -> Vec<SessionListItem> {
    let mut matcher = SimpleMatcher::default();
    matcher.set_items(
        items
            .iter()
            .map(|item| MatchItem {
                id: item.session_id.clone(),
                primary_text: item.label.clone(),
                secondary_text: item.active_window_label.clone(),
                search_text: item.picker_search_text(),
            })
            .collect(),
    );
    let results = matcher.query(query);
    if query.trim().is_empty() {
        return items.to_vec();
    }

    results
        .into_iter()
        .filter_map(|result| {
            items
                .iter()
                .find(|item| item.session_id == result.id)
                .cloned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use wisp_config::{KeyAction, ResolvedConfig, SessionSortMode};
    use wisp_kindra::{KindraError, KindraProvider};
    use wisp_status::StatusRenderMode;
    use wisp_tmux::{
        PopupCommand, PopupOptions, SidebarPaneSpec, TmuxCapabilities, TmuxClient, TmuxContext,
        TmuxError, TmuxPane, TmuxSession, TmuxSnapshot, TmuxVersion, TmuxWindow,
    };
    use wisp_ui::UiIntent;
    use wisp_zoxide::{DirectoryEntry, ZoxideError, ZoxideProvider};

    use crate::{
        SIDEBAR_PANE_TITLE, SIDEBAR_PANE_WIDTH, STATUSLINE_REFRESH_COMMAND,
        STATUSLINE_REFRESH_HOOKS, SidebarRuntime, SidebarTarget, StatuslineGroupCommand,
        StatuslineRenderCommand, StatuslineSubcommand, SurfaceKind, activate_filter_selection,
        apply_session_sort, clear_sidebar_ui_state, create_session_from_query, current_session_id,
        disable_sidebar_for_session, filter_items, install_statusline_refresh_hooks,
        load_sidebar_ui_state, persist_sidebar_ui_state, picker_bindings,
        reconcile_sidebar_for_current_context, selected_index_for_session,
        sidebar_requires_handoff, sidebar_state_path, sidebar_surface_command,
        statusline_command_expression, statusline_mode, uninstall_statusline_refresh_hooks,
        validate_statusline_flags,
    };

    #[derive(Default)]
    struct StubTmuxClient {
        context: TmuxContext,
        windows: Vec<TmuxWindow>,
        panes: Vec<TmuxPane>,
        existing_sessions: Vec<TmuxSession>,
        created_sessions: RefCell<Vec<(String, PathBuf)>>,
        switched_sessions: RefCell<Vec<String>>,
        opened_targets: RefCell<Vec<String>>,
        closed_panes: RefCell<Vec<String>>,
        selected_panes: RefCell<Vec<String>>,
        resized_panes: RefCell<Vec<(String, u16)>>,
        hooks: RefCell<Vec<(String, String)>>,
        cleared_hooks: RefCell<Vec<String>>,
        refreshed_status: Cell<bool>,
    }

    impl StubTmuxClient {
        fn with_context(mut self, context: TmuxContext) -> Self {
            self.context = context;
            self
        }

        fn with_windows(mut self, windows: Vec<TmuxWindow>) -> Self {
            self.windows = windows;
            self
        }

        fn with_panes(mut self, panes: Vec<TmuxPane>) -> Self {
            self.panes.extend(panes);
            self
        }

        fn with_existing_sessions(mut self, sessions: Vec<TmuxSession>) -> Self {
            self.existing_sessions = sessions;
            self
        }
    }

    impl TmuxClient for StubTmuxClient {
        fn capabilities(&self) -> Result<TmuxCapabilities, TmuxError> {
            Ok(TmuxCapabilities {
                version: TmuxVersion {
                    major: 3,
                    minor: 4,
                    patch: None,
                },
                supports_popup: true,
                supports_multi_status_lines: true,
                supports_status_mouse_ranges: true,
                mouse_enabled: true,
            })
        }

        fn current_context(&self) -> Result<TmuxContext, TmuxError> {
            Ok(self.context.clone())
        }

        fn list_sessions(&self) -> Result<Vec<TmuxSession>, TmuxError> {
            Ok(self.existing_sessions.clone())
        }

        fn list_windows(&self) -> Result<Vec<TmuxWindow>, TmuxError> {
            Ok(self.windows.clone())
        }

        fn list_panes(&self, target: Option<&str>) -> Result<Vec<TmuxPane>, TmuxError> {
            Ok(match target {
                Some(target) => {
                    let mut parts = target.split(':');
                    let session_name = parts.next().unwrap_or_default();
                    let window_index = parts
                        .next()
                        .and_then(|raw| raw.parse::<u32>().ok())
                        .unwrap_or_default();
                    self.panes
                        .iter()
                        .filter(|pane| {
                            pane.session_name == session_name && pane.window_index == window_index
                        })
                        .cloned()
                        .collect()
                }
                None => self.panes.clone(),
            })
        }

        fn capture_pane(&self, _target: &str) -> Result<String, TmuxError> {
            unreachable!("not used in test");
        }

        fn snapshot(&self, _query_windows: bool) -> Result<TmuxSnapshot, TmuxError> {
            unreachable!("not used in test");
        }

        fn ensure_session(&self, _session_name: &str, _directory: &Path) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn switch_or_attach_session(&self, session_name: &str) -> Result<(), TmuxError> {
            self.switched_sessions
                .borrow_mut()
                .push(session_name.to_string());
            Ok(())
        }

        fn rename_session(&self, _session_name: &str, _new_name: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn kill_session(&self, _session_name: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn create_or_switch_session(
            &self,
            session_name: &str,
            directory: &Path,
        ) -> Result<(), TmuxError> {
            self.created_sessions
                .borrow_mut()
                .push((session_name.to_string(), directory.to_path_buf()));
            Ok(())
        }

        fn open_popup(
            &self,
            _command: &PopupCommand,
            _options: &PopupOptions,
        ) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn open_sidebar_pane(&self, spec: &SidebarPaneSpec) -> Result<String, TmuxError> {
            self.opened_targets
                .borrow_mut()
                .push(spec.target.clone().expect("sidebar target"));
            Ok("%new".to_string())
        }

        fn close_sidebar_pane(&self, target: Option<&str>) -> Result<(), TmuxError> {
            self.closed_panes
                .borrow_mut()
                .push(target.expect("target pane").to_string());
            Ok(())
        }

        fn select_pane(&self, target: &str) -> Result<(), TmuxError> {
            self.selected_panes.borrow_mut().push(target.to_string());
            Ok(())
        }

        fn resize_pane_width(&self, target: &str, width: u16) -> Result<(), TmuxError> {
            self.resized_panes
                .borrow_mut()
                .push((target.to_string(), width));
            Ok(())
        }

        fn status_line_count(&self) -> Result<usize, TmuxError> {
            unreachable!("not used in test");
        }

        fn set_status_line_count(&self, _count: usize) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn clear_status_line(&self, _line: usize) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn update_status_line(&self, _line: usize, _content: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn set_hook(&self, hook: &str, command: &str) -> Result<(), TmuxError> {
            self.hooks
                .borrow_mut()
                .push((hook.to_string(), command.to_string()));
            Ok(())
        }

        fn clear_hook(&self, hook: &str) -> Result<(), TmuxError> {
            self.cleared_hooks.borrow_mut().push(hook.to_string());
            Ok(())
        }

        fn refresh_client_status(&self) -> Result<(), TmuxError> {
            self.refreshed_status.set(true);
            Ok(())
        }
    }

    #[derive(Default)]
    struct StubZoxideProvider {
        entries: RefCell<Vec<(String, DirectoryEntry)>>,
    }

    impl StubZoxideProvider {
        fn with_match(self, query: &str, directory: &Path) -> Self {
            self.entries.borrow_mut().push((
                query.to_string(),
                DirectoryEntry {
                    path: directory.to_path_buf(),
                    score: Some(42.0),
                    exists: true,
                },
            ));
            self
        }
    }

    impl ZoxideProvider for StubZoxideProvider {
        fn load_entries(&self, _max_entries: usize) -> Result<Vec<DirectoryEntry>, ZoxideError> {
            unreachable!("not used in test");
        }

        fn query_directory(&self, query: &str) -> Result<Option<DirectoryEntry>, ZoxideError> {
            Ok(self
                .entries
                .borrow()
                .iter()
                .find(|(candidate_query, _)| candidate_query == query)
                .map(|(_, entry)| entry.clone()))
        }
    }

    #[derive(Default)]
    struct StubKindraProvider {
        configured: bool,
        created: RefCell<Vec<(PathBuf, String, String)>>,
        result_path: Option<PathBuf>,
    }

    impl StubKindraProvider {
        fn configured_with(path: &Path) -> Self {
            Self {
                configured: true,
                created: RefCell::new(Vec::new()),
                result_path: Some(path.to_path_buf()),
            }
        }
    }

    impl KindraProvider for StubKindraProvider {
        fn temp_worktrees_configured(&self, _repo_root: &Path) -> bool {
            self.configured
        }

        fn create_temp_worktree(
            &self,
            repo_root: &Path,
            new_branch: &str,
            start_point: &str,
        ) -> Result<PathBuf, KindraError> {
            self.created.borrow_mut().push((
                repo_root.to_path_buf(),
                new_branch.to_string(),
                start_point.to_string(),
            ));
            self.result_path.clone().ok_or(KindraError::MissingPath)
        }
    }

    #[test]
    fn reconcile_sidebar_keeps_only_the_active_window_sidebar() {
        let tmux = StubTmuxClient::default()
            .with_context(TmuxContext {
                session_name: Some("alpha".to_string()),
                window_index: Some(2),
                ..TmuxContext::default()
            })
            .with_panes(vec![
                TmuxPane {
                    session_name: "alpha".to_string(),
                    window_index: 1,
                    pane_id: "%stale".to_string(),
                    title: SIDEBAR_PANE_TITLE.to_string(),
                    active: false,
                    current_command: Some("wisp".to_string()),
                },
                TmuxPane {
                    session_name: "alpha".to_string(),
                    window_index: 2,
                    pane_id: "%keep".to_string(),
                    title: SIDEBAR_PANE_TITLE.to_string(),
                    active: true,
                    current_command: Some("wisp".to_string()),
                },
            ])
            .with_windows(vec![TmuxWindow {
                session_name: "alpha".to_string(),
                index: 2,
                name: "logs".to_string(),
                active: true,
                activity: false,
                bell: false,
                silence: false,
                current_path: None,
                current_command: None,
            }]);

        reconcile_sidebar_for_current_context(
            &tmux,
            &sidebar_surface_command(PathBuf::from("/tmp/wisp")),
            None,
        )
        .expect("sidebar should reconcile");

        assert!(tmux.opened_targets.borrow().is_empty());
        assert_eq!(&*tmux.closed_panes.borrow(), &["%stale".to_string()]);
        assert_eq!(&*tmux.selected_panes.borrow(), &["%keep".to_string()]);
        assert_eq!(
            &*tmux.resized_panes.borrow(),
            &[("%keep".to_string(), SIDEBAR_PANE_WIDTH)]
        );
    }

    #[test]
    fn reconcile_sidebar_opens_a_new_sidebar_when_the_active_window_has_none() {
        let tmux = StubTmuxClient::default()
            .with_context(TmuxContext {
                session_name: Some("alpha".to_string()),
                window_index: Some(2),
                ..TmuxContext::default()
            })
            .with_panes(vec![TmuxPane {
                session_name: "alpha".to_string(),
                window_index: 1,
                pane_id: "%old".to_string(),
                title: SIDEBAR_PANE_TITLE.to_string(),
                active: false,
                current_command: Some("wisp".to_string()),
            }]);

        reconcile_sidebar_for_current_context(
            &tmux,
            &sidebar_surface_command(PathBuf::from("/tmp/wisp")),
            Some("%old"),
        )
        .expect("sidebar should be created");

        assert_eq!(&*tmux.opened_targets.borrow(), &["alpha:2".to_string()]);
        assert!(tmux.closed_panes.borrow().is_empty());
        assert_eq!(&*tmux.selected_panes.borrow(), &["%new".to_string()]);
        assert_eq!(
            &*tmux.resized_panes.borrow(),
            &[("%new".to_string(), SIDEBAR_PANE_WIDTH)]
        );
    }

    #[test]
    fn disable_sidebar_closes_other_sidebar_panes_in_the_session() {
        let tmux = StubTmuxClient::default().with_panes(vec![
            TmuxPane {
                session_name: "alpha".to_string(),
                window_index: 1,
                pane_id: "%1".to_string(),
                title: SIDEBAR_PANE_TITLE.to_string(),
                active: false,
                current_command: Some("wisp".to_string()),
            },
            TmuxPane {
                session_name: "alpha".to_string(),
                window_index: 2,
                pane_id: "%2".to_string(),
                title: SIDEBAR_PANE_TITLE.to_string(),
                active: false,
                current_command: Some("wisp".to_string()),
            },
            TmuxPane {
                session_name: "beta".to_string(),
                window_index: 1,
                pane_id: "%3".to_string(),
                title: SIDEBAR_PANE_TITLE.to_string(),
                active: false,
                current_command: Some("wisp".to_string()),
            },
        ]);

        disable_sidebar_for_session(&tmux, "alpha", Some("%2")).expect("sidebars should be closed");

        assert_eq!(&*tmux.closed_panes.borrow(), &["%1".to_string()]);
    }

    #[test]
    fn sidebar_state_round_trips_query_selection_and_kind() {
        let session_name = format!("alpha-{}", unique_suffix());

        let runtime = SidebarRuntime {
            session_name: session_name.clone(),
            target: SidebarTarget::Tmux {
                home_window_index: 1,
                pane_id: Some("%1".to_string()),
            },
        };
        let items = vec![session_item("alpha"), session_item("beta")];
        persist_sidebar_ui_state(
            &runtime,
            &items,
            "be",
            SurfaceKind::SidebarExpanded,
            SessionSortMode::Alphabetical,
            0,
        )
        .expect("state should persist");

        let state = load_sidebar_ui_state(&session_name)
            .expect("state should load")
            .expect("state should exist");
        assert_eq!(state.query, "be");
        assert_eq!(state.selected_session_id.as_deref(), Some("beta"));
        assert_eq!(state.kind, SurfaceKind::SidebarExpanded);
        assert_eq!(state.sort_mode, Some(SessionSortMode::Alphabetical));

        clear_sidebar_ui_state(&session_name).expect("state should clear");
        assert!(
            load_sidebar_ui_state(&session_name)
                .expect("state should load")
                .is_none()
        );
    }

    #[test]
    fn sidebar_state_loads_legacy_format_without_overriding_default_sort() {
        let session_name = format!(
            "legacy-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        let state_path = sidebar_state_path(&session_name);
        fs::create_dir_all(state_path.parent().expect("sidebar state parent"))
            .expect("state dir should exist");
        fs::write(&state_path, "compact\nbeta\nneedle").expect("legacy state should write");

        let state = load_sidebar_ui_state(&session_name)
            .expect("state should load")
            .expect("state should exist");
        assert_eq!(state.query, "needle");
        assert_eq!(state.selected_session_id.as_deref(), Some("beta"));
        assert_eq!(state.kind, SurfaceKind::SidebarCompact);
        assert_eq!(state.sort_mode, None);

        clear_sidebar_ui_state(&session_name).expect("state should clear");
    }

    #[test]
    fn sidebar_handoff_only_triggers_when_session_or_window_changes() {
        let tmux = StubTmuxClient::default().with_context(TmuxContext {
            session_name: Some("alpha".to_string()),
            window_index: Some(2),
            ..TmuxContext::default()
        });
        let runtime = SidebarRuntime {
            session_name: "alpha".to_string(),
            target: SidebarTarget::Tmux {
                home_window_index: 1,
                pane_id: Some("%1".to_string()),
            },
        };

        #[cfg(feature = "embers")]
        let handoff = sidebar_requires_handoff(&tmux, None, &runtime);
        #[cfg(not(feature = "embers"))]
        let handoff = sidebar_requires_handoff(&tmux, &runtime);
        assert!(handoff.expect("handoff should evaluate"));

        #[cfg(feature = "embers")]
        let stable_handoff = sidebar_requires_handoff(
            &StubTmuxClient::default().with_context(TmuxContext {
                session_name: Some("alpha".to_string()),
                window_index: Some(1),
                ..TmuxContext::default()
            }),
            None,
            &runtime,
        );
        #[cfg(not(feature = "embers"))]
        let stable_handoff = sidebar_requires_handoff(
            &StubTmuxClient::default().with_context(TmuxContext {
                session_name: Some("alpha".to_string()),
                window_index: Some(1),
                ..TmuxContext::default()
            }),
            &runtime,
        );
        assert!(!stable_handoff.expect("handoff should evaluate"));
    }

    #[test]
    fn statusline_render_rejects_conflicting_force_flags() {
        let command = StatuslineGroupCommand {
            command: StatuslineSubcommand::Render(StatuslineRenderCommand {
                force_passive: true,
                force_clickable: true,
            }),
        };
        let error =
            validate_statusline_flags(&command).expect_err("conflicting flags should be rejected");
        assert!(
            error
                .to_string()
                .contains("only one of --force-passive or --force-clickable")
        );
    }

    #[test]
    fn statusline_mode_requires_click_support_and_mouse() {
        let config = ResolvedConfig::default();
        assert_eq!(
            statusline_mode(
                &config,
                &TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 4,
                        patch: None,
                    },
                    supports_popup: true,
                    supports_multi_status_lines: true,
                    supports_status_mouse_ranges: true,
                    mouse_enabled: true,
                }
            ),
            StatusRenderMode::Clickable
        );
        assert_eq!(
            statusline_mode(
                &config,
                &TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 4,
                        patch: None,
                    },
                    supports_popup: true,
                    supports_multi_status_lines: true,
                    supports_status_mouse_ranges: true,
                    mouse_enabled: false,
                }
            ),
            StatusRenderMode::Passive
        );
    }

    #[test]
    fn statusline_install_uses_render_command_expression() {
        let command = statusline_command_expression(PathBuf::from("/tmp/wisp"));
        assert_eq!(command, "#('/tmp/wisp' 'statusline' 'render')");
    }

    #[test]
    fn installs_and_clears_statusline_refresh_hooks() {
        let tmux = StubTmuxClient::default();

        install_statusline_refresh_hooks(&tmux).expect("hooks should install");
        assert_eq!(
            tmux.hooks
                .borrow()
                .iter()
                .map(|(hook, _)| hook.as_str())
                .collect::<Vec<_>>(),
            STATUSLINE_REFRESH_HOOKS.to_vec()
        );
        assert!(
            tmux.hooks
                .borrow()
                .iter()
                .all(|(_, command)| command == STATUSLINE_REFRESH_COMMAND)
        );

        uninstall_statusline_refresh_hooks(&tmux).expect("hooks should uninstall");
        assert_eq!(
            tmux.cleared_hooks
                .borrow()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            STATUSLINE_REFRESH_HOOKS.to_vec()
        );
    }

    #[test]
    fn picker_bindings_follow_the_configured_actions() {
        let mut config = ResolvedConfig::default();
        config.actions.down = KeyAction::Close;
        config.actions.up = KeyAction::TogglePreview;
        config.actions.ctrl_j = KeyAction::ToggleSort;
        config.actions.ctrl_k = KeyAction::RenameSession;
        config.actions.enter = KeyAction::Open;
        config.actions.shift_enter = KeyAction::CreateSessionFromQuery;
        config.actions.backspace = KeyAction::Backspace;

        let bindings = picker_bindings(&config);

        assert_eq!(bindings.down, UiIntent::Close);
        assert_eq!(bindings.up, UiIntent::TogglePreview);
        assert_eq!(bindings.ctrl_j, UiIntent::ToggleSort);
        assert_eq!(bindings.ctrl_k, UiIntent::RenameSession);
        assert_eq!(bindings.enter, UiIntent::ActivateSelected);
        assert_eq!(bindings.shift_enter, UiIntent::CreateSessionFromQuery);
        assert_eq!(bindings.backspace, UiIntent::Backspace);
    }

    #[test]
    fn creates_session_from_query_using_zoxide_match() {
        let tmux = StubTmuxClient::default();
        let zoxide =
            StubZoxideProvider::default().with_match("demo app", Path::new("/tmp/demo-app"));

        assert!(
            create_session_from_query(&tmux, &zoxide, "  demo app  ", Path::new("/fallback"))
                .expect("query create should succeed")
        );
        assert_eq!(
            tmux.created_sessions.borrow().as_slice(),
            [("demo app".to_string(), PathBuf::from("/tmp/demo-app"))]
        );
    }

    #[test]
    fn create_session_from_query_falls_back_when_zoxide_has_no_match() {
        let tmux = StubTmuxClient::default();
        let zoxide = StubZoxideProvider::default();

        assert!(
            create_session_from_query(&tmux, &zoxide, "scratch", Path::new("/fallback"))
                .expect("query create should succeed")
        );
        assert_eq!(
            tmux.created_sessions.borrow().as_slice(),
            [("scratch".to_string(), PathBuf::from("/fallback"))]
        );
    }

    #[test]
    fn activate_filter_selection_switches_or_creates_as_needed() {
        let tmux = StubTmuxClient::default();
        let zoxide = StubZoxideProvider::default().with_match("new session", Path::new("/tmp/new"));
        let kindra = StubKindraProvider::default();
        let filtered = vec![session_item("alpha")];

        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                0,
                "ignored",
                Path::new("/fallback"),
                false,
            )
            .expect("existing selection should activate")
        );
        assert_eq!(tmux.switched_sessions.borrow().as_slice(), ["alpha"]);

        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                0,
                "new session",
                Path::new("/fallback"),
                true,
            )
            .expect("forced create should succeed")
        );
        assert_eq!(
            tmux.created_sessions.borrow().as_slice(),
            [("new session".to_string(), PathBuf::from("/tmp/new"))]
        );

        let no_matches = Vec::new();
        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &no_matches,
                0,
                "new session",
                Path::new("/fallback"),
                false,
            )
            .expect("empty filter should create from query")
        );
        assert_eq!(tmux.created_sessions.borrow().len(), 2);
    }

    #[test]
    fn activate_filter_selection_creates_session_for_worktree_rows() {
        let tmux = StubTmuxClient::default();
        let zoxide = StubZoxideProvider::default();
        let kindra = StubKindraProvider::default();
        let filtered = vec![wisp_core::SessionListItem {
            session_id: "worktree:/tmp/project".to_string(),
            label: "project".to_string(),
            kind: wisp_core::SessionListItemKind::Worktree,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: Some("/tmp/project".to_string()),
            command_hint: None,
            git_branch: None,
            worktree_path: Some(PathBuf::from("/tmp/project")),
            worktree_branch: Some("feature/demo".to_string()),
        }];

        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                0,
                "",
                Path::new("/fallback"),
                false,
            )
            .expect("worktree selection should create or switch")
        );
        assert!(tmux.switched_sessions.borrow().is_empty());
        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        // Uses basename directly since no collision with existing sessions
        assert_eq!(created[0].0, "project");
        assert_eq!(created[0].1, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn activate_filter_selection_creates_kindra_temp_worktree() {
        let worktree_path = PathBuf::from("/tmp/repo/.git/kindra-worktrees/temp/my-feature");
        let tmux = StubTmuxClient::default();
        let zoxide = StubZoxideProvider::default();
        let kindra = StubKindraProvider::configured_with(&worktree_path);
        let filtered = vec![wisp_core::SessionListItem {
            session_id: "kindra-temp:my-feature".to_string(),
            label: "my-feature".to_string(),
            kind: wisp_core::SessionListItemKind::CreateTempWorktree,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: Some("new temp worktree from main".to_string()),
            command_hint: None,
            git_branch: None,
            // repo root to run `kin` in, plus the branch name from the query.
            worktree_path: Some(PathBuf::from("/tmp/repo")),
            worktree_branch: Some("my-feature".to_string()),
        }];

        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                0,
                "my-feature",
                Path::new("/fallback"),
                false,
            )
            .expect("temp worktree selection should create the worktree and session")
        );

        // The worktree is created off the resolved trunk in the repo root.
        let created_worktrees = kindra.created.borrow();
        assert_eq!(created_worktrees.len(), 1);
        assert_eq!(created_worktrees[0].0, PathBuf::from("/tmp/repo"));
        assert_eq!(created_worktrees[0].1, "my-feature");
        assert_eq!(created_worktrees[0].2, "main");

        // A session is created in the freshly created worktree path.
        assert!(tmux.switched_sessions.borrow().is_empty());
        let created_sessions = tmux.created_sessions.borrow();
        assert_eq!(created_sessions.len(), 1);
        assert_eq!(created_sessions[0].0, "my-feature");
        assert_eq!(created_sessions[0].1, worktree_path);
    }

    #[test]
    fn kindra_temp_branch_name_normalizes_and_rejects_invalid_names() {
        use super::kindra_temp_branch_name;

        // Whitespace collapses to dashes; slashes are preserved.
        assert_eq!(
            kindra_temp_branch_name("  my  feature  ").as_deref(),
            Some("my-feature")
        );
        assert_eq!(
            kindra_temp_branch_name("feature/foo").as_deref(),
            Some("feature/foo")
        );

        // Empty or whitespace-only queries yield no row.
        assert_eq!(kindra_temp_branch_name("   "), None);

        // Names git would reject must not reach the temp-worktree flow.
        assert_eq!(kindra_temp_branch_name("fix: crash"), None);
        assert_eq!(kindra_temp_branch_name("foo..bar"), None);
        assert_eq!(kindra_temp_branch_name("foo.lock"), None);
        assert_eq!(kindra_temp_branch_name("~weird^"), None);
    }

    #[test]
    fn activate_filter_selection_creates_session_for_zoxide_row() {
        let tmux = StubTmuxClient::default();
        let zoxide = StubZoxideProvider::default();
        let kindra = StubKindraProvider::default();
        let filtered = vec![wisp_core::SessionListItem {
            session_id: "zoxide:/path/to/myproject".to_string(),
            label: "myproject".to_string(),
            kind: wisp_core::SessionListItemKind::Zoxide,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: Some("/path/to/myproject".to_string()),
            command_hint: None,
            git_branch: None,
            worktree_path: Some(PathBuf::from("/path/to/myproject")),
            worktree_branch: None,
        }];

        // Test with selected=0 (zoxide is first item)
        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                0,
                "myproject",
                Path::new("/fallback"),
                false,
            )
            .expect("zoxide selection should create or switch")
        );
        assert!(tmux.switched_sessions.borrow().is_empty());
        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        // Should use the query "myproject" as the session name
        assert_eq!(created[0].0, "myproject");
        assert_eq!(created[0].1, PathBuf::from("/path/to/myproject"));
    }

    #[test]
    fn activate_filter_selection_zoxide_row_at_end_of_list() {
        let tmux = StubTmuxClient::default();
        let zoxide = StubZoxideProvider::default();
        let kindra = StubKindraProvider::default();
        // Simulate filtered list with existing sessions PLUS zoxide at the end
        let filtered = vec![
            wisp_core::SessionListItem {
                session_id: "session:alpha".to_string(),
                label: "alpha".to_string(),
                kind: wisp_core::SessionListItemKind::Session,
                is_current: false,
                is_previous: false,
                last_activity: None,
                attached: false,
                attention: wisp_core::AttentionBadge::None,
                attention_count: 0,
                active_window_label: None,
                path_hint: None,
                command_hint: None,
                git_branch: None,
                worktree_path: None,
                worktree_branch: None,
            },
            wisp_core::SessionListItem {
                session_id: "zoxide:/path/to/mlm".to_string(),
                label: "mlm".to_string(),
                kind: wisp_core::SessionListItemKind::Zoxide,
                is_current: false,
                is_previous: false,
                last_activity: None,
                attached: false,
                attention: wisp_core::AttentionBadge::None,
                attention_count: 0,
                active_window_label: None,
                path_hint: Some("/path/to/mlm".to_string()),
                command_hint: None,
                git_branch: None,
                worktree_path: Some(PathBuf::from("/path/to/mlm")),
                worktree_branch: None,
            },
        ];

        // Selected is index 1 (the zoxide item at the end)
        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                1, // selected = 1, pointing to zoxide item
                "mlm",
                Path::new("/fallback"),
                false,
            )
            .expect("zoxide selection should create or switch")
        );
        assert!(tmux.switched_sessions.borrow().is_empty());
        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        // Should use the query "mlm" as the session name
        assert_eq!(created[0].0, "mlm");
        assert_eq!(created[0].1, PathBuf::from("/path/to/mlm"));
    }

    #[test]
    fn activate_filter_selection_zoxide_uses_hash_on_collision() {
        let tmux = StubTmuxClient::default().with_existing_sessions(vec![TmuxSession {
            id: "$1".to_string(),
            name: "myproject".to_string(),
            attached: false,
            windows: 1,
            current: false,
            last_activity: None,
        }]);
        let zoxide = StubZoxideProvider::default();
        let kindra = StubKindraProvider::default();
        let filtered = vec![wisp_core::SessionListItem {
            session_id: "zoxide:/path/to/myproject".to_string(),
            label: "myproject".to_string(),
            kind: wisp_core::SessionListItemKind::Zoxide,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: Some("/path/to/myproject".to_string()),
            command_hint: None,
            git_branch: None,
            worktree_path: Some(PathBuf::from("/path/to/myproject")),
            worktree_branch: None,
        }];

        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
                &kindra,
                &filtered,
                0,
                "myproject",
                Path::new("/fallback"),
                false,
            )
            .expect("zoxide selection should create or switch")
        );
        assert!(tmux.switched_sessions.borrow().is_empty());
        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        // Should use hash suffix due to collision with existing session
        assert!(created[0].0.starts_with("myproject-"));
    }

    #[test]
    fn create_session_with_basename_avoids_collision() {
        let tmux = StubTmuxClient::default().with_existing_sessions(vec![TmuxSession {
            id: "$1".to_string(),
            name: "project".to_string(),
            attached: false,
            windows: 1,
            current: false,
            last_activity: None,
        }]);

        crate::create_session_with_basename(&tmux, "project", Path::new("/tmp/repo-a/project"))
            .expect("worktree should create with hash suffix");

        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        // Should have hash suffix due to collision with existing session
        assert!(created[0].0.starts_with("project-"));
    }

    #[test]
    fn create_session_with_basename_uses_basename_when_no_collision() {
        let tmux = StubTmuxClient::default();

        crate::create_session_with_basename(&tmux, "myproject", Path::new("/tmp/repo/myproject"))
            .expect("worktree should create with basename");

        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        // Should use basename directly since no collision
        assert_eq!(created[0].0, "myproject");
    }

    #[test]
    fn create_session_with_basename_with_real_directory() {
        let temp_dir = std::env::temp_dir().join("wisp-test-create-session");
        fs::create_dir_all(&temp_dir).expect("should create temp dir");
        fs::write(temp_dir.join("test.txt"), "test").expect("should create file");

        let tmux = StubTmuxClient::default();
        crate::create_session_with_basename(&tmux, "testsession", &temp_dir)
            .expect("should work with real directory");

        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "testsession");
        assert_eq!(
            created[0].1,
            temp_dir.canonicalize().expect("canonical path")
        );

        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn generate_preview_for_zoxide_item_with_real_directory() {
        use wisp_preview::FilesystemPreviewProvider;

        let temp_dir = std::env::temp_dir().join("wisp-test-preview-zoxide");
        fs::create_dir_all(&temp_dir).expect("should create temp dir");
        fs::write(temp_dir.join("file1.txt"), "content1").expect("should create file");
        fs::write(temp_dir.join("file2.txt"), "content2").expect("should create file");

        let provider = FilesystemPreviewProvider::default();
        let item = wisp_core::SessionListItem {
            session_id: "zoxide:/path/to/test".to_string(),
            label: "test".to_string(),
            kind: wisp_core::SessionListItemKind::Zoxide,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: Some(temp_dir.to_string_lossy().to_string()),
            command_hint: None,
            git_branch: None,
            worktree_path: Some(temp_dir.clone()),
            worktree_branch: None,
        };

        let preview_lines = crate::generate_preview(&provider, &item);

        // Should list directory contents, not show "directory not found"
        assert!(!preview_lines.is_empty());
        assert!(
            !preview_lines
                .iter()
                .any(|line| line.contains("not found") || line.contains("no path"))
        );

        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn stable_path_hash_is_deterministic() {
        assert_eq!(
            crate::stable_path_hash(Path::new("/tmp/repo-a/project")),
            crate::stable_path_hash(Path::new("/tmp/repo-a/project"))
        );
        assert_ne!(
            crate::stable_path_hash(Path::new("/tmp/repo-a/project")),
            crate::stable_path_hash(Path::new("/tmp/repo-b/project"))
        );
    }

    #[test]
    fn applies_session_sort_modes_and_keeps_current_discoverable() {
        let mut items = vec![
            session_item_with_flags("beta", false, true, Some(2)),
            session_item_with_flags("alpha", true, false, Some(3)),
            session_item_with_flags("aardvark", false, false, Some(1)),
        ];

        apply_session_sort(&mut items, SessionSortMode::Alphabetical);
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["aardvark", "alpha", "beta"]
        );
        assert_eq!(current_session_id(&items), Some("alpha"));
        assert_eq!(
            selected_index_for_session(&items, "", Some("alpha")),
            Some(1)
        );
    }

    #[test]
    fn snapshot_client_id_defaults_tmux_to_default_client() {
        let snapshot = wisp_app::BackendSnapshot::Tmux(TmuxSnapshot {
            context: TmuxContext::default(),
            capabilities: TmuxCapabilities {
                version: TmuxVersion {
                    major: 3,
                    minor: 4,
                    patch: None,
                },
                supports_popup: true,
                supports_multi_status_lines: true,
                supports_status_mouse_ranges: true,
                mouse_enabled: true,
            },
            sessions: Vec::new(),
            windows: Vec::new(),
        });

        assert_eq!(
            crate::snapshot_client_id(&snapshot),
            crate::DEFAULT_CLIENT_ID
        );
    }

    #[cfg(feature = "embers")]
    #[test]
    fn snapshot_client_id_uses_embers_snapshot_client() {
        let snapshot = wisp_app::BackendSnapshot::Embers(wisp_embers::EmbersSnapshot {
            context: wisp_embers::EmbersContext {
                client_id: Some("client-42".to_string()),
                current_session_name: None,
                current_window_index: None,
                pane_id: None,
                previous_session_name: None,
            },
            sessions: Vec::new(),
            windows: Vec::new(),
            panes: Vec::new(),
        });

        assert_eq!(crate::snapshot_client_id(&snapshot), "client-42");
    }

    #[test]
    fn filter_items_matches_git_branch_names() {
        let mut item = session_item("alpha");
        item.git_branch = Some(wisp_core::GitBranchStatus {
            name: "feature/picker-branches".to_string(),
            sync: wisp_core::GitBranchSync::Unknown,
            dirty: false,
        });

        let filtered = filter_items(&[item], "picker-branches");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].session_id, "alpha");
    }

    #[test]
    fn session_items_for_picker_mode_returns_info_item_when_not_in_git_repo() {
        let items = crate::session_items_for_picker_mode(
            &wisp_core::DomainState::default(),
            Some(crate::DEFAULT_CLIENT_ID),
            wisp_core::PickerMode::Worktree,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, wisp_core::SessionListItemKind::Info);
        assert_eq!(items[0].label, "not in a git repository");
    }

    fn session_item(session_id: &str) -> wisp_core::SessionListItem {
        session_item_with_flags(session_id, false, false, None)
    }

    fn session_item_with_flags(
        session_id: &str,
        is_current: bool,
        is_previous: bool,
        last_activity: Option<u64>,
    ) -> wisp_core::SessionListItem {
        wisp_core::SessionListItem {
            session_id: session_id.to_string(),
            label: session_id.to_string(),
            kind: wisp_core::SessionListItemKind::Session,
            is_current,
            is_previous,
            last_activity,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: None,
            command_hint: None,
            git_branch: None,
            worktree_path: None,
            worktree_branch: None,
        }
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    }
}
