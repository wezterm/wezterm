use crate::config::validate_domain_name;
use crate::*;
use luahelper::impl_lua_conversion_dynamic;
use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Default, Debug, Clone, PartialEq, FromDynamic, ToDynamic)]
pub struct WslDomain {
    #[dynamic(validate = "validate_domain_name")]
    pub name: String,
    pub distribution: Option<String>,
    pub username: Option<String>,
    pub default_cwd: Option<PathBuf>,
    pub default_prog: Option<Vec<String>>,
}
impl_lua_conversion_dynamic!(WslDomain);

lazy_static::lazy_static! {
    /// Caches the result of `discover_wsl_domains()` for the lifetime of
    /// the process, used internally by `default_domains()` (see its doc
    /// comment). Deliberately *not* consulted by the public
    /// `wezterm.default_wsl_domains()` Lua API / `compute_default_domains()`,
    /// which is documented to reflect what's currently installed --
    /// callers who invoke it directly are opting into paying the
    /// discovery cost for a live answer, same as on `main` before this
    /// cache existed.
    ///
    /// Only ever locked for the brief "check/publish the cached value"
    /// steps, never while `wsl.exe` itself is running: holding it across
    /// the subprocess call would mean any concurrent caller blocks for
    /// the same duration a hung/slow `wsl.exe` does, with no bound.
    ///
    /// Exposed as `pub(crate)` for test access.
    pub(crate) static ref DEFAULT_DOMAINS_CACHE: Mutex<Option<Vec<WslDomain>>> = Mutex::new(None);

    /// Tracks the timestamp when the currently-in-flight discovery started.
    /// `None` means no discovery is running. This is accessed together with
    /// `DEFAULT_DOMAINS_CACHE`'s mutex, so there's no race between checking
    /// the flag and reading/writing the cache.
    ///
    /// Exposed as `pub(crate)` for test access.
    pub(crate) static ref DISCOVERY_CLAIM_START: Mutex<Option<Instant>> = Mutex::new(None);

    /// Signalled unconditionally whenever any `DiscoveryGuard` is dropped
    /// -- whether or not that guard actually still owned the claim it
    /// clears (see the `Drop` impl for why a guard whose claim was stolen
    /// by an abandonment-takeover clears nothing). A parked waiter always
    /// has to re-check the cache and claim state from scratch after being
    /// woken regardless of which guard woke it, so a spurious wakeup here
    /// costs nothing: the alternative (only notifying when a claim was
    /// actually cleared) would leave a waiter parked on a stolen claim's
    /// original owner asleep until its own deadline even after the result
    /// it's waiting for has already been published to the cache by the
    /// guard that stole it. Paired with `DEFAULT_DOMAINS_CACHE`'s mutex.
    pub(crate) static ref DISCOVERY_FINISHED: Condvar = Condvar::new();
}

/// How long `try_default_domains` will wait for an already-in-flight
/// discovery to finish before giving up and reporting failure. Generous,
/// because the thing being waited on is a `wsl.exe` that can legitimately
/// take many seconds on a cold WSL start, and the caller (a background
/// discovery thread) has nothing better to do meanwhile; bounded, because
/// a genuinely hung `wsl.exe` must not pin this thread for the life of the
/// process.
const DISCOVERY_BUSY_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Shells out to `wsl.exe -l -v` and parses its output into `WslDomain`s.
/// Always live; no caching here. Split out from `WslDomain::default_domains`/
/// `WslDomain::compute_default_domains` so both can share it while
/// differing only in whether they consult `DEFAULT_DOMAINS_CACHE`.
#[cfg(windows)]
fn discover_wsl_domains() -> anyhow::Result<Vec<WslDomain>> {
    Ok(WslDistro::load_distro_list()?
        .into_iter()
        .map(|distro| WslDomain {
            name: format!("WSL:{}", distro.name),
            distribution: Some(distro.name.clone()),
            username: None,
            default_cwd: Some("~".into()),
            default_prog: None,
        })
        .collect())
}
#[cfg(not(windows))]
fn discover_wsl_domains() -> anyhow::Result<Vec<WslDomain>> {
    Ok(vec![])
}

/// RAII guard that clears the `DISCOVERY_CLAIM_START` timestamp on drop.
/// Carries the `Instant` it claimed so `Drop` can tell whether the claim it
/// would clear is still *its own* claim (see the `Drop` impl below for why
/// that matters). Exposed as `pub(crate)` for test access.
pub(crate) struct DiscoveryGuard(Instant);

impl DiscoveryGuard {
    fn new() -> Option<Self> {
        // Try to claim the discovery flag. Returns None if another
        // discovery is already in flight.
        //
        // Claimed and released under `DEFAULT_DOMAINS_CACHE`'s lock so
        // that a caller which loses the race can inspect the flag and
        // park on `DISCOVERY_FINISHED` atomically, without the winner
        // finishing and signalling in the gap between those two steps.
        let _cache = DEFAULT_DOMAINS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut claim_start = DISCOVERY_CLAIM_START
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if claim_start.is_some() {
            None
        } else {
            let start = Instant::now();
            *claim_start = Some(start);
            Some(DiscoveryGuard(start))
        }
    }
}

