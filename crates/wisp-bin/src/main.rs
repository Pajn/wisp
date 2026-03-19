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
use wisp_app::{CandidateSources, build_domain_state};
use wisp_config::{
    CliOverrides, KeyAction, LoadOptions, ResolvedConfig, SessionSortMode, load_config,
};
use wisp_core::{
    DomainState, GitBranchStatus, GitBranchSync, PickerMode, PreviewKey, PreviewRequest,
    SessionListItem, SessionListSortMode, derive_session_list, derive_session_list_with_worktrees,
    derive_status_items, sanitize_session_name, sort_session_list_items,
};
use wisp_fuzzy::{MatchItem, Matcher, SimpleMatcher};
use wisp_preview::{ActivePanePreviewProvider, PreviewProvider, SessionDetailsPreviewProvider};
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
const STATUSLINE_REFRESH_HOOKS: &[&str] = &[
    "client-session-changed[200]",
    "session-created[200]",
    "session-closed[200]",
    "session-renamed[200]",
];
const STATUSLINE_REFRESH_COMMAND: &str = "refresh-client -S";

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
    home_window_index: u32,
    pane_id: Option<String>,
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
            run_surface(mode.surface_kind(), &config, mode.picker_mode())
        }
        ParsedCli::Public(cli) => match cli.command {
            Command::Doctor(_) => {
                doctor();
                Ok(())
            }
            Command::PrintConfig(_) => {
                let config = load_runtime_config()?;
                println!("{config:#?}");
                Ok(())
            }
            Command::Fullscreen(fullscreen_cmd) => {
                let config = load_runtime_config()?;
                let mode = if fullscreen_cmd.worktree {
                    PickerMode::Worktree
                } else {
                    PickerMode::AllSessions
                };
                run_surface(SurfaceKind::Picker, &config, mode)
            }
            Command::Popup(popup_cmd) => {
                let config = load_runtime_config()?;
                let mode = if popup_cmd.worktree {
                    PickerMode::Worktree
                } else {
                    PickerMode::AllSessions
                };
                open_popup_or_run_inline(SurfaceKind::Picker, &config, mode)
            }
            Command::SidebarPopup(_) => {
                let config = load_runtime_config()?;
                open_sidebar_popup_or_run_inline(&config)
            }
            Command::SidebarPane(_) => open_sidebar_pane(),
            Command::Statusline(statusline) => {
                validate_statusline_flags(&statusline)?;
                let config = load_runtime_config()?;
                run_statusline_group(&config, statusline)
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

fn doctor() {
    let tmux = CommandTmuxClient::new();
    let zoxide = CommandZoxideProvider::new();

    println!("wisp doctor");
    println!();
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

    match zoxide.load_entries(5) {
        Ok(entries) => println!("zoxide: available ({} sample entries)", entries.len()),
        Err(error) => println!("zoxide: unavailable ({error})"),
    }

    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    println!("event strategy: {:?}", backend.event_strategy());
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
) -> Result<(), Box<dyn Error>> {
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
    let state = load_domain_state()?;
    let items = derive_status_items(&state, Some("default"));
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

fn create_session_from_query(
    tmux: &impl TmuxClient,
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
    tmux.create_or_switch_session(session_name, &directory)?;
    Ok(true)
}

fn create_session_from_worktree_path(
    tmux: &impl TmuxClient,
    worktree_path: &Path,
) -> Result<bool, Box<dyn Error>> {
    let normalized_path = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let session_name = format!(
        "{}-{:08x}",
        sanitize_session_name(&normalized_path),
        stable_path_hash(&normalized_path) as u32
    );
    tmux.create_or_switch_session(&session_name, worktree_path)?;
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

fn activate_filter_selection(
    tmux: &impl TmuxClient,
    zoxide: &impl ZoxideProvider,
    filtered: &[SessionListItem],
    selected: usize,
    query: &str,
    fallback_directory: &Path,
    force_create_from_query: bool,
) -> Result<bool, Box<dyn Error>> {
    if force_create_from_query || filtered.get(selected).is_none() {
        return create_session_from_query(tmux, zoxide, query, fallback_directory);
    }

    if let Some(item) = filtered.get(selected) {
        match item.kind {
            wisp_core::SessionListItemKind::Info => return Ok(false),
            wisp_core::SessionListItemKind::Worktree => {
                if let Some(worktree_path) = &item.worktree_path {
                    return create_session_from_worktree_path(tmux, worktree_path);
                }
                return Ok(false);
            }
            _ => {}
        }

        tmux.switch_or_attach_session(&item.session_id)?;
        return Ok(true);
    }

    Ok(false)
}

fn open_popup_or_run_inline(
    kind: SurfaceKind,
    config: &ResolvedConfig,
    mode: PickerMode,
) -> Result<(), Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
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
    match backend.open_popup(&PopupSpec {
        command,
        options: PopupOptions::default(),
    }) {
        Ok(()) => Ok(()),
        Err(TmuxError::PopupUnavailable { .. }) | Err(TmuxError::CommandFailed { .. }) => {
            run_surface(kind, config, mode)
        }
        Err(error) => Err(Box::new(error)),
    }
}

fn open_sidebar_popup_or_run_inline(config: &ResolvedConfig) -> Result<(), Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let command = PopupCommand {
        program: env::current_exe()?,
        args: vec!["ui".to_string(), "sidebar-compact".to_string()],
    };
    match backend.open_popup(&PopupSpec {
        command,
        options: PopupOptions {
            width: wisp_tmux::PopupDimension::Percent(35),
            height: wisp_tmux::PopupDimension::Percent(85),
            title: Some("Wisp Sidebar".to_string()),
        },
    }) {
        Ok(()) => Ok(()),
        Err(TmuxError::PopupUnavailable { .. }) | Err(TmuxError::CommandFailed { .. }) => {
            run_surface(SurfaceKind::SidebarCompact, config, PickerMode::AllSessions)
        }
        Err(error) => Err(Box::new(error)),
    }
}

fn open_sidebar_pane() -> Result<(), Box<dyn Error>> {
    let tmux = CommandTmuxClient::new();
    reconcile_sidebar_for_current_context(
        &tmux,
        &sidebar_surface_command(env::current_exe()?),
        None,
    )?;
    Ok(())
}

fn run_surface(
    kind: SurfaceKind,
    config: &ResolvedConfig,
    mode: PickerMode,
) -> Result<(), Box<dyn Error>> {
    let state = load_domain_state()?;
    let mut session_sort = config.ui.session_sort;
    let (mut session_items, mut pending_branch_names, mut branch_status_updates) =
        rebuild_session_items_for_picker_mode(&state, Some(DEFAULT_CLIENT_ID), mode, session_sort);
    let mut pane_preview_provider = ActivePanePreviewProvider::new(CommandTmuxClient::new());
    let mut details_preview_provider = SessionDetailsPreviewProvider {
        state: state.clone(),
    };
    let tmux = CommandTmuxClient::new();
    let zoxide = CommandZoxideProvider::new();
    let current_directory = env::current_dir()?;
    let sidebar_command = sidebar_surface_command(env::current_exe()?);
    let mut sidebar_runtime = sidebar_runtime(&tmux, kind)?;
    let saved_sidebar_state = match &sidebar_runtime {
        Some(runtime) => load_sidebar_ui_state(&runtime.session_name)?,
        None => None,
    };
    if let Some(saved_state) = &saved_sidebar_state
        && let Some(saved_sort_mode) = saved_state.sort_mode
    {
        session_sort = saved_sort_mode;
        (session_items, pending_branch_names, branch_status_updates) =
            rebuild_session_items_for_picker_mode(
                &state,
                Some(DEFAULT_CLIENT_ID),
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

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = loop {
        pane_preview_provider.max_lines = preview_line_budget(&terminal, show_help)?;

        if let Some(runtime) = &sidebar_runtime
            && sidebar_requires_handoff(&tmux, runtime)?
        {
            persist_sidebar_ui_state(
                runtime,
                &session_items,
                &query,
                surface_kind,
                session_sort,
                selected,
            )?;
            reconcile_sidebar_for_current_context(
                &tmux,
                &sidebar_command,
                runtime.pane_id.as_deref(),
            )?;
            break Ok(());
        }

        let filtered = match input_mode {
            InputMode::Filter => filter_items(&session_items, &query),
            InputMode::Rename { .. } => session_items.clone(),
        };
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
            preview = Some(generate_preview(
                match preview_mode {
                    PreviewMode::Pane => &pane_preview_provider as &dyn PreviewProvider,
                    PreviewMode::Details => &details_preview_provider as &dyn PreviewProvider,
                },
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
                        let activated = activate_filter_selection(
                            &tmux,
                            &zoxide,
                            &filtered,
                            selected,
                            &query,
                            &current_directory,
                            matches!(activate_intent, UiIntent::CreateSessionFromQuery),
                        )?;
                        if activated {
                            if let Some(runtime) = &sidebar_runtime {
                                reconcile_sidebar_for_current_context(
                                    &tmux,
                                    &sidebar_command,
                                    runtime.pane_id.as_deref(),
                                )?;
                            }
                            break Ok(());
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

                        tmux.rename_session(&session_id, &new_name)?;
                        let reloaded_state = load_domain_state()?;
                        (session_items, pending_branch_names, branch_status_updates) =
                            rebuild_session_items_for_picker_mode(
                                &reloaded_state,
                                Some(DEFAULT_CLIENT_ID),
                                picker_mode,
                                session_sort,
                            );
                        details_preview_provider.state = reloaded_state.clone();
                        deferred_branch_status.clear();
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
                            && runtime.session_name == session_id
                        {
                            clear_sidebar_ui_state(&runtime.session_name)?;
                            runtime.session_name = new_name;
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
                        tmux.kill_session(&session_id)?;
                        let reloaded_state = load_domain_state()?;
                        (session_items, pending_branch_names, branch_status_updates) =
                            rebuild_session_items_for_picker_mode(
                                &reloaded_state,
                                Some(DEFAULT_CLIENT_ID),
                                picker_mode,
                                session_sort,
                            );
                        details_preview_provider.state = reloaded_state.clone();
                        deferred_branch_status.clear();
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
                            disable_sidebar_for_session(
                                &tmux,
                                &runtime.session_name,
                                runtime.pane_id.as_deref(),
                            )?;
                            clear_sidebar_ui_state(&runtime.session_name)?;
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

                        let reloaded_state = load_domain_state()?;
                        (session_items, pending_branch_names, branch_status_updates) =
                            rebuild_session_items_for_picker_mode(
                                &reloaded_state,
                                Some(DEFAULT_CLIENT_ID),
                                picker_mode,
                                session_sort,
                            );
                        details_preview_provider.state = reloaded_state.clone();
                        deferred_branch_status.clear();
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

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
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
                provider
                    .generate(&PreviewRequest::Directory {
                        key: PreviewKey::Directory(path.clone()),
                        path: path.clone(),
                    })
                    .map(|content| content.body)
                    .unwrap_or_else(|_| vec!["not an active session".to_string()])
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

fn sidebar_runtime(
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
        home_window_index,
        pane_id: env::var("TMUX_PANE").ok(),
    }))
}

fn sidebar_requires_handoff(
    tmux: &impl TmuxClient,
    runtime: &SidebarRuntime,
) -> Result<bool, TmuxError> {
    let context = tmux.current_context()?;
    Ok(
        context.session_name.as_deref() != Some(runtime.session_name.as_str())
            || context.window_index != Some(runtime.home_window_index),
    )
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
    let path = sidebar_state_path(&runtime.session_name);
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

fn load_domain_state() -> Result<DomainState, Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let tmux = backend.snapshot()?;
    let zoxide = CommandZoxideProvider::new()
        .load_entries(500)
        .unwrap_or_default();
    Ok(build_domain_state(&CandidateSources { tmux, zoxide }))
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

    for _ in 0..worker_count {
        let sender = sender.clone();
        let queue = Arc::clone(&queue);
        thread::spawn(move || {
            loop {
                let Some(work_item) = queue.lock().ok().and_then(|mut queue| queue.pop_front())
                else {
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
    use wisp_status::StatusRenderMode;
    use wisp_tmux::{
        PopupCommand, PopupOptions, SidebarPaneSpec, TmuxCapabilities, TmuxClient, TmuxContext,
        TmuxError, TmuxPane, TmuxSession, TmuxSnapshot, TmuxVersion, TmuxWindow,
    };
    use wisp_ui::UiIntent;
    use wisp_zoxide::{DirectoryEntry, ZoxideError, ZoxideProvider};

    use crate::{
        SIDEBAR_PANE_TITLE, SIDEBAR_PANE_WIDTH, STATUSLINE_REFRESH_COMMAND,
        STATUSLINE_REFRESH_HOOKS, SidebarRuntime, StatuslineGroupCommand, StatuslineRenderCommand,
        StatuslineSubcommand, SurfaceKind, activate_filter_selection, apply_session_sort,
        clear_sidebar_ui_state, create_session_from_query, current_session_id,
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
            Ok(Vec::new())
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
            home_window_index: 1,
            pane_id: Some("%1".to_string()),
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
            home_window_index: 1,
            pane_id: Some("%1".to_string()),
        };

        assert!(sidebar_requires_handoff(&tmux, &runtime).expect("handoff should evaluate"));
        assert!(
            !sidebar_requires_handoff(
                &StubTmuxClient::default().with_context(TmuxContext {
                    session_name: Some("alpha".to_string()),
                    window_index: Some(1),
                    ..TmuxContext::default()
                }),
                &runtime,
            )
            .expect("handoff should evaluate")
        );
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
        let filtered = vec![session_item("alpha")];

        assert!(
            activate_filter_selection(
                &tmux,
                &zoxide,
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
        assert!(created[0].0.starts_with("project-"));
        assert_eq!(created[0].1, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn create_session_from_worktree_path_uses_unique_names_for_same_leaf_directories() {
        let tmux = StubTmuxClient::default();

        crate::create_session_from_worktree_path(&tmux, Path::new("/tmp/repo-a/project"))
            .expect("first worktree should create");
        crate::create_session_from_worktree_path(&tmux, Path::new("/var/repo-b/project"))
            .expect("second worktree should create");

        let created = tmux.created_sessions.borrow();
        assert_eq!(created.len(), 2);
        assert_ne!(created[0].0, created[1].0);
        assert!(created[0].0.starts_with("project-"));
        assert!(created[1].0.starts_with("project-"));
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
