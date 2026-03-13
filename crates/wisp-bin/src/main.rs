use std::{
    env,
    error::Error,
    io::stdout,
    path::Path,
    process::Command,
    process::ExitCode,
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
use wisp_config::{CliOverrides, LoadOptions, load_config};
use wisp_core::{
    DomainState, GitBranchStatus, PreviewKey, PreviewRequest, SessionListItem, derive_session_list,
    derive_status_items,
};
use wisp_fuzzy::{MatchItem, Matcher, SimpleMatcher};
use wisp_preview::{ActivePanePreviewProvider, PreviewProvider, SessionDetailsPreviewProvider};
use wisp_status::{StatusFormatOptions, StatusRenderState};
use wisp_tmux::{
    CommandTmuxClient, PollingTmuxBackend, PopupCommand, PopupOptions, PopupSpec, SidebarPaneSpec,
    SidebarSide, TmuxBackend, TmuxClient, TmuxError,
};
use wisp_ui::{SurfaceKind, SurfaceModel, UiIntent, render_surface, translate_key};
use wisp_zoxide::{CommandZoxideProvider, ZoxideProvider};

const PREVIEW_REFRESH_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewMode {
    Pane,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupStage {
    BareList,
    BranchesReady,
    Interactive,
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
        "fullscreen" => run_surface(SurfaceKind::Picker),
        "popup" => open_popup_or_run_inline(SurfaceKind::Picker),
        "sidebar-popup" => open_sidebar_popup_or_run_inline(),
        "sidebar-pane" => open_sidebar_pane(),
        "ui" => {
            let inline = env::args().nth(2).unwrap_or_else(|| "picker".to_string());
            let kind = match inline.as_str() {
                "sidebar-compact" => SurfaceKind::SidebarCompact,
                "sidebar-expanded" => SurfaceKind::SidebarExpanded,
                _ => SurfaceKind::Picker,
            };
            run_surface(kind)
        }
        _ => run_surface(SurfaceKind::Picker),
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

fn open_popup_or_run_inline(kind: SurfaceKind) -> Result<(), Box<dyn Error>> {
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
            run_surface(kind)
        }
        Err(error) => Err(Box::new(error)),
    }
}

fn open_sidebar_popup_or_run_inline() -> Result<(), Box<dyn Error>> {
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
            run_surface(SurfaceKind::SidebarCompact)
        }
        Err(error) => Err(Box::new(error)),
    }
}

fn open_sidebar_pane() -> Result<(), Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let snapshot = backend.snapshot()?;
    backend.open_sidebar_pane(&SidebarPaneSpec {
        target: snapshot.context.session_name.clone(),
        side: SidebarSide::Left,
        width: 36,
        command: PopupCommand {
            program: env::current_exe()?,
            args: vec!["ui".to_string(), "sidebar-compact".to_string()],
        },
    })?;
    Ok(())
}

