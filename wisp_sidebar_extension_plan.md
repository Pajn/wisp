# Wisp Sidebar / Popup / Status Extension Plan

This document extends the original Rust native tmux-sessionx replacement plan.

It adds a shared **session and notification core** plus multiple thin UI frontends:

- default popup picker
- persistent sidebar pane
- sidebar popup
- second status-line renderer

It also describes the **refactors needed** so these new capabilities fit cleanly into the existing workspace without duplicating state or UI-specific logic.

---

## Relationship to the original plan

The original plan remains the base implementation plan for:

- workspace layout
- config loading
- tmux integration
- zoxide integration
- fuzzy matching
- preview generation
- ratatui picker
- testing and benchmarks

This extension should be read as:

1. a functional expansion of the product
2. a refactor plan for the internal architecture
3. a new set of acceptance criteria and tests

Where this document conflicts with the original plan, prefer this document for the **session-state architecture**, **event model**, and **crate boundaries related to tmux session awareness**.

---

## New product goals

Wisp should support three complementary tmux-aware navigation surfaces powered by one shared session engine:

### 1. Picker
Fast, transient selection UI for switching sessions, windows, or directories.

### 2. Sidebar
A narrower always-visible or side-mounted navigation surface that makes it easy to switch back and forth between sessions and notice activity.

This must support two launch forms:
- **persistent pane** inside tmux
- **side popup** as an alternative default picker mode

### 3. Status line
A compact, passive awareness surface rendered into a second tmux status line.

This is not the primary interactive UI. It is a secondary projection of the shared state.

---

## New design principles

The extension introduces one major architectural rule:

> There must be exactly one canonical in-memory model for tmux sessions, windows, focus, and notification state.

From that model, Wisp derives different views for:
- popup picker
- persistent sidebar
- sidebar popup
- status line
- previews

### Why this matters

Without this rule, the codebase will drift into:
- repeated tmux polling per UI mode
- duplicated alert logic
- inconsistent unread/activity semantics
- hard-to-test branching between popup/sidebar/status code paths

The implementation should instead prefer:
- one domain model
- one event stream
- one reducer / state transition system
- multiple thin renderers

---

## Refactor summary against the original plan

The original plan is a strong base, but to support sidebar and status views well, the following refactors are recommended.

### Refactor 1: rename or broaden `sessionx-core` into a stronger domain crate

The original `sessionx-core` is candidate-oriented. That is not enough anymore.

It should be expanded to own:
- session model
- window model
- pane model
- focus history
- notifications and unseen state
- candidate projections derived from session state
- view projections for UI and status renderers

Recommended options:
- keep the name and broaden responsibilities, or
- rename to `wisp-core` if rebranding is already happening

### Refactor 2: move candidate construction out of ad hoc startup glue

The original plan implies building candidates from tmux + zoxide results as part of application startup.

That still works for directories, but tmux session data should now feed a dedicated state store first.

New rule:
- tmux snapshot and live tmux events update the canonical state
- candidate lists are derived views, not primary state

### Refactor 3: tmux integration must support both snapshot and live updates

The original plan was optimized for a short-lived picker and suggested that aggressive resync may not be worth it.

That assumption no longer holds for:
- persistent sidebar pane
- status line updates
- session notifications

New rule:
- tmux integration must support an initial snapshot **and** an ongoing live event stream
- polling is acceptable only as a temporary fallback or compatibility mode

### Refactor 4: separate domain state from view state

The original plan mixes some UI state and app state in one `AppState`.

That remains workable, but for the extended product it should be formalized as:
- `DomainState`: sessions, windows, panes, alerts, focus history, directory source state
- `UiState`: query, selection, current mode, compact/expanded sidebar mode, help visibility
- `DerivedView`: projections for the active renderer and status line

### Refactor 5: status rendering becomes its own projection layer

The second status-line implementation should not be a bespoke side effect bolted onto tmux actions.

It should be fed by a dedicated view/projection layer so its formatting rules are testable without tmux.

