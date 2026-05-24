use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use embers_client::{ClientState, MuxClient, SocketTransport};
use embers_core::{ActivityState, BufferId, FloatGeometry, MuxError, NodeId, PtySize, SessionId};
use embers_protocol::{
    BufferRequest, ClientMessage, ClientRecord, ClientRequest, FloatingRequest, NodeJoinPlacement,
    NodeRecord, NodeRecordKind, NodeRequest, ServerEvent, ServerResponse, SessionRecord,
    SessionRequest,
};
use thiserror::Error;

pub use embers_core::ActivityState as EmbersActivityState;
pub use embers_protocol::NodeJoinPlacement as EmbersJoinPlacement;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbersSnapshot {
    pub context: EmbersContext,
    pub sessions: Vec<EmbersSession>,
    pub windows: Vec<EmbersWindow>,
    pub panes: Vec<EmbersPane>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbersContext {
    pub client_id: Option<String>,
    pub current_session_name: Option<String>,
    pub current_window_index: Option<u32>,
    pub pane_id: Option<String>,
    pub previous_session_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbersSession {
    pub native_id: String,
    pub name: String,
    pub attached: bool,
    pub last_activity: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbersWindow {
    pub session_name: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub activity: bool,
    pub bell: bool,
    pub silence: bool,
    pub current_path: Option<PathBuf>,
    pub current_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbersPane {
    pub session_name: String,
    pub window_index: u32,
    pub pane_id: String,
    pub title: String,
    pub active: bool,
    pub current_path: Option<PathBuf>,
    pub current_command: Option<String>,
    pub activity: ActivityState,
}

#[derive(Debug, Error)]
pub enum EmbersError {
    #[error("failed to build tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("failed to connect to embers at {socket_path}: {source}")]
    Connect {
        socket_path: PathBuf,
        #[source]
        source: MuxError,
    },
    #[error("embers state lock was poisoned")]
    Poisoned,
    #[error("{0}")]
    Mux(#[from] MuxError),
    #[error("session `{0}` was not found")]
    MissingSession(String),
    #[error("session `{0}` has no previewable buffers")]
    MissingPreview(String),
    #[error("embers client has no current session")]
    MissingCurrentSession,
    #[error("embers protocol returned an unexpected response: {0}")]
    UnexpectedResponse(&'static str),
    #[error("invalid embers state: {0}")]
    InvalidState(String),
    #[error("invalid embers identifier `{value}` for {kind}")]
    InvalidIdentifier { kind: &'static str, value: String },
}

#[derive(Debug)]
pub struct EmbersClient {
    socket_path: PathBuf,
    runtime: tokio::runtime::Runtime,
    state: Mutex<EmbersClientState>,
}

#[derive(Debug)]
struct EmbersClientState {
    client: MuxClient<SocketTransport>,
    client_id: u64,
    previous_session_id: Option<SessionId>,
}

#[derive(Debug)]
struct WindowProjection {
    window: EmbersWindow,
    panes: Vec<EmbersPane>,
}

impl EmbersClient {
    pub fn connect(socket_path: impl Into<PathBuf>) -> Result<Self, EmbersError> {
        let socket_path = socket_path.into();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .map_err(EmbersError::Runtime)?;
        let mut client = runtime
            .block_on(MuxClient::connect(&socket_path))
            .map_err(|source| EmbersError::Connect {
                socket_path: socket_path.clone(),
                source,
            })?;
        let current_client = runtime.block_on(client.current_client())?;
        runtime.block_on(client.subscribe(None))?;
        runtime.block_on(client.resync_all_sessions())?;

        Ok(Self {
            socket_path,
            runtime,
            state: Mutex::new(EmbersClientState {
                client,
                client_id: current_client.id,
                previous_session_id: None,
            }),
        })
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn snapshot(&self) -> Result<EmbersSnapshot, EmbersError> {
        self.with_state(|runtime, state| {
            runtime.block_on(state.client.resync_all_sessions())?;
            let current_client = current_client_record(runtime, state)?;
            let clients = list_clients(runtime, state)?;
            build_snapshot(state, &clients, &current_client)
        })
    }

    pub fn list_session_names(&self) -> Result<Vec<String>, EmbersError> {
        Ok(self
            .snapshot()?
            .sessions
            .into_iter()
            .map(|session| session.name)
            .collect())
    }

    pub fn poll_updates(&self) -> Result<bool, EmbersError> {
        self.with_state(|runtime, state| {
            let mut saw_event = false;
            while let Some(event) =
                runtime.block_on(state.client.process_next_event_timeout(Duration::ZERO))?
            {
                saw_event = true;
                if let ServerEvent::ClientChanged(event) = event
                    && event.client.id == state.client_id
                {
                    state.previous_session_id = event.previous_session_id;
                }
            }
            Ok(saw_event)
        })
    }

    pub fn switch_session(&self, session_name: &str) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            runtime.block_on(state.client.resync_all_sessions())?;
            let current_client = current_client_record(runtime, state)?;
            let session = resolve_session_by_name(state.client.state(), session_name)?;
            if current_client.current_session_id == Some(session.id) {
                // Already on the target session; switching would clobber
                // previous_session_id with the current session itself.
                return Ok(());
            }
            state.previous_session_id = current_client.current_session_id;
            runtime.block_on(state.client.switch_current_session(session.id))?;
            // The next snapshot()/poll_updates refreshes session state, so no
            // trailing resync_all_sessions is needed here.
            Ok(())
        })
    }

    pub fn create_or_switch_session(
        &self,
        session_name: &str,
        directory: &Path,
    ) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            runtime.block_on(state.client.resync_all_sessions())?;
            let current_client = current_client_record(runtime, state)?;
            if let Ok(existing_id) = resolve_session_by_name(state.client.state(), session_name)
                .map(|session| session.id)
            {
                ensure_root_window(runtime, state, existing_id, directory)?;
                if current_client.current_session_id != Some(existing_id) {
                    // Skip the no-op switch so previous_session_id is not
                    // clobbered when the target is already the current session.
                    state.previous_session_id = current_client.current_session_id;
                    runtime.block_on(state.client.switch_current_session(existing_id))?;
                }
                return Ok(());
            }

            let session_id = create_session(runtime, state, session_name)?;
            ensure_root_window(runtime, state, session_id, directory)?;
            state.previous_session_id = current_client.current_session_id;
            runtime.block_on(state.client.switch_current_session(session_id))?;
            // The next snapshot()/poll_updates refreshes session state, so no
            // trailing resync_all_sessions is needed here.
            Ok(())
        })
    }

    pub fn rename_session(&self, session_name: &str, new_name: &str) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            runtime.block_on(state.client.resync_all_sessions())?;
            let session = resolve_session_by_name(state.client.state(), session_name)?;
            let response = runtime.block_on(state.client.request_message(
                ClientMessage::Session(SessionRequest::Rename {
                    request_id: state.client.next_request_id(),
                    session_id: session.id,
                    name: new_name.to_string(),
                }),
            ))?;
            match response {
                ServerResponse::Ok(_) => {
                    runtime.block_on(state.client.resync_all_sessions())?;
                    Ok(())
                }
                _ => Err(EmbersError::UnexpectedResponse("session rename")),
            }
        })
    }

    pub fn kill_session(&self, session_name: &str) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            runtime.block_on(state.client.resync_all_sessions())?;
            let session = resolve_session_by_name(state.client.state(), session_name)?;
            let response = runtime.block_on(state.client.request_message(
                ClientMessage::Session(SessionRequest::Close {
                    request_id: state.client.next_request_id(),
                    session_id: session.id,
                    force: false,
                }),
            ))?;
            match response {
                ServerResponse::Ok(_) => {
                    runtime.block_on(state.client.resync_all_sessions())?;
                    Ok(())
                }
                _ => Err(EmbersError::UnexpectedResponse("session close")),
            }
        })
    }

    pub fn capture_session_preview(
        &self,
        session_name: &str,
        max_lines: usize,
    ) -> Result<String, EmbersError> {
        self.with_state(|runtime, state| {
            runtime.block_on(state.client.resync_all_sessions())?;
            let session_id = resolve_session_by_name(state.client.state(), session_name)
                .map(|session| session.id)?;
            runtime.block_on(state.client.resync_session(session_id))?;
            let buffer_id = preview_buffer_id(state.client.state(), session_id)
                .ok_or_else(|| EmbersError::MissingPreview(session_name.to_string()))?;
            let snapshot = runtime.block_on(state.client.capture_buffer(buffer_id))?;
            let line_count = snapshot.lines.len();
            let start = line_count.saturating_sub(max_lines);
            Ok(snapshot.lines[start..].join("\n"))
        })
    }

    pub fn current_session_name(&self) -> Result<Option<String>, EmbersError> {
        // Resolving only the current session's name does not need a full
        // `resync_all_sessions()`: `current_client()` is a single lightweight
        // request, and the name is read from the already-synced session cache
        // (kept fresh by `poll_updates`). This runs on the per-frame sidebar
        // handoff check, so the full resync would be wasteful here.
        self.with_state(|runtime, state| {
            let current_client = current_client_record(runtime, state)?;
            Ok(current_client.current_session_id.and_then(|session_id| {
                state
                    .client
                    .state()
                    .sessions
                    .get(&session_id)
                    .map(|session| session.name.clone())
            }))
        })
    }

    pub fn focused_buffer_id(&self) -> Result<Option<String>, EmbersError> {
        // Only the current session is needed, so resolve it via the lightweight
        // `current_client_record` + a single `resync_session`, skipping the full
        // `resync_all_sessions` (same reasoning as `current_session_name`).
        self.with_state(|runtime, state| {
            let current_client = current_client_record(runtime, state)?;
            let Some(session_id) = current_client.current_session_id else {
                return Ok(None);
            };
            runtime.block_on(state.client.resync_session(session_id))?;
            Ok(current_focused_buffer_id(state.client.state(), session_id)
                .map(|buffer_id| buffer_id.0.to_string()))
        })
    }

    pub fn current_session_viewport_size(&self) -> Result<Option<(u16, u16)>, EmbersError> {
        // See `focused_buffer_id`: only the current session matters here, so a
        // full `resync_all_sessions` is unnecessary.
        self.with_state(|runtime, state| {
            let current_client = current_client_record(runtime, state)?;
            let Some(session_id) = current_client.current_session_id else {
                return Ok(None);
            };
            runtime.block_on(state.client.resync_session(session_id))?;
            Ok(
                visible_size_for_session_root(state.client.state(), session_id)
                    .map(|size| (size.cols, size.rows)),
            )
        })
    }

    pub fn create_buffer(
        &self,
        command: &[String],
        title: &str,
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<String, EmbersError> {
        self.with_state(|runtime, state| {
            let buffer_id = create_buffer_record(runtime, state, command, title, cwd, env)?;
            Ok(buffer_id.0.to_string())
        })
    }

    pub fn create_floating_for_buffer_in_current_session(
        &self,
        buffer_id: &str,
        title: Option<&str>,
        width: u16,
        height: u16,
        focus: bool,
        close_on_empty: bool,
    ) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            let (session_id, _) = current_session_info(runtime, state)?;
            runtime.block_on(state.client.resync_session(session_id))?;
            let viewport = visible_size_for_session_root(state.client.state(), session_id)
                .unwrap_or_else(|| PtySize::new(80, 24));
            let geometry = centered_geometry(viewport, width, height);
            let buffer_id = parse_buffer_id(buffer_id)?;
            let response = runtime.block_on(state.client.request_message(
                ClientMessage::Floating(FloatingRequest::Create {
                    request_id: state.client.next_request_id(),
                    session_id,
                    root_node_id: None,
                    buffer_id: Some(buffer_id),
                    geometry,
                    title: title.map(ToString::to_string),
                    focus,
                    close_on_empty,
                }),
            ))?;
            match response {
                ServerResponse::Floating(_) => {
                    runtime.block_on(state.client.resync_session(session_id))?;
                    Ok(())
                }
                _ => Err(EmbersError::UnexpectedResponse("floating create")),
            }
        })
    }

    pub fn join_buffer_to_current_session_root(
        &self,
        buffer_id: &str,
        placement: NodeJoinPlacement,
        leading_size: Option<u16>,
        focus: bool,
    ) -> Result<String, EmbersError> {
        self.with_state(|runtime, state| {
            let (session_id, session_name) = current_session_info(runtime, state)?;
            runtime.block_on(state.client.resync_session(session_id))?;
            let root_node_id = state
                .client
                .state()
                .sessions
                .get(&session_id)
                .map(|session| session.root_node_id)
                .ok_or_else(|| {
                    EmbersError::InvalidState(format!(
                        "session {session_id} is not cached for root join"
                    ))
                })?;
            let viewport = visible_size_for_session_root(state.client.state(), session_id)
                .unwrap_or_else(|| PtySize::new(80, 24));
            let buffer_id = parse_buffer_id(buffer_id)?;
            let location = buffer_location(runtime, state, buffer_id)?;
            // Detach the buffer from wherever it is currently attached before
            // rejoining it at the root. `JoinBufferAtNode` does not implicitly
            // detach (hence the explicit step), so a buffer already attached
            // within this same session — e.g. the sidebar buffer at its sidebar
            // node — must also be detached, not just cross-session buffers.
            if location.session_id().is_some() {
                detach_buffer_record(runtime, state, buffer_id)?;
            }
            let response = runtime.block_on(state.client.request_message(ClientMessage::Node(
                NodeRequest::JoinBufferAtNode {
                    request_id: state.client.next_request_id(),
                    node_id: root_node_id,
                    buffer_id,
                    placement,
                },
            )))?;
            apply_session_layout_response(
                runtime,
                &mut state.client,
                response,
                session_id,
                "join buffer at node",
            )?;

            if let Some(leading_size) = leading_size
                && let Some(sizes) = split_sizes_for_join(viewport, leading_size, placement)
            {
                let location = buffer_location(runtime, state, buffer_id)?;
                let leaf_node_id = location.node_id().ok_or_else(|| {
                    EmbersError::InvalidState(format!(
                        "buffer {buffer_id} did not attach to a node"
                    ))
                })?;
                let split_id = state
                    .client
                    .state()
                    .nodes
                    .get(&leaf_node_id)
                    .and_then(|node| node.parent_id)
                    .ok_or_else(|| {
                        EmbersError::InvalidState(format!(
                            "buffer {buffer_id} has no split parent after root join"
                        ))
                    })?;
                let response = runtime.block_on(state.client.request_message(
                    ClientMessage::Node(NodeRequest::Resize {
                        request_id: state.client.next_request_id(),
                        node_id: split_id,
                        sizes,
                    }),
                ))?;
                apply_session_layout_response(
                    runtime,
                    &mut state.client,
                    response,
                    session_id,
                    "resize split",
                )?;
            }

            if focus {
                let location = buffer_location(runtime, state, buffer_id)?;
                let leaf_node_id = location.node_id().ok_or_else(|| {
                    EmbersError::InvalidState(format!(
                        "buffer {buffer_id} did not resolve to a leaf"
                    ))
                })?;
                let response = runtime.block_on(state.client.request_message(
                    ClientMessage::Node(NodeRequest::Focus {
                        request_id: state.client.next_request_id(),
                        session_id,
                        node_id: leaf_node_id,
                    }),
                ))?;
                apply_session_layout_response(
                    runtime,
                    &mut state.client,
                    response,
                    session_id,
                    "focus buffer",
                )?;
            }

            Ok(session_name)
        })
    }

    pub fn detach_buffer(&self, buffer_id: &str) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            let buffer_id = parse_buffer_id(buffer_id)?;
            let response = runtime.block_on(state.client.request_message(
                ClientMessage::Buffer(BufferRequest::Detach {
                    request_id: state.client.next_request_id(),
                    buffer_id,
                }),
            ))?;
            match response {
                ServerResponse::Ok(_) => Ok(()),
                _ => Err(EmbersError::UnexpectedResponse("buffer detach")),
            }
        })
    }

    pub fn kill_buffer(&self, buffer_id: &str) -> Result<(), EmbersError> {
        self.with_state(|runtime, state| {
            let buffer_id = parse_buffer_id(buffer_id)?;
            let response = runtime.block_on(state.client.request_message(
                ClientMessage::Buffer(BufferRequest::Kill {
                    request_id: state.client.next_request_id(),
                    buffer_id,
                    force: true,
                }),
            ))?;
            match response {
                ServerResponse::Ok(_) => Ok(()),
                _ => Err(EmbersError::UnexpectedResponse("buffer kill")),
            }
        })
    }

    fn with_state<T>(
        &self,
        action: impl FnOnce(&tokio::runtime::Runtime, &mut EmbersClientState) -> Result<T, EmbersError>,
    ) -> Result<T, EmbersError> {
        let mut state = self.state.lock().map_err(|_| EmbersError::Poisoned)?;
        action(&self.runtime, &mut state)
    }
}

