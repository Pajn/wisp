# Wisp Architecture Overview

Wisp uses one canonical session-aware domain model and derives multiple tmux-facing surfaces from it. The project is intentionally split so pure behavior is easy to test and integration-heavy code stays narrow.

## Core design rule

There is exactly one canonical in-memory model for:

- tmux sessions, windows, and panes
- current and previous focus
- alert and unseen-output state
- derived candidate and status projections

Everything else is built on top of that shared state.

## Layering

### 1. Domain and projection layer

`wisp-core` owns the canonical model, reducer logic, alert semantics, and derived views such as:

- session list items for UI surfaces
- candidate projections
- compact status items

This layer stays free of tmux I/O, terminal rendering, and subprocess management.

### 2. Integration adapters

- `wisp-tmux` loads snapshots, issues switch/attach commands, opens popups or panes, and updates tmux status lines.
- `wisp-zoxide` loads and normalizes directory candidates.
- `wisp-preview` generates session and filesystem previews with a bounded cache.

These crates translate external systems into domain-friendly data and commands.

### 3. Rendering and interaction

- `wisp-ui` renders picker and sidebar variants with shared ratatui components.
- `wisp-status` formats compact tmux-safe status strings and suppresses duplicate updates within a process lifetime.
- `wisp-fuzzy` exposes a stable matcher interface for filtering.

These crates consume projections rather than owning session state directly.

### 4. Runtime wiring

`wisp-app` assembles candidate sources into domain state, while the `wisp-bin` crate powers the installed `wisp` CLI that loads config, creates adapters, runs TUI surfaces, and exposes top-level commands like `doctor`, `popup`, `sidebar-pane`, and `status-line`.

## Data flow

Typical flow:

1. `wisp-bin` loads config and asks `wisp-tmux` for a snapshot.
2. `wisp-zoxide` contributes directory data.
3. `wisp-app` builds domain state.
4. `wisp-core` derives session lists, candidates, previews, and status projections.
5. `wisp-ui` or `wisp-status` renders the selected projection.
6. User actions go back through `wisp-tmux` to switch sessions, attach clients, or open tmux surfaces.

## Why this shape

This structure keeps Wisp aligned with its main goals:

- fast startup with minimal process churn
- one place for focus and alert semantics
- thin UI surfaces that stay behaviorally consistent
- pure logic that can be unit-tested without tmux
- integration behavior that can still be verified against real isolated tmux sockets

## Testing and performance strategy

The workspace uses a mix of:

- unit tests for reducers, projections, formatting, and parsing
- real tmux integration tests for backend behavior
- CLI smoke tests for shipped commands
- Criterion bench targets for hot paths like projections and status formatting

When changing behavior, prefer adding coverage in the crate that owns the logic instead of relying on broad end-to-end checks alone.
