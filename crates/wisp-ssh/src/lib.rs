//! Hosts named in the user's OpenSSH client configuration.
//!
//! Reads `~/.ssh/config` and the files it includes the way `ssh` does, keeping
//! the concrete aliases of `Host` lines. Patterns (`*`, `?`) and negations
//! (`!`) name no single host, so they are left out, as are `Match` blocks.

use std::{
    collections::HashMap,
    env, fs,
    path::{Component, Path, PathBuf},
};

/// `ssh` stops following `Include` this deep.
const MAX_INCLUDE_DEPTH: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<String>,
}

impl SshHost {
    /// Where the alias connects, as `user@hostname:port` with the parts the
    /// config sets.
    #[must_use]
    pub fn destination(&self) -> String {
        let mut destination = String::new();
        if let Some(user) = &self.user {
            destination.push_str(user);
            destination.push('@');
        }
        destination.push_str(self.hostname.as_deref().unwrap_or(&self.alias));
        if let Some(port) = &self.port {
            destination.push(':');
            destination.push_str(port);
        }
        destination
    }
}

/// The hosts in `$HOME/.ssh/config`, in the order they first appear. A missing
/// or unreadable config has none.
#[must_use]
pub fn load_user_hosts() -> Vec<SshHost> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    load_hosts(&home.join(".ssh").join("config"), &home)
}

/// The hosts in `config`, resolving `~` and relative `Include` paths against
/// `home` as `ssh` does for a user config.
#[must_use]
pub fn load_hosts(config: &Path, home: &Path) -> Vec<SshHost> {
    let mut parser = Parser {
        home,
        ssh_dir: home.join(".ssh"),
        hosts: Vec::new(),
        index: HashMap::new(),
    };
    parser.read_file(config, 0);
    parser.hosts
}

/// The hosts in config `text` alone, without following `Include`.
#[must_use]
pub fn parse_hosts(text: &str) -> Vec<SshHost> {
    let mut parser = Parser {
        home: Path::new(""),
        ssh_dir: PathBuf::new(),
        hosts: Vec::new(),
        index: HashMap::new(),
    };
    parser.parse(text, MAX_INCLUDE_DEPTH);
    parser.hosts
}

struct Parser<'a> {
    home: &'a Path,
    ssh_dir: PathBuf,
    hosts: Vec<SshHost>,
    index: HashMap<String, usize>,
}

impl Parser<'_> {
    fn read_file(&mut self, path: &Path, depth: usize) {
        if let Ok(text) = fs::read_to_string(path) {
            self.parse(&text, depth);
        }
    }

    fn parse(&mut self, text: &str, depth: usize) {
        // The hosts the current `Host` block names; options there apply to them.
        let mut block = Vec::new();
        for line in text.lines() {
            let Some((keyword, args)) = split_line(line) else {
                continue;
            };
            match keyword.to_ascii_lowercase().as_str() {
                "host" => {
                    block = args
                        .iter()
                        .filter(|pattern| is_alias(pattern))
                        .map(|alias| self.host_index(alias))
                        .collect();
                }
                "match" => block.clear(),
                "include" if depth < MAX_INCLUDE_DEPTH => {
                    for pattern in &args {
                        for path in self.include_paths(pattern) {
                            self.read_file(&path, depth + 1);
                        }
                    }
                }
                // As in `ssh`, the first value given for a host wins.
                "hostname" | "user" | "port" => {
                    let Some(value) = args.first() else {
                        continue;
                    };
                    for &index in &block {
                        let host = &mut self.hosts[index];
                        let field = match keyword.to_ascii_lowercase().as_str() {
                            "hostname" => &mut host.hostname,
                            "user" => &mut host.user,
                            _ => &mut host.port,
                        };
                        field.get_or_insert_with(|| value.clone());
                    }
                }
                _ => {}
            }
        }
    }

    fn host_index(&mut self, alias: &str) -> usize {
        if let Some(&index) = self.index.get(alias) {
            return index;
        }
        let index = self.hosts.len();
        self.hosts.push(SshHost {
            alias: alias.to_string(),
            ..SshHost::default()
        });
        self.index.insert(alias.to_string(), index);
        index
    }

    fn include_paths(&self, pattern: &str) -> Vec<PathBuf> {
        let path = if let Some(rest) = pattern.strip_prefix("~/") {
            self.home.join(rest)
        } else if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            self.ssh_dir.join(pattern)
        };
        expand_glob(&path)
    }
}

/// Splits a config line into its keyword and arguments, which may be separated
/// by whitespace or `=` and quoted with `"`. Blank lines and comments have none.
fn split_line(line: &str) -> Option<(&str, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|character: char| character.is_whitespace() || character == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    Some((keyword, split_args(rest)))
}

fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    for character in text.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            character if character.is_whitespace() && !quoted => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            character => {
                current.push(character);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    args
}

fn is_alias(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.starts_with('!') && !pattern.contains(['*', '?'])
}