fn current_client_record(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
) -> Result<ClientRecord, EmbersError> {
    let record = runtime.block_on(state.client.current_client())?;
    state.client_id = record.id;
    Ok(record)
}

fn list_clients(
    runtime: &tokio::runtime::Runtime,
    state: &EmbersClientState,
) -> Result<Vec<ClientRecord>, EmbersError> {
    let response = runtime.block_on(state.client.request_message(ClientMessage::Client(
        ClientRequest::List {
            request_id: state.client.next_request_id(),
        },
    )))?;
    match response {
        ServerResponse::Clients(response) => Ok(response.clients),
        _ => Err(EmbersError::UnexpectedResponse("client list")),
    }
}

fn current_session_info(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
) -> Result<(SessionId, String), EmbersError> {
    runtime.block_on(state.client.resync_all_sessions())?;
    let current_client = current_client_record(runtime, state)?;
    let session_id = current_client
        .current_session_id
        .ok_or(EmbersError::MissingCurrentSession)?;
    let session_name = state
        .client
        .state()
        .sessions
        .get(&session_id)
        .map(|session| session.name.clone())
        .ok_or_else(|| {
            EmbersError::InvalidState(format!("current session {session_id} is not cached"))
        })?;
    Ok((session_id, session_name))
}

fn detach_buffer_record(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
    buffer_id: BufferId,
) -> Result<(), EmbersError> {
    let response = runtime.block_on(state.client.request_message(ClientMessage::Buffer(
        BufferRequest::Detach {
            request_id: state.client.next_request_id(),
            buffer_id,
        },
    )))?;
    match response {
        ServerResponse::Ok(_) => Ok(()),
        _ => Err(EmbersError::UnexpectedResponse("buffer detach")),
    }
}