impl Drop for DiscoveryGuard {
    fn drop(&mut self) {
        // Compare-and-clear, not unconditional-clear: only clear
        // `DISCOVERY_CLAIM_START` if it still holds *this* guard's own
        // claim.
        //
        // Without this check, a guard whose `wsl.exe` call has been running
        // long enough that a waiter in `try_default_domains_with` declared
        // it abandoned and stole the claim for itself would, once its own
        // (still-running) `wsl.exe` finally returns, unconditionally set
        // `DISCOVERY_CLAIM_START` back to `None` -- clobbering the new
        // claim the waiter installed, which by then is live and backing an
        // actual in-progress `wsl.exe` invocation of its own. A third
        // caller would then see no claim held and start a third concurrent
        // `wsl.exe`, violating the "at most one concurrent enumeration"
        // invariant this whole mechanism exists to guarantee. Comparing
        // against the exact `Instant` this guard stored when it claimed
        // (rather than merely checking `is_some()`) ensures this guard only
        // ever clears a claim it actually owns; a stale guard whose claim
        // was already stolen simply leaves the current claim alone, since
        // whoever holds it now is responsible for their own cleanup.
        {
            let _cache = DEFAULT_DOMAINS_CACHE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut claim_start = DISCOVERY_CLAIM_START
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *claim_start == Some(self.0) {
                *claim_start = None;
            }
        }
        // Notified unconditionally, regardless of whether this guard's
        // claim was still current above -- see `DISCOVERY_FINISHED`'s own
        // doc comment for why a spurious wakeup here is harmless while
        // suppressing it is not: a stolen claim's original owner (this
        // guard) may still be the one that ends up publishing the result
        // a parked waiter needs (eg. it "won" the race to actually finish
        // `discover()` first, even though it lost the claim), and that
        // waiter must not be left asleep until its own deadline just
        // because the guard that published the result wasn't the one
        // holding the claim it's watching.
        DISCOVERY_FINISHED.notify_all();
    }
}

impl WslDomain {
    /// Returns the currently-cached auto-discovered WSL domain list, if any
    /// discovery has completed and populated the cache yet. Never shells out to
    /// `wsl.exe` -- returns `None` on a cache miss instead of blocking. Callers
    /// on a latency-sensitive path (like resolving an already-registered
    /// domain's live settings during pane spawn) should use this instead of
    /// `default_domains()`, and treat `None` as "not discovered yet" rather than
    /// triggering discovery themselves.
    pub fn cached_default_domains() -> Option<Vec<WslDomain>> {
        let cache = DEFAULT_DOMAINS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clone()
    }

    /// Computes the current list of installed WSL distros by shelling
    /// out to `wsl.exe -l -v` every time this is called -- no caching.
    /// This is what the public `wezterm.default_wsl_domains()` Lua API
    /// calls, per its documented contract of reflecting what's currently
    /// installed. On error, logs a warning and returns an empty list.
    pub fn compute_default_domains() -> Vec<WslDomain> {
        discover_wsl_domains().unwrap_or_else(|err| {
            log::warn!("failed to enumerate WSL distros: {:#}", err);
            vec![]
        })
    }