/// The existing paths `path` matches, sorted, with `*` and `?` in any of its
/// components. Like glob(3), a wildcard does not match a leading `.`.
fn expand_glob(path: &Path) -> Vec<PathBuf> {
    let mut matches = vec![PathBuf::new()];
    for component in path.components() {
        let Component::Normal(name) = component else {
            for path in &mut matches {
                path.push(component);
            }
            continue;
        };
        let pattern = name.to_string_lossy();
        if !pattern.contains(['*', '?']) {
            for path in &mut matches {
                path.push(name);
            }
            continue;
        }
        matches = matches
            .into_iter()
            .flat_map(|directory| {
                let mut names = fs::read_dir(&directory)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .filter(|name| {
                        (pattern.starts_with('.') || !name.starts_with('.'))
                            && wildcard_match(&pattern, name)
                    })
                    .collect::<Vec<_>>();
                names.sort();
                names.into_iter().map(move |name| directory.join(name))
            })
            .collect();
    }
    matches.retain(|path| path.exists());
    matches
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let text = text.chars().collect::<Vec<_>>();
    let (mut p, mut t) = (0, 0);
    // Where the last `*` was and the text position it currently stands for.
    let mut star = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&character| character == '*')
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{SshHost, load_hosts, parse_hosts, wildcard_match};

    /// A home directory of the test's own, removed when the test ends,
    /// whether it passed or not.
    struct TestHome(PathBuf);

    impl TestHome {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            Self(std::env::temp_dir().join(format!("wisp-ssh-{name}-{nonce}")))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn aliases(hosts: &[SshHost]) -> Vec<&str> {
        hosts.iter().map(|host| host.alias.as_str()).collect()
    }

    #[test]
    fn keeps_concrete_aliases_and_skips_patterns() {
        let hosts = parse_hosts(
            "# comment\n\
             Host devbox build-*\n\
             Host *\n\
             \x20 ServerAliveInterval 30\n\
             Host !bastion web?? web1 \"quoted host\"\n",
        );

        assert_eq!(aliases(&hosts), ["devbox", "web1", "quoted host"]);
    }

    #[test]
    fn reads_destination_options_from_the_host_block() {
        let hosts = parse_hosts(
            "Host devbox\n\
             \x20 HostName devbox.example.com\n\
             \x20 user=deploy\n\
             \x20 Port = 2222\n\
             Host other\n\
             \x20 HostName other.example.com\n",
        );

        assert_eq!(
            hosts[0],
            SshHost {
                alias: "devbox".to_string(),
                hostname: Some("devbox.example.com".to_string()),
                user: Some("deploy".to_string()),
                port: Some("2222".to_string()),
            }
        );
        assert_eq!(hosts[0].destination(), "deploy@devbox.example.com:2222");
        assert_eq!(hosts[1].destination(), "other.example.com");
    }

    #[test]
    fn first_value_wins_and_match_blocks_set_nothing() {
        let hosts = parse_hosts(
            "Host devbox\n\
             \x20 HostName first\n\
             Host devbox\n\
             \x20 HostName second\n\
             \x20 User deploy\n\
             Match host devbox\n\
             \x20 Port 2222\n",
        );

        assert_eq!(aliases(&hosts), ["devbox"]);
        assert_eq!(hosts[0].hostname.as_deref(), Some("first"));
        assert_eq!(hosts[0].port, None);
    }

    #[test]
    fn follows_includes_relative_to_the_ssh_directory() {
        let test_home = TestHome::new("includes");
        let home = test_home.path();
        let ssh_dir = home.join(".ssh");
        fs::create_dir_all(ssh_dir.join("config.d")).expect("should create config.d");
        fs::write(
            ssh_dir.join("config"),
            "Host first\nInclude config.d/*\nInclude ~/extra missing\nHost last\n",
        )
        .expect("should write config");
        fs::write(ssh_dir.join("config.d/b"), "Host from-b\n").expect("should write b");
        fs::write(ssh_dir.join("config.d/a"), "Host from-a\n").expect("should write a");
        fs::write(ssh_dir.join("config.d/.hidden"), "Host hidden\n").expect("should write hidden");
        fs::write(home.join("extra"), "Host extra\nInclude ~/extra\n").expect("should write extra");

        let hosts = load_hosts(&ssh_dir.join("config"), home);

        assert_eq!(
            aliases(&hosts),
            ["first", "from-a", "from-b", "extra", "last"]
        );
    }

    #[test]
    fn a_missing_config_has_no_hosts() {
        let test_home = TestHome::new("missing");
        let home = test_home.path();
        assert!(load_hosts(&home.join(".ssh/config"), home).is_empty());
    }

    #[test]
    fn wildcards_match_like_glob() {
        assert!(wildcard_match("*.conf", "work.conf"));
        assert!(wildcard_match("a?c*", "abcdef"));
        assert!(wildcard_match("*", ""));
        assert!(!wildcard_match("*.conf", "work.config"));
        assert!(!wildcard_match("a?c", "ac"));
    }
}
