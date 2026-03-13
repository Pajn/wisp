use std::{
    collections::BTreeMap,
    collections::VecDeque,
    env,
    error::Error,
    fs,
    io::stdout,
    path::{Path, PathBuf},
    process::Command,
    process::ExitCode,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::widgets::Clear;
use ratatui::{Terminal, backend::CrosstermBackend};
use wisp_app::{CandidateSources, build_domain_state};
use wisp_config::{CliOverrides, KeyAction, LoadOptions, ResolvedConfig, load_config};
use wisp_core::{
    DomainState, GitBranchStatus, GitBranchSync, PreviewKey, PreviewRequest, SessionListItem,
    derive_session_list, derive_status_items,
};
use wisp_fuzzy::{MatchItem, Matcher, SimpleMatcher};
use wisp_preview::{ActivePanePreviewProvider, PreviewProvider, SessionDetailsPreviewProvider};
use wisp_status::{StatusFormatOptions, StatusRenderState};
use wisp_tmux::{
    CommandTmuxClient, PollingTmuxBackend, PopupCommand, PopupOptions, PopupSpec, SidebarPaneSpec,
    SidebarSide, TmuxBackend, TmuxClient, TmuxError,
};
use wisp_ui::{KeyBindings, SurfaceKind, SurfaceModel, UiIntent, render_surface, translate_key};
use wisp_zoxide::{CommandZoxideProvider, ZoxideProvider};

const PREVIEW_REFRESH_DEBOUNCE: Duration = Duration::from_millis(400);
const SIDEBAR_PANE_TITLE: &str = "Wisp Sidebar";
const SIDEBAR_PANE_WIDTH: u16 = 36;

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
    let command = env::args().nth(1).unwrap_or_else(|| "popup".to_string());
    let config = load_config(&LoadOptions {
        cli_overrides: CliOverrides::default(),
        ..LoadOptions::default()
    })?;

    match command.as_str() {
        "print-config" => {
            println!("{config:#?}");
            Ok(())
        }
        "doctor" => {
            doctor();
            Ok(())
        }
        "status-line" => update_status_line(),
        "fullscreen" => run_surface(SurfaceKind::Picker, &config),
        "popup" => open_popup_or_run_inline(SurfaceKind::Picker, &config),
        "sidebar-popup" => open_sidebar_popup_or_run_inline(&config),
        "sidebar-pane" => open_sidebar_pane(),
        "ui" => {
            let inline = env::args().nth(2).unwrap_or_else(|| "picker".to_string());
            let kind = match inline.as_str() {
                "sidebar-compact" => SurfaceKind::SidebarCompact,
                "sidebar-expanded" => SurfaceKind::SidebarExpanded,
                _ => SurfaceKind::Picker,
            };
            run_surface(kind, &config)
        }
        _ => run_surface(SurfaceKind::Picker, &config),
    }
}