fn apply_session_layout_response(
    runtime: &tokio::runtime::Runtime,
    client: &mut MuxClient<SocketTransport>,
    response: ServerResponse,
    session_id: SessionId,
    operation: &'static str,
) -> Result<(), EmbersError> {
    match response {
        ServerResponse::SessionSnapshot(response) => {
            client.state_mut().apply_session_snapshot(response.snapshot);
            Ok(())
        }
        ServerResponse::Ok(_) => {
            runtime.block_on(client.resync_session(session_id))?;
            Ok(())
        }
        _ => Err(EmbersError::UnexpectedResponse(operation)),
    }
}

fn buffer_location(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
    buffer_id: BufferId,
) -> Result<embers_protocol::BufferLocation, EmbersError> {
    let response = runtime.block_on(state.client.request_message(ClientMessage::Buffer(
        BufferRequest::GetLocation {
            request_id: state.client.next_request_id(),
            buffer_id,
        },
    )))?;
    match response {
        ServerResponse::BufferLocation(response) => Ok(response.location),
        _ => Err(EmbersError::UnexpectedResponse("buffer location")),
    }
}

fn create_session(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
    name: &str,
) -> Result<SessionId, EmbersError> {
    let response = runtime.block_on(state.client.request_message(ClientMessage::Session(
        SessionRequest::Create {
            request_id: state.client.next_request_id(),
            name: name.to_string(),
        },
    )))?;
    match response {
        ServerResponse::SessionSnapshot(response) => {
            let session_id = response.snapshot.session.id;
            state
                .client
                .state_mut()
                .apply_session_snapshot(response.snapshot);
            Ok(session_id)
        }
        _ => Err(EmbersError::UnexpectedResponse("session create")),
    }
}

