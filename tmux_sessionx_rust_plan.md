# Rust Native tmux-sessionx Replacement Plan

## Goal

Build a fast, feature-rich Rust replacement for a shell-heavy `tmux-sessionx`-style workflow with:

- tmux session and window awareness
- zoxide-backed directory selection
- rich previews
- fuzzy finding
- popup / full-screen TUI operation inside tmux
- minimal process spawning
- strong test coverage
- maintainable architecture that prefers crates over handwritten subsystems

The implementation should prioritize **startup latency**, **interactive responsiveness**, **low process churn**, and **clean separation between pure logic and integration boundaries**.

---

## Non-goals

The first implementation should not try to:

- replicate every legacy shell quirk or every exact tmux-sessionx CLI flag
- reimplement all of `bat` or `fzf` behavior byte-for-byte
- support non-tmux environments as a first-class target
- depend on unstable internals of third-party tools unless clearly isolated behind an adapter
- over-optimize with unsafe code or bespoke rendering before measuring actual bottlenecks

---

## Product goals

The tool should feel as capable as the current shell solution while being noticeably faster.

### Required capabilities

- list and switch to tmux sessions
- optionally create / attach sessions from directories
- integrate with zoxide for directory candidates
- provide live filtering with fuzzy matching
- show previews for the currently selected candidate
- open in tmux popup when available
- support a full-screen fallback mode
- configurable behavior through a TOML config file
- minimal runtime dependence on shell parsing

### Performance goals

Targets should be validated in benchmarks, but the intent is:

- single startup path with no shell pipeline fanout
- tmux state loaded in a small number of calls
- zoxide state loaded at most once per launch in the baseline design
- previews rendered lazily and cancellably
- no blocking UI while computing preview content
- smooth filtering on hundreds to low-thousands of candidates

---

## High-level architecture

Use a layered architecture:

1. **Pure domain layer**
   - candidate types
   - ranking inputs
   - preview request types
   - action resolution
   - config model

2. **Application layer**
   - event loop
   - state transitions
   - command scheduling
   - async task orchestration
   - cache coordination

3. **Adapter layer**
   - tmux integration
   - zoxide integration
   - preview rendering adapters
   - filesystem access
   - terminal UI

4. **Binary layer**
   - CLI argument parsing
   - configuration loading
   - app bootstrap
   - logging / tracing initialization

This separation is important because most of the interesting behavior should be testable without requiring tmux, a real terminal, or real zoxide state.

---

## Suggested crate layout

Use a small Rust workspace. Keep pure logic isolated from runtime-heavy integrations.

```text
sessionx-rs/
├─ Cargo.toml
├─ crates/
│  ├─ sessionx-core/
│  ├─ sessionx-config/
│  ├─ sessionx-tmux/
│  ├─ sessionx-zoxide/
│  ├─ sessionx-preview/
│  ├─ sessionx-fuzzy/
│  ├─ sessionx-ui/
│  ├─ sessionx-app/
│  └─ sessionx-bin/
├─ tests/
│  ├─ integration/
│  └─ e2e/
└─ docs/
```

### `sessionx-core`

Purpose:
- pure domain models and logic

Owns:
- `Candidate`
- `CandidateKind`
- `CandidateId`
- `SessionRef`, `DirectoryRef`, `WindowRef`
- `PreviewRequest`, `PreviewContent`
- `Action`, `ResolvedAction`
- ranking inputs and normalization helpers
- pure filtering / sorting policies
- app-independent cache key types

Rules:
- no tokio
- no terminal dependencies
- no tmux process calls
- no direct filesystem reads unless abstracted via traits and used only in tests

### `sessionx-config`

Purpose:
- config schema, defaults, loading, validation, merging

Owns:
- TOML schema structs
- default config values
- merge logic: defaults + file + env + CLI overrides
- validation and helpful diagnostics

Likely deps:
- `serde`
- `toml`
- `thiserror`
- maybe `directories` / `dirs`

### `sessionx-tmux`

Purpose:
- tmux integration adapter