### Refactor 6: session notifications become first-class domain behavior

The original plan mentions session previews and current markers, but not a strong alert model.

That must change.

Notification semantics should live in the core domain, not in tmux parsing code and not in the UI.

---

## Revised workspace recommendation

If rebranding to Wisp now, prefer this workspace naming:

```text
wisp/
├─ Cargo.toml
├─ crates/
│  ├─ wisp-core/
│  ├─ wisp-config/
│  ├─ wisp-tmux/
│  ├─ wisp-zoxide/
│  ├─ wisp-preview/
│  ├─ wisp-fuzzy/
│  ├─ wisp-ui/
│  ├─ wisp-status/
│  ├─ wisp-app/
│  └─ wisp-bin/
├─ tests/
│  ├─ integration/
│  ├─ e2e/
│  └─ fixtures/
└─ docs/
```

If renaming the workspace immediately is too disruptive, keep the original crate names and apply the same structural changes.

---

## Revised crate responsibilities

### `wisp-core`

This becomes the heart of the session-aware product.

Owns:
- tmux session/window/pane domain types
- focus tracking
- notification state and aggregation
- unseen-output semantics
- sort and grouping rules
- previous-session resolution
- candidate and list projections
- status projection models
- reducer logic for state transitions

Must not own:
- direct tmux I/O
- ratatui widgets
- tmux status formatting strings
- subprocess execution

Suggested modules:
- `model.rs`
- `focus.rs`
- `notifications.rs`
- `reduce.rs`
- `view.rs`
- `sort.rs`
- `filter.rs`
- `commands.rs`

### `wisp-tmux`

Still owns tmux integration, but its role broadens.

Owns:
- startup snapshot queries
- live control-mode stream or equivalent event stream
- capability detection
- command execution
- popup and pane launch integration
- status-line update commands
- parsing tmux events into normalized internal events

Suggested modules:
- `snapshot.rs`
- `control_mode.rs`
- `parser.rs`
- `commands.rs`
- `capabilities.rs`
- `backend.rs`

### `wisp-app`

The application orchestrator around the shared store.

Owns:
- event ingestion
- reducer calls
- task scheduling
- preview invalidation / debouncing
- mode transitions
- status refresh fan-out
- wiring between domain state and renderers

Suggested modules:
- `app.rs`
- `event_loop.rs`
- `store.rs`
- `effects.rs`
- `tasks.rs`

### `wisp-ui`

Now supports multiple render surfaces.

Owns:
- popup picker renderer
- sidebar renderer
- shared list widgets
- query input widget
- preview widget
- key translation
- sidebar compact/expanded layouts

Suggested modules:
- `picker.rs`
- `sidebar.rs`
- `components/list.rs`
- `components/preview.rs`
- `input.rs`
- `theme.rs`

### `wisp-status`

New dedicated status projection and formatting crate.

Owns:
- compact session strip view model
- badge rendering rules
- truncation and ordering
- tmux-safe escaping for final strings

Suggested modules:
- `model.rs`
- `format.rs`
- `compact.rs`
- `escape.rs`

### `wisp-preview`

No major change in purpose, but it must support session-aware previews fed by domain state rather than fresh tmux calls whenever possible.

---

## Canonical domain model

The core model should represent tmux state directly enough to support sidebar and status use cases cleanly.