fn ensure_root_window(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
    session_id: SessionId,
    directory: &Path,
) -> Result<(), EmbersError> {
    runtime.block_on(state.client.resync_session(session_id))?;
    if session_has_root_window(state.client.state(), session_id)? {
        return Ok(());
    }

    let command = default_shell_command();
    let title = default_title(&command, "shell");
    let buffer_id = create_buffer_record(
        runtime,
        state,
        &command,
        &title,
        Some(directory),
        &BTreeMap::new(),
    )?;
    let response = runtime.block_on(state.client.request_message(ClientMessage::Session(
        SessionRequest::AddRootTab {
            request_id: state.client.next_request_id(),
            session_id,
            title,
            buffer_id: Some(buffer_id),
            child_node_id: None,
        },
    )))?;
    match response {
        ServerResponse::SessionSnapshot(response) => {
            state
                .client
                .state_mut()
                .apply_session_snapshot(response.snapshot);
            Ok(())
        }
        _ => Err(EmbersError::UnexpectedResponse("add root tab")),
    }
}

fn create_buffer_record(
    runtime: &tokio::runtime::Runtime,
    state: &mut EmbersClientState,
    command: &[String],
    title: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Result<BufferId, EmbersError> {
    let response = runtime.block_on(state.client.request_message(ClientMessage::Buffer(
        BufferRequest::Create {
            request_id: state.client.next_request_id(),
            title: Some(title.to_string()),
            command: command.to_vec(),
            cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
            env: env.clone(),
        },
    )))?;
    match response {
        ServerResponse::Buffer(response) => {
            let buffer_id = response.buffer.id;
            state
                .client
                .state_mut()
                .apply_buffer_record(response.buffer);
            Ok(buffer_id)
        }
        _ => Err(EmbersError::UnexpectedResponse("buffer create")),
    }
}

fn session_has_root_window(
    state: &ClientState,
    session_id: SessionId,
) -> Result<bool, EmbersError> {
    let session = state
        .sessions
        .get(&session_id)
        .ok_or_else(|| EmbersError::InvalidState(format!("session {session_id} is not cached")))?;
    let root = state.nodes.get(&session.root_node_id).ok_or_else(|| {
        EmbersError::InvalidState(format!("node {} is not cached", session.root_node_id))
    })?;
    let tabs = root.tabs.as_ref();
    Ok(tabs.is_none_or(|tabs| !tabs.tabs.is_empty()))
}

fn resolve_session_by_name<'a>(
    state: &'a ClientState,
    session_name: &str,
) -> Result<&'a SessionRecord, EmbersError> {
    state
        .sessions
        .values()
        .find(|session| session.name == session_name)
        .ok_or_else(|| EmbersError::MissingSession(session_name.to_string()))
}