Owns:
- tmux client abstraction trait
- implementation via `tmux_interface` or a carefully scoped command adapter
- batching helpers for querying tmux state
- popup launch support
- action execution: switch / attach / new session / send keys if needed

Interface should expose domain-oriented methods, not raw command strings.

Example surface:
- `list_sessions()`
- `list_windows()`
- `current_context()`
- `switch_client_to_session(session_name)`
- `open_popup(command, options)`
- `supports_popup()`

### `sessionx-zoxide`

Purpose:
- fetch and normalize zoxide-backed directory candidates

Prefer a provider trait:
- `ZoxideProvider`

Initial implementation:
- single cached CLI invocation on startup or refresh

Optional later implementation:
- library-backed integration if a stable public API is suitable

### `sessionx-preview`

Purpose:
- preview generation and caching

Owns:
- preview provider traits
- file preview generation via `bat` crate where appropriate
- directory preview generation
- tmux session preview rendering
- async preview cache
- preview cancellation / dedupe logic

This crate should not own terminal widget rendering; it should return structured preview output.

### `sessionx-fuzzy`

Purpose:
- fuzzy matching adapter

Possible implementations:
- `nucleo`
- `skim`

This crate should normalize search input and expose a stable app-facing matcher API so the engine can be swapped later.

### `sessionx-ui`

Purpose:
- ratatui widgets and rendering

Owns:
- layout
- candidate list widget
- preview widget
- footer / help widget
- theme mapping
- keyboard event translation into app intents

Should avoid owning business logic.

### `sessionx-app`

Purpose:
- application state machine and event loop orchestration

Owns:
- `AppState`
- `AppEvent`
- reducers / handlers
- async command spawning
- cache coordination
- startup bootstrap flow
- refresh behavior

This is the heart of the program.

### `sessionx-bin`

Purpose:
- executable entrypoint

Owns:
- `main`
- CLI parsing
- tracing setup
- config loading
- runtime startup
- wiring concrete adapters into `sessionx-app`

---

## Candidate model

Define a unified candidate abstraction so the UI and fuzzy engine do not care where entries came from.

```rust
pub enum CandidateKind {
    TmuxSession,
    TmuxWindow,
    Directory,
    Project,
}

pub struct Candidate {
    pub id: CandidateId,
    pub kind: CandidateKind,
    pub primary_text: String,
    pub secondary_text: Option<String>,
    pub preview_key: PreviewKey,
    pub score_hints: ScoreHints,
    pub action: CandidateAction,
    pub metadata: CandidateMetadata,
}
```

### Candidate design principles

- everything shown in the picker becomes a `Candidate`
- preview lookup uses a stable `PreviewKey`
- action execution is derived from candidate kind + metadata
- fuzzy search indexes precomputed search fields
- UI does not contain source-specific branching beyond styling

### Suggested candidate metadata

For session candidates:
- session name
- attached flag
- window count
- last activity if available
- current flag

For directory candidates:
- full path
- display path
- zoxide score if available
- git root hint if available
- exists / missing flag

For window candidates, later if added:
- session name
- window index
- window name
- active flag

---

## Configuration design

Use TOML as the primary configuration format.

Suggested path:
- `~/.config/sessionx/config.toml`

### Config principles

- all stable user preferences live in TOML, not tmux options
- tmux options are read only for narrow compatibility or runtime context
- all config fields get defaults
- validation errors should identify exact bad field paths
- unknown fields should optionally warn in strict mode

### Example config sketch

```toml
[ui]
mode = "popup"               # popup | fullscreen | auto
show_help = true
preview_position = "right"   # right | bottom
preview_width = 0.55
border_style = "rounded"

[fuzzy]
engine = "nucleo"            # nucleo | skim
case_mode = "smart"

[tmux]
query_windows = false
prefer_popup = true
popup_width = "80%"
popup_height = "85%"

[zoxide]
enabled = true
mode = "query"               # query | frecency-list
max_entries = 500

[preview]
enabled = true
timeout_ms = 120
max_file_bytes = 262144
syntax_highlighting = true
cache_entries = 512

[preview.file]
line_numbers = true
truncate_long_lines = true

[actions]
enter = "open"
ctrl_s = "switch-session"
ctrl_e = "open-shell-here"

[logging]
level = "warn"
```