    /// Like `default_domains()`, but preserves a discovery failure as an
    /// `Err` instead of silently converting it to an empty, successfully-cached
    /// list. Still caches only on `Ok`; still runs with the cache lock
    /// released across the actual `wsl.exe` call, same as `default_domains()`.
    /// Also guarded by `DISCOVERY_CLAIM_START` to prevent concurrent enumerations.
    ///
    /// If another discovery is already in flight, this *waits* for it (up
    /// to `DISCOVERY_BUSY_WAIT_TIMEOUT`) and returns its result, rather
    /// than reporting failure immediately. Reporting failure would be
    /// wrong in a way that loses domains outright: the caller treats a
    /// failed run as terminal and resets to "not started", while the run
    /// actually in flight is typically one that's already been superseded
    /// (a config reload away and back cancels it), so nothing would ever
    /// publish the list it eventually produces -- leaving a populated
    /// cache and no registered domains until some later reload happened
    /// to come along.
    fn try_default_domains_with(
        discover: impl FnOnce() -> anyhow::Result<Vec<WslDomain>>,
    ) -> anyhow::Result<Vec<WslDomain>> {
        let deadline = Instant::now() + DISCOVERY_BUSY_WAIT_TIMEOUT;

        let _guard = loop {
            // Hold the cache lock continuously from this check through the
            // `wait_timeout` call below: `DiscoveryGuard::new`/`Drop` both
            // take this same lock first (nested: cache outer, claim inner),
            // so as long as we never release it in between, a discovery
            // that's finishing up (populating the cache and/or clearing its
            // claim, then notifying) cannot complete "in the gap" between
            // our decision to wait and the point where we actually start
            // waiting -- it simply blocks on this same lock until we've
            // called `wait_timeout`, which is the only thing that releases
            // it while parked. Releasing this lock between the claim check
            // and the wait (as an earlier version of this function did)
            // reopens exactly that gap: a discovery could finish, clear its
            // claim under this lock, and `notify_all()` before we ever
            // start waiting on the condvar, and a `notify_all()` that runs
            // before `wait_timeout` is called is not queued -- it's simply
            // missed, and we'd block for the full remaining deadline
            // instead of noticing immediately.
            let cache = DEFAULT_DOMAINS_CACHE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(domains) = &*cache {
                return Ok(domains.clone());
            }

            // Check the claim state while still holding the cache lock.
            // Keeps the claimed-at `Instant` (not just a bool) so the
            // `wait_timeout` call below can clamp its sleep to when this
            // specific claim would go stale, rather than only to this
            // waiter's own overall deadline; see the comment there.
            let (is_claimed, is_abandoned, claimed_at) = {
                let claim_start = DISCOVERY_CLAIM_START
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match *claim_start {
                    None => (false, false, None),
                    Some(start) => (
                        true,
                        Instant::now().duration_since(start) >= DISCOVERY_BUSY_WAIT_TIMEOUT,
                        Some(start),
                    ),
                }
            };

            if !is_claimed || is_abandoned {
                // Nothing running, or the in-flight discovery is abandoned.
                if is_abandoned {
                    log::warn!(
                        "a previous WSL domain discovery has been running for over {:?} without \
                         finishing; treating it as stuck and starting a new attempt",
                        DISCOVERY_BUSY_WAIT_TIMEOUT
                    );
                    // Still holding the cache lock here, so this can't race
                    // a concurrent `DiscoveryGuard::new`/`Drop` (both take
                    // the cache lock first too).
                    let mut claim_start = DISCOVERY_CLAIM_START
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *claim_start = None;
                }
                // `DiscoveryGuard::new` takes this same cache lock, so it
                // has to be released first; losing a race to claim it here
                // just sends us around the loop again.
                drop(cache);
                if let Some(guard) = DiscoveryGuard::new() {
                    break guard;
                }
                continue;
            }

            // The in-flight discovery is still within the timeout. Wait for
            // it. `wait_timeout` atomically releases `cache` and parks, so
            // there's no window between our checks above and actually
            // waiting where a finishing discovery's notify could be missed.
            let now = Instant::now();
            if now >= deadline {
                anyhow::bail!(
                    "timed out after {:?} waiting for an already-in-flight WSL \
                     discovery to finish",
                    DISCOVERY_BUSY_WAIT_TIMEOUT
                );
            }
            // Clamp the sleep to the sooner of (a) this waiter's own
            // overall deadline, and (b) when the claim we're currently
            // watching would itself be considered abandoned. Without (b),
            // a waiter that parks here sleeps until its own deadline or
            // `DISCOVERY_FINISHED` fires -- neither of which necessarily
            // happens promptly if the claim we're watching started well
            // before we arrived: it could go stale seconds (or, in the
            // pathological case, tens of seconds) before our own deadline
            // does, and we'd have no chance to notice and steal it until
            // some other, unrelated wakeup. Waking up right as `claimed_at`
            // crosses the abandonment threshold lets the loop immediately
            // re-check `is_abandoned` and steal the claim if it's still
            // stuck, instead of sleeping longer than necessary. This is
            // purely about waking up earlier when useful -- it never
            // extends `deadline - now`, only shortens the sleep.
            let wait_for = match claimed_at {
                Some(start) => {
                    let abandoned_at = start + DISCOVERY_BUSY_WAIT_TIMEOUT;
                    (deadline - now).min(abandoned_at.saturating_duration_since(now))
                }
                None => deadline - now,
            };
            let (_cache, _timeout) = DISCOVERY_FINISHED
                .wait_timeout(cache, wait_for)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        };

        // Deliberately runs with the cache lock released: `wsl.exe` can
        // take many seconds (or hang outright), and nothing else that
        // touches `DEFAULT_DOMAINS_CACHE` should have to wait behind
        // that. Note the error is *not* propagated with `?` here: it has
        // to be held until after the cache has been re-checked below.
        let result = discover();

        // Note this drops before `_guard` does (reverse declaration
        // order), which matters because `DiscoveryGuard::drop` takes this
        // same lock.
        let mut cache = DEFAULT_DOMAINS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Whatever is already in the cache wins, and our own result is
        // discarded -- *including* when ours succeeded and theirs is
        // arguably staler, and including when ours failed outright.
        //
        // The cache can only be non-empty here if a concurrent run
        // published while `discover()` was running, which in turn can only
        // happen via the abandonment-takeover path above (a run whose
        // claim we stole, or which stole ours, finishing in the meantime).
        // Returning our own list in that case silently breaks the
        // invariant that the mux domain registry and this cache describe
        // the same set of WSL distros: `Mux` would register the domains
        // *we* returned, while `LocalDomain::resolve_wsl_domain` looks up
        // each domain's live settings in this *cache*. A domain present
        // only in our list would then resolve to `None`, and
        // `fixup_command` would take its "not a WSL domain at all" branch
        // and run the command directly on the Windows host instead of
        // inside WSL -- silently wrong, not merely slow, which is exactly
        // the failure mode `resolve_wsl_domain`'s own doc comment calls
        // out as the thing to never allow.
        //
        // Preferring the published value over a fresher local one is the
        // right trade: the cache is process-lifetime, so the published
        // value is what every later spawn resolves against no matter what
        // we do here. Adopting it makes the registry agree with it. Being
        // slightly stale merely means a distro is missing (which every
        // consumer already handles); disagreeing means running commands
        // on the wrong machine.
        //
        // Adopting it on our *error* path matters for the same reason,
        // and additionally avoids throwing away a perfectly good answer:
        // the caller treats an `Err` as a failed run and resets to "not
        // started" without registering anything, even though a usable
        // list is sitting right here.
        if let Some(published) = &*cache {
            return Ok(published.clone());
        }

        let domains = result?;
        *cache = Some(domains.clone());
        Ok(domains)
    }