fn preview_buffer_id(state: &ClientState, session_id: SessionId) -> Option<BufferId> {
    let session = state.sessions.get(&session_id)?;
    let focus = session
        .focused_leaf_id
        .filter(|leaf_id| node_belongs_to_subtree(state, session.root_node_id, *leaf_id))
        .and_then(|leaf_id| buffer_id_for_buffer_view(state, leaf_id));
    focus.or_else(|| first_buffer_id_in_subtree(state, session.root_node_id))
}

fn current_focused_buffer_id(state: &ClientState, session_id: SessionId) -> Option<BufferId> {
    let session = state.sessions.get(&session_id)?;
    session
        .focused_leaf_id
        .and_then(|leaf_id| buffer_id_for_buffer_view(state, leaf_id))
        .or_else(|| preview_buffer_id(state, session_id))
}

fn visible_size_for_session_root(state: &ClientState, session_id: SessionId) -> Option<PtySize> {
    let session = state.sessions.get(&session_id)?;
    visible_size_for_node(state, session.root_node_id)
}

fn visible_size_for_node(state: &ClientState, node_id: NodeId) -> Option<PtySize> {
    let node = state.nodes.get(&node_id)?;
    match node.kind {
        NodeRecordKind::BufferView => {
            let view = node.buffer_view.as_ref()?;
            non_zero_size(view.last_render_size).or_else(|| {
                buffer_for_node(state, node_id)
                    .ok()
                    .and_then(|buffer| non_zero_size(buffer.pty_size))
            })
        }
        NodeRecordKind::Split => {
            let split = node.split.as_ref()?;
            let child_sizes = split
                .child_ids
                .iter()
                .filter_map(|child_id| visible_size_for_node(state, *child_id))
                .collect::<Vec<_>>();
            if child_sizes.is_empty() {
                return None;
            }
            let aggregate = match split.direction {
                embers_core::SplitDirection::Vertical => PtySize::new(
                    saturating_u16_sum(child_sizes.iter().map(|size| size.cols)),
                    child_sizes.iter().map(|size| size.rows).max().unwrap_or(0),
                ),
                embers_core::SplitDirection::Horizontal => PtySize::new(
                    child_sizes.iter().map(|size| size.cols).max().unwrap_or(0),
                    saturating_u16_sum(child_sizes.iter().map(|size| size.rows)),
                ),
            };
            non_zero_size(aggregate)
        }
        NodeRecordKind::Tabs => {
            let tabs = node.tabs.as_ref()?;
            let active = usize::try_from(tabs.active).ok()?;
            tabs.tabs
                .get(active)
                .or_else(|| tabs.tabs.first())
                .and_then(|tab| visible_size_for_node(state, tab.child_id))
        }
    }
}