fn run_surface(kind: SurfaceKind) -> Result<(), Box<dyn Error>> {
    let state = load_domain_state()?;
    let mut session_items = derive_session_list(&state, Some("default"));
    let mut pane_preview_provider = ActivePanePreviewProvider::new(CommandTmuxClient::new());
    let details_preview_provider = SessionDetailsPreviewProvider {
        state: state.clone(),
    };
    let tmux = CommandTmuxClient::new();
    let mut query = String::new();
    let mut selected = 0usize;
    let mut show_help = true;
    let mut preview_enabled = matches!(kind, SurfaceKind::Picker);
    let mut preview_mode = PreviewMode::Pane;
    let mut preview = preview_enabled.then_some(Vec::new());
    let mut preview_session_id = None;
    let mut preview_refreshed_at: Option<Instant> = None;
    let mut surface_kind = kind;
    let mut startup_stage = StartupStage::BareList;

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = loop {
        pane_preview_provider.max_lines = preview_line_budget(&terminal, show_help)?;

        let filtered = filter_items(&session_items, &query);
        if selected >= filtered.len() {
            selected = filtered.len().saturating_sub(1);
        }
        let selected_item = filtered.get(selected);
        let should_refresh_preview = startup_stage == StartupStage::Interactive
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
            title: match surface_kind {
                SurfaceKind::Picker => "Wisp Picker".to_string(),
                SurfaceKind::SidebarCompact => "Wisp Sidebar".to_string(),
                SurfaceKind::SidebarExpanded => "Wisp Sidebar+".to_string(),
            },
            query: query.clone(),
            items: filtered.clone(),
            selected,
            show_help,
            preview: preview.clone(),
            kind: surface_kind,
        };

        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Clear, area);
            render_surface(area, frame.buffer_mut(), &model);
        })?;

        match startup_stage {
            StartupStage::BareList => {
                enrich_session_list_items(&mut session_items, &state);
                startup_stage = StartupStage::BranchesReady;
                continue;
            }
            StartupStage::BranchesReady => {
                startup_stage = StartupStage::Interactive;
                preview_session_id = None;
                preview_refreshed_at = None;
                continue;
            }
            StartupStage::Interactive => {}
        }

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && let Some(intent) = translate_key(key)
        {
            match intent {
                UiIntent::SelectNext => {
                    if !filtered.is_empty() {
                        selected = (selected + 1).min(filtered.len() - 1);
                    }
                }
                UiIntent::SelectPrev => {
                    selected = selected.saturating_sub(1);
                }
                UiIntent::FilterChanged(fragment) => {
                    query.push_str(&fragment);
                    selected = 0;
                }
                UiIntent::Backspace => {
                    query.pop();
                    selected = 0;
                }
                UiIntent::ToggleCompactSidebar => {
                    surface_kind = match surface_kind {
                        SurfaceKind::SidebarCompact => SurfaceKind::SidebarExpanded,
                        SurfaceKind::SidebarExpanded => SurfaceKind::SidebarCompact,
                        other => other,
                    };
                }
                UiIntent::TogglePreview => {
                    preview_enabled = !preview_enabled;
                    preview = preview_enabled.then_some(Vec::new());
                    preview_session_id = None;
                    preview_refreshed_at = None;
                }
                UiIntent::ToggleDetails => {
                    preview_mode = match preview_mode {
                        PreviewMode::Pane => PreviewMode::Details,
                        PreviewMode::Details => PreviewMode::Pane,
                    };
                    preview_session_id = None;
                    preview_refreshed_at = None;
                }
                UiIntent::ActivateSelected => {
                    if let Some(item) = filtered.get(selected) {
                        tmux.switch_or_attach_session(&item.session_id)?;
                    }
                    break Ok(());
                }
                UiIntent::Close => break Ok(()),
            }
        }
        show_help = matches!(surface_kind, SurfaceKind::Picker);
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
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

fn load_domain_state() -> Result<DomainState, Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let tmux = backend.snapshot()?;
    let zoxide = CommandZoxideProvider::new()
        .load_entries(500)
        .unwrap_or_default();
    Ok(build_domain_state(&CandidateSources { tmux, zoxide }))
}

fn enrich_session_list_items(items: &mut [SessionListItem], state: &DomainState) {
    let branches = git_branches_by_session(state);
    for item in items {
        item.git_branch = branches.get(&item.session_id).cloned();
    }
}

fn git_branches_by_session(
    state: &DomainState,
) -> std::collections::BTreeMap<String, GitBranchStatus> {
    state
        .sessions
        .iter()
        .filter_map(|(session_id, session)| {
            let active_window = session
                .windows
                .values()
                .find(|window| window.active)
                .or_else(|| session.windows.values().next())?;
            let path = active_window.current_path.as_deref()?;
            branch_status_for_directory(path).map(|branch| (session_id.clone(), branch))
        })
        .collect()
}

fn branch_status_for_directory(path: &Path) -> Option<GitBranchStatus> {
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
    let mut branch_name = None;
    let mut upstream = None;
    let mut ahead = 0usize;
    let mut dirty = false;

    for line in stdout.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            if head != "(detached)" {
                branch_name = Some(head.to_string());
            }
        } else if let Some(remote) = line.strip_prefix("# branch.upstream ") {
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

    Some(GitBranchStatus {
        name: branch_name.or_else(|| detached_head_name(path))?,
        pushed: upstream.is_some() && ahead == 0,
        dirty,
    })
}

fn detached_head_name(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
