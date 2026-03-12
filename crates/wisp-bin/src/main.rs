use std::{env, error::Error, io::stdout, process::ExitCode, time::Duration};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use wisp_app::{CandidateSources, build_domain_state};
use wisp_config::{CliOverrides, LoadOptions, load_config};
use wisp_core::{
    DomainState, PreviewKey, PreviewRequest, SessionListItem, derive_session_list,
    derive_status_items,
};
use wisp_fuzzy::{MatchItem, Matcher, SimpleMatcher};
use wisp_preview::{PreviewProvider, SessionPreviewProvider};
use wisp_status::{StatusFormatOptions, StatusRenderState};
use wisp_tmux::{
    CommandTmuxClient, PollingTmuxBackend, PopupCommand, PopupOptions, PopupSpec, SidebarPaneSpec,
    SidebarSide, TmuxBackend, TmuxClient, TmuxError,
};
use wisp_ui::{SurfaceKind, SurfaceModel, UiIntent, render_surface, translate_key};
use wisp_zoxide::{CommandZoxideProvider, ZoxideProvider};

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
    let session_items = derive_session_list(&state, Some("default"));
    let preview_provider = SessionPreviewProvider {
        state: state.clone(),
    };
    let mut query = String::new();
    let mut selected = 0usize;
    let mut show_help = true;
    let mut preview_enabled = matches!(kind, SurfaceKind::Picker);
    let mut surface_kind = kind;

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = loop {
        let filtered = filter_items(&session_items, &query);
        if selected >= filtered.len() {
            selected = filtered.len().saturating_sub(1);
        }
        let preview = if preview_enabled {
            filtered.get(selected).and_then(|item| {
                preview_provider
                    .generate(&PreviewRequest::SessionSummary {
                        key: PreviewKey::Session(item.session_id.clone()),
                        session_name: item.session_id.clone(),
                    })
                    .ok()
                    .map(|content| content.body)
            })
        } else {
            None
        };
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
            preview,
            kind: surface_kind,
        };

        terminal.draw(|frame| {
            let area = frame.area();
            render_surface(area, frame.buffer_mut(), &model);
        })?;

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
                }
                UiIntent::ActivateSelected => {
                    if let Some(item) = filtered.get(selected) {
                        let tmux = CommandTmuxClient::new();
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

fn load_domain_state() -> Result<DomainState, Box<dyn Error>> {
    let backend = PollingTmuxBackend::new(CommandTmuxClient::new());
    let tmux = backend.snapshot()?;
    let zoxide = CommandZoxideProvider::new()
        .load_entries(500)
        .unwrap_or_default();
    Ok(build_domain_state(&CandidateSources { tmux, zoxide }))
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