```rust
pub type SessionId = String;
pub type WindowId = String;
pub type PaneId = String;
pub type ClientId = String;

pub struct DomainState {
    pub sessions: IndexMap<SessionId, SessionRecord>,
    pub clients: IndexMap<ClientId, ClientFocus>,
    pub previous_session_by_client: IndexMap<ClientId, SessionId>,
    pub directories: Vec<DirectoryEntry>,
    pub config: DomainConfig,
}

pub struct SessionRecord {
    pub id: SessionId,
    pub name: String,
    pub attached: bool,
    pub windows: IndexMap<WindowId, WindowRecord>,
    pub aggregate_alerts: AlertAggregate,
    pub has_unseen: bool,
    pub sort_key: SessionSortKey,
}

pub struct WindowRecord {
    pub id: WindowId,
    pub index: i32,
    pub name: String,
    pub active: bool,
    pub panes: IndexMap<PaneId, PaneRecord>,
    pub alerts: AlertState,
    pub has_unseen: bool,
    pub current_path: Option<PathBuf>,
    pub active_command: Option<String>,
}

pub struct PaneRecord {
    pub id: PaneId,
    pub index: i32,
    pub title: Option<String>,
    pub current_path: Option<PathBuf>,
    pub current_command: Option<String>,
    pub is_active: bool,
}

pub struct ClientFocus {
    pub session_id: SessionId,
    pub window_id: WindowId,
    pub pane_id: Option<PaneId>,
}
```

Use stable IDs where possible, but ensure there is a clean mapping from tmux-native identifiers to internal IDs.

---

## Notification model

This is the most important new domain behavior.

### Sources

Notifications can come from two classes of signals:

1. **tmux-native alerts**
   - activity
   - bell
   - silence

2. **Wisp-owned computed state**
   - unseen output since focus
   - previous session marker
   - current session marker

The computed state is necessary because tmux does not provide a high-level "session unread badge" abstraction in the shape Wisp needs.

### Recommended types

```rust
pub struct AlertState {
    pub activity: bool,
    pub bell: bool,
    pub silence: bool,
    pub unseen_output: bool,
}

pub struct AlertAggregate {
    pub any_activity: bool,
    pub any_bell: bool,
    pub any_silence: bool,
    pub any_unseen: bool,
    pub attention_count: usize,
    pub highest_priority: AttentionBadge,
}

pub enum AttentionBadge {
    None,
    Silence,
    Unseen,
    Activity,
    Bell,
}
```

### Priority rules

Use one consistent priority order across all UIs:

1. bell
2. activity
3. unseen
4. silence
5. none

This prevents mismatches between popup, sidebar, and status line.

### Semantics

#### `activity`
Raw tmux-native activity signal.

#### `bell`
Raw tmux-native bell signal.

#### `silence`
Raw tmux-native silence signal.

#### `unseen_output`
Wisp-owned state set when output changes after the user last focused the window.

Recommended behavior:
- set when output changes in a non-focused window
- clear when that window becomes focused
- optionally clear on explicit mark-read command
- aggregate to session level

---

## Focus history and previous-session behavior

Wisp should treat "jump to previous session" as a first-class behavior.

Suggested model:

```rust
pub struct FocusHistory {
    pub current_by_client: IndexMap<ClientId, SessionId>,
    pub previous_by_client: IndexMap<ClientId, SessionId>,
    pub last_focus_change_at: IndexMap<ClientId, Instant>,
}
```

Rules:
- on focus change to a different session, move current to previous
- do not overwrite previous when the focus event is redundant
- previous-session resolution is per tmux client where possible
- if client identity is unavailable, fall back to process-wide current/previous tracking

The UI should always be able to highlight:
- current session
- previous session

---

## Domain events

The original event model should be expanded into a stronger application event system.

```rust
pub enum AppEvent {
    Startup,
    Tmux(TmuxEvent),
    Ui(UiEvent),
    Preview(PreviewEvent),
    Config(ConfigEvent),
    Quit,
}
```

### `TmuxEvent`

```rust
pub enum TmuxEvent {
    SnapshotLoaded(TmuxSnapshot),

    SessionAdded(SessionData),
    SessionRemoved(SessionId),
    SessionRenamed {
        session_id: SessionId,
        new_name: String,
    },

    WindowAdded {
        session_id: SessionId,
        window: WindowData,
    },
    WindowRemoved {
        session_id: SessionId,
        window_id: WindowId,
    },
    WindowUpdated {
        session_id: SessionId,
        window: WindowData,
    },

    PaneAdded {
        window_id: WindowId,
        pane: PaneData,
    },
    PaneRemoved {
        window_id: WindowId,
        pane_id: PaneId,
    },
    PaneUpdated {
        window_id: WindowId,
        pane: PaneData,
    },

    FocusChanged {
        client_id: ClientId,
        session_id: SessionId,
        window_id: WindowId,
        pane_id: Option<PaneId>,
    },

    AlertChanged {
        window_id: WindowId,
        alerts: AlertState,
    },

    OutputChanged {
        pane_id: PaneId,
    },

    ClientAttached {
        client_id: ClientId,
        session_id: SessionId,
    },
    ClientDetached {
        client_id: ClientId,
    },
}
```