### Config merge order

1. hardcoded defaults
2. config file
3. environment overrides
4. CLI flags

### Validation examples

- `preview_width` must be within a safe range
- `timeout_ms` must be nonzero and below a max cap
- `popup_width` and `popup_height` must parse as percent or cells
- `engine` must map to a supported backend

---

## Tmux integration strategy

The tmux boundary is a major performance concern.

### Design goals

- avoid repeated tiny tmux queries
- use structured commands / formats
- cache tmux state for the lifetime of the picker session
- isolate all tmux interaction in one adapter crate
- support popup mode and fallback modes cleanly

### Integration options

#### Preferred path
Use a Rust tmux integration crate such as `tmux_interface` if it supports the required operations cleanly.

#### Fallback path
Use a carefully scoped direct command adapter that invokes the tmux client with explicit arguments and format strings, without shell interpolation.

This still counts as significantly better than a shell script because:
- no shell parsing
- no pipelines
- central control over call count
- robust parsing of output

### Required tmux data to load at startup

Try to load startup state in as few calls as possible.

Recommended queries:
- current client / session / pane context
- all sessions with needed metadata
- optionally all windows if enabled
- popup support / version capability once

### Caching rules

For a single run of the picker:
- tmux capability cache: immutable
- startup session list: immutable unless explicit refresh
- current context: immutable unless explicit refresh

The picker is short-lived enough that aggressive live resync is usually not worth the startup cost.

### Action execution

Provide strongly typed action methods, for example:
- switch to existing session
- create session from directory
- attach or switch client
- open popup wrapper
- detach / kill only if explicitly supported later

Do not build commands with string concatenation in the UI layer.

---

## zoxide integration strategy

### Initial version

Use a single zoxide provider call during startup or manual refresh.

This gets most of the benefit while minimizing coupling risk.

### Provider API sketch

```rust
pub trait ZoxideProvider {
    fn load_entries(&self, max_entries: usize) -> Result<Vec<DirectoryEntry>, ZoxideError>;
}
```

### Future optimization path

If a stable library API is practical, add a second implementation behind a feature flag.

### Important behavior

- normalize and deduplicate paths
- optionally drop nonexistent paths
- preserve frecency score if available
- map entries into unified `Candidate`s before passing to UI

### Failure handling

If zoxide is unavailable:
- degrade gracefully
- continue with tmux sessions only
- surface a soft warning in logs or a small status message, not a hard crash

---

## Preview architecture

Previews are often the biggest source of interactive lag after startup.

### Design goals

- preview generation must be lazy
- preview generation must be cancellable
- repeated selection churn must not queue stale work
- results should be cached by `PreviewKey`
- file preview rendering should be bounded by size and timeouts

### Preview provider model

```rust
pub trait PreviewProvider {
    fn can_preview(&self, req: &PreviewRequest) -> bool;
    async fn generate(&self, req: PreviewRequest) -> Result<PreviewContent, PreviewError>;
}
```

### Preview types

- tmux session summary preview
- directory preview
- file preview
- plain metadata preview
- error / unavailable preview

### File preview behavior

Use `bat` crate for syntax-highlighted previews where appropriate.

Rules:
- cap file size
- cap line count rendered
- short timeout for interactive responsiveness
- if highlight setup is expensive, cache syntax assets lazily and reuse
- fall back to plain text preview on unsupported content or errors

### Directory preview behavior

Reasonable options:
- show path and basic metadata
- optionally show child entries, capped
- optionally detect git repo and show branch status only if cheap or cached

Avoid expensive recursive scans on selection changes.

### Session preview behavior

Can include:
- session name
- attached / current markers
- windows count
- current window
- maybe recent windows if already available from startup query

Do not issue new tmux subprocesses on every cursor move if avoidable.

