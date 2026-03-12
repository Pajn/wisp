# AGENTS.md

Wisp is a Rust 2024 tmux navigation workspace built around one canonical session model in `wisp-core`, thin integration adapters (`wisp-tmux`, `wisp-zoxide`, `wisp-preview`), and multiple projections/renderers (`wisp-ui`, `wisp-status`, `wisp-bin`). Keep logic in the lowest pure crate that can own it.

## Testing expectations

- Always keep these green before handing off:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-targets`
- If you touch tmux integration, run the real isolated-socket tests in `crates/wisp-tmux/tests/integration.rs`.
- If you touch CLI behavior, run `cargo test -p wisp-bin --test smoke`.
- If you touch hot-path projections or status formatting, make sure the benches still compile.

## Performance expectations

- Prefer pure projections and reducers over extra subprocess work.
- Avoid adding shell pipelines or repeated tmux/zoxide queries on the interactive path.
- Preserve fast startup, lazy preview generation, and smooth filtering on hundreds to low-thousands of candidates.
- Measure hot paths with the existing Criterion benches before adding complexity.
