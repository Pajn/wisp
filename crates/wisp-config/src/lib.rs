use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedConfig {
    pub ui: UiConfig,
    pub fuzzy: FuzzyConfig,
    pub tmux: TmuxConfig,
    pub status: StatusConfig,
    pub zoxide: ZoxideConfig,
    pub preview: PreviewConfig,
    pub actions: ActionsConfig,
    pub logging: LoggingConfig,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            ui: UiConfig {
                mode: UiMode::Auto,
                show_help: true,
                preview_position: PreviewPosition::Right,
                preview_width: 0.55,
                border_style: BorderStyle::Rounded,
                session_sort: SessionSortMode::Recent,
            },
            fuzzy: FuzzyConfig {
                engine: FuzzyEngine::Nucleo,
                case_mode: CaseMode::Smart,
            },
            tmux: TmuxConfig {
                query_windows: false,
                prefer_popup: true,
                popup_width: Dimension::Percent(80),
                popup_height: Dimension::Percent(85),
            },
            status: StatusConfig {
                line: 2,
                interactive: true,
                icon: "󰖔".to_string(),
                max_sessions: None,
                show_previous: true,
            },
            zoxide: ZoxideConfig {
                enabled: true,
                mode: ZoxideMode::Query,
                max_entries: 500,
            },
            preview: PreviewConfig {
                enabled: true,
                timeout_ms: 120,
                max_file_bytes: 262_144,
                syntax_highlighting: true,
                cache_entries: 512,
                file: FilePreviewConfig {
                    line_numbers: true,
                    truncate_long_lines: true,
                },
            },
            actions: ActionsConfig {
                down: KeyAction::MoveDown,
                up: KeyAction::MoveUp,
                ctrl_j: KeyAction::MoveDown,
                ctrl_k: KeyAction::MoveUp,
                enter: KeyAction::Open,
                shift_enter: KeyAction::CreateSessionFromQuery,
                backspace: KeyAction::Backspace,
                ctrl_r: KeyAction::RenameSession,
                ctrl_s: KeyAction::ToggleSort,
                ctrl_x: KeyAction::CloseSession,
                ctrl_p: KeyAction::TogglePreview,
                ctrl_d: KeyAction::ToggleDetails,
                ctrl_m: KeyAction::ToggleCompactSidebar,
                ctrl_w: KeyAction::ToggleWorktreeMode,
                esc: KeyAction::Close,
                ctrl_c: KeyAction::Close,
            },
            logging: LoggingConfig {
                level: LogLevel::Warn,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiConfig {
    pub mode: UiMode,
    pub show_help: bool,
    pub preview_position: PreviewPosition,
    pub preview_width: f32,
    pub border_style: BorderStyle,
    pub session_sort: SessionSortMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyConfig {
    pub engine: FuzzyEngine,
    pub case_mode: CaseMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxConfig {
    pub query_windows: bool,
    pub prefer_popup: bool,
    pub popup_width: Dimension,
    pub popup_height: Dimension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusConfig {
    pub line: usize,
    pub interactive: bool,
    pub icon: String,
    pub max_sessions: Option<usize>,
    pub show_previous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoxideConfig {
    pub enabled: bool,
    pub mode: ZoxideMode,
    pub max_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewConfig {
    pub enabled: bool,
    pub timeout_ms: u64,
    pub max_file_bytes: usize,
    pub syntax_highlighting: bool,
    pub cache_entries: usize,
    pub file: FilePreviewConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePreviewConfig {
    pub line_numbers: bool,
    pub truncate_long_lines: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionsConfig {
    pub down: KeyAction,
    pub up: KeyAction,
    pub ctrl_j: KeyAction,
    pub ctrl_k: KeyAction,
    pub enter: KeyAction,
    pub shift_enter: KeyAction,
    pub backspace: KeyAction,
    pub ctrl_r: KeyAction,
    pub ctrl_s: KeyAction,
    pub ctrl_x: KeyAction,
    pub ctrl_p: KeyAction,
    pub ctrl_d: KeyAction,
    pub ctrl_m: KeyAction,
    pub ctrl_w: KeyAction,
    pub esc: KeyAction,
    pub ctrl_c: KeyAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingConfig {
    pub level: LogLevel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    pub config_path: Option<PathBuf>,
    pub mode: Option<UiMode>,
    pub engine: Option<FuzzyEngine>,
    pub log_level: Option<LogLevel>,
    pub no_zoxide: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOptions {
    pub config_path: Option<PathBuf>,
    pub strict: bool,
    pub cli_overrides: CliOverrides,
    pub env_overrides: BTreeMap<String, String>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            config_path: None,
            strict: false,
            cli_overrides: CliOverrides::default(),
            env_overrides: env::vars().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiMode {
    Popup,
    Fullscreen,
    #[default]
    Auto,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewPosition {
    #[default]
    Right,
    Bottom,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BorderStyle {
    Plain,
    #[default]
    Rounded,
    Double,
    Thick,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FuzzyEngine {
    #[default]
    Nucleo,
    Skim,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseMode {
    Ignore,
    Respect,
    #[default]
    Smart,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZoxideMode {
    #[default]
    Query,
    FrecencyList,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionSortMode {
    #[default]
    Recent,
    Alphabetical,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyAction {
    MoveDown,
    MoveUp,
    #[default]
    Open,
    CreateSessionFromQuery,
    Backspace,
    RenameSession,
    ToggleSort,
    CloseSession,
    TogglePreview,
    ToggleDetails,
    ToggleCompactSidebar,
    ToggleWorktreeMode,
    Close,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    Error,
    #[default]
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dimension {
    Percent(u8),
    Cells(u16),
}

impl Default for Dimension {
    fn default() -> Self {
        Self::Percent(80)
    }
}

impl FromStr for Dimension {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(percent) = value.strip_suffix('%') {
            let parsed = percent
                .parse::<u8>()
                .map_err(|_| "must be a valid percent")?;
            if (1..=100).contains(&parsed) {
                Ok(Self::Percent(parsed))
            } else {
                Err("percent must be between 1 and 100")
            }
        } else {
            let parsed = value
                .parse::<u16>()
                .map_err(|_| "must be a positive cell count")?;
            if parsed == 0 {
                Err("cell count must be greater than zero")
            } else {
                Ok(Self::Cells(parsed))
            }
        }
    }
}

macro_rules! impl_from_str_for_enum {
    ($ty:ty { $($name:literal => $variant:expr),+ $(,)? }) => {
        impl FromStr for $ty {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value.trim().to_ascii_lowercase().as_str() {
                    $($name => Ok($variant),)+
                    _ => Err(format!("unsupported value `{value}`")),
                }
            }
        }
    };
}

impl_from_str_for_enum!(UiMode {
    "popup" => UiMode::Popup,
    "fullscreen" => UiMode::Fullscreen,
    "auto" => UiMode::Auto,
});
impl_from_str_for_enum!(FuzzyEngine {
    "nucleo" => FuzzyEngine::Nucleo,
    "skim" => FuzzyEngine::Skim,
});
impl_from_str_for_enum!(SessionSortMode {
    "recent" => SessionSortMode::Recent,
    "alphabetical" => SessionSortMode::Alphabetical,
});
impl_from_str_for_enum!(LogLevel {
    "error" => LogLevel::Error,
    "warn" => LogLevel::Warn,
    "info" => LogLevel::Info,
    "debug" => LogLevel::Debug,
    "trace" => LogLevel::Trace,
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
}

impl ValidationErrors {
    #[must_use]
    pub fn new(errors: Vec<ValidationError>) -> Self {
        Self { errors }
    }

    pub fn iter(&self) -> impl Iterator<Item = &ValidationError> {
        self.errors.iter()
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index > 0 {
                writeln!(formatter)?;
            }
            write!(formatter, "{}: {}", error.path, error.message)?;
        }

        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config from {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse config{path_suffix}: {source}")]
    Parse {
        path_suffix: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("unknown config fields: {fields:?}")]
    UnknownFields { fields: Vec<String> },
    #[error("invalid environment override {key}: {message}")]
    InvalidEnvironment { key: String, message: String },
    #[error("invalid configuration:\n{0}")]
    Validation(ValidationErrors),
}

#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    if let Ok(config_home) = env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(config_home).join("wisp/config.toml"));
    }

    env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config/wisp/config.toml"))
}

pub fn load_config(options: &LoadOptions) -> Result<ResolvedConfig, ConfigError> {
    let selected_path = options
        .config_path
        .clone()
        .or_else(|| options.cli_overrides.config_path.clone())
        .or_else(|| options.env_overrides.get("WISP_CONFIG").map(PathBuf::from))
        .or_else(default_config_path);

    let is_default_path = options.config_path.is_none()
        && options.cli_overrides.config_path.is_none()
        && !options.env_overrides.contains_key("WISP_CONFIG");

    let config_text = match selected_path {
        Some(path) if path.exists() => Some(read_config(&path)?),
        Some(_) if is_default_path => None,
        Some(path) => {
            return Err(ConfigError::Io {
                path,
                source: io::Error::new(io::ErrorKind::NotFound, "config file not found"),
            });
        }
        None => None,
    };

    resolve_config(
        config_text.as_deref(),
        &options.env_overrides,
        &options.cli_overrides,
        options.strict,
    )
}

pub fn resolve_config(
    file_toml: Option<&str>,
    env_overrides: &BTreeMap<String, String>,
    cli_overrides: &CliOverrides,
    strict: bool,
) -> Result<ResolvedConfig, ConfigError> {
    let mut merged = PartialConfig::default();

    if let Some(input) = file_toml {
        merged.merge(parse_partial_config(input, strict)?);
    }

    merged.merge(PartialConfig::from_environment(env_overrides)?);
    merged.merge(PartialConfig::from_cli(cli_overrides));

    merged.resolve()
}

fn read_config(path: &Path) -> Result<String, ConfigError> {
    fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_partial_config(input: &str, strict: bool) -> Result<PartialConfig, ConfigError> {
    if strict {
        let mut unknown_fields = Vec::new();
        let deserializer = toml::Deserializer::new(input);
        let parsed = serde_ignored::deserialize(deserializer, |path| {
            unknown_fields.push(path.to_string());
        })
        .map_err(|source| ConfigError::Parse {
            path_suffix: String::new(),
            source,
        })?;

        if unknown_fields.is_empty() {
            Ok(parsed)
        } else {
            Err(ConfigError::UnknownFields {
                fields: unknown_fields,
            })
        }
    } else {
        toml::from_str(input).map_err(|source| ConfigError::Parse {
            path_suffix: String::new(),
            source,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialConfig {
    ui: PartialUiConfig,
    fuzzy: PartialFuzzyConfig,
    tmux: PartialTmuxConfig,
    status: PartialStatusConfig,
    zoxide: PartialZoxideConfig,
    preview: PartialPreviewConfig,
    actions: PartialActionsConfig,
    logging: PartialLoggingConfig,
}

impl PartialConfig {
    fn merge(&mut self, other: Self) {
        self.ui.merge(other.ui);
        self.fuzzy.merge(other.fuzzy);
        self.tmux.merge(other.tmux);
        self.status.merge(other.status);
        self.zoxide.merge(other.zoxide);
        self.preview.merge(other.preview);
        self.actions.merge(other.actions);
        self.logging.merge(other.logging);
    }

    fn from_environment(env_overrides: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let mut config = Self::default();

        if let Some(value) = env_overrides
            .get("WISP_MODE")
            .or_else(|| env_overrides.get("WISP_UI_MODE"))
        {
            config.ui.mode =
                Some(
                    value
                        .parse()
                        .map_err(|message| ConfigError::InvalidEnvironment {
                            key: "WISP_MODE".to_string(),
                            message,
                        })?,
                );
        }

        if let Some(value) = env_overrides
            .get("WISP_ENGINE")
            .or_else(|| env_overrides.get("WISP_FUZZY_ENGINE"))
        {
            config.fuzzy.engine =
                Some(
                    value
                        .parse()
                        .map_err(|message| ConfigError::InvalidEnvironment {
                            key: "WISP_ENGINE".to_string(),
                            message,
                        })?,
                );
        }

        if let Some(value) = env_overrides.get("WISP_LOG_LEVEL") {
            config.logging.level =
                Some(
                    value
                        .parse()
                        .map_err(|message| ConfigError::InvalidEnvironment {
                            key: "WISP_LOG_LEVEL".to_string(),
                            message,
                        })?,
                );
        }

        if let Some(value) = env_overrides.get("WISP_PREVIEW_ENABLED") {
            config.preview.enabled = Some(parse_bool("WISP_PREVIEW_ENABLED", value)?);
        }

        if let Some(value) = env_overrides.get("WISP_TMUX_PREFER_POPUP") {
            config.tmux.prefer_popup = Some(parse_bool("WISP_TMUX_PREFER_POPUP", value)?);
        }

        if let Some(value) = env_overrides.get("WISP_NO_ZOXIDE") {
            config.zoxide.enabled = Some(!parse_bool("WISP_NO_ZOXIDE", value)?);
        }

        Ok(config)
    }

    fn from_cli(cli_overrides: &CliOverrides) -> Self {
        let mut config = Self::default();
        config.ui.mode = cli_overrides.mode;
        config.fuzzy.engine = cli_overrides.engine;
        config.logging.level = cli_overrides.log_level;
        if cli_overrides.no_zoxide {
            config.zoxide.enabled = Some(false);
        }
        config
    }

    fn resolve(self) -> Result<ResolvedConfig, ConfigError> {
        let mut config = ResolvedConfig::default();
        let mut errors = Vec::new();

        if let Some(mode) = self.ui.mode {
            config.ui.mode = mode;
        }
        if let Some(show_help) = self.ui.show_help {
            config.ui.show_help = show_help;
        }
        if let Some(preview_position) = self.ui.preview_position {
            config.ui.preview_position = preview_position;
        }
        if let Some(preview_width) = self.ui.preview_width {
            config.ui.preview_width = preview_width;
        }
        if let Some(border_style) = self.ui.border_style {
            config.ui.border_style = border_style;
        }
        if let Some(session_sort) = self.ui.session_sort {
            config.ui.session_sort = session_sort;
        }

        if let Some(engine) = self.fuzzy.engine {
            config.fuzzy.engine = engine;
        }
        if let Some(case_mode) = self.fuzzy.case_mode {
            config.fuzzy.case_mode = case_mode;
        }

        if let Some(query_windows) = self.tmux.query_windows {
            config.tmux.query_windows = query_windows;
        }
        if let Some(prefer_popup) = self.tmux.prefer_popup {
            config.tmux.prefer_popup = prefer_popup;
        }
        if let Some(value) = self.tmux.popup_width {
            match value.parse() {
                Ok(parsed) => config.tmux.popup_width = parsed,
                Err(message) => errors.push(ValidationError {
                    path: "tmux.popup_width".to_string(),
                    message: message.to_string(),
                }),
            }
        }
        if let Some(value) = self.tmux.popup_height {
            match value.parse() {
                Ok(parsed) => config.tmux.popup_height = parsed,
                Err(message) => errors.push(ValidationError {
                    path: "tmux.popup_height".to_string(),
                    message: message.to_string(),
                }),
            }
        }

        if let Some(line) = self.status.line {
            config.status.line = line;
        }
        if let Some(interactive) = self.status.interactive {
            config.status.interactive = interactive;
        }
        if let Some(icon) = self.status.icon {
            config.status.icon = icon;
        }
        config.status.max_sessions = self.status.max_sessions;
        if let Some(show_previous) = self.status.show_previous {
            config.status.show_previous = show_previous;
        }

        if let Some(enabled) = self.zoxide.enabled {
            config.zoxide.enabled = enabled;
        }
        if let Some(mode) = self.zoxide.mode {
            config.zoxide.mode = mode;
        }
        if let Some(max_entries) = self.zoxide.max_entries {
            config.zoxide.max_entries = max_entries;
        }

        if let Some(enabled) = self.preview.enabled {
            config.preview.enabled = enabled;
        }
        if let Some(timeout_ms) = self.preview.timeout_ms {
            config.preview.timeout_ms = timeout_ms;
        }
        if let Some(max_file_bytes) = self.preview.max_file_bytes {
            config.preview.max_file_bytes = max_file_bytes;
        }
        if let Some(syntax_highlighting) = self.preview.syntax_highlighting {
            config.preview.syntax_highlighting = syntax_highlighting;
        }
        if let Some(cache_entries) = self.preview.cache_entries {
            config.preview.cache_entries = cache_entries;
        }
        if let Some(line_numbers) = self.preview.file.line_numbers {
            config.preview.file.line_numbers = line_numbers;
        }
        if let Some(truncate_long_lines) = self.preview.file.truncate_long_lines {
            config.preview.file.truncate_long_lines = truncate_long_lines;
        }

        if let Some(enter) = self.actions.enter {
            config.actions.enter = enter;
        }
        if let Some(down) = self.actions.down {
            config.actions.down = down;
        }
        if let Some(up) = self.actions.up {
            config.actions.up = up;
        }
        if let Some(ctrl_j) = self.actions.ctrl_j {
            config.actions.ctrl_j = ctrl_j;
        }
        if let Some(ctrl_k) = self.actions.ctrl_k {
            config.actions.ctrl_k = ctrl_k;
        }
        if let Some(shift_enter) = self.actions.shift_enter {
            config.actions.shift_enter = shift_enter;
        }
        if let Some(backspace) = self.actions.backspace {
            config.actions.backspace = backspace;
        }
        if let Some(ctrl_r) = self.actions.ctrl_r {
            config.actions.ctrl_r = ctrl_r;
        }
        if let Some(ctrl_s) = self.actions.ctrl_s {
            config.actions.ctrl_s = ctrl_s;
        }
        if let Some(ctrl_x) = self.actions.ctrl_x {
            config.actions.ctrl_x = ctrl_x;
        }
        if let Some(ctrl_p) = self.actions.ctrl_p {
            config.actions.ctrl_p = ctrl_p;
        }
        if let Some(ctrl_d) = self.actions.ctrl_d {
            config.actions.ctrl_d = ctrl_d;
        }
        if let Some(ctrl_m) = self.actions.ctrl_m {
            config.actions.ctrl_m = ctrl_m;
        }
        if let Some(ctrl_w) = self.actions.ctrl_w {
            config.actions.ctrl_w = ctrl_w;
        }
        if let Some(esc) = self.actions.esc {
            config.actions.esc = esc;
        }
        if let Some(ctrl_c) = self.actions.ctrl_c {
            config.actions.ctrl_c = ctrl_c;
        }

        if let Some(level) = self.logging.level {
            config.logging.level = level;
        }

        validate_config(&config, &mut errors);

        if errors.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError::Validation(ValidationErrors::new(errors)))
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialUiConfig {
    mode: Option<UiMode>,
    show_help: Option<bool>,
    preview_position: Option<PreviewPosition>,
    preview_width: Option<f32>,
    border_style: Option<BorderStyle>,
    session_sort: Option<SessionSortMode>,
}

impl PartialUiConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.mode, other.mode);
        merge_option(&mut self.show_help, other.show_help);
        merge_option(&mut self.preview_position, other.preview_position);
        merge_option(&mut self.preview_width, other.preview_width);
        merge_option(&mut self.border_style, other.border_style);
        merge_option(&mut self.session_sort, other.session_sort);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialFuzzyConfig {
    engine: Option<FuzzyEngine>,
    case_mode: Option<CaseMode>,
}

impl PartialFuzzyConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.engine, other.engine);
        merge_option(&mut self.case_mode, other.case_mode);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialTmuxConfig {
    query_windows: Option<bool>,
    prefer_popup: Option<bool>,
    popup_width: Option<String>,
    popup_height: Option<String>,
}

impl PartialTmuxConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.query_windows, other.query_windows);
        merge_option(&mut self.prefer_popup, other.prefer_popup);
        merge_option(&mut self.popup_width, other.popup_width);
        merge_option(&mut self.popup_height, other.popup_height);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialStatusConfig {
    line: Option<usize>,
    interactive: Option<bool>,
    icon: Option<String>,
    max_sessions: Option<usize>,
    show_previous: Option<bool>,
}

impl PartialStatusConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.line, other.line);
        merge_option(&mut self.interactive, other.interactive);
        merge_option(&mut self.icon, other.icon);
        merge_option(&mut self.max_sessions, other.max_sessions);
        merge_option(&mut self.show_previous, other.show_previous);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialZoxideConfig {
    enabled: Option<bool>,
    mode: Option<ZoxideMode>,
    max_entries: Option<usize>,
}

impl PartialZoxideConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.enabled, other.enabled);
        merge_option(&mut self.mode, other.mode);
        merge_option(&mut self.max_entries, other.max_entries);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialPreviewConfig {
    enabled: Option<bool>,
    timeout_ms: Option<u64>,
    max_file_bytes: Option<usize>,
    syntax_highlighting: Option<bool>,
    cache_entries: Option<usize>,
    file: PartialFilePreviewConfig,
}

impl PartialPreviewConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.enabled, other.enabled);
        merge_option(&mut self.timeout_ms, other.timeout_ms);
        merge_option(&mut self.max_file_bytes, other.max_file_bytes);
        merge_option(&mut self.syntax_highlighting, other.syntax_highlighting);
        merge_option(&mut self.cache_entries, other.cache_entries);
        self.file.merge(other.file);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialFilePreviewConfig {
    line_numbers: Option<bool>,
    truncate_long_lines: Option<bool>,
}

impl PartialFilePreviewConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.line_numbers, other.line_numbers);
        merge_option(&mut self.truncate_long_lines, other.truncate_long_lines);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialActionsConfig {
    down: Option<KeyAction>,
    up: Option<KeyAction>,
    ctrl_j: Option<KeyAction>,
    ctrl_k: Option<KeyAction>,
    enter: Option<KeyAction>,
    shift_enter: Option<KeyAction>,
    backspace: Option<KeyAction>,
    ctrl_r: Option<KeyAction>,
    ctrl_s: Option<KeyAction>,
    ctrl_x: Option<KeyAction>,
    ctrl_p: Option<KeyAction>,
    ctrl_d: Option<KeyAction>,
    ctrl_m: Option<KeyAction>,
    ctrl_w: Option<KeyAction>,
    esc: Option<KeyAction>,
    ctrl_c: Option<KeyAction>,
}

impl PartialActionsConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.down, other.down);
        merge_option(&mut self.up, other.up);
        merge_option(&mut self.ctrl_j, other.ctrl_j);
        merge_option(&mut self.ctrl_k, other.ctrl_k);
        merge_option(&mut self.enter, other.enter);
        merge_option(&mut self.shift_enter, other.shift_enter);
        merge_option(&mut self.backspace, other.backspace);
        merge_option(&mut self.ctrl_r, other.ctrl_r);
        merge_option(&mut self.ctrl_s, other.ctrl_s);
        merge_option(&mut self.ctrl_x, other.ctrl_x);
        merge_option(&mut self.ctrl_p, other.ctrl_p);
        merge_option(&mut self.ctrl_d, other.ctrl_d);
        merge_option(&mut self.ctrl_m, other.ctrl_m);
        merge_option(&mut self.ctrl_w, other.ctrl_w);
        merge_option(&mut self.esc, other.esc);
        merge_option(&mut self.ctrl_c, other.ctrl_c);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PartialLoggingConfig {
    level: Option<LogLevel>,
}

impl PartialLoggingConfig {
    fn merge(&mut self, other: Self) {
        merge_option(&mut self.level, other.level);
    }
}

fn merge_option<T>(slot: &mut Option<T>, incoming: Option<T>) {
    if let Some(value) = incoming {
        *slot = Some(value);
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidEnvironment {
            key: key.to_string(),
            message: format!("expected a boolean, got `{value}`"),
        }),
    }
}

fn validate_config(config: &ResolvedConfig, errors: &mut Vec<ValidationError>) {
    if !(0.2..=0.8).contains(&config.ui.preview_width) {
        errors.push(ValidationError {
            path: "ui.preview_width".to_string(),
            message: "must be between 0.2 and 0.8".to_string(),
        });
    }

    if config.preview.timeout_ms == 0 || config.preview.timeout_ms > 5_000 {
        errors.push(ValidationError {
            path: "preview.timeout_ms".to_string(),
            message: "must be between 1 and 5000 milliseconds".to_string(),
        });
    }

    if config.zoxide.max_entries == 0 {
        errors.push(ValidationError {
            path: "zoxide.max_entries".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }

    if config.status.line == 0 {
        errors.push(ValidationError {
            path: "status.line".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }

    if config.status.max_sessions == Some(0) {
        errors.push(ValidationError {
            path: "status.max_sessions".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }

    if config.preview.max_file_bytes == 0 {
        errors.push(ValidationError {
            path: "preview.max_file_bytes".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }

    if config.preview.cache_entries == 0 {
        errors.push(ValidationError {
            path: "preview.cache_entries".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CliOverrides, ConfigError, FuzzyEngine, KeyAction, LogLevel, SessionSortMode, UiMode,
        resolve_config,
    };

    #[test]
    fn resolves_default_config_values() {
        let config = resolve_config(None, &BTreeMap::new(), &CliOverrides::default(), false)
            .expect("default config should resolve");

        assert_eq!(config.ui.mode, UiMode::Auto);
        assert_eq!(config.fuzzy.engine, FuzzyEngine::Nucleo);
        assert!(config.zoxide.enabled);
        assert_eq!(config.logging.level, LogLevel::Warn);
        assert_eq!(config.ui.session_sort, SessionSortMode::Recent);
        assert_eq!(config.status.line, 2);
        assert!(config.status.interactive);
        assert_eq!(config.status.icon, "󰖔");
        assert_eq!(config.status.max_sessions, None);
        assert_eq!(config.actions.down, KeyAction::MoveDown);
        assert_eq!(config.actions.up, KeyAction::MoveUp);
        assert_eq!(config.actions.ctrl_j, KeyAction::MoveDown);
        assert_eq!(config.actions.ctrl_k, KeyAction::MoveUp);
        assert_eq!(
            config.actions.shift_enter,
            KeyAction::CreateSessionFromQuery
        );
        assert_eq!(config.actions.backspace, KeyAction::Backspace);
        assert_eq!(config.actions.ctrl_s, KeyAction::ToggleSort);
        assert_eq!(config.actions.ctrl_w, KeyAction::ToggleWorktreeMode);
    }

    #[test]
    fn parses_toml_config_values() {
        let input = r#"
            [ui]
            mode = "popup"
            preview_width = 0.6
            session_sort = "alphabetical"

            [fuzzy]
            engine = "skim"

            [tmux]
            popup_width = "90%"
            popup_height = "40"

            [status]
            line = 3
            icon = "Wisp"
            max_sessions = 5
            show_previous = false

            [actions]
            down = "move-down"
            up = "move-up"
            ctrl_j = "move-down"
            ctrl_k = "move-up"
            ctrl_r = "rename-session"
            ctrl_s = "toggle-sort"
            ctrl_x = "close"
            ctrl_p = "open"
            shift_enter = "create-session-from-query"
            backspace = "backspace"
        "#;

        let config = resolve_config(
            Some(input),
            &BTreeMap::new(),
            &CliOverrides::default(),
            false,
        )
        .expect("toml config should resolve");

        assert_eq!(config.ui.mode, UiMode::Popup);
        assert_eq!(config.ui.preview_width, 0.6);
        assert_eq!(config.ui.session_sort, SessionSortMode::Alphabetical);
        assert_eq!(config.fuzzy.engine, FuzzyEngine::Skim);
        assert_eq!(config.status.line, 3);
        assert_eq!(config.status.icon, "Wisp");
        assert_eq!(config.status.max_sessions, Some(5));
        assert!(!config.status.show_previous);
        assert_eq!(config.actions.down, KeyAction::MoveDown);
        assert_eq!(config.actions.up, KeyAction::MoveUp);
        assert_eq!(config.actions.ctrl_j, KeyAction::MoveDown);
        assert_eq!(config.actions.ctrl_k, KeyAction::MoveUp);
        assert_eq!(config.actions.ctrl_r, KeyAction::RenameSession);
        assert_eq!(config.actions.ctrl_s, KeyAction::ToggleSort);
        assert_eq!(config.actions.ctrl_x, KeyAction::Close);
        assert_eq!(config.actions.ctrl_p, KeyAction::Open);
        assert_eq!(
            config.actions.shift_enter,
            KeyAction::CreateSessionFromQuery
        );
        assert_eq!(config.actions.backspace, KeyAction::Backspace);
    }

    #[test]
    fn applies_file_then_environment_then_cli_precedence() {
        let input = r#"
            [ui]
            mode = "fullscreen"

            [fuzzy]
            engine = "skim"

            [logging]
            level = "info"
        "#;
        let env = BTreeMap::from([
            ("WISP_MODE".to_string(), "popup".to_string()),
            ("WISP_ENGINE".to_string(), "nucleo".to_string()),
            ("WISP_LOG_LEVEL".to_string(), "debug".to_string()),
        ]);
        let cli = CliOverrides {
            mode: Some(UiMode::Auto),
            engine: Some(FuzzyEngine::Skim),
            log_level: Some(LogLevel::Trace),
            no_zoxide: true,
            ..CliOverrides::default()
        };

        let config =
            resolve_config(Some(input), &env, &cli, false).expect("merged config should resolve");

        assert_eq!(config.ui.mode, UiMode::Auto);
        assert_eq!(config.fuzzy.engine, FuzzyEngine::Skim);
        assert_eq!(config.logging.level, LogLevel::Trace);
        assert!(!config.zoxide.enabled);
    }

    #[test]
    fn returns_validation_errors_with_field_paths() {
        let input = r#"
            [ui]
            preview_width = 0.95

            [tmux]
            popup_width = "101%"

            [preview]
            timeout_ms = 0

            [status]
            line = 0
        "#;

        let error = resolve_config(
            Some(input),
            &BTreeMap::new(),
            &CliOverrides::default(),
            false,
        )
        .expect_err("invalid config should fail");

        match error {
            ConfigError::Validation(errors) => {
                let paths = errors
                    .iter()
                    .map(|error| error.path.as_str())
                    .collect::<Vec<_>>();
                assert!(paths.contains(&"ui.preview_width"));
                assert!(paths.contains(&"tmux.popup_width"));
                assert!(paths.contains(&"preview.timeout_ms"));
                assert!(paths.contains(&"status.line"));
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_fields_in_strict_mode() {
        let input = r#"
            [ui]
            mode = "popup"
            impossible = true
        "#;

        let error = resolve_config(
            Some(input),
            &BTreeMap::new(),
            &CliOverrides::default(),
            true,
        )
        .expect_err("strict mode should reject unknown fields");

        match error {
            ConfigError::UnknownFields { fields } => {
                assert_eq!(fields, vec!["ui.impossible".to_string()]);
            }
            other => panic!("expected unknown fields error, got {other:?}"),
        }
    }
}