### Preview cache

Use an LRU or size-bounded map keyed by `PreviewKey`.

Cache policy:
- keep successful previews
- optionally keep lightweight failure previews briefly
- invalidate only on refresh or if source is known dynamic

---

## Fuzzy engine strategy

Wrap the fuzzy engine behind a small adapter interface.

```rust
pub trait Matcher {
    fn set_items(&mut self, items: Vec<MatchItem>);
    fn query(&mut self, input: &str) -> Vec<MatchResult>;
}
```

### Why wrap it

- decouple app from one fuzzy crate
- allow A/B benchmarking between `nucleo` and `skim`
- keep search-specific normalization in one place

### Matching behavior

Index these fields:
- primary label
- secondary label
- path segments
- session names
- optional aliases or tags later

### Recommended first backend

Use `nucleo` first if it integrates cleanly with the desired UX and performs well on candidate counts in scope.

Fallback to `skim` if maturity or ergonomics are better for the initial release.

---

## UI architecture with ratatui

The UI should be dumb enough to render state, but smart enough to handle local layout and key translation.

### Main regions

- query input
- candidate list
- preview pane
- status / footer line
- optional help area

### UI state should not own business logic

The UI may own:
- scroll offset
- viewport position
- focused pane if multiple panes are later added
- temporary help visibility

The application state should own:
- candidates
- selected index
- preview state
- pending tasks
- resolved config
- current mode

### Rendering rules

- every frame renders from immutable-ish snapshot state
- no blocking work in draw path
- preview pane renders loading state when needed
- long preview content should support scrolling later, but keep initial implementation simple

### Input model

Translate terminal events into domain-friendly intents.

Example intents:
- `MoveUp`
- `MoveDown`
- `QueryChanged(String)`
- `ConfirmSelection`
- `Refresh`
- `ToggleHelp`
- `Cancel`

---

## Event loop architecture

Use an event-driven async app with clear separation between synchronous state mutation and asynchronous side effects.

### Core event types

```rust
pub enum AppEvent {
    Startup,
    Input(UserIntent),
    TmuxStateLoaded(Result<TmuxSnapshot, AppError>),
    ZoxideLoaded(Result<Vec<DirectoryEntry>, AppError>),
    CandidatesRebuilt,
    PreviewRequested(PreviewKey),
    PreviewReady(PreviewKey, Result<PreviewContent, AppError>),
    RefreshRequested,
    ActionResolved(Result<ResolvedAction, AppError>),
    ActionCompleted(Result<(), AppError>),
    Quit,
}
```

### State sketch

```rust
pub struct AppState {
    pub mode: AppMode,
    pub config: ResolvedConfig,
    pub tmux: LoadState<TmuxSnapshot>,
    pub zoxide: LoadState<Vec<DirectoryEntry>>,
    pub candidates: Vec<Candidate>,
    pub filtered: Vec<CandidateId>,
    pub selection: usize,
    pub query: String,
    pub preview: PreviewState,
    pub status: StatusLine,
    pub pending_tasks: PendingTasks,
}
```

### Loop responsibilities

The event loop should:
- receive input events
- update state synchronously through reducer-like handlers
- spawn async jobs for loading / previews / actions
- receive task completions back as `AppEvent`s
- request redraws when relevant state changes

### Startup flow

1. load config
2. initialize UI
3. emit `Startup`
4. on `Startup`, concurrently request:
   - tmux snapshot
   - zoxide entries
5. when results arrive, rebuild unified candidates
6. select initial item
7. trigger preview for selected item

### Query change flow

1. update `state.query`
2. rerun matcher on current candidates
3. reset or preserve selection intelligently
4. trigger preview for newly selected item

### Selection move flow

1. update selection index
2. emit preview request for new `PreviewKey`
3. cancel or supersede prior in-flight preview if necessary

### Confirm flow

1. resolve action from selected candidate
2. execute via async adapter
3. on success, exit with status 0
4. on failure, surface status message and stay open

### Refresh flow

