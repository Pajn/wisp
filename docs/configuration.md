# Wisp Configuration Reference

Wisp loads configuration by merging sources in this order:

1. built-in defaults
2. TOML config file
3. environment overrides
4. CLI overrides from the config library API

The `wisp` CLI uses built-in environment support and config-file discovery, but it does not yet expose public CLI flags for config overrides.

## Config file location

Wisp looks for a config file in this order:

1. `WISP_CONFIG`
2. `$XDG_CONFIG_HOME/wisp/config.toml`
3. `$HOME/.config/wisp/config.toml`

If no default-path config file exists, Wisp continues with defaults. If `WISP_CONFIG` points at a missing file, loading fails.

## Strict mode

The config library supports a strict mode that rejects unknown TOML keys using `serde_ignored`. This is useful for editor integrations or tests that want schema-like validation.

## Value formats

- `tmux.popup_width` and `tmux.popup_height` accept either percentages like `"80%"` or cell counts like `"40"`.
- `ui.preview_width` is a float from `0.2` to `0.8`.
- Boolean environment overrides accept `1`, `true`, `yes`, `on`, `0`, `false`, `no`, or `off`.

## Sections and keys

### `[ui]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `mode` | string | `"auto"` | `auto`, `popup`, `fullscreen` | Preferred surface mode. |
| `show_help` | bool | `true` | `true`, `false` | Enables the inline help region when the surface supports it. |
| `preview_position` | string | `"right"` | `right`, `bottom` | Placement of the preview pane in picker layouts. |
| `preview_width` | float | `0.55` | `0.2..=0.8` | Width share for the preview pane. |
| `border_style` | string | `"rounded"` | `plain`, `rounded`, `double`, `thick` | Shared renderer border style. |

### `[fuzzy]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `engine` | string | `"nucleo"` | `nucleo`, `skim` | Matcher backend selection. |
| `case_mode` | string | `"smart"` | `ignore`, `respect`, `smart` | Case-sensitivity strategy for fuzzy matching. |

### `[tmux]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `query_windows` | bool | `false` | `true`, `false` | Enables extra tmux window querying when needed. |
| `prefer_popup` | bool | `true` | `true`, `false` | Prefer popup UI when tmux supports it. |
| `popup_width` | string | `"80%"` | percent or cells | Popup width, for example `"80%"` or `"120"`. |
| `popup_height` | string | `"85%"` | percent or cells | Popup height, for example `"85%"` or `"40"`. |

### `[zoxide]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `enabled` | bool | `true` | `true`, `false` | Enables zoxide-backed directory candidates. |
| `mode` | string | `"query"` | `query`, `frecency-list` | zoxide fetch mode. |
| `max_entries` | integer | `500` | `> 0` | Upper bound on loaded directory candidates. |

### `[preview]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `enabled` | bool | `true` | `true`, `false` | Global preview toggle. |
| `timeout_ms` | integer | `120` | `1..=5000` | Preview work budget in milliseconds. |
| `max_file_bytes` | integer | `262144` | `> 0` | Maximum file size read for previews. |
| `syntax_highlighting` | bool | `true` | `true`, `false` | Enables syntax-oriented rendering behavior. |
| `cache_entries` | integer | `512` | `> 0` | Preview cache capacity. |

### `[preview.file]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `line_numbers` | bool | `true` | `true`, `false` | Shows line numbers in file previews. |
| `truncate_long_lines` | bool | `true` | `true`, `false` | Truncates long lines in plain-text file previews. |

### `[actions]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `enter` | string | `"open"` | `open`, `close-session`, `toggle-preview`, `toggle-details`, `toggle-compact-sidebar`, `close` | Action bound to Enter. |
| `ctrl_x` | string | `"close-session"` | same as above | Action bound to Ctrl-X. |
| `ctrl_p` | string | `"toggle-preview"` | same as above | Action bound to Ctrl-P. |
| `ctrl_d` | string | `"toggle-details"` | same as above | Action bound to Ctrl-D. |
| `ctrl_m` | string | `"toggle-compact-sidebar"` | same as above | Action bound to Ctrl-M. |
| `esc` | string | `"close"` | same as above | Action bound to Escape. |
| `ctrl_c` | string | `"close"` | same as above | Action bound to Ctrl-C. |

These bindings control the picker shortcuts Wisp shows in its inline help footer. Navigation keys like arrow keys, `Ctrl-J`, `Ctrl-K`, typing to filter, and Backspace remain built in.

### `[logging]`

| Key | Type | Default | Valid values | Notes |
| --- | --- | --- | --- | --- |
| `level` | string | `"warn"` | `error`, `warn`, `info`, `debug`, `trace` | Log verbosity. |

## Environment overrides

These environment variables are recognized today:

| Variable | Effect |
| --- | --- |
| `WISP_CONFIG` | Overrides the config file path. |
| `WISP_MODE` or `WISP_UI_MODE` | Overrides `ui.mode`. |
| `WISP_ENGINE` or `WISP_FUZZY_ENGINE` | Overrides `fuzzy.engine`. |
| `WISP_LOG_LEVEL` | Overrides `logging.level`. |
| `WISP_PREVIEW_ENABLED` | Overrides `preview.enabled`. |
| `WISP_TMUX_PREFER_POPUP` | Overrides `tmux.prefer_popup`. |
| `WISP_NO_ZOXIDE` | Disables zoxide by forcing `zoxide.enabled = false`. |

## Example

```toml
[ui]
mode = "popup"
preview_position = "right"
preview_width = 0.6

[tmux]
prefer_popup = true
popup_width = "80%"
popup_height = "85%"

[zoxide]
enabled = true
max_entries = 500

[preview]
enabled = true
timeout_ms = 120

[logging]
level = "warn"
```