fn build_snapshot(
    state: &EmbersClientState,
    clients: &[ClientRecord],
    current_client: &ClientRecord,
) -> Result<EmbersSnapshot, EmbersError> {
    let client_state = state.client.state();
    let mut session_records = client_state.sessions.values().cloned().collect::<Vec<_>>();
    session_records.sort_by(|left, right| left.name.cmp(&right.name));

    let mut sessions = Vec::with_capacity(session_records.len());
    let mut windows = Vec::new();
    let mut panes = Vec::new();

    for session in session_records {
        let attached = clients
            .iter()
            .any(|client| client.current_session_id == Some(session.id));
        let session_windows = project_session_windows(client_state, &session)?;
        for projection in &session_windows {
            windows.push(projection.window.clone());
            panes.extend(projection.panes.iter().cloned());
        }
        sessions.push(EmbersSession {
            native_id: session.id.to_string(),
            name: session.name,
            attached,
            last_activity: None,
        });
    }

    let current_session_name = current_client.current_session_id.and_then(|session_id| {
        client_state
            .sessions
            .get(&session_id)
            .map(|session| session.name.clone())
    });
    let current_window_index = current_client
        .current_session_id
        .and_then(|session_id| current_window_index(client_state, session_id));
    let pane_id = current_client
        .current_session_id
        .and_then(|session_id| client_state.sessions.get(&session_id))
        .and_then(|session| session.focused_leaf_id)
        .map(|pane_id| pane_id.to_string());
    let previous_session_name = state.previous_session_id.and_then(|session_id| {
        client_state
            .sessions
            .get(&session_id)
            .map(|session| session.name.clone())
    });

    Ok(EmbersSnapshot {
        context: EmbersContext {
            client_id: Some(current_client.id.to_string()),
            current_session_name,
            current_window_index,
            pane_id,
            previous_session_name,
        },
        sessions,
        windows,
        panes,
    })
}

fn current_window_index(state: &ClientState, session_id: SessionId) -> Option<u32> {
    let session = state.sessions.get(&session_id)?;
    let root = state.nodes.get(&session.root_node_id)?;
    if root.kind != NodeRecordKind::Tabs {
        return Some(1);
    }
    let tabs = root.tabs.as_ref()?;
    if let Some(focused_leaf_id) = session.focused_leaf_id {
        for (index, tab) in tabs.tabs.iter().enumerate() {
            if node_belongs_to_subtree(state, tab.child_id, focused_leaf_id) {
                return u32::try_from(index + 1).ok();
            }
        }
    }
    u32::try_from(usize::try_from(tabs.active).ok()?.saturating_add(1)).ok()
}

fn project_session_windows(
    state: &ClientState,
    session: &SessionRecord,
) -> Result<Vec<WindowProjection>, EmbersError> {
    let root = state.nodes.get(&session.root_node_id).ok_or_else(|| {
        EmbersError::InvalidState(format!("node {} is not cached", session.root_node_id))
    })?;
    if root.kind == NodeRecordKind::Tabs {
        let tabs = root.tabs.as_ref().ok_or_else(|| {
            EmbersError::InvalidState(format!(
                "tabs node {} is missing tabs payload",
                session.root_node_id
            ))
        })?;
        return tabs
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let index = u32::try_from(index + 1).map_err(|_| {
                    EmbersError::InvalidState("tab index exceeded u32 range".to_string())
                })?;
                build_window_projection(
                    state,
                    session,
                    index,
                    tab.title.clone(),
                    index == tabs.active.saturating_add(1),
                    tab.child_id,
                )
            })
            .collect();
    }

    vec![build_window_projection(
        state,
        session,
        1,
        default_window_name(state, session.root_node_id, &session.name),
        true,
        session.root_node_id,
    )]
    .into_iter()
    .collect()
}

fn build_window_projection(
    state: &ClientState,
    session: &SessionRecord,
    index: u32,
    name: String,
    active: bool,
    root_node_id: NodeId,
) -> Result<WindowProjection, EmbersError> {
    let mut pane_nodes = Vec::new();
    collect_buffer_view_nodes(state, root_node_id, &mut pane_nodes)?;
    let panes = pane_nodes
        .into_iter()
        .map(|pane_node_id| {
            let buffer = buffer_for_node(state, pane_node_id)?;
            Ok(EmbersPane {
                session_name: session.name.clone(),
                window_index: index,
                pane_id: pane_node_id.to_string(),
                title: buffer.title.clone(),
                active: session.focused_leaf_id == Some(pane_node_id),
                current_path: buffer.cwd.as_ref().map(PathBuf::from),
                current_command: display_command(&buffer.command),
                activity: buffer.activity,
            })
        })
        .collect::<Result<Vec<_>, EmbersError>>()?;

    let active_pane = panes
        .iter()
        .find(|pane| pane.active)
        .or_else(|| panes.first());
    let activity = panes
        .iter()
        .any(|pane| matches!(pane.activity, ActivityState::Activity));
    let bell = panes
        .iter()
        .any(|pane| matches!(pane.activity, ActivityState::Bell));

    Ok(WindowProjection {
        window: EmbersWindow {
            session_name: session.name.clone(),
            index,
            name,
            active,
            activity,
            bell,
            silence: false,
            current_path: active_pane.and_then(|pane| pane.current_path.clone()),
            current_command: active_pane.and_then(|pane| pane.current_command.clone()),
        },
        panes,
    })
}