1. mark state as refreshing
2. rerun tmux and zoxide loads
3. clear / selectively invalidate caches
4. rebuild candidates
5. retrigger preview

### Cancellation / supersession

Preview tasks should be generation-tagged.

Example:
- each new preview request increments a `preview_generation`
- only the newest generation may commit to state
- older completions are discarded

This prevents stale preview rendering after rapid cursor movement.

---

## Concurrency model

Use `tokio`.

### Recommended async boundaries

Async tasks are appropriate for:
- tmux state loading
- zoxide loading
- preview generation
- action execution
- optional logging flushes if needed

### Avoid

- locking the whole app state behind a global `Mutex`
- doing expensive work in the draw loop
- ad hoc spawning everywhere without ownership tracking

### Suggested pattern

- single main event receiver loop
- background tasks communicate via event sender
- state mutations happen on one thread / task context
- bounded channels where practical

This makes behavior easier to test and reason about.

---

## Logging and observability

Use `tracing`.

### Instrument important spans

- startup
- tmux snapshot query
- zoxide provider load
- candidate rebuild
- fuzzy query execution
- preview generation by type
- action execution

### Useful fields

- candidate count
- preview key
- preview generation
- tmux query duration
- zoxide load duration
- current query length
- render frame timing if profiled

### Debug mode

Provide a way to enable structured logs without polluting normal terminal UI.

Example:
- write to a temp file
- or to stderr only in non-TUI mode

---

## Error handling

Use typed errors per crate and map them into app-facing categories.

Suggested categories:
- config error
- tmux unavailable / incompatible
- zoxide unavailable
- preview failure
- action failure
- terminal UI failure

Principles:
- recover where possible
- degrade gracefully for optional providers
- show clear user-facing status text for operational failures
- keep technical detail in logs

---

## Performance strategy

### Priority 1: reduce process spawns

- no shell pipelines
- avoid repeated tmux option lookups
- zoxide loaded once per refresh cycle
- no external `bat` process for file previews
- no external `fzf` process in the preferred architecture

### Priority 2: reduce tmux roundtrips

- batch tmux data queries
- prefer rich format strings over many tiny calls
- cache startup snapshot

### Priority 3: keep UI responsive under preview load

- preview generation async + cancellable
- bounded preview work
- cache previews

### Priority 4: keep fuzzy filtering fast

- precompute search text
- avoid rebuilding candidates unnecessarily
- benchmark both candidate rebuild and query latency

### Measurements to collect

At minimum benchmark:
- cold startup to first interactive frame
- startup with tmux only
- startup with tmux + zoxide
- query update latency at 100 / 500 / 2000 candidates
- preview latency for small and medium files
- action execution latency

---

## CLI design

Keep the CLI small and focused.

Possible commands:

```text
sessionx              # normal picker launch
sessionx popup        # force popup
sessionx fullscreen   # force fullscreen
sessionx print-config # dump effective config
sessionx doctor       # check tmux / zoxide / terminal integration
sessionx bench        # optional dev-only benchmark mode
```

### Useful flags

- `--config <path>`
- `--no-zoxide`
- `--log-level <level>`
- `--engine <nucleo|skim>`
- `--mode <popup|fullscreen|auto>`

---

## Implementation phases

## Phase 1: workspace and core domain

Deliverables:
- workspace scaffold
- core domain types
- config crate with defaults and TOML parsing
- app state skeleton
- test harness for pure logic

Acceptance:
- config can be loaded and validated
- candidate model is stable enough for next phases
- pure logic tests pass

## Phase 2: tmux snapshot and action execution

Deliverables:
- tmux adapter trait
- concrete tmux implementation
- snapshot loading
- switch / create / attach actions
- popup support detection

Acceptance:
- app can load and print tmux sessions without UI
- actions work in integration tests against a real tmux server

## Phase 3: zoxide provider and candidate unification

Deliverables:
- zoxide provider trait and initial implementation
- candidate normalization / deduplication
- rebuild pipeline from tmux + zoxide data