    pub fn try_default_domains() -> anyhow::Result<Vec<WslDomain>> {
        Self::try_default_domains_with(discover_wsl_domains)
    }

    /// Like `compute_default_domains`, but memoized for the lifetime of
    /// the process after the first successful call. Discovery shells out
    /// to `wsl.exe -l -v`, which can take many seconds (starting the
    /// LxssManager service and/or booting the WSL2 utility VM) the first
    /// time it runs; this used to be re-run on every call, including
    /// once per pane spawn via `LocalDomain::resolve_wsl_domain`, so a
    /// config that leaves `wsl_domains` unset paid that cost repeatedly
    /// instead of just once. `resolve_wsl_domain` has since moved to the
    /// non-blocking `config.wsl_domains_cached()` and no longer calls this
    /// (or anything that shells out) on that path -- see its own doc
    /// comment. The remaining internal consumer is the background
    /// domain-discovery thread: the WSL-specific `discover` closure built
    /// in `wezterm-mux-server-impl`'s `update_mux_domains_impl` calls
    /// `try_default_domains()` (this function's fallible sibling, sharing
    /// the same memoized cache) from inside the closure passed to
    /// `Mux::begin_domain_discovery`/`begin_domain_discovery_with_default_filter`,
    /// so that cost is paid at most once per process even though the
    /// closure can in principle run again on a later config reload -- see
    /// `compute_default_domains` for the always-live public API.
    ///
    /// Distros added/removed after the first successful call won't be
    /// picked up here until the process restarts; that's the accepted
    /// trade-off for not paying the discovery cost repeatedly. A
    /// *failed* discovery is deliberately not cached, so the next call
    /// gets another chance instead of being stuck with an empty list for
    /// the rest of the process's life.
    pub fn default_domains() -> Vec<WslDomain> {
        Self::try_default_domains().unwrap_or_else(|err| {
            log::warn!(
                "failed to enumerate WSL distros, will retry next time: {:#}",
                err
            );
            vec![]
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    pub state: String,
    pub version: String,
    pub is_default: bool,
}

impl WslDistro {
    pub fn load_distro_list() -> anyhow::Result<Vec<Self>> {
        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.arg("-l");
        cmd.arg("-v");
        #[cfg(windows)]
        cmd.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW);
        let output = cmd.output()?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(
            output.status.success(),
            "wsl -l command invocation failed: {}",
            stderr
        );

        /// Ungh: https://github.com/microsoft/WSL/issues/4456
        fn utf16_to_utf8(bytes: &[u8]) -> anyhow::Result<String> {
            if bytes.len() % 2 != 0 {
                anyhow::bail!("input data has odd length, cannot be utf16");
            }

            // This is "safe" because we checked that the length seems reasonable,
            // and our new slice is within those same bounds.
            let wide: &[u16] = unsafe {
                std::slice::from_raw_parts(bytes.as_ptr() as *const u16, bytes.len() / 2)
            };

            String::from_utf16(wide).map_err(|_| anyhow!("wsl -l -v output is not valid utf16"))
        }

        let wsl_list = utf16_to_utf8(&output.stdout)?.replace("\r\n", "\n");

        Ok(parse_wsl_distro_list(&wsl_list))
    }
}

/// This function parses the `wsl -l -v` output.
/// It tries to be robust in the face of future changes
/// by looking at the tabulated output headers, determining
/// where the columns are and then collecting the information
/// into a hashmap and then grokking from there.
#[allow(dead_code)]
fn parse_wsl_distro_list(output: &str) -> Vec<WslDistro> {
    let lines = output.lines().collect::<Vec<_>>();

    // Determine where the field columns start
    let mut field_starts = vec![];
    {
        let mut last_char = ' ';
        for (idx, c) in lines[0].char_indices() {
            if last_char == ' ' && c != ' ' {
                field_starts.push(idx);
            }
            last_char = c;
        }
    }

    fn field_slice(s: &str, start: usize, end: Option<usize>) -> &str {
        if let Some(end) = end {
            &s[start..end]
        } else {
            &s[start..]
        }
    }

    fn opt_field_slice(s: &str, start: usize, end: Option<usize>) -> Option<&str> {
        if let Some(end) = end {
            s.get(start..end)
        } else {
            s.get(start..)
        }
    }

    // Now build up a name -> column position map
    let mut field_map = HashMap::new();
    {
        let mut iter = field_starts.into_iter().peekable();

        while let Some(start_idx) = iter.next() {
            let end_idx = iter.peek().copied();
            let label = field_slice(&lines[0], start_idx, end_idx).trim();
            field_map.insert(label, (start_idx, end_idx));
        }
    }

    let mut result = vec![];

    // and now process the output rows
    for line in lines.iter().skip(1) {
        if line.is_empty() {
            continue;
        }

        let is_default = line.starts_with("*");

        let mut fields = HashMap::new();
        for (label, (start_idx, end_idx)) in field_map.iter() {
            if let Some(value) = opt_field_slice(line, *start_idx, *end_idx) {
                fields.insert(*label, value.trim().to_string());
            } else {
                return result;
            }
        }

        result.push(WslDistro {
            name: fields.get("NAME").cloned().unwrap_or_default(),
            state: fields.get("STATE").cloned().unwrap_or_default(),
            version: fields.get("VERSION").cloned().unwrap_or_default(),
            is_default,
        });
    }

    result
}

#[cfg(test)]
#[test]
fn test_parse_wsl_distro_list() {
    let data = "  NAME                   STATE           VERSION
* Arch                   Running         2
  docker-desktop-data    Stopped         2
  docker-desktop         Stopped         2
  Ubuntu                 Stopped         2
  nvim                   Stopped         2";

    assert_eq!(
        parse_wsl_distro_list(data),
        vec![
            WslDistro {
                name: "Arch".to_string(),
                state: "Running".to_string(),
                version: "2".to_string(),
                is_default: true
            },
            WslDistro {
                name: "docker-desktop-data".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "docker-desktop".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "Ubuntu".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "nvim".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
        ]
    );
}

#[cfg(test)]
mod try_default_domains_tests {
    use crate::wsl::{
        DiscoveryGuard, DEFAULT_DOMAINS_CACHE, DISCOVERY_BUSY_WAIT_TIMEOUT, DISCOVERY_CLAIM_START,
        DISCOVERY_FINISHED,
    };
    use crate::WslDomain;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Every test in this module reads and writes the process-global
    /// `DEFAULT_DOMAINS_CACHE`/`DISCOVERY_CLAIM_START` statics directly, so
    /// they can't run concurrently with each other without racing (one
    /// test's "set the flag" step landing between another test's own
    /// check-and-clear steps). `cargo test`'s default parallel test runner
    /// would otherwise make this module flaky/reliably-failing depending
    /// on scheduling -- verified: every one of these tests failed
    /// intermittently under the default (parallel) test runner before this
    /// mutex was added. Each test acquires this for its entire body,
    /// serializing them against each other while leaving every other test
    /// in the crate free to run in parallel as normal.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// Sets the process-global cache, returning the previous value.
    fn set_cache(value: Option<Vec<WslDomain>>) -> Option<Vec<WslDomain>> {
        let mut cache = DEFAULT_DOMAINS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::replace(&mut *cache, value)
    }

    fn read_cache() -> Option<Vec<WslDomain>> {
        DEFAULT_DOMAINS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn clear_claim_start() {
        let mut claim_start = DISCOVERY_CLAIM_START
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *claim_start = None;
    }

    fn is_claimed() -> bool {
        let claim_start = DISCOVERY_CLAIM_START
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        claim_start.is_some()
    }

    fn domain_named(name: &str) -> WslDomain {
        WslDomain {
            name: name.to_string(),
            distribution: Some(name.to_string()),
            username: None,
            default_cwd: None,
            default_prog: None,
        }
    }

    /// `cached_default_domains()` must never block or shell out: on a cache
    /// miss it reports the miss rather than triggering discovery.
    ///
    /// Lives in this module (rather than one of its own) precisely so it
    /// can clear the shared cache under `TEST_MUTEX` and actually assert
    /// `None`, instead of merely asserting that the call was fast -- which
    /// a cache *hit* would satisfy just as well, making the assertion
    /// vacuous.
    #[test]
    fn cached_default_domains_returns_none_when_cache_empty() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);

        let start = Instant::now();
        let result = WslDomain::cached_default_domains();
        let elapsed = start.elapsed();

        assert!(
            result.is_none(),
            "a cache miss must read as None, not trigger discovery"
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "cached_default_domains() must not block (took {:?})",
            elapsed
        );
    }

    /// The other half of the contract: once populated, the cached list is
    /// what comes back, still without shelling out.
    #[test]
    fn cached_default_domains_returns_the_cached_list_once_populated() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expected = vec![domain_named("WSL:Cached")];
        set_cache(Some(expected.clone()));

        assert_eq!(WslDomain::cached_default_domains(), Some(expected));

        set_cache(None);
    }

    /// A caller that finds a discovery already in flight must wait for it
    /// and adopt its result, rather than reporting failure.
    ///
    /// Reporting failure is what the previous implementation did, and it
    /// lost domains outright: the mux treats a failed run as terminal
    /// (resetting to "not started"), while the run actually in flight is
    /// typically one that has already been superseded by a config reload,
    /// so nothing would ever publish the list it eventually produced.
    #[test]
    fn try_default_domains_waits_for_an_in_flight_discovery_and_adopts_its_result() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let expected = vec![domain_named("WSL:Discovered-by-the-other-run")];

        // Stand in for the in-flight discovery: hold a real guard (so the
        // waiter observes the flag exactly the way it would in
        // production), publish a result, then release.
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel::<()>();
        let expected_for_thread = expected.clone();
        let in_flight = thread::spawn(move || {
            let guard = DiscoveryGuard::new().expect("flag must be free at the start of this test");
            claimed_tx.send(()).expect("main thread is still waiting");
            // Long enough that the waiter is parked on the condvar rather
            // than racing past it, but not part of the assertion: the test
            // asserts on the *result*, not on timing.
            thread::sleep(Duration::from_millis(150));
            set_cache(Some(expected_for_thread));
            drop(guard);
        });

        claimed_rx
            .recv()
            .expect("the in-flight thread must claim the flag");

        // Must block until the in-flight run publishes, then return its
        // list. Note this also proves no second discovery ran: a second
        // one would have overwritten nothing (the cache is write-once) but
        // would have had to return its own, different, list.
        let result = WslDomain::try_default_domains().expect("waiting must not report failure");
        assert_eq!(result, expected);

        in_flight.join().expect("in-flight thread must not panic");
        assert!(
            !is_claimed(),
            "the flag must be clear once the in-flight run released it"
        );

        set_cache(None);
    }

