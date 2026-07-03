//! Parse an ssh_config(5) formatted config file
use regex::{Captures, Regex};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub type ConfigMap = BTreeMap<String, String>;

/// A Pattern in a `Host` list
#[derive(Debug, PartialEq, Eq, Clone)]
struct Pattern {
    negated: bool,
    pattern: String,
    original: String,
    is_literal: bool,
}

/// Compile a glob style pattern string into a regex pattern string
fn wildcard_to_pattern(s: &str) -> (String, bool) {
    let mut pattern = String::new();
    let mut is_literal = true;
    pattern.push('^');
    for c in s.chars() {
        if c == '*' {
            pattern.push_str(".*");
            is_literal = false;
        } else if c == '?' {
            pattern.push('.');
            is_literal = false;
        } else {
            let s = regex::escape(&c.to_string());
            pattern.push_str(&s);
        }
    }
    pattern.push('$');
    (pattern, is_literal)
}

impl Pattern {
    /// Returns true if this pattern matches the provided hostname
    fn match_text(&self, hostname: &str) -> bool {
        if let Ok(re) = Regex::new(&self.pattern) {
            re.is_match(hostname)
        } else {
            false
        }
    }

    fn new(text: &str, negated: bool) -> Self {
        let (pattern, is_literal) = wildcard_to_pattern(text);
        Self {
            pattern,
            is_literal,
            negated,
            original: text.to_string(),
        }
    }