fn buffer_for_node(
    state: &ClientState,
    node_id: NodeId,
) -> Result<&embers_protocol::BufferRecord, EmbersError> {
    let node = state
        .nodes
        .get(&node_id)
        .ok_or_else(|| EmbersError::InvalidState(format!("node {node_id} is not cached")))?;
    let buffer_view = node
        .buffer_view
        .as_ref()
        .ok_or_else(|| EmbersError::InvalidState(format!("node {node_id} is not a buffer view")))?;
    state.buffers.get(&buffer_view.buffer_id).ok_or_else(|| {
        EmbersError::InvalidState(format!("buffer {} is not cached", buffer_view.buffer_id))
    })
}

fn collect_buffer_view_nodes(
    state: &ClientState,
    node_id: NodeId,
    buffer_views: &mut Vec<NodeId>,
) -> Result<(), EmbersError> {
    let node = state
        .nodes
        .get(&node_id)
        .ok_or_else(|| EmbersError::InvalidState(format!("node {node_id} is not cached")))?;
    match node.kind {
        NodeRecordKind::BufferView => buffer_views.push(node_id),
        NodeRecordKind::Split => {
            let split = node.split.as_ref().ok_or_else(|| {
                EmbersError::InvalidState(format!("split node {node_id} is missing split payload"))
            })?;
            for child_id in &split.child_ids {
                collect_buffer_view_nodes(state, *child_id, buffer_views)?;
            }
        }
        NodeRecordKind::Tabs => {
            let tabs = node.tabs.as_ref().ok_or_else(|| {
                EmbersError::InvalidState(format!("tabs node {node_id} is missing tabs payload"))
            })?;
            for tab in &tabs.tabs {
                collect_buffer_view_nodes(state, tab.child_id, buffer_views)?;
            }
        }
    }
    Ok(())
}

fn first_buffer_id_in_subtree(state: &ClientState, node_id: NodeId) -> Option<BufferId> {
    let mut buffer_views = Vec::new();
    collect_buffer_view_nodes(state, node_id, &mut buffer_views).ok()?;
    buffer_views
        .into_iter()
        .find_map(|buffer_node_id| buffer_id_for_buffer_view(state, buffer_node_id))
}

fn buffer_id_for_buffer_view(state: &ClientState, node_id: NodeId) -> Option<BufferId> {
    let node = state.nodes.get(&node_id)?;
    let buffer_view = node.buffer_view.as_ref()?;
    Some(buffer_view.buffer_id)
}

fn centered_geometry(viewport: PtySize, width: u16, height: u16) -> FloatGeometry {
    let max_width = viewport.cols.max(1);
    let max_height = viewport.rows.max(1);
    let width = width.clamp(1, max_width);
    let height = height.clamp(1, max_height);
    FloatGeometry::new(
        max_width.saturating_sub(width) / 2,
        max_height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn split_sizes_for_join(
    viewport: PtySize,
    leading_size: u16,
    placement: NodeJoinPlacement,
) -> Option<Vec<u16>> {
    let total = match placement {
        NodeJoinPlacement::Left | NodeJoinPlacement::Right => viewport.cols,
        NodeJoinPlacement::Up | NodeJoinPlacement::Down => viewport.rows,
        NodeJoinPlacement::TabBefore | NodeJoinPlacement::TabAfter => return None,
    };
    if total <= 1 {
        return None;
    }
    let leading = leading_size.clamp(1, total.saturating_sub(1));
    let trailing = total.saturating_sub(leading).max(1);
    Some(match placement {
        NodeJoinPlacement::Left | NodeJoinPlacement::Up => vec![leading, trailing],
        NodeJoinPlacement::Right | NodeJoinPlacement::Down => vec![trailing, leading],
        NodeJoinPlacement::TabBefore | NodeJoinPlacement::TabAfter => unreachable!(),
    })
}

fn non_zero_size(size: PtySize) -> Option<PtySize> {
    (size.cols > 0 && size.rows > 0).then_some(size)
}

fn saturating_u16_sum(values: impl Iterator<Item = u16>) -> u16 {
    values
        .fold(0_u32, |total, value| total.saturating_add(u32::from(value)))
        .min(u32::from(u16::MAX)) as u16
}

fn node_belongs_to_subtree(
    state: &ClientState,
    root_node_id: NodeId,
    target_node_id: NodeId,
) -> bool {
    if root_node_id == target_node_id {
        return true;
    }
    let Some(node) = state.nodes.get(&root_node_id) else {
        return false;
    };
    child_ids(node)
        .into_iter()
        .any(|child_id| node_belongs_to_subtree(state, child_id, target_node_id))
}

fn child_ids(node: &NodeRecord) -> Vec<NodeId> {
    match node.kind {
        NodeRecordKind::BufferView => Vec::new(),
        NodeRecordKind::Split => node
            .split
            .as_ref()
            .map(|split| split.child_ids.clone())
            .unwrap_or_default(),
        NodeRecordKind::Tabs => node
            .tabs
            .as_ref()
            .map(|tabs| tabs.tabs.iter().map(|tab| tab.child_id).collect())
            .unwrap_or_default(),
    }
}

fn default_window_name(state: &ClientState, root_node_id: NodeId, fallback: &str) -> String {
    first_buffer_id_in_subtree(state, root_node_id)
        .and_then(|buffer_id| state.buffers.get(&buffer_id))
        .map(|buffer| buffer.title.clone())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn display_command(command: &[String]) -> Option<String> {
    let first = command.first()?;
    Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string)
        .or_else(|| Some(first.clone()))
}

fn default_shell_command() -> Vec<String> {
    vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())]
}