    /// If the in-flight run finishes *without* publishing anything (ie. it
    /// failed), the waiter must pick the work up itself rather than
    /// returning an empty answer, or a transient failure in one run would
    /// silently become "there are no WSL domains" for the next one.
    #[test]
    fn try_default_domains_runs_discovery_itself_if_the_in_flight_one_published_nothing() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel::<()>();
        let in_flight = thread::spawn(move || {
            let guard = DiscoveryGuard::new().expect("flag must be free at the start of this test");
            claimed_tx.send(()).expect("main thread is still waiting");
            thread::sleep(Duration::from_millis(100));
            // Releases without ever populating the cache: a failed run.
            drop(guard);
        });

        claimed_rx
            .recv()
            .expect("the in-flight thread must claim the flag");

        // Waits for the failed run, finds no published result, and so
        // claims the flag and discovers for itself. Uses a stub closure
        // to avoid shelling out to the real wsl.exe on Windows CI.
        let expected = vec![domain_named("WSL:Stub")];
        let result = WslDomain::try_default_domains_with(|| Ok(expected.clone()))
            .expect("a failed in-flight run must not make this one fail too");

        in_flight.join().expect("in-flight thread must not panic");

        assert_eq!(result, expected, "must run discovery itself with the stub");
        assert_eq!(read_cache().as_ref(), Some(&result));
        assert!(!is_claimed());