### `UiEvent`

```rust
pub enum UiEvent {
    OpenDefaultPicker,
    OpenSidebarPane,
    OpenSidebarPopup,
    CloseSidebar,
    ToggleSidebar,
    ToggleStatusLine,

    SelectNext,
    SelectPrev,
    ActivateSelected,
    JumpPreviousSession,
    MarkSelectedRead,

    FilterChanged(String),
    ToggleCompactSidebar,
    TogglePreview,
    ResizeSidebar(u16),
}
```

### `PreviewEvent`

```rust
pub enum PreviewEvent {
    Requested(PreviewKey, u64),
    Ready(PreviewKey, u64, Result<PreviewContent, PreviewError>),
}
```

---

## Reducer architecture

All domain transitions should go through a reducer-style entry point.

```rust
fn reduce(state: &mut AppState, event: AppEvent) -> Vec<AppCommand>
```

The reducer must:
- update canonical state
- recompute notification aggregates
- update current and previous session markers
- decide whether derived view models are dirty
- emit side-effect commands rather than executing them directly

Examples of emitted commands:
- open popup
- open persistent sidebar pane
- switch client to session
- update tmux status line
- refresh preview
- persist sidebar settings

This should be heavily unit tested.

---

## Revised application state split

```rust
pub struct AppState {
    pub domain: DomainState,
    pub ui: UiState,
    pub preview: PreviewState,
    pub capabilities: CapabilityState,
    pub status: StatusState,
}

pub struct UiState {
    pub mode: UiMode,
    pub query: String,
    pub selection: usize,
    pub compact_sidebar: bool,
    pub help_visible: bool,
    pub preview_enabled: bool,
    pub sidebar_visible: bool,
    pub sidebar_kind: SidebarKind,
}
```

### `UiMode`

```rust
pub enum UiMode {
    PickerPopup,
    PickerFullscreen,
    SidebarPane,
    SidebarPopup,
    BackgroundStatus,
}
```

### `SidebarKind`

```rust
pub enum SidebarKind {
    Pane,
    Popup,
}
```

This split makes it easier to keep rendering concerns out of the domain logic.

---

## Derived view layer

The derived view layer becomes critical.

### Shared list projection

Create a single list projection used by popup picker, sidebar pane, and sidebar popup.

```rust
pub struct SessionListItem {
    pub session_id: SessionId,
    pub label: String,
    pub is_current: bool,
    pub is_previous: bool,
    pub attached: bool,
    pub attention: AttentionBadge,
    pub attention_count: usize,
    pub active_window_label: Option<String>,
    pub path_hint: Option<String>,
    pub command_hint: Option<String>,
}
```

### Status projection

Create a compact projection just for the status line.

```rust
pub struct StatusSessionItem {
    pub label: String,
    pub is_current: bool,
    pub is_previous: bool,
    pub badge: AttentionBadge,
}
```

### Why separate projections

The status line should not depend on the full sidebar row model, and the sidebar should not depend on tmux formatting details.

---

## tmux integration changes

The original plan should be upgraded from a mostly request/response tmux adapter to one that supports a live stream.

### Recommended model

1. initial tmux snapshot on startup
2. long-lived event stream using control mode where feasible
3. command sink for writes

### Minimum adapter trait

