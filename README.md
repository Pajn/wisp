# Wisp

Wisp is a native Rust multiplexer navigation tool inspired by `tmux-sessionx`. It shares one session-aware core across tmux, an Embers backend, sidebar surfaces, and a compact tmux status-line renderer.

## High-level features

- tmux session discovery, switching, and attachment, with optional Embers backend support
- sidebar pane and sidebar popup surfaces in addition to the main picker
- git worktree-aware picker: see only sessions for the current repo, or browse all worktrees
- zoxide-backed directory discovery
- fuzzy filtering and session previews
- configurable behavior through TOML config plus environment overrides
- strong validation with workspace-wide `fmt`, `clippy`, tests, smoke tests, and benchmark targets

## Workspace layout

- `wisp-core`: canonical session, alert, reducer, and projection logic
- `wisp-config`: config defaults, loading, merge precedence, and validation
- `wisp-tmux`: tmux snapshot/actions backend plus polling fallback
- `wisp-embers`: optional Embers snapshot/actions adapter and subscription bridge
- `wisp-zoxide`: zoxide provider and normalization
- `wisp-preview`: preview generation and cache
- `wisp-fuzzy`: matcher abstraction
- `wisp-ui`: shared ratatui renderers and key translation
- `wisp-status`: status-line formatting and dedup logic
- `wisp-app`: app-facing state assembly helpers
- `wisp`: CLI entrypoint and runtime wiring

## Quick start

Requirements:

- `tmux` for tmux-backed flows
- an Embers checkout at `../embers` only when building with `--features embers`
- `zoxide` for directory candidates
- Rust toolchain new enough for edition 2024

Install the CLI:

```bash
cargo install --git https://github.com/Pajn/wisp.git --bin wisp
```

Install the latest tagged binary with `cargo-binstall`:

```bash
cargo binstall --git https://github.com/Pajn/wisp.git wisp
```

If you want a specific revision while the project is evolving, pin a branch, tag, or commit:

```bash
cargo install --git https://github.com/Pajn/wisp.git --bin wisp --branch main
```

For a specific tagged release with `cargo-binstall`, pin the tag:

```bash
cargo binstall --git https://github.com/Pajn/wisp.git wisp --tag v0.1.0
```

Common commands after install:

```bash
wisp doctor
wisp popup
wisp popup --worktree
wisp fullscreen
wisp fullscreen --worktree
wisp sidebar-popup
wisp sidebar-pane
wisp statusline install
```

For Embers, build with the opt-in feature and select the backend with config or an environment override:

```bash
cargo install --path crates/wisp-bin --features embers
WISP_BACKEND=embers wisp fullscreen
WISP_BACKEND=embers WISP_EMBERS_SOCKET=/tmp/embers.sock wisp popup
```

Current Embers support covers the main picker, session actions, previews, live refresh, native floating `wisp popup`, floating `wisp sidebar-popup`, and root-split `wisp sidebar-pane`. `wisp statusline ...` remains tmux-only.

Use `--worktree` (or `-w`) to start the picker in worktree mode, which shows only sessions belonging to worktrees of the current repo alongside worktrees that don't yet have sessions.

Example tmux binding:

Add this to `~/.tmux.conf` to open Wisp with `prefix + o`:

```tmux
bind-key o run-shell "wisp popup"
```

If you prefer the sidebar instead of a popup:

```tmux
bind-key O run-shell "wisp sidebar-pane"
```

`wisp sidebar-pane` keeps a Wisp sidebar pane available across the windows in the
session you open or switch into, so it behaves more like a persistent file tree.

`wisp statusline install` installs a persistent tmux status row backed by
`wisp statusline render`, with native clickable session switches on tmux 3.4+
when mouse mode is enabled. Installation also adds tmux hooks so the row redraws
immediately when sessions are switched, created, renamed, or closed.

To remove it again, run `wisp statusline uninstall`.

By default the statusline renders every session in stable alphabetical order, and
tmux truncates the final row if the screen is narrower than the rendered strip.
The leading status label defaults to the Nerd Font icon `󰖔` and can be
overridden with `[status].icon`, including plain text like `"Wisp"` or `""`.

The picker and sidebar default to a recent-session order so the active session
stays first and the previously visited session stays near the top. Press `Ctrl-S`
to toggle to stable alphabetical ordering, or set `[ui].session_sort` in config.

Config file discovery:

- `$WISP_CONFIG`
- `$XDG_CONFIG_HOME/wisp/config.toml`
- `$HOME/.config/wisp/config.toml`

## Documentation

- `docs/configuration.md` - full configuration reference
- `docs/config.schema.toml` - commented TOML template you can copy into your config
- `docs/architecture.md` - architecture overview and crate responsibilities

## Quality gates

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings # requires ../embers
cargo test --workspace --all-targets
cargo test --workspace --all-targets --all-features # requires ../embers
cargo test --manifest-path crates/wisp-embers/Cargo.toml --test integration # requires ../embers
cargo test -p wisp --test smoke
cargo bench -p wisp-core --bench projections --no-run
cargo bench -p wisp-status --bench formatting --no-run
```