fn doctor() {
    let tmux = CommandTmuxClient::new();
    let zoxide = CommandZoxideProvider::new();

    println!("wisp doctor");
    println!();
    match tmux.capabilities() {
        Ok(capabilities) => {
            println!(
                "tmux: {}.{} (popup: {})",
                capabilities.version.major, capabilities.version.minor, capabilities.supports_popup
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

fn update_status_line() -> Result<(), Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let state = load_domain_state()?;
    let items = derive_status_items(&state, Some("default"));
    let mut render_state = StatusRenderState::default();
    if let Some(line) = render_state.next_update(&items, &StatusFormatOptions::default()) {
        backend.update_status_line(2, &line)?;
        println!("{line}");
    }
    Ok(())
}

fn open_popup_or_run_inline(
    kind: SurfaceKind,
    config: &ResolvedConfig,
) -> Result<(), Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let command = PopupCommand {
        program: env::current_exe()?,
        args: vec!["ui".to_string(), "picker".to_string()],
    };
    match backend.open_popup(&PopupSpec {
        command,
        options: PopupOptions::default(),
    }) {
        Ok(()) => Ok(()),
        Err(TmuxError::PopupUnavailable { .. }) | Err(TmuxError::CommandFailed { .. }) => {
            run_surface(kind, config)
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
            run_surface(SurfaceKind::SidebarCompact, config)
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

fn run_surface(kind: SurfaceKind, config: &ResolvedConfig) -> Result<(), Box<dyn Error>> {
    let state = load_domain_state()?;
    let mut session_items = derive_session_list(&state, Some("default"));
    let mut pane_preview_provider = ActivePanePreviewProvider::new(CommandTmuxClient::new());
    let mut details_preview_provider = SessionDetailsPreviewProvider {
        state: state.clone(),
    };
    let tmux = CommandTmuxClient::new();
    let sidebar_command = sidebar_surface_command(env::current_exe()?);
    let mut sidebar_runtime = sidebar_runtime(&tmux, kind)?;
    let saved_sidebar_state = match &sidebar_runtime {
        Some(runtime) => load_sidebar_ui_state(&runtime.session_name)?,
        None => None,
    };
    let mut query = saved_sidebar_state
        .as_ref()
        .map(|state| state.query.clone())
        .unwrap_or_default();
    let mut selected = saved_sidebar_state
        .as_ref()
        .and_then(|state| {
            let filtered = filter_items(&session_items, &query);
            state.selected_session_id.as_ref().and_then(|session_id| {
                filtered
                    .iter()
                    .position(|item| item.session_id == *session_id)
            })
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
    let mut first_frame = true;
    let mut pending_branch_names = git_work_items(&state);
    let branch_status_updates =
        spawn_git_status_workers(pending_branch_names.iter().cloned().collect());
    let mut deferred_branch_status = BTreeMap::new();

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = loop {
        pane_preview_provider.max_lines = preview_line_budget(&terminal, show_help)?;

        if let Some(runtime) = &sidebar_runtime
            && sidebar_requires_handoff(&tmux, runtime)?
        {
            persist_sidebar_ui_state(runtime, &session_items, &query, surface_kind, selected)?;
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
            if let Some(branch_name) = branch_name_for_directory(&work_item.path) {
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
                UiIntent::ActivateSelected => match &input_mode {
                    InputMode::Filter => {
                        if let Some(item) = filtered.get(selected) {
                            if let Some(runtime) = &sidebar_runtime {
                                persist_sidebar_ui_state(
                                    runtime,
                                    &session_items,
                                    &query,
                                    surface_kind,
                                    selected,
                                )?;
                            }
                            tmux.switch_or_attach_session(&item.session_id)?;
                            if let Some(runtime) = &sidebar_runtime {
                                reconcile_sidebar_for_current_context(
                                    &tmux,
                                    &sidebar_command,
                                    runtime.pane_id.as_deref(),
                                )?;
                            }
                        }
                        break Ok(());
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
                        session_items = derive_session_list(&reloaded_state, Some("default"));
                        details_preview_provider.state = reloaded_state.clone();
                        pending_branch_names = git_work_items(&reloaded_state);
                        deferred_branch_status.clear();
                        query = filter_query.clone();
                        input_mode = InputMode::Filter;
                        if let Some(index) = session_items
                            .iter()
                            .position(|item| item.session_id == new_name)
                        {
                            selected = index;
                        }
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
                    {
                        let session_id = item.session_id.clone();
                        tmux.kill_session(&session_id)?;
                        session_items.retain(|session| session.session_id != session_id);
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
            }

            if let Some(runtime) = &sidebar_runtime
                && matches!(input_mode, InputMode::Filter)
            {
                persist_sidebar_ui_state(runtime, &session_items, &query, surface_kind, selected)?;
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
        enter: ui_intent_for_action(config.actions.enter),
        ctrl_r: ui_intent_for_action(config.actions.ctrl_r),
        ctrl_x: ui_intent_for_action(config.actions.ctrl_x),
        ctrl_p: ui_intent_for_action(config.actions.ctrl_p),
        ctrl_d: ui_intent_for_action(config.actions.ctrl_d),
        ctrl_m: ui_intent_for_action(config.actions.ctrl_m),
        esc: ui_intent_for_action(config.actions.esc),
        ctrl_c: ui_intent_for_action(config.actions.ctrl_c),
    }
}

fn ui_intent_for_action(action: KeyAction) -> UiIntent {
    match action {
        KeyAction::Open => UiIntent::ActivateSelected,
        KeyAction::RenameSession => UiIntent::RenameSession,
        KeyAction::CloseSession => UiIntent::CloseSession,
        KeyAction::TogglePreview => UiIntent::TogglePreview,
        KeyAction::ToggleDetails => UiIntent::ToggleDetails,
        KeyAction::ToggleCompactSidebar => UiIntent::ToggleCompactSidebar,
        KeyAction::Close => UiIntent::Close,
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
    provider
        .generate(&PreviewRequest::SessionSummary {
            key: PreviewKey::Session(item.session_id.clone()),
            session_name: item.session_id.clone(),
        })
        .map(|content| content.body)
        .unwrap_or_else(|error| vec![error.to_string()])
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

    let mut parts = raw.splitn(3, '\n');
    let kind = match parts.next().unwrap_or_default().trim() {
        "compact" => SurfaceKind::SidebarCompact,
        "expanded" => SurfaceKind::SidebarExpanded,
        value => {
            return Err(format!("invalid sidebar state kind `{value}`").into());
        }
    };
    let selected_session_id = match parts.next() {
        Some(value) if !value.trim().is_empty() => Some(value.to_string()),
        _ => None,
    };
    let query = parts.next().unwrap_or_default().to_string();

    Ok(Some(SidebarUiState {
        query,
        selected_session_id,
        kind,
    }))
}

fn persist_sidebar_ui_state(
    runtime: &SidebarRuntime,
    session_items: &[SessionListItem],
    query: &str,
    kind: SurfaceKind,
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
        "{kind_token}\n{}\n{}",
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

                if let Some((sync, dirty)) = branch_status_for_directory(&work_item.path) {
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

fn branch_status_for_directory(path: &Path) -> Option<(GitBranchSync, bool)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain=2", "--branch"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut upstream = None;
    let mut ahead = 0usize;
    let mut dirty = false;

    for line in stdout.lines() {
        if let Some(remote) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(remote.to_string());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut parts = ab.split_whitespace();
            let ahead_raw = parts.next().and_then(|part| part.strip_prefix('+'));
            ahead = ahead_raw
                .and_then(|part| part.parse::<usize>().ok())
                .unwrap_or(0);
        } else if !line.starts_with("# ") && !line.is_empty() {
            dirty = true;
        }
    }

    let sync = if upstream.is_none() || ahead > 0 {
        GitBranchSync::NotPushed
    } else {
        GitBranchSync::Pushed
    };

    Some((sync, dirty))
}

fn branch_name_for_directory(path: &Path) -> Option<String> {
    path.ancestors().find_map(branch_name_for_git_root)
}

fn branch_name_for_git_root(path: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(path)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();

    if let Some(reference) = head.strip_prefix("ref: ") {
        return reference.rsplit('/').next().map(ToOwned::to_owned);
    }

    Some(head.chars().take(7).collect())
}

fn resolve_git_dir(path: &Path) -> Option<PathBuf> {
    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if !dot_git.is_file() {
        return None;
    }

    let pointer = fs::read_to_string(&dot_git).ok()?;
    let target = pointer.trim().strip_prefix("gitdir: ")?;
    let git_dir = Path::new(target);
    if git_dir.is_absolute() {
        Some(git_dir.to_path_buf())
    } else {
        Some(path.join(git_dir))
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
                search_text: format!(
                    "{} {} {}",
                    item.label,
                    item.active_window_label.clone().unwrap_or_default(),
                    item.command_hint.clone().unwrap_or_default()
                ),
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
        cell::RefCell,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use wisp_tmux::{
        PopupCommand, PopupOptions, SidebarPaneSpec, TmuxCapabilities, TmuxClient, TmuxContext,
        TmuxError, TmuxPane, TmuxSession, TmuxSnapshot, TmuxVersion, TmuxWindow,
    };

    use crate::{
        SIDEBAR_PANE_TITLE, SIDEBAR_PANE_WIDTH, SidebarRuntime, SurfaceKind,
        clear_sidebar_ui_state, disable_sidebar_for_session, load_sidebar_ui_state,
        persist_sidebar_ui_state, reconcile_sidebar_for_current_context, sidebar_requires_handoff,
        sidebar_surface_command,
    };

    #[derive(Default)]
    struct StubTmuxClient {
        context: TmuxContext,
        windows: Vec<TmuxWindow>,
        panes: Vec<TmuxPane>,
        opened_targets: RefCell<Vec<String>>,
        closed_panes: RefCell<Vec<String>>,
        selected_panes: RefCell<Vec<String>>,
        resized_panes: RefCell<Vec<(String, u16)>>,
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

        fn switch_or_attach_session(&self, _session_name: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn rename_session(&self, _session_name: &str, _new_name: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn kill_session(&self, _session_name: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
        }

        fn create_or_switch_session(
            &self,
            _session_name: &str,
            _directory: &Path,
        ) -> Result<(), TmuxError> {
            unreachable!("not used in test");
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

        fn update_status_line(&self, _line: usize, _content: &str) -> Result<(), TmuxError> {
            unreachable!("not used in test");
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
        persist_sidebar_ui_state(&runtime, &items, "be", SurfaceKind::SidebarExpanded, 0)
            .expect("state should persist");

        let state = load_sidebar_ui_state(&session_name)
            .expect("state should load")
            .expect("state should exist");
        assert_eq!(state.query, "be");
        assert_eq!(state.selected_session_id.as_deref(), Some("beta"));
        assert_eq!(state.kind, SurfaceKind::SidebarExpanded);

        clear_sidebar_ui_state(&session_name).expect("state should clear");
        assert!(
            load_sidebar_ui_state(&session_name)
                .expect("state should load")
                .is_none()
        );
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

    fn session_item(session_id: &str) -> wisp_core::SessionListItem {
        wisp_core::SessionListItem {
            session_id: session_id.to_string(),
            label: session_id.to_string(),
            is_current: false,
            is_previous: false,
            attached: false,
            attention: wisp_core::AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: None,
            command_hint: None,
            git_branch: None,
        }
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    }
}