```rust
#[async_trait::async_trait]
pub trait TmuxBackend {
    async fn snapshot(&self) -> Result<TmuxSnapshot, TmuxError>;
    async fn stream_events(&self) -> Result<Pin<Box<dyn Stream<Item = Result<TmuxEvent, TmuxError>> + Send>>, TmuxError>;
    async fn send(&self, cmd: TmuxCommand) -> Result<(), TmuxError>;
    async fn open_popup(&self, spec: PopupSpec) -> Result<(), TmuxError>;
    async fn open_sidebar_pane(&self, spec: SidebarPaneSpec) -> Result<(), TmuxError>;
    async fn close_sidebar_pane(&self) -> Result<(), TmuxError>;
    async fn update_status_line(&self, content: &str) -> Result<(), TmuxError>;
}
```

### Compatibility fallback

If control mode is too difficult in the first implementation, permit a temporary fallback mode:
- startup snapshot
- periodic refresh polling
- explicit hooks for focus/activity if available

But this should be treated as an implementation compromise, not the preferred design.

---

## Launch surface design

Wisp must support multiple frontends launched from tmux.

### Default picker

The default picker remains a popup unless config or tmux capability forces fullscreen.

### Sidebar pane

The sidebar pane is a long-lived narrow split.

Requirements:
- open left or right
- configurable width
- reuse shared list component
- optional preview disabled by default in compact mode
- visible current and previous session markers
- visible notification badges
- keyboard switch and close commands

### Sidebar popup

The sidebar popup is the same logical UI as the pane sidebar, launched as a popup attached to the side of the screen.

Requirements:
- same state and keybindings as pane sidebar where possible
- no duplicated list logic
- no duplicated notification logic

### Status line

The status line is an ambient renderer.

Requirements:
- use second status line when enabled
- compact and stable formatting
- update only on meaningful state changes
- no uncontrolled tmux option spam

---

## Sidebar-specific UI behavior

### Compact mode

Show only:
- session label
- current marker
- previous marker
- one badge
- maybe count if space allows

### Expanded mode

Show:
- session label
- active window name
- path or command hint
- attention badge and count
- attached/current/previous markers

### Width handling

Sidebar widths should be configurable and validated.

Recommended config rules:
- minimum width clamp
- sensible default width
- optional percentage support later
- persisted width if user resizes interactively

### Selection behavior

The sidebar should have its own selection cursor but stay synchronized with:
- current client session when opened
- explicit user navigation while open

---

## Status-line design

The status line should be intentionally compact and passive.

### Rendering goals

- quickly show which session is current
- show which session was previous
- show which sessions need attention
- avoid excessive clutter
- degrade well when many sessions exist

### Suggested compact format

Example shape:

```text
Wisp  main • dev! • api# • docs~ • ops+
```

Suggested symbols:
- `•` current
- `‹` or brackets for previous
- `!` bell
- `#` activity
- `+` unseen
- `~` silence

### Truncation policy

When there are too many sessions:
- keep current and previous visible if possible
- keep highest-priority alert sessions visible
- truncate lower-priority idle sessions first
- include overflow marker if useful

### Update policy

Do not write the status line on every event blindly.

Only update when:
- rendered content changes
- line target changes
- enabled/disabled state changes

This should be covered by integration tests with a fake backend.

---

## Config extensions

Extend the original config schema with session-aware UI settings.

```toml
[ui]
default_mode = "popup"      # popup | fullscreen | sidebar-pane | sidebar-popup
show_preview = true

[sidebar]
enabled = true
side = "left"               # left | right
width = 36
compact = false
remember = true
auto_open = false

[status]
enabled = true
line = 2
max_sessions = 8
show_previous = true
show_counts = false

[notifications]
track_unseen_output = true
clear_on_focus = true
show_silence = true
bell_priority = 100
activity_priority = 80
unseen_priority = 60
silence_priority = 20
```

### Validation rules

- sidebar width must be within safe bounds
- status line number must be supported by tmux capability detection or gracefully mapped
- priorities must be positive and distinct if the implementation requires deterministic sorting
- unsupported combinations should warn or downgrade gracefully