        set_cache(None);
    }

    /// Regression test: when a concurrent run publishes to the cache while
    /// our own `discover()` is still running, we must adopt the published
    /// list rather than returning our own divergent one. `Mux` registers
    /// whatever this returns, but `LocalDomain::resolve_wsl_domain` looks
    /// domains up in the *cache* -- so returning a list the cache doesn't
    /// contain leaves a registered domain that resolves to `None`, and
    /// `fixup_command` then runs its command on the Windows host instead
    /// of inside WSL.
    ///
    /// Only reachable via the abandonment-takeover path (which the
    /// mux-level 90s stuck-supersede made live), so the ordering is forced
    /// explicitly here rather than waiting on a real timeout: our stub
    /// `discover` blocks until the "other" run has published, guaranteeing
    /// the cache is already populated by the time we reach the publish
    /// step.
    #[test]
    fn a_concurrently_published_cache_wins_over_our_own_result() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let published = vec![domain_named("WSL:Published-by-the-other-run")];
        let ours = vec![domain_named("WSL:Ours")];

        // Our `discover` publishes the *other* run's result partway
        // through, standing in for a stolen-from run finishing while ours
        // is still shelling out.
        let published_for_stub = published.clone();
        let ours_for_stub = ours.clone();
        let result = WslDomain::try_default_domains_with(move || {
            set_cache(Some(published_for_stub));
            Ok(ours_for_stub)
        })
        .expect("must succeed by adopting the published list");

        assert_eq!(
            result, published,
            "must return the list already published to the cache, not our own"
        );
        assert_eq!(
            read_cache().as_ref(),
            Some(&published),
            "must not overwrite the already-published cache"
        );
        assert_ne!(result, ours, "our own divergent list must not win");

        // Same requirement on the error path: a published list is a usable
        // answer, so our own failure must not be reported as one (which
        // would make the caller reset to "not started" and register
        // nothing, despite a good list being right there).
        set_cache(None);
        clear_claim_start();
        let published_for_err = published.clone();
        let result = WslDomain::try_default_domains_with(move || {
            set_cache(Some(published_for_err));
            anyhow::bail!("simulated wsl.exe failure")
        })
        .expect("a published list must be adopted even when our own run failed");
        assert_eq!(result, published);

        set_cache(None);
    }

    /// A successful result is cached; a subsequent call is served from the
    /// cache rather than shelling out again.
    #[test]
    fn try_default_domains_caches_a_successful_result() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let expected = vec![domain_named("WSL:Stub")];
        let runs = Arc::new(AtomicUsize::new(0));

        let runs_first = Arc::clone(&runs);
        let expected_first = expected.clone();
        let first = WslDomain::try_default_domains_with(move || {
            runs_first.fetch_add(1, Ordering::SeqCst);
            Ok(expected_first)
        })
        .expect("discovery must succeed");
        assert_eq!(
            read_cache().as_ref(),
            Some(&first),
            "a successful result must be cached"
        );
        assert_eq!(first, expected, "must return the stub value");
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the first call must discover"
        );

        // The second closure counts its own invocations rather than just
        // returning the same value again: without a negative control here,
        // deleting the cache fast path entirely would still leave this
        // test green, since both closures would produce identical output.
        let runs_second = Arc::clone(&runs);
        let second = WslDomain::try_default_domains_with(move || {
            runs_second.fetch_add(1, Ordering::SeqCst);
            Ok(vec![domain_named("WSL:Should-never-be-reached")])
        })
        .expect("the cached call must succeed");
        assert_eq!(first, second);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the second call must be served from the cache without invoking discovery again"
        );

        set_cache(None);
    }

    /// Verify that the `DISCOVERY_CLAIM_START` flag is correctly
    /// cleared after a successful discovery, so later calls can proceed.
    ///
    /// Also regression-tests that a *successful* discovery finding zero
    /// distributions caches `Some(vec![])`, not `None`: the two are not
    /// interchangeable to `resolve_wsl_domain`, which reads `None` as
    /// "discovery hasn't finished yet" (fall back to the domain's
    /// construction-time snapshot) and `Some(vec![])` as "discovery
    /// finished and there is nothing" (a real "no longer a WSL domain").
    /// Caching `None` here -- eg. a future "don't bother caching an empty
    /// result" change -- would leave every WSL domain permanently on the
    /// fallback path instead of correctly resolving to `None` once removed
    /// from the config.
    #[test]
    fn discovery_flag_cleared_after_success() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let result = WslDomain::try_default_domains_with(|| Ok(vec![]));
        assert!(result.is_ok());

        // Verify flag is cleared after success.
        assert!(!is_claimed());
        assert_eq!(
            read_cache(),
            Some(vec![]),
            "a successful discovery that found zero distributions must cache Some(vec![]), \
             not leave the cache empty"
        );

        set_cache(None);
    }

    /// Regression test: verify that a discovery failure is propagated
    /// correctly and does not populate the cache or leave the flag set.
    #[test]
    fn try_default_domains_with_propagates_a_real_discovery_failure() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let result =
            WslDomain::try_default_domains_with(|| anyhow::bail!("simulated wsl.exe failure"));

        assert!(result.is_err());
        assert!(
            read_cache().is_none(),
            "a failed discovery must not populate the cache"
        );
        assert!(!is_claimed(), "the flag must be cleared after a failure");

        set_cache(None);
    }

    /// Verify that the RAII guard correctly clears the flag
    /// even if the guard is dropped explicitly (simulating a panic recovery
    /// scenario where the guard's Drop is called).
    #[test]
    fn discovery_guard_clears_flag_on_drop() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        // Create a guard manually (this is what happens inside try_default_domains).
        let guard = DiscoveryGuard::new();
        assert!(guard.is_some());
        assert!(is_claimed());

        // A second claim while the first is live must fail: that's the
        // whole point of the flag.
        assert!(
            DiscoveryGuard::new().is_none(),
            "only one discovery may hold the flag at a time"
        );

        // Drop the guard explicitly.
        drop(guard);

        // Verify flag is cleared after drop.
        assert!(!is_claimed());
    }

    /// Regression test: dropping a
    /// `DiscoveryGuard` whose claim was already stolen by an
    /// abandonment-takeover (its compare-and-clear is a no-op) must still
    /// notify `DISCOVERY_FINISHED` unconditionally. An earlier version of
    /// the fix for "DiscoveryGuard has no owner" made the notify
    /// conditional on the clear actually happening, which meant a waiter
    /// parked behind the *new* owner's claim could sleep for its full
    /// timeout even after the stolen-from guard published a result to the
    /// cache on its way out.
    #[test]
    fn dropping_a_superseded_guard_still_notifies_finished() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        // Run A claims first.
        let guard_a = DiscoveryGuard::new().expect("flag must be free at the start of this test");

        // Run B steals A's claim, standing in for the abandonment-takeover
        // path in `try_default_domains_with` (which does exactly these
        // two steps once it observes A's claim as stale). B is left
        // "still running" for the rest of this test (its guard is simply
        // never dropped).
        clear_claim_start();
        let guard_b = DiscoveryGuard::new().expect("flag must be free once cleared");

        // A waiter parks on `DISCOVERY_FINISHED`, holding the cache lock
        // exactly like `try_default_domains_with`'s wait loop does.
        let timed_out = Arc::new(AtomicBool::new(false));
        let timed_out2 = Arc::clone(&timed_out);
        let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
        let waiter = thread::spawn(move || {
            let cache = DEFAULT_DOMAINS_CACHE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            parked_tx.send(()).expect("main thread is still waiting");
            let (_cache, timeout) = DISCOVERY_FINISHED
                .wait_timeout(cache, Duration::from_secs(5))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            timed_out2.store(timeout.timed_out(), Ordering::SeqCst);
        });

        // "Signalled right before calling `wait_timeout`" is not quite the
        // same as "actually parked on the condvar" -- there's a small gap
        // between the channel send returning and `wait_timeout` itself
        // starting to block, with no lock or syscall in between to hang a
        // more precise handoff on. Give it a moment; this is the same
        // technique, and the same caveat, as
        // `mux::domain_discovery_tests::cancel_domain_discovery_defers_sync_wakeup_until_notify_is_called`
        // -- it only affects how long the handshake takes, never whether
        // the assertion below is valid once it completes.
        parked_rx
            .recv()
            .expect("waiter thread must have started parking");
        thread::sleep(Duration::from_millis(50));

        // A's stale guard (its claim was already stolen by B, which is
        // still "running") drops now, simulating A's long-hung `wsl.exe`
        // finally returning. This must notify `DISCOVERY_FINISHED` even
        // though A's own compare-and-clear is a no-op -- otherwise the
        // waiter sleeps for its full timeout instead of waking promptly.
        drop(guard_a);

        waiter.join().expect("waiter thread must not panic");
        assert!(
            !timed_out.load(Ordering::SeqCst),
            "dropping a guard whose claim was already stolen must still notify \
             DISCOVERY_FINISHED, not silently do nothing"
        );

        drop(guard_b);
        set_cache(None);
    }

    /// `default_domains()` is the infallible wrapper, and it inherits the
    /// waiting behaviour: it also adopts an in-flight run's result rather
    /// than reporting the emptiness that its error path would produce.
    #[test]
    fn default_domains_adopts_an_in_flight_discoverys_result() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        let expected = vec![domain_named("WSL:From-the-other-run")];

        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel::<()>();
        let expected_for_thread = expected.clone();
        let in_flight = thread::spawn(move || {
            let guard = DiscoveryGuard::new().expect("flag must be free at the start of this test");
            claimed_tx.send(()).expect("main thread is still waiting");
            thread::sleep(Duration::from_millis(150));
            set_cache(Some(expected_for_thread));
            drop(guard);
        });

        claimed_rx
            .recv()
            .expect("the in-flight thread must claim the flag");

        assert_eq!(WslDomain::default_domains(), expected);

        in_flight.join().expect("in-flight thread must not panic");
        set_cache(None);
    }

    /// Regression test: verify that an abandoned discovery (one that's
    /// been running longer than the timeout) is treated as stuck and a new
    /// discovery is started instead of waiting indefinitely.
    #[test]
    fn abandoned_discovery_triggers_new_attempt() {
        let _guard = TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_cache(None);
        clear_claim_start();

        // Manually set the claim timestamp to a time that's older than the timeout.
        // This simulates a hung discovery without actually waiting 60 seconds.
        let mut claim_start = DISCOVERY_CLAIM_START
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *claim_start = Some(
            // checked_sub avoids a panic on underflow on a host whose
            // monotonic clock hasn't reached DISCOVERY_BUSY_WAIT_TIMEOUT +
            // 1s of uptime yet (very early boot, some CI/container clock
            // sources). Falling back to Instant::now() means the claim
            // won't actually read as abandoned in that edge case, which is
            // an acceptable trade-off for a test that cannot meaningfully
            // run on a host with under a minute of uptime anyway -- the
            // point is just that this must not panic.
            Instant::now()
                .checked_sub(DISCOVERY_BUSY_WAIT_TIMEOUT + Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
        );
        drop(claim_start);

        // Verify that the flag is set.
        assert!(is_claimed());

        // This should trigger the abandonment path and start a new discovery,
        // returning successfully without waiting for the stuck one.
        let expected = vec![domain_named("WSL:Abandoned")];
        let result = WslDomain::try_default_domains_with(|| Ok(expected.clone()))
            .expect("abandoned discovery must not block");

        assert_eq!(result, expected, "must run discovery with the stub");
        assert_eq!(read_cache().as_ref(), Some(&result));
        assert!(!is_claimed(), "the old claim must be cleared");

        set_cache(None);
    }
}
