# Wisp

Wisp is a native Rust tmux navigation tool inspired by `tmux-sessionx`. It shares one session-aware core across a popup picker, sidebar surfaces, and a compact tmux status-line renderer.

## High-level features

- tmux-aware session discovery, switching, and attachment
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
- `wisp-zoxide`: zoxide provider and normalization
- `wisp-preview`: preview generation and cache
- `wisp-fuzzy`: matcher abstraction
- `wisp-ui`: shared ratatui renderers and key translation
- `wisp-status`: status-line formatting and dedup logic
- `wisp-app`: app-facing state assembly helpers
- `wisp-bin`: CLI entrypoint and runtime wiring

## Quick start

Requirements:

- `tmux`
- `zoxide` for directory candidates
- Rust toolchain new enough for edition 2024

Install the CLI:

```bash
cargo install --git https://github.com/Pajn/wisp.git --bin wisp
```

If you want a specific revision while the project is evolving, pin a branch, tag, or commit:

```bash
cargo install --git https://github.com/Pajn/wisp.git --bin wisp --branch main
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
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo test -p wisp-bin --test smoke
cargo bench -p wisp-core --bench projections --no-run
cargo bench -p wisp-status --bench formatting --no-run
```