---

## Revised event loop architecture

The original event loop remains valid in spirit, but the runtime shape should be upgraded to support long-lived sidebars and status rendering.

### Recommended tasks

#### 1. tmux input task
- loads initial snapshot
- connects to live tmux event stream
- forwards normalized `TmuxEvent`s into the app loop

#### 2. app loop
- owns `AppState`
- applies reducer
- emits app commands / effects
- fans out derived view updates

#### 3. UI task
- runs popup or sidebar ratatui frontend
- converts key events into `UiEvent`s
- receives render snapshots

#### 4. status task
- listens for status projection changes
- formats output via `wisp-status`
- updates tmux only when content changes

#### 5. preview task pool
- handles cancellable previews
- emits `PreviewEvent::Ready`

### Channel sketch

```text
Tmux task   -> mpsc<AppEvent> -> App loop -> broadcast<RenderModel>
UI task     -> mpsc<AppEvent> -> App loop
Preview pool-> mpsc<AppEvent> -> App loop
Status task <- watch<StatusView>
```

This avoids each UI mode implementing its own tmux polling logic.

---

## Preview refactor notes

The original preview plan still applies, but now previews should prefer the canonical domain state for tmux session metadata whenever possible.

### Good preview sources

- active window name
- session attached/current flags
- path hints from domain model
- command hints from domain model

### Avoid

- querying tmux again on every cursor move to build session preview text

Session previews should be mostly projection work, not I/O.

---

## Testing additions

This extension introduces a large new body of testable behavior.

### New unit test areas

#### `wisp-core`
- alert aggregation from windows to sessions
- badge priority resolution
- unseen-output set/clear rules
- current/previous session transitions
- focus history updates
- list projection correctness
- status projection correctness
- reducer emits `UpdateStatusLine` only when needed

#### `wisp-status`
- compact session strip rendering
- symbol selection per badge
- truncation policy
- escaping session names safely for tmux
- preserving current/previous sessions under truncation

#### `wisp-app`
- opening sidebar pane changes mode correctly
- opening sidebar popup changes mode correctly
- toggling status line updates state and emits commands
- resize sidebar persists width when configured
- status updates are de-duplicated

#### `wisp-ui`
- compact and expanded sidebar row rendering
- current/previous markers render correctly
- selection movement in sidebar mode
- shared list widget behaves the same in popup and sidebar contexts

### New integration tests

#### Fake tmux backend tests

Build a deterministic fake backend that can:
- return startup snapshots
- emit synthetic focus/activity/output events
- record outgoing tmux commands

Cover:
- activity in background session updates badge and status line
- focus clears unseen state when configured
- sidebar open/close emits correct tmux pane commands
- popup sidebar mode emits popup command instead of pane command
- previous-session jump targets the correct session
- status line updates only when rendered text changes

#### Real tmux integration tests

Cover at least:
- open sidebar pane in isolated tmux server
- open sidebar popup
- switch sessions from sidebar
- status line string updated in tmux options
- focus and activity scenarios if reliable to script in CI

### New end-to-end tests

Critical scenarios:
- launch default popup picker and switch session
- launch sidebar pane and switch to previous session
- trigger activity in a non-current window and verify badge appears
- focus that session and verify unseen clears
- run with status line enabled and verify compact session strip updates
- run in compact sidebar mode and verify stable rendering under many sessions

### Property tests

Recommended areas:
- alert aggregation invariants
- truncation keeps current/previous if capacity permits
- duplicate events do not corrupt focus history
- repeated focus on same session is idempotent

### Benchmarks

Add or expand benchmarks for:
- reducer throughput under event bursts
- status-line rendering under many sessions
- list projection generation cost
- activity flood handling without excessive status rewrites
- sidebar render projection for 100+ sessions

---

## Implementation phases for the extension

### Phase E1: session core refactor

Deliverables:
- broaden `sessionx-core` / create `wisp-core`
- canonical session/window/pane domain model
- alert aggregation
- focus history and previous-session tracking
- updated reducer