    /// Returns true if hostname matches the
    /// condition specified by a list of patterns
    fn match_group(hostname: &str, patterns: &[Self]) -> bool {
        for pat in patterns {
            if pat.match_text(hostname) {
                // We got a definitive name match.
                // If it was an exlusion then we've been told
                // that this doesn't really match, otherwise
                // we got one that we were looking for
                return !pat.negated;
            }
        }
        false
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
enum Criteria {
    Host(Vec<Pattern>),
    Exec(String),
    OriginalHost(Vec<Pattern>),
    User(Vec<Pattern>),
    LocalUser(Vec<Pattern>),
    All,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Context {
    FirstPass,
    Canonical,
    Final,
}

/// A single `IdentityFile` directive value after parsing.
///
/// The `path` field holds the literal path string as OpenSSH's
/// `argv_split` would emit it: outer quotes stripped, backslash
/// escapes (`\\`, `\"`, `\'` and `\ `) consumed, but
/// `~` / `%h` / `$VAR` style expansion is *not* performed — that
/// happens later in the resolver when the concrete host identity
/// is known. Consumers should treat `path` as an opaque file path
/// suitable for `std::fs::read` once expansion has occurred.
///
/// A list of these entries is kept alongside the legacy
/// space-concatenated `ConfigMap` value by [`Config`]. Everything
/// that needs a faithful view of the declared identities (agent-key
/// filtering under `IdentitiesOnly=yes`, pubkey auth, libssh's
/// `AddIdentity`) should read the list, not the legacy string,
/// because only the typed form survives paths containing whitespace
/// and preserves declaration order across `Host` and `Match`
/// stanzas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityFileEntry {
    /// The literal path as declared in the configuration.
    pub path: String,
}

impl IdentityFileEntry {
    fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

/// The resolved ssh_config options for a given host.
///
/// `HostOptions` is the preferred input shape for
/// [`crate::Session::connect`]: it bundles the flat `ConfigMap` that
/// covers every single-value directive with a typed list of
/// `IdentityFile` entries that preserves declaration order and
/// tolerates paths containing whitespace — two things the flat
/// `ConfigMap` representation cannot round-trip cleanly.
///
/// Construct a `HostOptions` with [`Config::resolve_host`] for the
/// common case of resolving an ssh_config tree against a hostname.
/// A `From<ConfigMap>` impl is also provided so that legacy callers
/// that already own a flat map can keep working; that conversion
/// falls back to splitting the legacy `identityfile` string on
/// whitespace, which means it cannot faithfully represent paths that
/// contain spaces — new code should go through `resolve_host`
/// whenever possible.
///
/// # Invariant: typed list ↔ flat map
///
/// The typed [`Self::identity_files`] list and the legacy
/// `options["identityfile"]` string are kept in sync by every
/// mutation site in this crate. The contract is:
///
/// * **Source of truth**: `identity_files` holds the canonical,
///   tokeniser-correct view. `options["identityfile"]` is a
///   space-joined legacy serialisation for downstream code that
///   still reads the flat `ConfigMap`.
/// * **Mutation through [`Self::push_identity_file`]**: when you
///   add a new entry (e.g. in a `-o IdentityFile=...` CLI override
///   handler), go through the helper. It updates both views
///   atomically and is the only code path guaranteed to keep them
///   consistent.
/// * **Direct field access is allowed but dangerous**: the fields
///   are `pub` so that existing callers (and serialisation-driven
///   Lua config) do not break, but touching them bypasses the
///   invariant. Tests and diagnostics may read them freely; code
///   that pushes new entries should use `push_identity_file`.
///
/// # Example
///
/// ```no_run
/// use wezterm_ssh::{Config, Session};
///
/// let mut config = Config::new();
/// config.add_default_config_files();
/// let host_options = config.resolve_host("example.com");
/// let (_session, _events) = Session::connect(host_options)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostOptions {
    /// Flat key-value options as produced by [`Config::for_host`].
    ///
    /// This is a legacy representation that cannot losslessly
    /// round-trip `IdentityFile` paths containing whitespace — see
    /// the struct-level doc on the typed-list-vs-flat-map
    /// invariant. Prefer [`Self::identity_files`] for
    /// `IdentityFile` reads and [`Self::push_identity_file`] for
    /// writes.
    pub options: ConfigMap,
    /// `IdentityFile` entries in OpenSSH declaration order (file
    /// global first, then matching stanzas), with the built-in
    /// defaults applied when no directive matched.
    ///
    /// Canonical source of truth. Safe to read freely; when
    /// appending new entries, use
    /// [`Self::push_identity_file`] so that the legacy
    /// `options["identityfile"]` view stays in sync.
    pub identity_files: Vec<IdentityFileEntry>,
}

impl HostOptions {
    /// Append a new `IdentityFile` entry while keeping the typed
    /// list and the legacy space-concatenated `ConfigMap` entry in
    /// sync.
    ///
    /// Prefer this helper over touching `identity_files` and
    /// `options["identityfile"]` directly in mutation sites such as
    /// the `-o IdentityFile=...` CLI override handler. The typed
    /// list is the source of truth for paths containing whitespace;
    /// the flat map view follows the legacy convention of
    /// space-joined values so that downstream code reading the
    /// `ConfigMap` still sees the entry (albeit lossy for any path
    /// containing a literal space).
    pub fn push_identity_file(&mut self, path: impl Into<String>) {
        let path = path.into();
        self.identity_files
            .push(IdentityFileEntry { path: path.clone() });
        self.options
            .entry("identityfile".to_string())
            .and_modify(|existing| {
                if !existing.is_empty() {
                    existing.push(' ');
                }
                existing.push_str(&path);
            })
            .or_insert(path);
    }
}

impl From<ConfigMap> for HostOptions {
    /// Build a [`HostOptions`] from a legacy flat `ConfigMap`.
    ///
    /// This exists so that older callers that constructed a
    /// `ConfigMap` by hand still compile against the newer
    /// [`crate::Session::connect`] API. The conversion recovers the
    /// typed `identity_files` list by splitting the legacy
    /// `identityfile` value on whitespace, which **cannot**
    /// faithfully represent paths containing spaces — the very
    /// problem the typed list was introduced to fix.
    ///
    /// **Prefer [`Config::resolve_host`] for new code.** It runs
    /// the full OpenSSH `argv_split`-style tokeniser and produces
    /// a tokeniser-correct `identity_files` list even for paths
    /// containing whitespace. This impl logs a single
    /// `log::warn!` per process when it runs to help operators
    /// notice that they are on the lossy compat route.
    fn from(options: ConfigMap) -> Self {
        static WARN_ONCE: std::sync::Once = std::sync::Once::new();
        WARN_ONCE.call_once(|| {
            log::warn!(
                "HostOptions::from(ConfigMap) is a lossy compat path for \
                 IdentityFile entries that contain spaces; prefer \
                 Config::resolve_host() for tokeniser-correct parsing."
            );
        });

        let identity_files = options
            .get("identityfile")
            .map(|value| {
                value
                    .split_whitespace()
                    .map(|path| IdentityFileEntry {
                        path: path.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            options,
            identity_files,
        }
    }
}

/// Represents `Host pattern,list` stanza in the config,
/// and the options that it logically contains
#[derive(Debug, PartialEq, Eq, Clone)]
struct MatchGroup {
    criteria: Vec<Criteria>,
    context: Context,
    options: ConfigMap,
    /// `IdentityFile` entries declared inside this stanza, in the
    /// order they were parsed. Kept separately from `options` so that
    /// paths containing whitespace survive the round-trip.
    identity_files: Vec<IdentityFileEntry>,
}

impl MatchGroup {
    fn is_match(&self, hostname: &str, user: &str, local_user: &str, context: Context) -> bool {
        if self.context != context {
            return false;
        }
        for c in &self.criteria {
            match c {
                Criteria::Host(patterns) => {
                    if !Pattern::match_group(hostname, patterns) {
                        return false;
                    }
                }
                Criteria::Exec(_) => {
                    log::warn!("Match Exec is not implemented");
                }
                Criteria::OriginalHost(patterns) => {
                    if !Pattern::match_group(hostname, patterns) {
                        return false;
                    }
                }
                Criteria::User(patterns) => {
                    if !Pattern::match_group(user, patterns) {
                        return false;
                    }
                }
                Criteria::LocalUser(patterns) => {
                    if !Pattern::match_group(local_user, patterns) {
                        return false;
                    }
                }
                Criteria::All => {
                    // Always matches
                }
            }
        }
        true
    }
}

/// Holds the ordered set of parsed options.
/// The config file semantics are that the first matching value
/// for a given option takes precedence
#[derive(Debug, PartialEq, Eq, Clone)]
struct ParsedConfigFile {
    /// options that appeared before any `Host` stanza
    options: ConfigMap,
    /// options inside a `Host` stanza
    groups: Vec<MatchGroup>,
    /// list of loaded file names
    loaded_files: Vec<PathBuf>,
    /// `IdentityFile` entries declared before any `Host` stanza, in
    /// declaration order.
    identity_files: Vec<IdentityFileEntry>,
}

impl ParsedConfigFile {
    fn parse(s: &str, cwd: Option<&Path>, source_file: Option<&Path>) -> Self {
        let mut options = ConfigMap::new();
        let mut groups = vec![];
        let mut loaded_files = vec![];
        let mut identity_files = vec![];

        if let Some(source) = source_file {
            loaded_files.push(source.to_path_buf());
        }

        Self::parse_impl(
            s,
            cwd,
            &mut options,
            &mut groups,
            &mut loaded_files,
            &mut identity_files,
        );

        Self {
            options,
            groups,
            loaded_files,
            identity_files,
        }
    }

    fn do_include(
        pattern: &str,
        cwd: Option<&Path>,
        options: &mut ConfigMap,
        groups: &mut Vec<MatchGroup>,
        loaded_files: &mut Vec<PathBuf>,
        identity_files: &mut Vec<IdentityFileEntry>,
    ) {
        match filenamegen::Glob::new(&pattern) {
            Ok(g) => {
                match cwd
                    .as_ref()
                    .map(|p| p.to_path_buf())
                    .or_else(|| std::env::current_dir().ok())
                {
                    Some(cwd) => {
                        for path in g.walk(&cwd) {
                            let path = if path.is_absolute() {
                                path
                            } else {
                                cwd.join(path)
                            };
                            match std::fs::read_to_string(&path) {
                                Ok(data) => {
                                    loaded_files.push(path.clone());
                                    Self::parse_impl(
                                        &data,
                                        Some(&cwd),
                                        options,
                                        groups,
                                        loaded_files,
                                        identity_files,
                                    );
                                }
                                Err(err) => {
                                    log::error!(
                                        "error expanding `Include {}`: unable to open {}: {:#}",
                                        pattern,
                                        path.display(),
                                        err
                                    );
                                }
                            }
                        }
                    }
                    None => {
                        log::error!(
                            "error expanding `Include {}`: unable to determine cwd",
                            pattern
                        );
                    }
                }
            }
            Err(err) => {
                log::error!("error expanding `Include {}`: {:#}", pattern, err);
            }
        }
    }

    fn parse_impl(
        s: &str,
        cwd: Option<&Path>,
        options: &mut ConfigMap,
        groups: &mut Vec<MatchGroup>,
        loaded_files: &mut Vec<PathBuf>,
        identity_files: &mut Vec<IdentityFileEntry>,
    ) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some(sep) = line.find(|c: char| c == '=' || c.is_whitespace()) else {
                continue;
            };
            let (k, rest) = line.split_at(sep);
            let key = k.trim().to_lowercase();
            let rest = rest[1..].trim_start();

            let tokens = match crate::tokenizer::argv_split(rest, true) {
                Ok(t) => t,
                Err(err) => {
                    // OpenSSH aborts the whole config load here with
                    // "bad configuration options". We cannot
                    // propagate that without breaking the public
                    // API (`Config::add_config_string` and
                    // `add_config_file` are `()`-returning), so we
                    // drop the offending line and raise the log
                    // level from `warn` to `error` so the user
                    // actually notices that a directive vanished.
                    log::error!(
                        "ssh_config: dropping line with {} — this directive will not take effect: {:?}",
                        err,
                        line
                    );
                    continue;
                }
            };

            if tokens.is_empty() {
                continue;
            }

            fn parse_pattern_list(v: &str) -> Vec<Pattern> {
                let mut patterns = vec![];
                for p in v.split(',') {
                    let p = p.trim();
                    if let Some(stripped) = p.strip_prefix('!') {
                        patterns.push(Pattern::new(stripped, true));
                    } else {
                        patterns.push(Pattern::new(p, false));
                    }
                }
                patterns
            }

            fn patterns_from_tokens(tokens: &[String]) -> Vec<Pattern> {
                tokens
                    .iter()
                    .map(|t| {
                        if let Some(stripped) = t.strip_prefix('!') {
                            Pattern::new(stripped, true)
                        } else {
                            Pattern::new(t, false)
                        }
                    })
                    .collect()
            }

            if key == "include" {
                for token in &tokens {
                    Self::do_include(token, cwd, options, groups, loaded_files, identity_files);
                }
                continue;
            }

            if key == "host" {
                groups.push(MatchGroup {
                    criteria: vec![Criteria::Host(patterns_from_tokens(&tokens))],
                    options: ConfigMap::new(),
                    context: Context::FirstPass,
                    identity_files: Vec::new(),
                });
                continue;
            }

            if key == "match" {
                let mut criteria = vec![];
                let mut context = Context::FirstPass;
                let mut iter = tokens.iter();

                while let Some(cname) = iter.next() {
                    match cname.to_lowercase().as_str() {
                        "all" => {
                            criteria.push(Criteria::All);
                        }
                        "canonical" => {
                            context = Context::Canonical;
                        }
                        "final" => {
                            context = Context::Final;
                        }
                        "exec" => {
                            criteria.push(Criteria::Exec(
                                iter.next().cloned().unwrap_or_else(|| "false".to_string()),
                            ));
                        }
                        "host" => {
                            criteria.push(Criteria::Host(parse_pattern_list(
                                iter.next().map(String::as_str).unwrap_or(""),
                            )));
                        }
                        "originalhost" => {
                            criteria.push(Criteria::OriginalHost(parse_pattern_list(
                                iter.next().map(String::as_str).unwrap_or(""),
                            )));
                        }
                        "user" => {
                            criteria.push(Criteria::User(parse_pattern_list(
                                iter.next().map(String::as_str).unwrap_or(""),
                            )));
                        }
                        "localuser" => {
                            criteria.push(Criteria::LocalUser(parse_pattern_list(
                                iter.next().map(String::as_str).unwrap_or(""),
                            )));
                        }
                        _ => break,
                    }
                }

                groups.push(MatchGroup {
                    criteria,
                    options: ConfigMap::new(),
                    context,
                    identity_files: Vec::new(),
                });
                continue;
            }

            // Command directives (ProxyCommand, LocalCommand,
            // RemoteCommand, KnownHostsCommand) take the raw
            // remainder of the line verbatim so that shell-style
            // arguments and trailing `#` characters survive intact,
            // matching OpenSSH's `parse_command` branch in
            // readconf.c:1531.
            let is_command_directive = matches!(
                key.as_str(),
                "proxycommand" | "localcommand" | "remotecommand" | "knownhostscommand"
            );
            let value: String = if is_command_directive {
                rest.to_string()
            } else {
                // Single-token directive. OpenSSH errors on extra
                // arguments; until wave 4 introduces typed
                // multi-value fields we warn and keep the first
                // token.
                if tokens.len() > 1 {
                    log::warn!(
                        "ssh_config: keyword {:?} takes one argument per line; ignoring extras: {:?}",
                        key,
                        &tokens[1..]
                    );
                }
                tokens[0].clone()
            };

            fn add_option(options: &mut ConfigMap, k: String, v: &str) {
                // first option wins in ssh_config, except for identityfile
                // which explicitly allows multiple entries to combine together
                let is_identity_file = k == "identityfile";
                options
                    .entry(k)
                    .and_modify(|e| {
                        if is_identity_file {
                            e.push(' ');
                            e.push_str(v);
                        }
                    })
                    .or_insert_with(|| v.to_string());
            }

            // IdentityFile needs a parallel typed list so that paths
            // containing whitespace survive. The legacy ConfigMap
            // entry is still populated so that consumers that have
            // not yet migrated keep working (wave 5 flips them over).
            if key == "identityfile" {
                let entry = IdentityFileEntry::new(value.clone());
                if let Some(group) = groups.last_mut() {
                    group.identity_files.push(entry);
                } else {
                    identity_files.push(entry);
                }
            }

            if let Some(group) = groups.last_mut() {
                add_option(&mut group.options, key, &value);
            } else {
                add_option(options, key, &value);
            }
        }
    }

    /// Apply configuration values that match the specified hostname to target,
    /// but only if a given key is not already present in target, because the
    /// semantics are that the first match wins
    fn apply_matches(
        &self,
        hostname: &str,
        user: &str,
        local_user: &str,
        context: Context,
        target: &mut ConfigMap,
    ) -> bool {
        let mut needs_reparse = false;

        for (k, v) in &self.options {
            target.entry(k.to_string()).or_insert_with(|| v.to_string());
        }
        for group in &self.groups {
            if group.context != Context::FirstPass {
                needs_reparse = true;
            }
            if group.is_match(hostname, user, local_user, context) {
                for (k, v) in &group.options {
                    target.entry(k.to_string()).or_insert_with(|| v.to_string());
                }
            }
        }

        needs_reparse
    }

    /// Append IdentityFile entries that apply to the given host onto
    /// `target`, in declaration order: file-global entries first,
    /// then entries from every matching stanza in the order the
    /// stanzas appeared in the file. This mirrors OpenSSH's
    /// additive semantics ("multiple IdentityFile directives add to
    /// the list of identities tried").
    fn collect_identity_files(
        &self,
        hostname: &str,
        user: &str,
        local_user: &str,
        context: Context,
        target: &mut Vec<IdentityFileEntry>,
    ) {
        for entry in &self.identity_files {
            target.push(entry.clone());
        }
        for group in &self.groups {
            if group.is_match(hostname, user, local_user, context) {
                for entry in &group.identity_files {
                    target.push(entry.clone());
                }
            }
        }
    }
}

/// A context for resolving configuration values.
/// Holds a combination of environment and token expansion state,
/// as well as the set of configs that should be consulted.
#[derive(Debug, Clone)]
pub struct Config {
    config_files: Vec<ParsedConfigFile>,
    options: ConfigMap,
    tokens: ConfigMap,
    environment: Option<ConfigMap>,
}

impl Config {
    /// Create a new context without any config files loaded
    pub fn new() -> Self {
        Self {
            config_files: vec![],
            options: ConfigMap::new(),
            tokens: ConfigMap::new(),
            environment: None,
        }
    }

    /// Assign a fake environment map, useful for testing.
    /// The environment is used to expand certain values
    /// from the config.
    pub fn assign_environment(&mut self, env: ConfigMap) {
        self.environment.replace(env);
    }

    /// Assigns token names and expansions for use with a number of
    /// options.  The names and expansions are specified
    /// by `man 5 ssh_config`
    pub fn assign_tokens(&mut self, tokens: ConfigMap) {
        self.tokens = tokens;
    }

    /// Assign the value for an option.
    /// This is logically equivalent to the user specifying command
    /// line options to override config values.
    /// These values take precedence over any values found in config files.
    pub fn set_option<K: AsRef<str>, V: AsRef<str>>(&mut self, key: K, value: V) {
        self.options
            .insert(key.as_ref().to_lowercase(), value.as_ref().to_string());
    }

    /// Parse `config_string` as if it were the contents of an `ssh_config` file,
    /// and add that to the list of configs.
    pub fn add_config_string(&mut self, config_string: &str) {
        self.config_files
            .push(ParsedConfigFile::parse(config_string, None, None));
    }

    /// Open `path`, read its contents and parse it as an `ssh_config` file,
    /// adding that to the list of configs
    pub fn add_config_file<P: AsRef<Path>>(&mut self, path: P) {
        if let Ok(data) = std::fs::read_to_string(path.as_ref()) {
            self.config_files.push(ParsedConfigFile::parse(
                &data,
                path.as_ref().parent(),
                Some(path.as_ref()),
            ));
        }
    }

    /// Convenience method for adding the ~/.ssh/config and system-wide
    /// `/etc/ssh/config` files to the list of configs
    pub fn add_default_config_files(&mut self) {
        if let Some(home) = dirs_next::home_dir() {
            self.add_config_file(home.join(".ssh").join("config"));
        }
        self.add_config_file("/etc/ssh/ssh_config");
        if let Ok(sysdrive) = std::env::var("SystemDrive") {
            self.add_config_file(format!("{}/ProgramData/ssh/ssh_config", sysdrive));
        }
    }

    fn resolve_local_host(&self, include_domain_name: bool) -> String {
        let hostname = if cfg!(test) {
            // Use a fixed and plausible name for the local hostname
            // when running tests.  This isn't an ideal solution, but
            // it is convenient and sufficient at the time of writing
            "localhost".to_string()
        } else {
            gethostname::gethostname().to_string_lossy().to_string()
        };

        if include_domain_name {
            hostname
        } else {
            match hostname.split_once('.') {
                Some((hostname, _domain)) => hostname.to_string(),
                None => hostname,
            }
        }
    }

    fn resolve_local_user(&self) -> String {
        for user in &["USER", "USERNAME"] {
            if let Some(user) = self.resolve_env(user) {
                return user;
            }
        }
        "unknown-user".to_string()
    }

    /// Resolve the `IdentityFile` entries that apply to `host`, in
    /// the order OpenSSH would try them, with tilde, percent-token
    /// and `${VAR}` expansion applied per entry so that each
    /// returned path is ready to hand to `std::fs::read`.
    ///
    /// Thin wrapper around [`Config::resolve_host`]; prefer
    /// `resolve_host` when you also need the flat `ConfigMap` view.
    ///
    /// The ordering mirrors upstream `ssh_config(5)` semantics:
    ///
    /// 1. Entries declared outside any `Host`/`Match` stanza (file
    ///    global), in declaration order.
    /// 2. Entries from each `Host`/`Match` stanza whose pattern
    ///    matches `host`, in the order the stanzas appear on disk,
    ///    and for each stanza in declaration order.
    ///
    /// If no `IdentityFile` directive matched anywhere, the built-in
    /// OpenSSH defaults are substituted: `~/.ssh/id_dsa`,
    /// `~/.ssh/id_ecdsa`, `~/.ssh/id_ed25519`, `~/.ssh/id_rsa`, in
    /// that order. This matches OpenSSH's behaviour in
    /// `readconf.c::fill_default_options`: defaults are only added
    /// when the user did not supply any `IdentityFile` of their own.
    pub fn resolve_identity_files<H: AsRef<str>>(&self, host: H) -> Vec<IdentityFileEntry> {
        self.resolve_host(host).identity_files
    }

    /// Collect the raw, **unexpanded** identity file entries that
    /// apply to `host`. Used internally by [`Config::resolve_host`]
    /// before per-entry token / environment expansion. See
    /// [`Config::resolve_identity_files`] for the public entry
    /// point with expansion applied.
    fn collect_raw_identity_files(&self, host: &str) -> Vec<IdentityFileEntry> {
        let local_user = self.resolve_local_user();
        let target_user = &local_user;
        let mut entries: Vec<IdentityFileEntry> = Vec::new();

        for config in &self.config_files {
            config.collect_identity_files(
                host,
                target_user,
                &local_user,
                Context::FirstPass,
                &mut entries,
            );
        }

        if entries.is_empty() {
            if let Some(home) = self.resolve_home() {
                // The OpenSSH default identity list. `id_dsa` is
                // **deprecated** and slated for removal in a future
                // wezterm release: OpenSSH itself disabled DSA at the
                // protocol level in 2015 and has been removing
                // remaining DSA support across releases since
                // OpenSSH 9.x. The new in-tree envelope parser in
                // `pubkey_from_private` does not understand DSA's
                // PEM-PKCS#8 layout (DSA keys never use the
                // `openssh-key-v1` format), so the agent-key filter
                // for `IdentitiesOnly=yes` cannot match a DSA
                // identity even today. The path is kept in the
                // default list for now to avoid breaking any setup
                // that still relies on the ssh2/libssh backends'
                // native DSA loaders, but new configurations should
                // not depend on it.
                for name in ["id_dsa", "id_ecdsa", "id_ed25519", "id_rsa"] {
                    entries.push(IdentityFileEntry::new(format!("{}/.ssh/{}", home, name)));
                }
            }
        }

        entries
    }

    /// Build the `%`-token map in exactly the same shape that
    /// [`Config::for_host`] uses for flat-map expansion, so that
    /// the typed `IdentityFile` list expands with the same
    /// `%h`/`%p`/`%r`/`%n` values.
    fn build_token_map_for_host(&self, host: &str, resolved: &ConfigMap) -> ConfigMap {
        let target_user = self.resolve_local_user();
        let mut token_map = self.tokens.clone();
        let hostname = resolved
            .get("hostname")
            .cloned()
            .unwrap_or_else(|| host.to_string());
        let port = resolved
            .get("port")
            .cloned()
            .unwrap_or_else(|| "22".to_string());
        token_map.insert("%h".to_string(), hostname);
        token_map.insert("%n".to_string(), host.to_string());
        token_map.insert("%r".to_string(), target_user);
        token_map.insert("%p".to_string(), port);
        token_map
    }

    /// Expand tilde, `%`-tokens and `${VAR}` references in a single
    /// path string, **without** whitespace-splitting the value.
    /// This is the per-entry equivalent of `expand_tokens` +
    /// `expand_environment` as used by [`Config::for_host`] on the
    /// flat map, but safe for identity paths that legitimately
    /// contain spaces.
    fn expand_identity_path(&self, path: &mut String, token_map: &ConfigMap) {
        // Tilde shortcut at the very start of the path.
        if path.starts_with("~/") {
            if let Some(home) = self.resolve_home() {
                path.replace_range(0..1, &home);
            }
        }

        // Percent-token substitution on the whole string.
        let tokens: [&str; 10] = ["%C", "%d", "%h", "%i", "%L", "%l", "%n", "%p", "%r", "%u"];
        for &t in &tokens {
            if !path.contains(t) {
                continue;
            }
            let replacement: Option<String> = if let Some(v) = token_map.get(t) {
                Some(v.clone())
            } else if t == "%i" {
                Some(self.resolve_uid())
            } else if t == "%u" {
                Some(self.resolve_local_user())
            } else if t == "%l" {
                Some(self.resolve_local_host(false))
            } else if t == "%L" {
                Some(self.resolve_local_host(true))
            } else if t == "%d" {
                self.resolve_home()
            } else if t == "%C" {
                // Hash of %l%h%p%r%j, recursively expanded against
                // the same token_map. Uses the existing
                // `expand_tokens` because the intermediate value is
                // a synthesised non-path string, not a user-visible
                // path.
                use sha2::Digest;
                let mut c_value = "%l%h%p%r%j".to_string();
                self.expand_tokens(&mut c_value, &["%l", "%h", "%p", "%r", "%j"], token_map);
                Some(hex::encode(sha2::Sha256::digest(c_value.as_bytes())))
            } else {
                None
            };
            if let Some(v) = replacement {
                *path = path.replace(t, &v);
            }
        }

        // `%%` escape → literal `%`.
        *path = path.replace("%%", "%");

        // `${VAR}` environment substitution.
        self.expand_environment(path);
    }

    /// Resolve both the flat ConfigMap and the typed IdentityFile
    /// list for the given host in a single call. This is the
    /// recommended entry point for [`crate::Session::connect`] so
    /// that consumers see a consistent view of both representations.
    ///
    /// `IdentityFile` paths are expanded per entry (tilde, `%`
    /// tokens, `${VAR}`) using the same resolution rules as the
    /// flat map, but without the whitespace-splitting step that the
    /// flat map applies — paths containing spaces survive intact.
    pub fn resolve_host<H: AsRef<str>>(&self, host: H) -> HostOptions {
        let host = host.as_ref();
        let options = self.for_host(host);
        let token_map = self.build_token_map_for_host(host, &options);

        let mut identity_files = self.collect_raw_identity_files(host);
        for entry in &mut identity_files {
            self.expand_identity_path(&mut entry.path, &token_map);
        }

        HostOptions {
            options,
            identity_files,
        }
    }

    /// Resolve the configuration for a given host as a flat
    /// [`ConfigMap`].
    ///
    /// The returned map will expand environment and tokens for
    /// options where that is specified. Note that in some
    /// configurations, the config should be parsed once to
    /// resolve the main configuration, and then based on some
    /// options (such as `CanonicalHostname`), the tokens should
    /// be updated and the config parsed a second time in order
    /// for value expansion to have the same results as `ssh`.
    ///
    /// **For `IdentityFile` entries containing spaces**, prefer
    /// [`Config::resolve_host`] — the flat `ConfigMap` joins
    /// multiple IdentityFile directives with a single space
    /// separator, which cannot be round-tripped unambiguously
    /// when any of the paths themselves contain whitespace.
    /// `resolve_host` returns a typed list that survives this
    /// round-trip and is the recommended input for
    /// [`crate::Session::connect`].
    pub fn for_host<H: AsRef<str>>(&self, host: H) -> ConfigMap {
        let host = host.as_ref();
        let local_user = self.resolve_local_user();
        let target_user = &local_user;

        let mut result = self.options.clone();
        let mut needs_reparse = false;

        for config in &self.config_files {
            if config.apply_matches(
                host,
                target_user,
                &local_user,
                Context::FirstPass,
                &mut result,
            ) {
                needs_reparse = true;
            }
        }

        if needs_reparse {
            log::debug!(
                "ssh configuration uses options that require two-phase \
                parsing, which isn't supported"
            );
        }

        let mut token_map = self.tokens.clone();
        token_map.insert("%h".to_string(), host.to_string());
        result
            .entry("hostname".to_string())
            .and_modify(|curr| {
                if let Some(tokens) = self.should_expand_tokens("hostname") {
                    self.expand_tokens(curr, tokens, &token_map);
                }
            })
            .or_insert_with(|| host.to_string());
        token_map.insert("%h".to_string(), result["hostname"].to_string());
        token_map.insert("%n".to_string(), host.to_string());
        token_map.insert("%r".to_string(), target_user.to_string());
        token_map.insert(
            "%p".to_string(),
            result
                .get("port")
                .map(|p| p.to_string())
                .unwrap_or_else(|| "22".to_string()),
        );

        for (k, v) in &mut result {
            if let Some(tokens) = self.should_expand_tokens(k) {
                self.expand_tokens(v, tokens, &token_map);
            }

            if self.should_expand_environment(k) {
                self.expand_environment(v);
            }
        }

        result
            .entry("port".to_string())
            .or_insert_with(|| "22".to_string());

        result
            .entry("user".to_string())
            .or_insert_with(|| target_user.clone());

        if !result.contains_key("userknownhostsfile") {
            if let Some(home) = self.resolve_home() {
                result.insert(
                    "userknownhostsfile".to_string(),
                    format!("{}/.ssh/known_hosts {}/.ssh/known_hosts2", home, home,),
                );
            }
        }

        if !result.contains_key("identityfile") {
            if let Some(home) = self.resolve_home() {
                // Mirrors the typed default list in
                // `collect_raw_identity_files`. `id_dsa` is
                // deprecated (see the longer notice on that helper)
                // and will be dropped in a future wezterm release;
                // for now both views agree to avoid drift.
                result.insert(
                    "identityfile".to_string(),
                    format!(
                        "{}/.ssh/id_dsa {}/.ssh/id_ecdsa {}/.ssh/id_ed25519 {}/.ssh/id_rsa",
                        home, home, home, home
                    ),
                );
            }
        }

        if !result.contains_key("identityagent") {
            if let Some(sock_path) = self.resolve_env("SSH_AUTH_SOCK") {
                result.insert("identityagent".to_string(), sock_path);
            }
        }

        result
    }

    /// Return true if a given option name is subject to environment variable
    /// expansion.
    fn should_expand_environment(&self, key: &str) -> bool {
        match key {
            "certificatefile" | "controlpath" | "identityagent" | "identityfile"
            | "userknownhostsfile" | "localforward" | "remoteforward" => true,
            _ => false,
        }
    }

    /// Returns a set of tokens that should be expanded for a given option name
    fn should_expand_tokens(&self, key: &str) -> Option<&[&str]> {
        match key {
            "certificatefile" | "controlpath" | "identityagent" | "identityfile"
            | "localforward" | "remotecommand" | "remoteforward" | "userknownhostsfile" => {
                Some(&["%C", "%d", "%h", "%i", "%L", "%l", "%n", "%p", "%r", "%u"])
            }
            "hostname" => Some(&["%h"]),
            "localcommand" => Some(&[
                "%C", "%d", "%h", "%i", "%k", "%L", "%l", "%n", "%p", "%r", "%T", "%u",
            ]),
            "proxycommand" => Some(&["%h", "%n", "%p", "%r"]),
            _ => None,
        }
    }

    /// Resolve the home directory.
    /// For the sake of unit testing, this will look for HOME in the provided
    /// environment override before asking the system for the home directory.
    fn resolve_home(&self) -> Option<String> {
        if let Some(env) = self.environment.as_ref() {
            if let Some(home) = env.get("HOME") {
                return Some(home.to_string());
            }
        }
        if let Some(home) = dirs_next::home_dir() {
            if let Some(home) = home.to_str() {
                return Some(home.to_string());
            }
        }
        None
    }

    fn resolve_uid(&self) -> String {
        #[cfg(test)]
        if let Some(env) = self.environment.as_ref() {
            // For testing purposes only, allow pretending that we
            // have a specific fixed UID so that test expectations
            // are easier to handle with snapshots
            if let Some(uid) = env.get("WEZTERM_SSH_UID") {
                return uid.to_string();
            }
        }

        #[cfg(unix)]
        {
            let uid = unsafe { libc::getuid() };
            return uid.to_string();
        }

        #[cfg(not(unix))]
        {
            String::new()
        }
    }

    /// Perform token substitution
    fn expand_tokens(&self, value: &mut String, tokens: &[&str], token_map: &ConfigMap) {
        let orig_value = value.to_string();
        for &t in tokens {
            if let Some(v) = token_map.get(t) {
                *value = value.replace(t, v);
            } else if t == "%i" {
                *value = value.replace(t, &self.resolve_uid());
            } else if t == "%u" {
                *value = value.replace(t, &self.resolve_local_user());
            } else if t == "%l" {
                *value = value.replace(t, &self.resolve_local_host(false));
            } else if t == "%L" {
                *value = value.replace(t, &self.resolve_local_host(true));
            } else if t == "%d" {
                if let Some(home) = self.resolve_home() {
                    let mut items = value
                        .split_whitespace()
                        .map(|s| s.to_string())
                        .collect::<Vec<String>>();
                    for item in &mut items {
                        if item.starts_with("~/") {
                            item.replace_range(0..1, &home);
                        } else {
                            *item = item.replace(t, &home);
                        }
                    }
                    *value = items.join(" ");
                }
            } else if t == "%j" {
                // %j: The contents of the ProxyJump option, or the empty string if this option is unset
                // We don't directly support ProxyJump, and this %j token referencing
                // may technically put this into two-phase evaluation territory which
                // we don't support.
                // Let's silently gloss over this and treat this token as the empty
                // string.
                // Someone in the future will probably curse this.
                *value = value.replace(t, "");
            } else if t == "%T" {
                // %T: The local tun(4) or tap(4) network interface assigned if tunnel
                // forwarding was requested, or "NONE" otherwise.
                // We don't support this function, so it is always NONE
                *value = value.replace(t, "NONE");
            } else if t == "%C" && value.contains("%C") {
                // %C: Hash of %l%h%p%r%j
                use sha2::Digest;
                let mut c_value = "%l%h%p%r%j".to_string();
                self.expand_tokens(&mut c_value, tokens, token_map);
                let hashed = hex::encode(sha2::Sha256::digest(&c_value.as_bytes()));
                *value = value.replace("%C", &hashed);
            } else if value.contains(t) {
                log::warn!("Unsupported token {t} when evaluating `{orig_value}`");
            }
        }

        *value = value.replace("%%", "%");
    }

    /// Resolve an environment variable; if an override is set use that,
    /// otherwise read from the real environment.
    fn resolve_env(&self, name: &str) -> Option<String> {
        if let Some(env) = self.environment.as_ref() {
            env.get(name).cloned()
        } else {
            std::env::var(name).ok()
        }
    }

    /// Look for `${NAME}` and substitute the value of the `NAME` env var
    /// into the provided string.
    fn expand_environment(&self, value: &mut String) {
        let re = Regex::new(r#"\$\{([a-zA-Z_][a-zA-Z_0-9]+)\}"#).unwrap();
        *value = re
            .replace_all(value, |caps: &Captures| -> String {
                if let Some(rep) = self.resolve_env(&caps[1]) {
                    rep
                } else {
                    caps[0].to_string()
                }
            })
            .to_string();
    }

    /// Returns the list of file names that were loaded as part of parsing
    /// the ssh config
    pub fn loaded_config_files(&self) -> Vec<PathBuf> {
        let mut files = vec![];

        for config in &self.config_files {
            for file in &config.loaded_files {
                if !files.contains(file) {
                    files.push(file.to_path_buf());
                }
            }
        }

        files
    }

    /// Returns the list of host names that have defined ssh config entries.
    /// The host names are literal (non-pattern), non-negated hosts extracted
    /// from `Host` and `Match` stanzas in the ssh config.
    pub fn enumerate_hosts(&self) -> Vec<String> {
        let mut hosts = vec![];

        for config in &self.config_files {
            for group in &config.groups {
                for c in &group.criteria {
                    if let Criteria::Host(patterns) = c {
                        for pattern in patterns {
                            if pattern.is_literal && !pattern.negated {
                                if !hosts.contains(&pattern.original) {
                                    hosts.push(pattern.original.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        hosts
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use k9::snapshot;

    #[test]
    fn parse_keepalive() {
        let mut config = Config::new();
        config.add_config_string(
            r#"
        Host foo
            ServerAliveInterval 60
            "#,
        );
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        let opts = config.for_host("foo");
        snapshot!(
            opts,
            r#"
{
    "hostname": "foo",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "serveraliveinterval": "60",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn parse_proxy_command_tokens() {
        let mut config = Config::new();
        config.add_config_string(
            r#"
        Host foo
            ProxyCommand /usr/bin/corp-ssh-helper -dst_username=%r %h %p
            Port 2222
            "#,
        );
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        let opts = config.for_host("foo");
        snapshot!(
            opts,
            r#"
{
    "hostname": "foo",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "2222",
    "proxycommand": "/usr/bin/corp-ssh-helper -dst_username=me foo 2222",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn parse_proxy_command() {
        let mut config = Config::new();
        config.add_config_string(
            r#"
        Host foo
            ProxyCommand /usr/bin/ssh-proxy-helper -oX=Y host 22
            "#,
        );

        snapshot!(
            &config,
            r#"
Config {
    config_files: [
        ParsedConfigFile {
            options: {},
            groups: [
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^foo$",
                                    original: "foo",
                                    is_literal: true,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "proxycommand": "/usr/bin/ssh-proxy-helper -oX=Y host 22",
                    },
                    identity_files: [],
                },
            ],
            loaded_files: [],
            identity_files: [],
        },
    ],
    options: {},
    tokens: {},
    environment: None,
}
"#
        );
    }

    #[test]
    fn misc_tokens() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        fake_env.insert("WEZTERM_SSH_UID".to_string(), "1000".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host target-host
            LocalCommand C=%C d=%d h=%h i=%i L=%L l=%L n=%n p=%p r=%r T=%T u=%u
            "#,
        );

        let opts = config.for_host("target-host");
        snapshot!(
            opts,
            r#"
{
    "hostname": "target-host",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "localcommand": "C=8de28522efb92214d9c442ea0402863e34d095a4006467ad9136a48e930870ea d=/home/me h=target-host i=1000 L=localhost l=localhost n=target-host p=22 r=me T=NONE u=me",
    "port": "22",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn parse_user() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host foo
            HostName 10.0.0.1
            User foo
            IdentityFile "%d/.ssh/id_pub.dsa"
            "#,
        );

        snapshot!(
            &config,
            r#"
Config {
    config_files: [
        ParsedConfigFile {
            options: {},
            groups: [
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^foo$",
                                    original: "foo",
                                    is_literal: true,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "hostname": "10.0.0.1",
                        "identityfile": "%d/.ssh/id_pub.dsa",
                        "user": "foo",
                    },
                    identity_files: [
                        IdentityFileEntry {
                            path: "%d/.ssh/id_pub.dsa",
                        },
                    ],
                },
            ],
            loaded_files: [],
            identity_files: [],
        },
    ],
    options: {},
    tokens: {},
    environment: Some(
        {
            "HOME": "/home/me",
            "USER": "me",
        },
    ),
}
"#
        );

        let opts = config.for_host("foo");
        snapshot!(
            opts,
            r#"
{
    "hostname": "10.0.0.1",
    "identityfile": "/home/me/.ssh/id_pub.dsa",
    "port": "22",
    "user": "foo",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn hostname_expansion() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host foo0 foo1 foo2
            HostName server-%h
            "#,
        );

        let opts = config.for_host("foo0");
        snapshot!(
            opts,
            r#"
{
    "hostname": "server-foo0",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("foo1");
        snapshot!(
            opts,
            r#"
{
    "hostname": "server-foo1",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("foo2");
        snapshot!(
            opts,
            r#"
{
    "hostname": "server-foo2",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn userknownhostsfile_expands_host_token() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host example.com
            UserKnownHostsFile ~/.ssh/known_hosts.%h
            "#,
        );

        let opts = config.for_host("example.com");
        let value = opts
            .get("userknownhostsfile")
            .expect("userknownhostsfile should be set");
        assert!(
            value.contains("example.com"),
            "expected %h to expand to example.com, got {:?}",
            value
        );
        assert!(
            !value.contains("%h"),
            "expected %h token to be expanded, got {:?}",
            value
        );
    }

    #[test]
    fn parse_proxy_command_hostname_expansion() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host foo
            HostName server-%h
            ProxyCommand nc -x localhost:1080 %h %p
            "#,
        );

        let opts = config.for_host("foo");
        snapshot!(
            opts,
            r#"
{
    "hostname": "server-foo",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "proxycommand": "nc -x localhost:1080 server-foo 22",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn multiple_identityfile() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host foo
            HostName 10.0.0.1
            User foo
            IdentityFile "~/.ssh/id_pub.dsa"
            IdentityFile "~/.ssh/id_pub.rsa"
            "#,
        );

        let opts = config.for_host("foo");
        snapshot!(
            opts,
            r#"
{
    "hostname": "10.0.0.1",
    "identityfile": "/home/me/.ssh/id_pub.dsa /home/me/.ssh/id_pub.rsa",
    "port": "22",
    "user": "foo",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn sub_tilde() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        Host foo
            HostName 10.0.0.1
            User foo
            IdentityFile "~/.ssh/id_pub.dsa"
            "#,
        );

        let opts = config.for_host("foo");
        snapshot!(
            opts,
            r#"
{
    "hostname": "10.0.0.1",
    "identityfile": "/home/me/.ssh/id_pub.dsa",
    "port": "22",
    "user": "foo",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn parse_match() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        # I am a comment
        Something first
        # the prior Something takes precedence
        Something ignored
        Match Host 192.168.1.8,wopr
            FowardAgent yes
            IdentityFile "%d/.ssh/id_pub.dsa"

        Match Host !a.b,*.b User fred
            ForwardAgent no
            IdentityAgent "${HOME}/.ssh/agent"

        Match Host !a.b,*.b User me
            ForwardAgent no
            IdentityAgent "${HOME}/.ssh/agent-me"

        Host *
            Something  else
            "#,
        );

        snapshot!(
            &config,
            r#"
Config {
    config_files: [
        ParsedConfigFile {
            options: {
                "something": "first",
            },
            groups: [
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^192\\.168\\.1\\.8$",
                                    original: "192.168.1.8",
                                    is_literal: true,
                                },
                                Pattern {
                                    negated: false,
                                    pattern: "^wopr$",
                                    original: "wopr",
                                    is_literal: true,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "fowardagent": "yes",
                        "identityfile": "%d/.ssh/id_pub.dsa",
                    },
                    identity_files: [
                        IdentityFileEntry {
                            path: "%d/.ssh/id_pub.dsa",
                        },
                    ],
                },
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: true,
                                    pattern: "^a\\.b$",
                                    original: "a.b",
                                    is_literal: true,
                                },
                                Pattern {
                                    negated: false,
                                    pattern: "^.*\\.b$",
                                    original: "*.b",
                                    is_literal: false,
                                },
                            ],
                        ),
                        User(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^fred$",
                                    original: "fred",
                                    is_literal: true,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "forwardagent": "no",
                        "identityagent": "${HOME}/.ssh/agent",
                    },
                    identity_files: [],
                },
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: true,
                                    pattern: "^a\\.b$",
                                    original: "a.b",
                                    is_literal: true,
                                },
                                Pattern {
                                    negated: false,
                                    pattern: "^.*\\.b$",
                                    original: "*.b",
                                    is_literal: false,
                                },
                            ],
                        ),
                        User(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^me$",
                                    original: "me",
                                    is_literal: true,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "forwardagent": "no",
                        "identityagent": "${HOME}/.ssh/agent-me",
                    },
                    identity_files: [],
                },
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^.*$",
                                    original: "*",
                                    is_literal: false,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "something": "else",
                    },
                    identity_files: [],
                },
            ],
            loaded_files: [],
            identity_files: [],
        },
    ],
    options: {},
    tokens: {},
    environment: Some(
        {
            "HOME": "/home/me",
            "USER": "me",
        },
    ),
}
"#
        );

        snapshot!(
            config.enumerate_hosts(),
            r#"
[
    "192.168.1.8",
    "wopr",
]
"#
        );

        let opts = config.for_host("random");
        snapshot!(
            opts,
            r#"
{
    "hostname": "random",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("192.168.1.8");
        snapshot!(
            opts,
            r#"
{
    "fowardagent": "yes",
    "hostname": "192.168.1.8",
    "identityfile": "/home/me/.ssh/id_pub.dsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("a.b");
        snapshot!(
            opts,
            r#"
{
    "hostname": "a.b",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("b.b");
        snapshot!(
            opts,
            r#"
{
    "forwardagent": "no",
    "hostname": "b.b",
    "identityagent": "/home/me/.ssh/agent-me",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/fred".to_string());
        fake_env.insert("USER".to_string(), "fred".to_string());
        config.assign_environment(fake_env);

        let opts = config.for_host("b.b");
        snapshot!(
            opts,
            r#"
{
    "forwardagent": "no",
    "hostname": "b.b",
    "identityagent": "/home/fred/.ssh/agent",
    "identityfile": "/home/fred/.ssh/id_dsa /home/fred/.ssh/id_ecdsa /home/fred/.ssh/id_ed25519 /home/fred/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "fred",
    "userknownhostsfile": "/home/fred/.ssh/known_hosts /home/fred/.ssh/known_hosts2",
}
"#
        );
    }

    #[test]
    fn parse_simple() {
        let mut config = Config::new();

        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);

        config.add_config_string(
            r#"
        # I am a comment
        Something first
        # the prior Something takes precedence
        Something ignored
        Host 192.168.1.8 wopr
            FowardAgent yes
            IdentityFile "%d/.ssh/id_pub.dsa"

        Host !a.b *.b
            ForwardAgent no
            IdentityAgent "${HOME}/.ssh/agent"

        Host *
            Something  else
            "#,
        );

        snapshot!(
            &config,
            r#"
Config {
    config_files: [
        ParsedConfigFile {
            options: {
                "something": "first",
            },
            groups: [
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^192\\.168\\.1\\.8$",
                                    original: "192.168.1.8",
                                    is_literal: true,
                                },
                                Pattern {
                                    negated: false,
                                    pattern: "^wopr$",
                                    original: "wopr",
                                    is_literal: true,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "fowardagent": "yes",
                        "identityfile": "%d/.ssh/id_pub.dsa",
                    },
                    identity_files: [
                        IdentityFileEntry {
                            path: "%d/.ssh/id_pub.dsa",
                        },
                    ],
                },
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: true,
                                    pattern: "^a\\.b$",
                                    original: "a.b",
                                    is_literal: true,
                                },
                                Pattern {
                                    negated: false,
                                    pattern: "^.*\\.b$",
                                    original: "*.b",
                                    is_literal: false,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "forwardagent": "no",
                        "identityagent": "${HOME}/.ssh/agent",
                    },
                    identity_files: [],
                },
                MatchGroup {
                    criteria: [
                        Host(
                            [
                                Pattern {
                                    negated: false,
                                    pattern: "^.*$",
                                    original: "*",
                                    is_literal: false,
                                },
                            ],
                        ),
                    ],
                    context: FirstPass,
                    options: {
                        "something": "else",
                    },
                    identity_files: [],
                },
            ],
            loaded_files: [],
            identity_files: [],
        },
    ],
    options: {},
    tokens: {},
    environment: Some(
        {
            "HOME": "/home/me",
            "USER": "me",
        },
    ),
}
"#
        );

        let opts = config.for_host("random");
        snapshot!(
            opts,
            r#"
{
    "hostname": "random",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("192.168.1.8");
        snapshot!(
            opts,
            r#"
{
    "fowardagent": "yes",
    "hostname": "192.168.1.8",
    "identityfile": "/home/me/.ssh/id_pub.dsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("a.b");
        snapshot!(
            opts,
            r#"
{
    "hostname": "a.b",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );

        let opts = config.for_host("b.b");
        snapshot!(
            opts,
            r#"
{
    "forwardagent": "no",
    "hostname": "b.b",
    "identityagent": "/home/me/.ssh/agent",
    "identityfile": "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
    "port": "22",
    "something": "first",
    "user": "me",
    "userknownhostsfile": "/home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2",
}
"#
        );
    }

    // ---------------------------------------------------------------
    // Characterization tests (Wave 1 of ssh_config parser refactor)
    //
    // Each test carries the OpenSSH ground truth (`ssh -G`) in a
    // comment. The snapshot records what wezterm-ssh actually does
    // today. Divergences are intentional — they pin the current
    // (partially buggy) behaviour as a baseline so later refactor
    // waves can show the exact diff.
    // ---------------------------------------------------------------

    fn characterize_parse(config_str: &str) -> ConfigMap {
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(config_str);
        config.for_host("foo")
    }

    fn characterize_identity(config_str: &str) -> Option<String> {
        characterize_parse(config_str).get("identityfile").cloned()
    }

    #[test]
    fn characterize_identity_file_simple() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/id_rsa
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/id_rsa
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/id_rsa",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_quoted_space() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/path with space/id_rsa
        // (outer quotes stripped, inner space preserved)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile "/home/me/.ssh/path with space/id_rsa"
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/path with space/id_rsa",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_escaped_space() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/path with space/id_rsa
        // (backslash-escapes consumed, inner space preserved)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/path\ with\ space/id_rsa
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/path with space/id_rsa",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_single_quotes() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/single quoted
        // (single quotes are treated identically to double quotes)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile '/home/me/.ssh/single quoted'
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/single quoted",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_two_tokens_one_line() {
        // OpenSSH `ssh -G` HARD ERROR:
        //   "keyword identityfile extra arguments at end of line"
        //   terminating, 1 bad configuration options (exit 255)
        // wezterm-ssh currently accepts and stores both tokens; after
        // the refactor this should become a parser error.
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/first /home/me/.ssh/second
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/first",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_multi_line() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/first
        //   identityfile /home/me/.ssh/second
        //   identityfile /home/me/.ssh/third
        // (three additive, order preserved)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/first
                IdentityFile /home/me/.ssh/second
                IdentityFile /home/me/.ssh/third
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/first /home/me/.ssh/second /home/me/.ssh/third",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_multi_mixed_quotes() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/first one
        //   identityfile /home/me/.ssh/second
        //   identityfile /home/me/.ssh/third path
        // (three distinct paths, some with inner spaces, all intact)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile "/home/me/.ssh/first one"
                IdentityFile /home/me/.ssh/second
                IdentityFile /home/me/.ssh/third\ path
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/first one /home/me/.ssh/second /home/me/.ssh/third path",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_none_literal() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/first
        //   identityfile none
        //   identityfile /home/me/.ssh/second
        // (`none` is a literal entry at the ssh_config layer; the
        // special semantics fire later in load_public_identity_files
        // in ssh.c:2400 – not the parser's concern)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/first
                IdentityFile none
                IdentityFile /home/me/.ssh/second
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/first none /home/me/.ssh/second",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_unbalanced_quote() {
        // OpenSSH `ssh -G` HARD ERROR:
        //   terminating, 1 bad configuration options
        // wezterm-ssh currently accepts the input silently and keeps
        // the literal `"` character in the stored value.
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile "/home/me/.ssh/unclosed
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/id_dsa /home/me/.ssh/id_ecdsa /home/me/.ssh/id_ed25519 /home/me/.ssh/id_rsa",
)
"#
        );
    }

    #[test]
    fn characterize_identity_file_trailing_comment() {
        // OpenSSH `ssh -G`:
        //   identityfile /home/me/.ssh/id_rsa
        // (trailing `#` comment stripped by the tokenizer)
        let ident = characterize_identity(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/id_rsa # trailing comment
            "#,
        );
        snapshot!(
            ident,
            r#"
Some(
    "/home/me/.ssh/id_rsa",
)
"#
        );
    }

    #[test]
    fn characterize_identities_only_mixed_case_value() {
        // OpenSSH `ssh -G` normalises the value to lowercase:
        //   identitiesonly yes
        // wezterm-ssh preserves the original casing at parse time and
        // relies on case-insensitive compare at the consumer site.
        let opts = characterize_parse(
            r#"
            Host foo
                IdentitiesOnly YES
                IdentityFile /home/me/.ssh/id_rsa
            "#,
        );
        snapshot!(
            opts.get("identitiesonly").cloned(),
            r#"
Some(
    "YES",
)
"#
        );
    }

    // ---------------------------------------------------------------
    // Target tests (Wave 4): these assert the spec-correct behaviour
    // of `Config::resolve_identity_files`, which is the typed
    // replacement for the legacy space-concatenated ConfigMap
    // string. Paths containing whitespace round-trip cleanly here
    // even though the characterisation tests above still show the
    // lossy concat on the legacy API.
    // ---------------------------------------------------------------

    fn resolve_identity_file_paths(config_str: &str) -> Vec<String> {
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(config_str);
        config
            .resolve_identity_files("foo")
            .into_iter()
            .map(|e| e.path)
            .collect()
    }

    #[test]
    fn resolve_identity_files_default_list_when_empty() {
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                User me
            "#,
        );
        assert_eq!(
            paths,
            vec![
                "/home/me/.ssh/id_dsa".to_string(),
                "/home/me/.ssh/id_ecdsa".to_string(),
                "/home/me/.ssh/id_ed25519".to_string(),
                "/home/me/.ssh/id_rsa".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_identity_files_preserves_declaration_order() {
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/first
                IdentityFile /home/me/.ssh/second
                IdentityFile /home/me/.ssh/third
            "#,
        );
        assert_eq!(
            paths,
            vec![
                "/home/me/.ssh/first".to_string(),
                "/home/me/.ssh/second".to_string(),
                "/home/me/.ssh/third".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_identity_files_quoted_space_is_distinct_from_separator() {
        // The key guarantee: quoted and escaped paths containing
        // whitespace survive as individual entries, distinct from
        // neighbouring paths. This is the fix for the
        // characterize_identity_file_multi_mixed_quotes info loss.
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                IdentityFile "/home/me/.ssh/first one"
                IdentityFile /home/me/.ssh/second
                IdentityFile /home/me/.ssh/third\ path
            "#,
        );
        assert_eq!(
            paths,
            vec![
                "/home/me/.ssh/first one".to_string(),
                "/home/me/.ssh/second".to_string(),
                "/home/me/.ssh/third path".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_identity_files_none_is_literal_at_parser_layer() {
        // `none` special-casing happens in OpenSSH's
        // load_public_identity_files, not at the parser. At our
        // layer it is just another literal path entry.
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                IdentityFile /home/me/.ssh/first
                IdentityFile none
                IdentityFile /home/me/.ssh/second
            "#,
        );
        assert_eq!(
            paths,
            vec![
                "/home/me/.ssh/first".to_string(),
                "none".to_string(),
                "/home/me/.ssh/second".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_identity_files_file_global_first_then_host_group() {
        // A directive outside any Host stanza applies globally and
        // is tried before per-host entries.
        let paths = resolve_identity_file_paths(
            r#"
            IdentityFile /home/me/.ssh/global_first

            Host foo
                IdentityFile /home/me/.ssh/host_second
            "#,
        );
        assert_eq!(
            paths,
            vec![
                "/home/me/.ssh/global_first".to_string(),
                "/home/me/.ssh/host_second".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_identity_files_only_matching_host_contributes() {
        let paths = resolve_identity_file_paths(
            r#"
            Host bar
                IdentityFile /home/me/.ssh/bar_only

            Host foo
                IdentityFile /home/me/.ssh/foo_only
            "#,
        );
        assert_eq!(paths, vec!["/home/me/.ssh/foo_only".to_string()]);
    }

    #[test]
    fn host_options_push_identity_file_keeps_typed_list_and_flat_map_in_sync() {
        // Regression guard for the `-o IdentityFile=...` CLI override
        // path: the typed list and the legacy `ConfigMap` entry must
        // not drift.
        let mut host_options = HostOptions::default();
        host_options.push_identity_file("/home/me/.ssh/first");
        host_options.push_identity_file("/home/me/.ssh/second");

        assert_eq!(host_options.identity_files.len(), 2);
        assert_eq!(host_options.identity_files[0].path, "/home/me/.ssh/first");
        assert_eq!(host_options.identity_files[1].path, "/home/me/.ssh/second");

        // The flat `ConfigMap` entry follows the legacy space-joined
        // convention so downstream code reading it observes both
        // overrides.
        assert_eq!(
            host_options.options.get("identityfile").cloned(),
            Some("/home/me/.ssh/first /home/me/.ssh/second".to_string())
        );
    }

    #[test]
    fn resolve_identity_files_expands_tilde_in_path() {
        // `~/.ssh/id_rsa` must become `/home/me/.ssh/id_rsa` so
        // that `std::fs::read` can open it directly — unexpanded
        // paths would silently fail the agent filter and pubkey
        // auth.
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                IdentityFile ~/.ssh/id_rsa
            "#,
        );
        assert_eq!(paths, vec!["/home/me/.ssh/id_rsa".to_string()]);
    }

    #[test]
    fn resolve_identity_files_expands_percent_d_to_home() {
        // `%d` is OpenSSH's alias for the current user's home
        // directory. It must expand per entry without
        // whitespace-splitting the path.
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                IdentityFile %d/.ssh/id_rsa
            "#,
        );
        assert_eq!(paths, vec!["/home/me/.ssh/id_rsa".to_string()]);
    }

    #[test]
    fn resolve_identity_files_expands_percent_h_and_percent_r() {
        // `%h` expands to the resolved hostname, `%r` to the target
        // user. Both must be populated from the same token_map
        // that `for_host` uses so the flat map and the typed list
        // agree.
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                User someone
                IdentityFile /home/me/.ssh/%r@%h
            "#,
        );
        assert_eq!(paths, vec!["/home/me/.ssh/me@foo".to_string()]);
    }

    #[test]
    fn resolve_identity_files_expansion_preserves_space_in_quoted_path() {
        // Regression guard: a quoted path containing both a space
        // and a `%d` token must expand `%d` without ever
        // whitespace-splitting, which would otherwise tear
        // `path with space/id_rsa` into three fragments.
        let paths = resolve_identity_file_paths(
            r#"
            Host foo
                IdentityFile "%d/path with space/id_rsa"
            "#,
        );
        assert_eq!(paths, vec!["/home/me/path with space/id_rsa".to_string()]);
    }

    #[test]
    fn host_options_push_identity_file_appends_onto_existing_legacy_value() {
        // If the flat map already carries an `identityfile` (e.g.
        // because `Config::resolve_host` populated it from the
        // parsed ssh_config), pushing an override must extend it
        // rather than replace it.
        let mut host_options = HostOptions::default();
        host_options
            .options
            .insert("identityfile".to_string(), "/existing".to_string());
        host_options.push_identity_file("/new");

        assert_eq!(
            host_options.options.get("identityfile").cloned(),
            Some("/existing /new".to_string())
        );
        assert_eq!(host_options.identity_files.len(), 1);
        assert_eq!(host_options.identity_files[0].path, "/new");
    }

    // ---------------------------------------------------------------
    // H11: direct unit tests closing the coverage gaps surfaced by
    // the 24-agent review. These exercise paths that were previously
    // only reached indirectly (or not at all) by the existing
    // suite.
    // ---------------------------------------------------------------

    #[test]
    fn resolve_host_returns_options_and_identity_files_consistently() {
        // The main public entry point `resolve_host` should produce
        // a `HostOptions` whose flat `ConfigMap` and typed
        // `identity_files` list agree for a straightforward parsed
        // config. Previously only indirectly covered via
        // `resolve_identity_files` tests.
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(
            r#"
            Host foo
                User someone
                IdentityFile /home/me/.ssh/custom_key
            "#,
        );

        let resolved = config.resolve_host("foo");

        assert_eq!(
            resolved.options.get("user").cloned(),
            Some("someone".to_string())
        );
        assert_eq!(
            resolved.options.get("hostname").cloned(),
            Some("foo".to_string())
        );
        assert_eq!(resolved.identity_files.len(), 1);
        assert_eq!(resolved.identity_files[0].path, "/home/me/.ssh/custom_key");
    }

    #[test]
    fn from_configmap_splits_identityfile_on_whitespace_silently() {
        // Compat conversion pins: legacy `From<ConfigMap>` splits
        // the `identityfile` value on whitespace, which cannot
        // represent paths containing spaces. This test documents
        // the known-lossy round-trip and serves as a deprecation
        // signal for callers who land on this route instead of the
        // correct `Config::resolve_host` entry point.
        let mut map = ConfigMap::new();
        map.insert(
            "identityfile".to_string(),
            "/a/b /c/d with space".to_string(),
        );

        let host_options: HostOptions = map.into();

        let paths: Vec<String> = host_options
            .identity_files
            .iter()
            .map(|e| e.path.clone())
            .collect();

        // The intended single path `/c/d with space` has been
        // shredded into three separate bogus entries. This is the
        // whole reason the typed list was introduced; callers who
        // need correctness should use `Config::resolve_host`.
        assert_eq!(
            paths,
            vec![
                "/a/b".to_string(),
                "/c/d".to_string(),
                "with".to_string(),
                "space".to_string()
            ]
        );
    }

    #[test]
    fn match_all_applies_identityfile_to_any_host() {
        // `Match all` is supposed to match every hostname. The
        // criterion was never exercised by the test suite before.
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(
            r#"
            Match all
                IdentityFile /home/me/.ssh/universal_key
            "#,
        );

        let paths: Vec<String> = config
            .resolve_identity_files("any_host_at_all")
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, vec!["/home/me/.ssh/universal_key".to_string()]);
    }

    #[test]
    fn match_originalhost_applies_on_host_pattern() {
        // `Match originalhost` shares the hostname-matching path
        // in `is_match`. The test pins the current behaviour.
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(
            r#"
            Match originalhost bar
                IdentityFile /home/me/.ssh/bar_key
            "#,
        );

        let matching: Vec<String> = config
            .resolve_identity_files("bar")
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(matching, vec!["/home/me/.ssh/bar_key".to_string()]);

        let non_matching: Vec<String> = config
            .resolve_identity_files("quux")
            .into_iter()
            .map(|e| e.path)
            .collect();
        // A non-matching host falls back to the OpenSSH defaults.
        assert!(!non_matching.contains(&"/home/me/.ssh/bar_key".to_string()));
    }

    #[test]
    fn match_localuser_applies_on_local_user_pattern() {
        // `Match localuser` matches against the environment USER
        // (fake_env provides it). The criterion was previously
        // untested.
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(
            r#"
            Match localuser me
                IdentityFile /home/me/.ssh/me_key
            "#,
        );

        let paths: Vec<String> = config
            .resolve_identity_files("irrelevant_host")
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, vec!["/home/me/.ssh/me_key".to_string()]);
    }

    #[test]
    fn resolve_identity_files_expands_env_var_in_path() {
        // `${VAR}` expansion inside an IdentityFile path goes
        // through the new `expand_identity_path` helper (which in
        // turn delegates to `expand_environment`). Previously only
        // IdentityAgent tests touched the environment expansion
        // path; IdentityFile needed its own guard.
        let mut config = Config::new();
        let mut fake_env = ConfigMap::new();
        fake_env.insert("HOME".to_string(), "/home/me".to_string());
        fake_env.insert("USER".to_string(), "me".to_string());
        fake_env.insert("SSH_KEY_DIR".to_string(), "/custom/keys".to_string());
        config.assign_environment(fake_env);
        config.add_config_string(
            r#"
            Host foo
                IdentityFile ${SSH_KEY_DIR}/id_ed25519
            "#,
        );

        let paths: Vec<String> = config
            .resolve_identity_files("foo")
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, vec!["/custom/keys/id_ed25519".to_string()]);
    }
}