Acceptance:
- unified candidate list contains both sources
- duplicates and invalid paths handled predictably

## Phase 4: ratatui UI + fuzzy matching

Deliverables:
- basic TUI
- search input
- list selection
- fuzzy engine adapter
- confirm / quit / refresh flows

Acceptance:
- interactive picker usable end-to-end without previews
- filtering is responsive on target dataset sizes

## Phase 5: previews and caching

Deliverables:
- preview provider architecture
- file preview via `bat` crate
- session preview
- preview cache and cancellation logic

Acceptance:
- moving selection rapidly does not freeze UI
- stale previews do not overwrite newer ones
- cache behavior is test-covered

## Phase 6: polish, compatibility, and benchmarks

Deliverables:
- config polish
- doctor command
- benchmark suite
- logging improvements
- ergonomic keybindings and help text

Acceptance:
- measured startup and query performance documented
- key workflows verified in e2e tests

---

## Test strategy

Everything testable should be tested. Structure the code so most behavior is testable without spawning a real terminal.

### 1. Unit tests

Focus on pure logic and crate-local behavior.

#### `sessionx-core`
- candidate display normalization
- candidate deduplication policy
- action resolution
- preview key derivation
- sort / tie-break rules

#### `sessionx-config`
- default config values
- TOML parsing
- merge precedence
- validation failures with correct field paths
- backwards-compatible config evolution cases if supported

#### `sessionx-fuzzy`
- query normalization
- match ranking expectations
- engine adapter behavior on empty queries
- stable handling of duplicate labels

#### `sessionx-preview`
- preview cache hit/miss behavior
- preview truncation rules
- timeout behavior
- stale generation discard logic
- fallback plain text preview behavior

#### `sessionx-app`
- reducer/event handling
- startup state transitions
- query change behavior
- selection behavior
- refresh behavior
- action failure stays in app and updates status

### 2. Integration tests

Test crate boundaries with real dependencies where practical.

#### tmux integration tests
Use isolated tmux server sockets / temp environments.

Cover:
- listing sessions
- parsing session metadata
- switching sessions
- popup support detection logic
- handling tmux unavailable / version mismatch

#### zoxide integration tests
Prefer a fake provider for most tests, but add narrow real-path tests if feasible.

Cover:
- provider output parsing
- deduplication of repeated paths
- nonexistent path handling

#### preview integration tests
Cover:
- file preview with actual temp files
- syntax highlighting path through `bat`
- binary or oversized file fallback

### 3. UI tests

Avoid screenshot-only tests as the main strategy.

Test at two levels:

#### widget-level render snapshot tests
- render list states
- render loading preview state
- render help footer state
- render error banner state

These can use buffer snapshot assertions against ratatui buffers.

#### input translation tests
- key events map to expected intents
- unsupported keys are ignored or surfaced predictably

### 4. End-to-end tests

Use real subprocess-driven tests for the most important workflows.

Run against:
- a real tmux server on an isolated socket
- temp config directory
- optional fake zoxide command or fixture provider

Critical e2e cases:
- launch picker in fullscreen, filter, select session, switch successfully
- launch picker with zoxide enabled, select directory, create / attach session
- preview renders for selected candidate without hanging
- refresh updates candidate set
- fallback behavior when zoxide is missing
- fallback behavior when popup unsupported

### 5. Property tests

Use where valuable, especially for normalization and filtering.

Good candidates:
- path normalization idempotence
- candidate dedup invariants
- sort stability invariants
- config merge monotonicity for certain fields

### 6. Benchmark suite

Use `criterion` for component-level benchmarks.

Benchmarks to include:
- candidate rebuild from fixed tmux + zoxide fixtures
- fuzzy query latency across candidate counts
- preview rendering for representative files
- preview cache throughput
- action resolution overhead

Optionally add startup-style microbench harnesses for adapter layers.

---

## Testability requirements by design

To make testing effective, enforce these design rules:

- all external integrations behind traits or narrow adapters
- reducer/event logic pure or near-pure wherever possible
- no direct singleton globals for config or state
- UI rendering separated from event processing
- time sources abstracted where used for caching / timeout behavior
- filesystem reads abstracted where repeated logic depends on them
- preview generation returns structured content, not terminal side effects

---

## Suggested testing tools

- `tokio::test`
- `tempfile`
- `assert_matches`
- `pretty_assertions`
- `proptest`
- `criterion`
- `insta` for carefully scoped snapshot tests
- tmux subprocess integration via `std::process::Command` or a thin harness helper

Avoid overusing golden snapshots for dynamic behavior; favor semantic assertions first.

---

## Risk areas and mitigations

### Risk: tmux crate does not cover all needed functionality cleanly

Mitigation:
- keep a narrow internal `TmuxClient` trait
- allow a fallback implementation using direct tmux client invocations without shell

### Risk: zoxide library coupling is brittle

Mitigation:
- start with one-shot CLI provider
- keep provider swappable
- avoid parsing undocumented DB formats in v1

### Risk: bat integration adds heavy startup cost

Mitigation:
- initialize preview resources lazily
- keep preview highlighting optional and configurable
- benchmark plain vs highlighted preview path

### Risk: ratatui input/render loop becomes tangled with app logic

Mitigation:
- keep reducer/app loop in `sessionx-app`
- let `sessionx-ui` focus on render + key translation only

### Risk: stale preview races create flicker or incorrect panes

Mitigation:
- generation-based preview supersession
- central evented preview completion handling
- tests for rapid selection churn

---

## Acceptance checklist

## Architecture
- [ ] workspace created with proposed crate boundaries or a clearly justified equivalent
- [ ] pure logic separated from integration code
- [ ] all external tools hidden behind adapters or traits where practical
- [ ] no shell pipeline orchestration in core workflows

## Config
- [ ] TOML config supported with defaults
- [ ] config validation implemented with actionable error messages
- [ ] precedence rules covered by tests

## tmux
- [ ] tmux session listing works reliably
- [ ] popup mode works when supported
- [ ] fullscreen fallback works
- [ ] action execution avoids shell interpolation
- [ ] tmux integration covered by integration tests

## zoxide
- [ ] zoxide-backed candidates load once per startup or refresh
- [ ] absence of zoxide degrades gracefully
- [ ] path normalization and deduplication covered by tests

## UI and search
- [ ] ratatui picker renders correctly
- [ ] fuzzy search responds smoothly on representative candidate sets
- [ ] keybindings mapped and documented
- [ ] widget rendering has targeted tests

## previews
- [ ] previews are lazy
- [ ] previews are cancellable or superseded safely
- [ ] preview cache implemented and tested
- [ ] file preview uses `bat` crate or a justified alternative

## performance
- [ ] process spawns minimized and documented
- [ ] tmux calls minimized and documented
- [ ] benchmark suite added
- [ ] performance results recorded for key workflows

## testing
- [ ] unit tests added for all major pure logic modules
- [ ] integration tests added for tmux adapter
- [ ] e2e tests cover core picker flows
- [ ] benchmark coverage exists for candidate rebuild and query latency

---

## Suggested implementation notes for the agent

- Prefer shipping a narrower but fast and well-tested v1 over an overly broad compatibility layer.
- Keep the `TmuxClient`, `ZoxideProvider`, `Matcher`, and `PreviewProvider` interfaces clean and small.
- Do not let ratatui widgets call adapters directly.
- Optimize only after measuring, but design up front to avoid obvious process churn.
- When uncertain between an embedded crate and a subprocess, choose the option with the cleaner long-term abstraction boundary unless the performance difference is proven important.
- Add tracing early so performance investigations are cheap later.
- Write tests as each layer is introduced rather than backfilling at the end.

---

## Nice-to-have future extensions

Not required for v1, but worth keeping architecture-friendly:

- worktree and repo candidate sources
- MRU / recency layer across all candidate kinds
- richer tmux window browsing
- split-pane preview or detail mode
- persistent cache between launches
- user-defined custom actions
- shell completion generation
- theme customization
- remote session sources