Acceptance:
- pure tests cover core notification and focus semantics
- candidate list becomes a derived projection

### Phase E2: tmux live event support

Deliverables:
- snapshot + event stream tmux backend
- normalized `TmuxEvent` parsing
- fake backend for tests

Acceptance:
- app can process synthetic live tmux events without UI
- activity/focus changes update domain state correctly

### Phase E3: shared list and sidebar UI

Deliverables:
- shared `SessionListItem` projection
- sidebar ratatui renderer
- compact/expanded sidebar layouts
- popup and pane launch adapters

Acceptance:
- same list component powers popup and sidebar
- no duplicated session-state logic between picker and sidebar

### Phase E4: status-line renderer

Deliverables:
- `wisp-status` crate
- compact session strip formatter
- tmux status-line update effects
- de-duplication of repeated writes

Acceptance:
- status line reflects session attention and current/previous markers
- updates are only sent when content changes

### Phase E5: polish and hardening

Deliverables:
- config persistence for sidebar settings
- doctor/debug improvements for tmux capabilities
- benchmark coverage for event-driven usage
- full e2e scenarios

Acceptance:
- sidebar, popup, and status modes are all covered in CI
- performance remains acceptable with many sessions and frequent events

---

## Updated acceptance checklist

## Shared core
- [ ] session/window/pane state is canonical and UI-independent
- [ ] notification aggregation is implemented in the domain layer
- [ ] current and previous session tracking works per client where possible
- [ ] candidate lists are derived from canonical state, not primary state

## tmux integration
- [ ] initial snapshot loading works
- [ ] live event stream is supported or a clearly documented temporary fallback exists
- [ ] sidebar pane open/close commands are implemented
- [ ] sidebar popup launch is implemented
- [ ] status-line update command path is implemented

## Sidebar UI
- [ ] persistent pane mode works
- [ ] side popup mode works
- [ ] compact and expanded layouts are supported
- [ ] current/previous markers are visible
- [ ] notification badges are visible and consistent with status line

## Status line
- [ ] second status-line renderer exists
- [ ] formatting is compact and stable
- [ ] current, previous, and attention states are represented
- [ ] status updates are de-duplicated
- [ ] truncation behavior is tested

## Notifications
- [ ] activity, bell, silence, and unseen-output states are represented
- [ ] unseen-output clears on focus when configured
- [ ] session-level attention aggregates correctly from windows
- [ ] badge priority is consistent across all renderers

## Testing
- [ ] new unit tests cover notification and focus logic
- [ ] fake-backend integration tests cover sidebar and status behavior
- [ ] real-tmux integration tests cover pane/popup/status flows
- [ ] e2e tests cover picker, sidebar, and status workflows
- [ ] benchmark coverage exists for event burst and status rendering scenarios

---

## Guidance for the implementation agent

- Do not bolt sidebar logic directly onto the original popup picker state.
- Introduce the canonical session model first, then project popup/sidebar/status views from it.
- Keep tmux-native alert parsing separate from Wisp-owned unseen/focus semantics.
- Do not let the status-line formatter read tmux state directly; it should only receive a projected view model.
- Favor a fake tmux backend early so reducer and notification logic can be built test-first.
- If control mode is deferred, structure the tmux adapter as if live events already exist so polling can be swapped out later without architectural churn.
- Keep popup picker, sidebar pane, and sidebar popup on the same shared list projection and keybinding vocabulary as much as possible.
- Avoid per-renderer copies of sorting, filtering, or badge logic.

---

## Recommended naming update

Since the product name is now Wisp, prefer using `wisp-*` crate names in new code and docs unless a staged rename would be too disruptive.

If a staged rename is chosen, add a small note in the repository docs mapping:
- `sessionx-core` -> future `wisp-core`
- `sessionx-tmux` -> future `wisp-tmux`
- etc.

That keeps the architecture aligned with the product identity while preserving implementation pragmatism.