fn parse_buffer_id(value: &str) -> Result<BufferId, EmbersError> {
    value
        .parse::<u64>()
        .map(BufferId)
        .map_err(|_| EmbersError::InvalidIdentifier {
            kind: "buffer",
            value: value.to_string(),
        })
}

fn default_title(command: &[String], fallback: &str) -> String {
    command
        .first()
        .and_then(|value| {
            Path::new(value)
                .file_name()
                .and_then(|file_name| file_name.to_str())
        })
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        EmbersJoinPlacement, centered_geometry, split_sizes_for_join, visible_size_for_node,
    };
    use embers_client::ClientState;
    use embers_core::{ActivityState, BufferId, FloatGeometry, NodeId, PtySize, SessionId};
    use embers_protocol::{
        BufferRecord, BufferRecordKind, BufferRecordState, BufferViewRecord, NodeRecord,
        NodeRecordKind, SessionRecord, SplitRecord,
    };

    #[test]
    fn visible_size_for_split_sums_vertical_children() {
        let mut state = ClientState::default();
        state.sessions.insert(
            SessionId(1),
            SessionRecord {
                id: SessionId(1),
                name: "alpha".to_string(),
                root_node_id: NodeId(1),
                floating_ids: Vec::new(),
                focused_leaf_id: Some(NodeId(2)),
                focused_floating_id: None,
                zoomed_node_id: None,
            },
        );
        state.nodes.insert(
            NodeId(1),
            NodeRecord {
                id: NodeId(1),
                session_id: SessionId(1),
                parent_id: None,
                kind: NodeRecordKind::Split,
                buffer_view: None,
                split: Some(SplitRecord {
                    direction: embers_core::SplitDirection::Vertical,
                    child_ids: vec![NodeId(2), NodeId(3)],
                    sizes: vec![36, 84],
                }),
                tabs: None,
            },
        );
        state.nodes.insert(
            NodeId(2),
            buffer_view_node(
                NodeId(2),
                Some(NodeId(1)),
                BufferId(10),
                PtySize::new(36, 20),
            ),
        );
        state.nodes.insert(
            NodeId(3),
            buffer_view_node(
                NodeId(3),
                Some(NodeId(1)),
                BufferId(11),
                PtySize::new(84, 20),
            ),
        );
        state
            .buffers
            .insert(BufferId(10), buffer_record(BufferId(10)));
        state
            .buffers
            .insert(BufferId(11), buffer_record(BufferId(11)));

        assert_eq!(
            visible_size_for_node(&state, NodeId(1)),
            Some(PtySize::new(120, 20))
        );
    }

    #[test]
    fn split_sizes_for_join_respects_left_and_right_ordering() {
        let viewport = PtySize::new(120, 40);
        assert_eq!(
            split_sizes_for_join(viewport, 36, EmbersJoinPlacement::Left),
            Some(vec![36, 84])
        );
        assert_eq!(
            split_sizes_for_join(viewport, 36, EmbersJoinPlacement::Right),
            Some(vec![84, 36])
        );
    }

    #[test]
    fn centered_geometry_clamps_to_viewport() {
        assert_eq!(
            centered_geometry(PtySize::new(100, 30), 120, 40),
            FloatGeometry::new(0, 0, 100, 30)
        );
    }

    fn buffer_view_node(
        id: NodeId,
        parent_id: Option<NodeId>,
        buffer_id: BufferId,
        size: PtySize,
    ) -> NodeRecord {
        NodeRecord {
            id,
            session_id: SessionId(1),
            parent_id,
            kind: NodeRecordKind::BufferView,
            buffer_view: Some(BufferViewRecord {
                buffer_id,
                focused: false,
                zoomed: false,
                follow_output: true,
                last_render_size: size,
            }),
            split: None,
            tabs: None,
        }
    }

    fn buffer_record(id: BufferId) -> BufferRecord {
        BufferRecord {
            id,
            title: "buffer".to_string(),
            command: vec!["/bin/sh".to_string()],
            cwd: None,
            pipe: None,
            kind: BufferRecordKind::Pty,
            state: BufferRecordState::Running,
            pid: Some(1),
            attachment_node_id: None,
            read_only: false,
            helper_source_buffer_id: None,
            helper_scope: None,
            pty_size: PtySize::new(80, 24),
            activity: ActivityState::Idle,
            last_snapshot_seq: 0,
            exit_code: None,
            env: BTreeMap::new(),
        }
    }
}
