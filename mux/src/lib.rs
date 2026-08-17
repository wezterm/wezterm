use crate::client::{ClientId, ClientInfo};
use crate::pane::{CachePolicy, Pane, PaneId};
use crate::ssh_agent::AgentProxy;
use crate::tab::{SplitRequest, Tab, TabId};
use crate::window::{Window, WindowId};
use anyhow::{anyhow, Context, Error};
use config::keyassignment::SpawnTabDomain;
use config::{configuration, ExitBehavior, GuiPosition};
use domain::{Domain, DomainId, DomainState, SplitSource};
use filedescriptor::{poll, pollfd, socketpair, AsRawSocketDescriptor, FileDescriptor, POLLIN};
#[cfg(unix)]
use libc::{c_int, SOL_SOCKET, SO_RCVBUF, SO_SNDBUF};
use log::error;
use metrics::histogram;
use parking_lot::{
    Condvar, MappedRwLockReadGuard, MappedRwLockWriteGuard, Mutex, RwLock, RwLockReadGuard,
    RwLockWriteGuard,
};
use percent_encoding::percent_decode_str;
use portable_pty::{CommandBuilder, ExitStatus, PtySize};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};
use termwiz::escape::csi::{DecPrivateMode, DecPrivateModeCode, Device, Mode};
use termwiz::escape::{Action, CSI};
use thiserror::*;
use wezterm_term::{Clipboard, ClipboardSelection, DownloadHandler, TerminalSize};
#[cfg(windows)]
use winapi::um::winsock2::{SOL_SOCKET, SO_RCVBUF, SO_SNDBUF};

pub mod activity;
pub mod client;
pub mod connui;
pub mod domain;
pub mod localpane;
pub mod pane;
pub mod renderable;
pub mod ssh;
pub mod ssh_agent;
pub mod tab;
pub mod termwiztermtab;
pub mod tmux;
pub mod tmux_commands;
mod tmux_pty;
pub mod window;

use crate::activity::Activity;

pub const DEFAULT_WORKSPACE: &str = "default";

#[derive(Clone, Debug)]
pub enum MuxNotification {
    PaneOutput(PaneId),
    PaneAdded(PaneId),
    PaneRemoved(PaneId),
    WindowCreated(WindowId),
    WindowRemoved(WindowId),
    WindowInvalidated(WindowId),
    WindowWorkspaceChanged(WindowId),
    ActiveWorkspaceChanged(Arc<ClientId>),
    Alert {
        pane_id: PaneId,
        alert: wezterm_term::Alert,
    },
    Empty,
    AssignClipboard {
        pane_id: PaneId,
        selection: ClipboardSelection,
        clipboard: Option<String>,
    },
    SaveToDownloads {
        name: Option<String>,
        data: Arc<Vec<u8>>,
    },
    TabAddedToWindow {
        tab_id: TabId,
        window_id: WindowId,
    },
    PaneFocused(PaneId),
    TabResized(TabId),
    TabTitleChanged {
        tab_id: TabId,
        title: String,
    },
    WindowTitleChanged {
        window_id: WindowId,
        title: String,
    },
    WorkspaceRenamed {
        old_workspace: String,
        new_workspace: String,
    },
}

static LAST_SUBSCRIBER_ID: AtomicUsize = AtomicUsize::new(0);

/// How long resolving a domain *by name* will wait for an in-progress
/// background domain discovery (see `Mux::begin_domain_discovery`) before
/// giving up and falling back to its usual "domain not found" behavior.
///
/// Bounded, unlike the `default_domain`/`default_mux_server_domain`/`--domain`
/// paths, because the two differ in what a wrong answer costs. Those name the
/// domain the user has declared they want to start in, so treating "discovery
/// is still running" as "that domain doesn't exist" fails startup outright;
/// they wait for as long as discovery takes. An arbitrary later
/// `SpawnTabDomain::DomainName` -- a key assignment, or
/// `wezterm.mux.spawn_window{domain=...}` -- is far more likely to name
/// something that will never appear (a typo, or a distribution since removed),
/// and blocking it for the whole of a cold `wsl.exe` before saying so is its
/// own kind of wrong.
///
/// The trade-off worth stating plainly: `wsl.exe -l -v` can take longer than
/// this on exactly the machines this discovery exists to keep off the startup
/// path, so a by-name spawn issued in the first seconds of a session -- most
/// plausibly from a `gui-startup`/`mux-startup` handler -- can be told the
/// name is invalid even though discovery would have found it shortly after. A
/// config that needs a specific WSL domain available that early should set
/// `wsl_domains` explicitly instead of relying on auto-discovery.
pub const DOMAIN_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an `InProgress` discovery run may persist before a later
/// `begin_domain_discovery`/`begin_domain_discovery_with_default_filter`
/// call treats it as permanently stuck (eg. a `wsl.exe` that hangs
/// forever, not just one that's slow) and supersedes it with a fresh run
/// rather than the usual "already in progress, just refresh the fields
/// and return" no-op.
///
/// Deliberately longer than `config::wsl`'s own 60s
/// `DISCOVERY_BUSY_WAIT_TIMEOUT` (not exported across the crate boundary,
/// so not named directly here): that timeout is what lets a *second*
/// concurrent caller of `WslDomain::try_default_domains` steal an
/// abandoned claim and make real progress, but nothing here ever makes
/// that second call while a run is `InProgress` -- `begin_domain_discovery*`
/// refuses to spawn a competing discovery thread precisely to avoid
/// running `wsl.exe` twice at once. Without this mux-level check, a
/// permanently hung first attempt would mean nothing here ever produces
/// the second caller that the config-layer timeout is waiting to un-stick,
/// and any unbounded waiter (`await_domain_discovery_unbounded[_async]`)
/// would block forever with no way out. Set comfortably above the
/// config-layer timeout so a merely-slow (not hung) first attempt gets a
/// full chance to finish on its own before this starts a competing one.
const DOMAIN_DISCOVERY_STUCK_THRESHOLD: Duration = Duration::from_secs(90);

/// Which flavor of discovery wait `Mux::await_domain_resolution_async`
/// decided is needed for a given `resolve_spawn_tab_domain` target; see
/// `Mux::domain_resolution_wait_kind`.
#[derive(Debug, PartialEq, Eq)]
enum DomainResolutionWaitKind {
    None,
    Bounded,
    Unbounded,
}

/// Tracks the state of a background domain discovery run kicked off by
/// `Mux::begin_domain_discovery` (currently used only for WSL domains, but
/// deliberately not WSL-specific: `Mux` doesn't otherwise know what kind of
/// domain it's registering). `generation` identifies a particular run and
/// is compared-and-swapped as a single unit with the rest of this enum
/// under `Mux::domain_discovery`'s lock -- never read from a separate
/// atomic outside that lock -- so that starting, cancelling and finishing
/// a run can't observe or act on a torn combination of "which run" and
/// "what state that run is in". `pending_default` names the domain (if
/// any) that should become the mux default domain once discovery
/// completes, because it was requested (via
/// `default_domain`/`default_mux_server_domain`) but didn't exist among
/// the domains known synchronously at startup.
///
/// `wakers` stores any async tasks waiting for this discovery to complete.
/// These are woken when the discovery finishes (successfully or with an
/// error) or is cancelled.
///
/// `default_domain_id_at_start` is the default domain's id as of the most
/// recent `begin_domain_discovery`/`begin_domain_discovery_with_default_filter`
/// call for this run -- initially set when the run starts, and refreshed
/// (along with `pending_default`) on every repeat call that lands while
/// this run is still `InProgress`, all under the same lock. Living here
/// (rather than being captured once into `FinishDomainDiscoveryOnDrop` and
/// never updated again) is what lets a later repeat call's "did the
/// default change since *I* asked for `pending_default`" guard use *its
/// own* baseline instead of silently inheriting a stale one from whichever
/// call happened to be first to actually spawn the discovery thread.
///
/// `next_waiter_id` allocates a fresh ID for each async waiter that
/// registers, used to key `wakers` so individual waiters can be looked up
/// and removed (or their waker replaced) in O(1).
///
/// `accepts_as_default` is the predicate from the most recent
/// `begin_domain_discovery_with_default_filter` call for this run, refreshed
/// on every repeat call (same pattern as `pending_default` and
/// `default_domain_id_at_start`).
///
/// `started_at` is set once, when this run's discovery thread is first
/// spawned, and never refreshed by repeat calls (unlike the other fields
/// above) -- it exists purely to let `begin_domain_discovery_with_default_filter`
/// measure how long *this* run has actually been running, so it can tell
/// a merely-slow run apart from one that's stuck past
/// `DOMAIN_DISCOVERY_STUCK_THRESHOLD` and needs superseding. See that
/// function and the constant's own doc comment.
enum DomainDiscoveryState {
    NotStarted,
    InProgress {
        generation: u64,
        pending_default: Option<String>,
        default_domain_id_at_start: Option<DomainId>,
        wakers: HashMap<u64, Waker>,
        next_waiter_id: u64,
        accepts_as_default: Arc<dyn Fn(&Arc<dyn Domain>) -> bool + Send + Sync>,
        started_at: Instant,
    },
    Done {
        _generation: u64,
        not_found_pending_default: Option<String>,
    },
}

/// Future that waits for domain discovery to complete. Registers itself
/// in the `DomainDiscoveryState::InProgress::wakers` map and is woken
/// when discovery finishes or is cancelled. Unlike the old thread-per-waiter
/// approach, this is zero-cost from a thread perspective and doesn't spawn
/// additional OS threads.
///
/// The future now carries its own independent timer via
/// `async_io::Timer` when a timeout is configured, so it wakes at the
/// deadline even if discovery never completes (e.g. a hung `wsl.exe`).
/// The timer is lazily started on the first poll to avoid creating it
/// for futures that resolve immediately.
///
/// The future tracks its registration as `(generation,
/// waiter_id)` rather than just `generation`, and implements `Drop` to
/// unregister its slot from the run's wakers map when dropped. This
/// allows stale wakers to be cleaned up and enables the `will_wake`
/// check for updating the stored waker on a later poll.
pub struct DomainDiscoveryWaitFuture {
    mux: Arc<Mux>,
    timeout: Option<Duration>,
    timer: Option<async_io::Timer>,
    /// The `generation` of the `InProgress` run (if any) this future has
    /// already pushed its waker into, so it knows whether it needs to
    /// (re-)register. Deliberately keyed by generation rather than a plain
    /// "have we ever registered" bool: a cancelled run's `wakers` are
    /// drained and woken by `cancel_domain_discovery`/`FinishDomainDiscoveryOnDrop`,
    /// and if a *new* discovery run starts before this future gets polled
    /// again, a plain bool would see itself as "already registered" and
    /// skip joining the new run's (separate, freshly-empty) `wakers` list
    /// -- leaving it registered nowhere and pending forever. Comparing
    /// generations means a new run is always detected and re-registered
    /// against. The `waiter_id` half additionally lets this future find and
    /// remove or replace its own slot without disturbing anyone else's.
    registered: Option<(u64, u64)>,
}

impl DomainDiscoveryWaitFuture {
    fn new(mux: Arc<Mux>, timeout: Option<Duration>) -> Self {
        Self {
            mux,
            timeout,
            timer: None,
            registered: None,
        }
    }

    /// Unregister this future's waker from the run's map if
    /// it's still registered. Called from both Drop and when the future
    /// resolves via timeout.
    fn unregister(&self) {
        if let Some((generation, waiter_id)) = self.registered {
            let mut state = self.mux.domain_discovery.lock();
            // Check generation first, then access wakers
            let is_current_gen = matches!(
                &*state,
                DomainDiscoveryState::InProgress { generation: g, .. } if *g == generation
            );
            if is_current_gen {
                if let DomainDiscoveryState::InProgress { wakers, .. } = &mut *state {
                    wakers.remove(&waiter_id);
                }
            }
        }
    }
}

impl Drop for DomainDiscoveryWaitFuture {
    fn drop(&mut self) {
        // Clean up our waker registration when dropped
        self.unregister();
    }
}

impl std::future::Future for DomainDiscoveryWaitFuture {
    type Output = bool; // true if discovery completed, false if timed out

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<Self::Output> {
        // Lazily create the timer on the first poll (now that
        // we have a `Context` to register it against), then -- whether it
        // was just created or already existed from an earlier poll --
        // always poll it here. This is not optional: constructing an
        // `async_io::Timer` without ever polling it never registers a
        // wake with the reactor at all, so a timer that's created but not
        // immediately polled once would never fire -- reintroducing
        // exactly the "nothing wakes this task at the deadline" bug this
        // timer exists to fix, just one layer deeper.
        if self.timer.is_none() {
            if let Some(timeout) = self.timeout {
                self.timer = Some(async_io::Timer::after(timeout));
            }
        }
        if let Some(timer) = &mut self.timer {
            if std::pin::Pin::new(timer).poll(cx).is_ready() {
                // Timer fired: unregister our waker and report timeout.
                self.unregister();
                return Poll::Ready(false);
            }
        }

        let mut newly_registered = None;

        let result = {
            let mut state = self.mux.domain_discovery.lock();

            match &mut *state {
                DomainDiscoveryState::NotStarted => {
                    // Nothing to wait for
                    Some(Poll::Ready(true))
                }
                DomainDiscoveryState::Done { .. } => {
                    // Discovery already completed
                    Some(Poll::Ready(true))
                }
                DomainDiscoveryState::InProgress {
                    generation,
                    wakers,
                    next_waiter_id,
                    ..
                } => {
                    // Check if we need to register or update our waker
                    let waker = cx.waker();
                    let (current_gen, current_id) = match self.registered {
                        Some((gen, id)) => (Some(gen), Some(id)),
                        None => (None, None),
                    };

                    match (current_gen, current_id) {
                        // Not yet registered: allocate a new waiter_id
                        (None, None) => {
                            let waiter_id = *next_waiter_id;
                            *next_waiter_id = waiter_id.wrapping_add(1);
                            wakers.insert(waiter_id, waker.clone());
                            newly_registered = Some((*generation, waiter_id));
                            None
                        }
                        // Already registered, check if generation matches
                        (Some(gen), Some(id)) if gen == *generation => {
                            // Same generation: check if waker needs updating
                            match wakers.get(&id) {
                                Some(old_waker) => {
                                    if !old_waker.will_wake(waker) {
                                        wakers.insert(id, waker.clone());
                                    }
                                }
                                // Our slot is gone even though the
                                // generation still matches. No current code
                                // path does that -- everything that drains
                                // `wakers` also bumps the generation, and
                                // `unregister` only runs on terminal paths
                                // -- but returning Pending here without a
                                // registered waker would mean nothing ever
                                // wakes this task again, ie. a spawn hung
                                // forever. Re-register instead of trusting
                                // an invariant that lives in other
                                // functions. `next_waiter_id` never reuses
                                // ids, so reinstating our own is safe.
                                None => {
                                    wakers.insert(id, waker.clone());
                                }
                            }
                            None
                        }
                        // Different generation: need to re-register
                        _ => {
                            let waiter_id = *next_waiter_id;
                            *next_waiter_id = waiter_id.wrapping_add(1);
                            wakers.insert(waiter_id, waker.clone());
                            newly_registered = Some((*generation, waiter_id));
                            None
                        }
                    }
                }
            }
        };

        // Log after releasing `state`'s lock above: nothing here needs the
        // lock, and this `poll` runs on the executor thread, so there is no
        // reason to hold a lock the discovery thread wants across a log
        // backend of unknown cost. (Elsewhere in this file logging *does*
        // happen under that lock -- in `FinishDomainDiscoveryOnDrop::drop`
        // and the supersede paths -- because there the message describes a
        // decision made from the state being held, and splitting it out
        // would mean copying that state back out just to format it.)
        // Gated on `newly_registered` so this
        // fires once per (future, generation) -- ie. the moment this poll
        // is genuinely about to return `Pending` for the first time on
        // this run, meaning a real wait is starting -- rather than on
        // every re-poll/wake while still waiting. Gated on
        // `self.timeout.is_none()` so only the unbounded variant
        // (`await_domain_discovery_unbounded_async`) logs here; the
        // bounded variant (`await_domain_discovery_async`) shares this
        // same `poll` but has an explicit, self-describing timeout and
        // doesn't need this diagnostic.
        if newly_registered.is_some() && self.timeout.is_none() {
            log::info!("waiting for WSL domain discovery to finish (unbounded)");
        }

        if let Some(reg) = newly_registered {
            self.registered = Some(reg);
        }

        match result {
            Some(poll) => {
                // If we're resolving without timeout, clean up
                self.unregister();
                poll
            }
            None => Poll::Pending,
        }
    }
}

/// Finalizes a background domain discovery run when dropped: applies
/// `pending_default` if the named domain was found and the default domain
/// hasn't changed since discovery started, transitions the state to
/// `Done` (or `NotStarted` on error), and wakes anything blocked in
/// `Mux::await_domain_discovery`. Runs on both normal completion and panic
/// unwind (Drop runs either way), which is the whole point -- see the
/// comment at its construction site in `Mux::begin_domain_discovery`.
///
/// `generation` identifies which run this guard belongs to; it's compared
/// against the *current* `DomainDiscoveryState`'s own `generation` field
/// under `Mux::domain_discovery`'s lock (not a separately-read atomic), so
/// a superseded run's guard can't mistake a newer run's `InProgress` state
/// for its own and finish it early -- nor can it be fooled into thinking
/// it's still current by a state transition that happened after it made
/// an earlier, unlocked check.
///
/// `result` holds the outcome of the discover closure: `Ok(domains)` on
/// success, `Err(err)` on failure. On error, the state transitions to
/// `NotStarted` (retryable) instead of `Done`, allowing a later config
/// reload to retry discovery.
///
/// Deliberately does *not* carry its own snapshot of `pending_default`'s
/// "did the default change since I was asked to apply this" baseline
/// (`default_domain_id_at_start`) or of `pending_default` itself -- both
/// live in `DomainDiscoveryState::InProgress` instead, refreshed together
/// by every repeat `begin_domain_discovery`/
/// `begin_domain_discovery_with_default_filter` call that lands while this
/// run is still in progress. Reads them out of `state` (already locked
/// below) when it fires, so a later repeat call's own baseline is what
/// gets checked, not whichever call happened to be first to actually spawn
/// the discovery thread this guard belongs to.
struct FinishDomainDiscoveryOnDrop {
    mux: Arc<Mux>,
    generation: u64,
    /// The outcome of the discover closure, set before this guard is
    /// dropped. Must be `Some` when dropped.
    result: Option<anyhow::Result<Vec<Arc<dyn Domain>>>>,
}

impl Drop for FinishDomainDiscoveryOnDrop {
    fn drop(&mut self) {
        let mut state = self.mux.domain_discovery.lock();
        let is_current = matches!(
            &*state,
            DomainDiscoveryState::InProgress { generation, .. } if *generation == self.generation
        );
        let mut wakers: Vec<Waker> = vec![];
        if is_current {
            if let DomainDiscoveryState::InProgress {
                wakers: in_flight_wakers,
                ..
            } = &mut *state
            {
                wakers = std::mem::take(in_flight_wakers)
                    .drain()
                    .map(|(_, w)| w)
                    .collect();
            }

            // Wrap the pending_default application in catch_unwind.
            // Even if accepts_as_default or set_default_domain panic, we must
            // still transition state and wake waiters.
            let pending_apply_result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Check the result: on error, transition to NotStarted (retryable)
                    // instead of Done. On success (including panic - see below),
                    // transition to Done.
                    let is_success = self.result.as_ref().map(|r| r.is_ok()).unwrap_or(true);
                    if is_success {
                        // Determine which pending_default name (if any) we failed to find,
                        // so the Done shortcut can avoid spawning redundant runs for it.
                        let not_found_pending_default = if let DomainDiscoveryState::InProgress {
                            pending_default: Some(name),
                            ..
                        } = &*state
                        {
                            // The run was asked to find a domain by name. Did we find it?
                            if self.mux.get_domain_by_name(name).is_none() {
                                Some(name.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let DomainDiscoveryState::InProgress {
                            pending_default: Some(name),
                            default_domain_id_at_start,
                            accepts_as_default,
                            ..
                        } = &*state
                        {
                            // Read accepts_as_default from state (refreshed on repeat calls)
                            // rather than from the captured field.
                            // Do the compare and set under a single write lock.
                            {
                                let mut default = self.mux.default_domain.write();
                                let current_default_domain_id =
                                    default.as_ref().map(|d| d.domain_id());
                                let default_unchanged =
                                    current_default_domain_id == *default_domain_id_at_start;
                                if default_unchanged {
                                    if let Some(dom) = self.mux.get_domain_by_name(name) {
                                        if accepts_as_default(&dom) {
                                            *default = Some(Arc::clone(&dom));
                                        } else {
                                            log::error!(
                                                "pending default domain \"{name}\" is not an \
                                                 acceptable default here (eg. it resolved to a \
                                                 client domain where that isn't allowed); not \
                                                 applying it"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        DomainDiscoveryState::Done {
                            _generation: self.generation,
                            not_found_pending_default,
                        }
                    } else {
                        // Error or explicit failure: transition to NotStarted
                        // so a later config reload can retry.
                        //
                        // Log it on the way past. Distinguishing a real
                        // failure from "there genuinely are no domains" is
                        // the entire reason the error is threaded up here
                        // rather than swallowed at the source, and dropping
                        // it silently would leave a user whose `wsl.exe -l
                        // -v` is broken with no diagnostic at all -- just
                        // domains that never appear.
                        if let Some(Err(err)) = self.result.as_ref() {
                            log::warn!(
                                "background domain discovery failed; no domains were \
                                 registered from it, and it will be retried on the next \
                                 config reload: {err:#}"
                            );
                        }
                        DomainDiscoveryState::NotStarted
                    }
                }));

            let next_state = match pending_apply_result {
                Ok(s) => s,
                Err(_) => {
                    log::error!(
                        "panic while applying pending default or checking default unchanged; \
                         treating discovery as finished"
                    );
                    // Treat panic as success (transition to Done) so we don't retry forever.
                    // The pending default was not applied in this case, but that's acceptable.
                    // Still determine not_found_pending_default for the Done shortcut.
                    let not_found_pending_default = if let DomainDiscoveryState::InProgress {
                        pending_default: Some(name),
                        ..
                    } = &*state
                    {
                        if self.mux.get_domain_by_name(name).is_none() {
                            Some(name.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    DomainDiscoveryState::Done {
                        _generation: self.generation,
                        not_found_pending_default,
                    }
                }
            };

            *state = next_state;
        }
        drop(state);
        self.mux.domain_discovery_cond.notify_all();

        // Wake all async waiters *after* releasing the lock, matching
        // `cancel_domain_discovery_and_replace`. Waking under the lock
        // happens to be safe with the executor actually in use
        // (`async_task` never polls or drops the woken future inline on
        // this thread, so `DomainDiscoveryWaitFuture::drop` can't reenter
        // `domain_discovery`), but that's an implicit dependency on an
        // executor implementation detail, and it costs nothing to not
        // depend on it.
        for waker in wakers {
            waker.wake();
        }

        if std::thread::panicking() {
            log::error!("domain discovery thread panicked; treating discovery as finished");
        }
    }
}

pub struct Mux {
    tabs: RwLock<HashMap<TabId, Arc<Tab>>>,
    panes: RwLock<HashMap<PaneId, Arc<dyn Pane>>>,
    windows: RwLock<HashMap<WindowId, Window>>,
    default_domain: RwLock<Option<Arc<dyn Domain>>>,
    domains: RwLock<HashMap<DomainId, Arc<dyn Domain>>>,
    domains_by_name: RwLock<HashMap<String, Arc<dyn Domain>>>,
    subscribers: RwLock<HashMap<usize, Box<dyn Fn(MuxNotification) -> bool + Send + Sync>>>,
    banner: RwLock<Option<String>>,
    clients: RwLock<HashMap<ClientId, ClientInfo>>,
    identity: RwLock<Option<Arc<ClientId>>>,
    num_panes_by_workspace: RwLock<HashMap<String, usize>>,
    main_thread_id: std::thread::ThreadId,
    agent: Option<AgentProxy>,
    domain_discovery: Mutex<DomainDiscoveryState>,
    domain_discovery_cond: Condvar,
    /// Allocates a fresh id for each new discovery run started from
    /// `NotStarted`/`Done`; read only while holding `domain_discovery`'s
    /// lock (via `fetch_add`, which is itself atomic), never compared
    /// against a run's captured generation outside that lock.
    next_domain_discovery_generation: AtomicU64,
    /// Bumped by every `cancel_domain_discovery`/`cancel_domain_discovery_and_replace`
    /// call, always while holding `domain_discovery`'s lock, and likewise only
    /// read under it. Exists so `adopt_orphaned_discovery` can tell a
    /// `NotStarted` that means "the run that replaced me failed, retry welcome"
    /// from one that means "the config switched to an explicit `wsl_domains`
    /// list and no longer wants discovered domains" -- the state itself is the
    /// same in both cases, but only the former may adopt a late result. Bumped
    /// even when the cancel turns out to be a no-op, so it errs towards *not*
    /// adopting.
    domain_discovery_cancel_epoch: AtomicU64,
    /// The default-domain request carried by the most recent
    /// `begin_domain_discovery_with_default_filter` call -- what
    /// `adopt_orphaned_discovery` consults instead of values captured when
    /// the adopting run was spawned, so that a config reload which changed
    /// or dropped `default_domain` while that run was stuck is honoured
    /// rather than resurrecting the old ask. Overwritten at the top of
    /// every `begin_*` call; a leaf lock, only ever taken while
    /// `domain_discovery`'s lock is held, and never held across any other
    /// lock acquisition.
    domain_discovery_latest_default: Mutex<Option<DomainDiscoveryDefaultRequest>>,
}

/// See `Mux::domain_discovery_latest_default`.
#[derive(Clone)]
struct DomainDiscoveryDefaultRequest {
    pending_default: Option<String>,
    default_domain_id_at_start: Option<DomainId>,
    accepts_as_default: Arc<dyn Fn(&Arc<dyn Domain>) -> bool + Send + Sync>,
}

const BUFSIZE: usize = 1024 * 1024;

/// This function applies parsed actions to the pane and notifies any
/// mux subscribers about the output event
fn send_actions_to_mux(pane: &Weak<dyn Pane>, dead: &Arc<AtomicBool>, actions: Vec<Action>) {
    let start = Instant::now();
    match pane.upgrade() {
        Some(pane) => {
            pane.perform_actions(actions);
            histogram!("send_actions_to_mux.perform_actions.latency").record(start.elapsed());
            Mux::notify_from_any_thread(MuxNotification::PaneOutput(pane.pane_id()));
        }
        None => {
            // Something else removed the pane from
            // the mux, so signal that we should stop
            // trying to process it in read_from_pane_pty.
            dead.store(true, Ordering::Relaxed);
        }
    }
    histogram!("send_actions_to_mux.rate").record(1.);
}

/// This is the parsing loop for the given pane.
/// It reads all data sent to `rx` (from pane PTY) and handles all terminal events for this pane.
fn parse_buffered_data(pane: Weak<dyn Pane>, dead: &Arc<AtomicBool>, mut rx: FileDescriptor) {
    let mut buf = vec![0; configuration().mux_output_parser_buffer_size];
    let mut parser = termwiz::escape::parser::Parser::new();
    let mut actions = vec![];
    let mut hold = false;
    let mut action_size = 0;
    let mut delay = Duration::from_millis(configuration().mux_output_parser_coalesce_delay_ms);
    let mut deadline = None;

    loop {
        match rx.read(&mut buf) {
            Ok(size) if size == 0 => {
                dead.store(true, Ordering::Relaxed);
                break;
            }
            Err(_) => {
                dead.store(true, Ordering::Relaxed);
                break;
            }
            Ok(size) => {
                parser.parse(&buf[0..size], |action| {
                    let mut flush = false;
                    match &action {
                        Action::CSI(CSI::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
                            DecPrivateModeCode::SynchronizedOutput,
                        )))) => {
                            // Synchronized output frame started:
                            // => We hold off ~all actions that applies changes to the terminal.
                            hold = true;

                            // => We also flush prior actions
                            flush = true;
                        }
                        Action::CSI(CSI::Mode(Mode::ResetDecPrivateMode(
                            DecPrivateMode::Code(DecPrivateModeCode::SynchronizedOutput),
                        ))) => {
                            // Synchronized output frame ended:
                            // => We flush out all pending actions to the terminal.
                            hold = false;
                            flush = true;
                        }
                        Action::CSI(CSI::Device(dev)) if matches!(**dev, Device::SoftReset) => {
                            // Soft reset requested
                            hold = false;
                            flush = true;
                        }
                        _ => {}
                    };
                    action.append_to(&mut actions);

                    if flush && !actions.is_empty() {
                        send_actions_to_mux(&pane, &dead, std::mem::take(&mut actions));
                        action_size = 0;
                    }
                });
                action_size += size;
                if !actions.is_empty() && !hold {
                    // If we haven't accumulated too much data,
                    // pause for a short while to increase the chances
                    // that we coalesce a full "frame" from an unoptimized
                    // TUI program
                    if action_size < buf.len() {
                        let poll_delay = match deadline {
                            None => {
                                deadline.replace(Instant::now() + delay);
                                Some(delay)
                            }
                            Some(target) => target.checked_duration_since(Instant::now()),
                        };
                        if poll_delay.is_some() {
                            let mut pfd = [pollfd {
                                fd: rx.as_socket_descriptor(),
                                events: POLLIN,
                                revents: 0,
                            }];
                            if let Ok(1) = poll(&mut pfd, poll_delay) {
                                // We can read now without blocking, so accumulate
                                // more data into actions
                                continue;
                            }

                            // Not readable in time: let the data we have flow into
                            // the terminal model
                        }
                    }

                    send_actions_to_mux(&pane, &dead, std::mem::take(&mut actions));
                    deadline = None;
                    action_size = 0;
                }

                let config = configuration();
                buf.resize(config.mux_output_parser_buffer_size, 0);
                delay = Duration::from_millis(config.mux_output_parser_coalesce_delay_ms);
            }
        }
    }

    // Don't forget to send anything that we might have buffered
    // to be displayed before we return from here; this is important
    // for very short lived commands so that we don't forget to
    // display what they displayed.
    if !actions.is_empty() {
        send_actions_to_mux(&pane, &dead, std::mem::take(&mut actions));
    }
}

fn set_socket_buffer(fd: &mut FileDescriptor, option: i32, size: usize) -> anyhow::Result<()> {
    let size = size as c_int;
    let socklen = std::mem::size_of_val(&size);
    unsafe {
        let res = libc::setsockopt(
            fd.as_socket_descriptor(),
            SOL_SOCKET,
            option,
            &size as *const c_int as *const _,
            socklen as _,
        );
        if res == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error()).context("setsockopt")
        }
    }
}

fn allocate_socketpair() -> anyhow::Result<(FileDescriptor, FileDescriptor)> {
    let (mut tx, mut rx) = socketpair().context("socketpair")?;
    set_socket_buffer(&mut tx, SO_SNDBUF, BUFSIZE)
        .context("SO_SNDBUF")
        .ok();
    set_socket_buffer(&mut rx, SO_RCVBUF, BUFSIZE)
        .context("SO_RCVBUF")
        .ok();
    Ok((tx, rx))
}

/// This function is run in a separate thread; its purpose is to perform
/// blocking reads from the pty (non-blocking reads are not portable to
/// all platforms and pty/tty types), parse the escape sequences and
/// relay the actions to the mux thread to apply them to the pane.
fn read_from_pane_pty(
    pane: Weak<dyn Pane>,
    banner: Option<String>,
    mut reader: Box<dyn std::io::Read>,
) {
    let mut buf = vec![0; BUFSIZE];

    // This is used to signal that an error occurred either in this thread,
    // or in the main mux thread.  If `true`, this thread will terminate.
    let dead = Arc::new(AtomicBool::new(false));

    let (pane_id, exit_behavior) = match pane.upgrade() {
        Some(pane) => (pane.pane_id(), pane.exit_behavior()),
        None => return,
    };

    let (mut tx, rx) = match allocate_socketpair() {
        Ok(pair) => pair,
        Err(err) => {
            log::error!("read_from_pane_pty: Unable to allocate a socketpair: {err:#}");
            localpane::emit_output_for_pane(
                pane_id,
                &format!(
                    "⚠️  wezterm: read_from_pane_pty: \
                    Unable to allocate a socketpair: {err:#}"
                ),
            );
            return;
        }
    };

    // Spawn parser thread for this pane
    std::thread::spawn({
        let dead = Arc::clone(&dead);
        move || parse_buffered_data(pane, &dead, rx)
    });

    if let Some(banner) = banner {
        tx.write_all(banner.as_bytes()).ok();
    }

    // Loop until the pane or the main mux thread is dead.
    // Read data from the pane pty and send it to the parser thread via tx/rx.
    while !dead.load(Ordering::Relaxed) {
        match reader.read(&mut buf) {
            Ok(size) if size == 0 => {
                log::trace!("read_pty EOF: pane_id {}", pane_id);
                break;
            }
            Err(err) => {
                error!("read_pty failed: pane {} {:?}", pane_id, err);
                break;
            }
            Ok(size) => {
                histogram!("read_from_pane_pty.bytes.rate").record(size as f64);
                log::trace!("read_pty pane {pane_id} read {size} bytes");
                // Send received data to this pane parser thread.
                if let Err(err) = tx.write_all(&buf[..size]) {
                    error!(
                        "read_pty failed to write to parser for pane {}: {:?}",
                        pane_id, err
                    );
                    break;
                }
            }
        }
    }

    match exit_behavior.unwrap_or_else(|| configuration().exit_behavior) {
        ExitBehavior::Hold | ExitBehavior::CloseOnCleanExit => {
            // We don't know if we can unilaterally close
            // this pane right now, so don't!
            promise::spawn::spawn_into_main_thread(async move {
                let mux = Mux::get();
                log::trace!("checking for dead windows after EOF on pane {}", pane_id);
                mux.prune_dead_windows();
            })
            .detach();
        }
        ExitBehavior::Close => {
            promise::spawn::spawn_into_main_thread(async move {
                let mux = Mux::get();
                mux.remove_pane(pane_id);
            })
            .detach();
        }
    }

    dead.store(true, Ordering::Relaxed);
}

lazy_static::lazy_static! {
    static ref MUX: Mutex<Option<Arc<Mux>>> = Mutex::new(None);
}

pub struct MuxWindowBuilder {
    window_id: WindowId,
    activity: Option<Activity>,
    notified: bool,
}

impl MuxWindowBuilder {
    fn notify(&mut self) {
        if self.notified {
            return;
        }
        self.notified = true;
        let activity = self.activity.take().unwrap();
        let window_id = self.window_id;
        let mux = Mux::get();
        if mux.is_main_thread() {
            // If we're already on the mux thread, just send the notification
            // immediately.
            // This is super important for Wayland; if we push it to the
            // spawn queue below then the extra milliseconds of delay
            // causes it to get confused and shutdown the connection!?
            mux.notify(MuxNotification::WindowCreated(window_id));
        } else {
            promise::spawn::spawn_into_main_thread(async move {
                if let Some(mux) = Mux::try_get() {
                    mux.notify(MuxNotification::WindowCreated(window_id));
                    drop(activity);
                }
            })
            .detach();
        }
    }
}

impl Drop for MuxWindowBuilder {
    fn drop(&mut self) {
        self.notify();
    }
}

impl std::ops::Deref for MuxWindowBuilder {
    type Target = WindowId;

    fn deref(&self) -> &WindowId {
        &self.window_id
    }
}

/// Outcome of `add_discovered_domain` - 3-way result to
/// distinguish "superseded" from "already present" vs "inserted".
enum AddDiscoveredDomainOutcome {
    Inserted,
    AlreadyPresent,
    Superseded,
}

impl Mux {
    pub fn new(default_domain: Option<Arc<dyn Domain>>) -> Self {
        let mut domains = HashMap::new();
        let mut domains_by_name = HashMap::new();
        if let Some(default_domain) = default_domain.as_ref() {
            domains.insert(default_domain.domain_id(), Arc::clone(default_domain));

            domains_by_name.insert(
                default_domain.domain_name().to_string(),
                Arc::clone(default_domain),
            );
        }

        let agent = if config::configuration().mux_enable_ssh_agent {
            Some(AgentProxy::new())
        } else {
            None
        };

        Self {
            tabs: RwLock::new(HashMap::new()),
            panes: RwLock::new(HashMap::new()),
            windows: RwLock::new(HashMap::new()),
            default_domain: RwLock::new(default_domain),
            domains_by_name: RwLock::new(domains_by_name),
            domains: RwLock::new(domains),
            subscribers: RwLock::new(HashMap::new()),
            banner: RwLock::new(None),
            clients: RwLock::new(HashMap::new()),
            identity: RwLock::new(None),
            num_panes_by_workspace: RwLock::new(HashMap::new()),
            main_thread_id: std::thread::current().id(),
            agent,
            domain_discovery: Mutex::new(DomainDiscoveryState::NotStarted),
            domain_discovery_cond: Condvar::new(),
            next_domain_discovery_generation: AtomicU64::new(0),
            domain_discovery_cancel_epoch: AtomicU64::new(0),
            domain_discovery_latest_default: Mutex::new(None),
        }
    }

    fn get_default_workspace(&self) -> String {
        let config = configuration();
        config
            .default_workspace
            .as_deref()
            .unwrap_or(DEFAULT_WORKSPACE)
            .to_string()
    }

    pub fn is_main_thread(&self) -> bool {
        std::thread::current().id() == self.main_thread_id
    }

    fn recompute_pane_count(&self) {
        let mut count = HashMap::new();
        for window in self.windows.read().values() {
            let workspace = window.get_workspace();
            for tab in window.iter() {
                *count.entry(workspace.to_string()).or_insert(0) += match tab.count_panes() {
                    Some(n) => n,
                    None => {
                        // Busy: abort this and we'll retry later
                        return;
                    }
                };
            }
        }
        *self.num_panes_by_workspace.write() = count;
    }

    pub fn client_had_input(&self, client_id: &ClientId) {
        if let Some(info) = self.clients.write().get_mut(client_id) {
            info.update_last_input();
        }
        if let Some(agent) = &self.agent {
            agent.update_target();
        }
    }

    pub fn record_input_for_current_identity(&self) {
        if let Some(ident) = self.identity.read().as_ref() {
            self.client_had_input(ident);
        }
    }

    pub fn record_focus_for_current_identity(&self, pane_id: PaneId) {
        if let Some(ident) = self.identity.read().as_ref() {
            self.record_focus_for_client(ident, pane_id);
        }
    }

    pub fn resolve_focused_pane(
        &self,
        client_id: &ClientId,
    ) -> Option<(DomainId, WindowId, TabId, PaneId)> {
        let pane_id = self.clients.read().get(client_id)?.focused_pane_id?;
        let (domain, window, tab) = self.resolve_pane_id(pane_id)?;
        Some((domain, window, tab, pane_id))
    }

    pub fn record_focus_for_client(&self, client_id: &ClientId, pane_id: PaneId) {
        let mut prior = None;
        if let Some(info) = self.clients.write().get_mut(client_id) {
            prior = info.focused_pane_id;
            info.update_focused_pane(pane_id);
        }

        if prior == Some(pane_id) {
            return;
        }
        // Synthesize focus events
        if let Some(prior_id) = prior {
            if let Some(pane) = self.get_pane(prior_id) {
                pane.focus_changed(false);
            }
        }
        if let Some(pane) = self.get_pane(pane_id) {
            pane.focus_changed(true);
        }
    }

    /// Called by PaneFocused event handlers to reconcile a remote
    /// pane focus event and apply its effects locally
    pub fn focus_pane_and_containing_tab(&self, pane_id: PaneId) -> anyhow::Result<()> {
        let pane = self
            .get_pane(pane_id)
            .ok_or_else(|| anyhow::anyhow!("pane {pane_id} not found"))?;

        let (_domain, window_id, tab_id) = self
            .resolve_pane_id(pane_id)
            .ok_or_else(|| anyhow::anyhow!("can't find {pane_id} in the mux"))?;

        // Focus/activate the containing tab within its window
        {
            let mut win = self
                .get_window_mut(window_id)
                .ok_or_else(|| anyhow::anyhow!("window_id {window_id} not found"))?;
            let tab_idx = win
                .idx_by_id(tab_id)
                .ok_or_else(|| anyhow::anyhow!("tab {tab_id} not in {window_id}"))?;
            win.save_and_then_set_active(tab_idx);
        }

        // Focus/activate the pane locally
        let tab = self
            .get_tab(tab_id)
            .ok_or_else(|| anyhow::anyhow!("tab {tab_id} not found"))?;

        tab.set_active_pane(&pane);

        Ok(())
    }

    pub fn register_client(&self, client_id: Arc<ClientId>) {
        self.clients
            .write()
            .insert((*client_id).clone(), ClientInfo::new(client_id));
    }

    pub fn iter_clients(&self) -> Vec<ClientInfo> {
        self.clients
            .read()
            .values()
            .map(|info| info.clone())
            .collect()
    }

    /// Returns a list of the unique workspace names known to the mux.
    /// This is taken from all known windows.
    pub fn iter_workspaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .windows
            .read()
            .values()
            .map(|w| w.get_workspace().to_string())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Generate a new unique workspace name
    pub fn generate_workspace_name(&self) -> String {
        let used = self.iter_workspaces();
        for candidate in names::Generator::default() {
            if !used.contains(&candidate) {
                return candidate;
            }
        }
        unreachable!();
    }

    /// Returns the effective active workspace name
    pub fn active_workspace(&self) -> String {
        self.identity
            .read()
            .as_ref()
            .and_then(|ident| {
                self.clients
                    .read()
                    .get(&ident)
                    .and_then(|info| info.active_workspace.clone())
            })
            .unwrap_or_else(|| self.get_default_workspace())
    }

    /// Returns the effective active workspace name for a given client
    pub fn active_workspace_for_client(&self, ident: &Arc<ClientId>) -> String {
        self.clients
            .read()
            .get(&ident)
            .and_then(|info| info.active_workspace.clone())
            .unwrap_or_else(|| self.get_default_workspace())
    }

    pub fn set_active_workspace_for_client(&self, ident: &Arc<ClientId>, workspace: &str) {
        let mut clients = self.clients.write();
        if let Some(info) = clients.get_mut(&ident) {
            info.active_workspace.replace(workspace.to_string());
            self.notify(MuxNotification::ActiveWorkspaceChanged(ident.clone()));
        }
    }

    /// Assigns the active workspace name for the current identity
    pub fn set_active_workspace(&self, workspace: &str) {
        if let Some(ident) = self.identity.read().clone() {
            self.set_active_workspace_for_client(&ident, workspace);
        }
    }

    pub fn rename_workspace(&self, old_workspace: &str, new_workspace: &str) {
        if old_workspace == new_workspace {
            return;
        }
        self.notify(MuxNotification::WorkspaceRenamed {
            old_workspace: old_workspace.to_string(),
            new_workspace: new_workspace.to_string(),
        });

        for window in self.windows.write().values_mut() {
            if window.get_workspace() == old_workspace {
                window.set_workspace(new_workspace);
            }
        }
        self.recompute_pane_count();
        for client in self.clients.write().values_mut() {
            if client.active_workspace.as_deref() == Some(old_workspace) {
                client.active_workspace.replace(new_workspace.to_string());
                self.notify(MuxNotification::ActiveWorkspaceChanged(
                    client.client_id.clone(),
                ));
            }
        }
    }

    /// Overrides the current client identity.
    /// Returns `IdentityHolder` which will restore the prior identity
    /// when it is dropped.
    /// This can be used to change the identity for the duration of a block.
    pub fn with_identity(&self, id: Option<Arc<ClientId>>) -> IdentityHolder {
        let prior = self.replace_identity(id);
        IdentityHolder { prior }
    }

    /// Replace the identity, returning the prior identity
    pub fn replace_identity(&self, id: Option<Arc<ClientId>>) -> Option<Arc<ClientId>> {
        std::mem::replace(&mut *self.identity.write(), id)
    }

    /// Returns the active identity
    pub fn active_identity(&self) -> Option<Arc<ClientId>> {
        self.identity.read().clone()
    }

    pub fn unregister_client(&self, client_id: &ClientId) {
        self.clients.write().remove(client_id);
    }

    pub fn subscribe<F>(&self, subscriber: F)
    where
        F: Fn(MuxNotification) -> bool + 'static + Send + Sync,
    {
        let sub_id = LAST_SUBSCRIBER_ID.fetch_add(1, Ordering::Relaxed);
        self.subscribers
            .write()
            .insert(sub_id, Box::new(subscriber));
    }

    pub fn notify(&self, notification: MuxNotification) {
        let mut subscribers = self.subscribers.write();
        subscribers.retain(|_, notify| notify(notification.clone()));
    }

    pub fn notify_from_any_thread(notification: MuxNotification) {
        if let Some(mux) = Mux::try_get() {
            if mux.is_main_thread() {
                mux.notify(notification);
                return;
            }
        }
        promise::spawn::spawn_into_main_thread(async {
            if let Some(mux) = Mux::try_get() {
                mux.notify(notification);
            }
        })
        .detach();
    }

    pub fn default_domain(&self) -> Arc<dyn Domain> {
        self.default_domain.read().as_ref().map(Arc::clone).unwrap()
    }

    pub fn set_default_domain(&self, domain: &Arc<dyn Domain>) {
        *self.default_domain.write() = Some(Arc::clone(domain));
    }

    pub fn get_domain(&self, id: DomainId) -> Option<Arc<dyn Domain>> {
        self.domains.read().get(&id).cloned()
    }

    pub fn get_domain_by_name(&self, name: &str) -> Option<Arc<dyn Domain>> {
        self.domains_by_name.read().get(name).cloned()
    }

    /// Registers `domain` unconditionally. The mux model does not guarantee
    /// globally-unique domain *names* — only `DomainId`s are unique. Multiple
    /// domain objects with the same name but different `DomainId`s can be
    /// registered (e.g., multiple `TermWizTerminalDomain` instances for different
    /// debug overlay tabs, or concurrent tmux control-mode sessions).
    ///
    /// For domain discovery scenarios where deduplication by name is required
    /// (to prevent a background discovery thread and a synchronous config reload
    /// from registering the same logical domain twice), use `add_discovered_domain`,
    /// which checks the name before registering and skips if it's already taken.
    pub fn add_domain(&self, domain: &Arc<dyn Domain>) {
        self.domains_by_name
            .write()
            .insert(domain.domain_name().to_string(), Arc::clone(domain));

        self.domains
            .write()
            .insert(domain.domain_id(), Arc::clone(domain));

        if self.default_domain.read().is_none() {
            *self.default_domain.write() = Some(Arc::clone(domain));
        }
    }

    /// Like `add_domain`, but only registers `domain` if:
    /// 1. `generation` still identifies the currently active discovery run -- checked and
    ///    applied while holding `domain_discovery`'s lock for the whole
    ///    operation, so a `cancel_domain_discovery` or a newer
    ///    `begin_domain_discovery` call landing between the generation check
    ///    and the registration can't be missed the way a separate,
    ///    unlocked check would allow.
    /// 2. No domain with the same name is already registered. This deduplication is
    ///    specific to the discovery path to prevent a background discovery thread and
    ///    a synchronous config reload from both registering the same logical WSL domain.
    /// Used by the discovery thread spawned in `begin_domain_discovery` to register
    /// each domain it finds.
    ///
    /// The outcome distinguishes "superseded" from "already present" and
    /// "inserted" so that a single name collision doesn't abort registration of
    /// every other discovered domain.
    fn add_discovered_domain(
        &self,
        generation: u64,
        domain: &Arc<dyn Domain>,
    ) -> AddDiscoveredDomainOutcome {
        let discovery_state = self.domain_discovery.lock();
        let is_current = matches!(
            &*discovery_state,
            DomainDiscoveryState::InProgress { generation: g, .. } if *g == generation
        );
        if !is_current {
            return AddDiscoveredDomainOutcome::Superseded;
        }
        // Still holding domain_discovery's lock: a concurrent
        // cancel_domain_discovery/begin_domain_discovery call blocks on
        // the same lock and so cannot supersede this run until the
        // registration below has completed.
        self.register_discovered_domain_locked(domain)
    }

    /// The registration half of `add_discovered_domain`, split out so
    /// `adopt_orphaned_discovery` can reuse it while already holding
    /// `domain_discovery`'s lock (re-taking it there would deadlock).
    ///
    /// The caller must hold that lock and must already have decided this
    /// registration is allowed; this performs no generation check of its own.
    ///
    /// The by-name check and insert happen under one `domains_by_name`
    /// write lock (not two separate lock acquisitions) so that a
    /// same-named `add_domain` call landing from outside discovery (eg.
    /// explicit `wsl_domains` config registration, which doesn't hold
    /// `domain_discovery`'s lock) can't sneak in between the check and
    /// the insert and have its entry silently overwritten by this
    /// discovery run.
    fn register_discovered_domain_locked(
        &self,
        domain: &Arc<dyn Domain>,
    ) -> AddDiscoveredDomainOutcome {
        {
            let mut by_name = self.domains_by_name.write();
            if by_name.contains_key(domain.domain_name()) {
                return AddDiscoveredDomainOutcome::AlreadyPresent;
            }
            by_name.insert(domain.domain_name().to_string(), Arc::clone(domain));
        }

        self.domains
            .write()
            .insert(domain.domain_id(), Arc::clone(domain));

        if self.default_domain.read().is_none() {
            *self.default_domain.write() = Some(Arc::clone(domain));
        }

        AddDiscoveredDomainOutcome::Inserted
    }

    /// Salvages the result of a discovery run that succeeded but found itself
    /// superseded, in the one case where nothing else will ever publish it.
    ///
    /// The situation this exists for: a slow run A is superseded by
    /// `DOMAIN_DISCOVERY_STUCK_THRESHOLD`, the replacement run B fails, and
    /// only afterwards does A finally succeed. A's `discover` closure has by
    /// then published its list to `config`'s process-lifetime cache (that
    /// happens inside the closure, before this code runs and before A learns
    /// it was superseded), but A's generation is stale, so it registers
    /// nothing. The result is a populated cache and an empty registry --
    /// inconsistent, and since spawn resolves a domain's live settings against
    /// that cache, exactly the kind of divergence the rest of this machinery
    /// works to avoid.
    ///
    /// Adoption is deliberately narrow. It happens only when:
    ///
    /// * no cancellation has landed since this run started -- otherwise the
    ///   config has switched to an explicit `wsl_domains` list that no longer
    ///   wants discovered domains, and registering them would resurrect
    ///   entries the user just removed. `NotStarted` alone cannot tell the two
    ///   apart (a cancel and a failure both produce it), which is why
    ///   `domain_discovery_cancel_epoch` is consulted rather than the state
    ///   alone; and
    /// * the state is `NotStarted`, ie. no newer run is in flight (which would
    ///   publish its own, fresher answer) and none has completed (`Done`,
    ///   which already registered one).
    ///
    /// Requiring `NotStarted` rather than retrying leaves one gap: a run
    /// arriving in the window between a replacement deciding to fail (having
    /// just looked in the config-level cache and found it empty) and that
    /// replacement's guard actually recording the failure will see
    /// `InProgress`, decline, and then find the state turn `NotStarted` a
    /// moment too late -- so the divergence this exists to prevent survives
    /// until the next reload rather than being repaired now. Closing it means
    /// re-checking after the replacement settles, which means either blocking
    /// this thread on the state or a retry loop with no natural bound, to buy
    /// a window measured in the microseconds between two adjacent statements
    /// in the run that is failing.
    ///
    /// A configured `default_domain` naming a domain only this run can supply
    /// still has to be honoured here, or it silently stays unapplied until
    /// some later reload happens along -- `update_mux_domains_impl` already
    /// tried, before the domain existed, and found nothing. What gets applied
    /// is `domain_discovery_latest_default` -- the request carried by the most
    /// recent `begin_*` call -- NOT values captured when this run was spawned.
    /// The distinction matters precisely in the situation this function exists
    /// for: this run has been stuck long enough to be superseded, which means
    /// there has been at least one reload since it started, and that reload
    /// may have changed or dropped `default_domain`. A spawn-time copy would
    /// resurrect the old ask (a `default_domain` the config no longer
    /// contains, applied anyway, and a later reload can't undo it -- a config
    /// with no `default_domain` has nothing to apply); the slot tracks what
    /// was asked for last. It is applied with the same compare-and-set guard
    /// as the other two sites, so a default set deliberately in the meantime
    /// is not clobbered.
    fn adopt_orphaned_discovery(&self, cancel_epoch_at_start: u64, domains: &[Arc<dyn Domain>]) {
        let mut state = self.domain_discovery.lock();

        if self.domain_discovery_cancel_epoch.load(Ordering::SeqCst) != cancel_epoch_at_start {
            return;
        }
        if !matches!(&*state, DomainDiscoveryState::NotStarted) {
            return;
        }

        // Clone the latest request out and release the slot's lock
        // immediately: it is a leaf lock, deliberately never held while
        // taking `default_domain` (or anything else) below. `None` is
        // impossible in production -- reaching here means a `begin_*` call
        // published this run, and every such call writes the slot first --
        // but a `None` simply means there is no default to apply, which is
        // also the correct degraded behavior.
        let request = self.domain_discovery_latest_default.lock().clone();

        let mut adopted = 0usize;
        for domain in domains {
            if matches!(
                self.register_discovered_domain_locked(domain),
                AddDiscoveredDomainOutcome::Inserted
            ) {
                adopted += 1;
            }
        }

        log::info!(
            "a superseded WSL domain discovery finished successfully after the run that \
             replaced it had already failed; adopting its {adopted} domain(s) rather than \
             leaving the cache populated with nothing registered against it"
        );

        // Same shape as the `Done` shortcut and `FinishDomainDiscoveryOnDrop`:
        // compare and set under a single write guard so a concurrent writer
        // can't land in the gap, and run the caller-supplied predicate under
        // `catch_unwind` because it is arbitrary code executing while this
        // holds both `domain_discovery` and `default_domain`.
        let mut not_found_pending_default = None;
        if let Some(request) = &request {
            if let Some(name) = &request.pending_default {
                match self.get_domain_by_name(name) {
                    Some(dom) => {
                        let mut default = self.default_domain.write();
                        if default.as_ref().map(|d| d.domain_id())
                            == request.default_domain_id_at_start
                        {
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                (request.accepts_as_default)(&dom)
                            })) {
                                Ok(true) => *default = Some(Arc::clone(&dom)),
                                Ok(false) => {}
                                Err(_) => {
                                    log::error!(
                                        "panic while deciding whether \"{name}\" is an acceptable \
                                         default domain; not applying it"
                                    );
                                }
                            }
                        }
                    }
                    None => not_found_pending_default = Some(name.to_string()),
                }
            }
        }

        *state = DomainDiscoveryState::Done {
            _generation: self
                .next_domain_discovery_generation
                .fetch_add(1, Ordering::SeqCst),
            not_found_pending_default,
        };

        drop(state);
        self.domain_discovery_cond.notify_all();
    }

    /// Kicks off a background domain discovery run, using `discover` to
    /// produce the domains to register (in production, `discover`
    /// constructs WSL `LocalDomain`s from `config::WslDomain::default_domains`,
    /// which shells out to `wsl.exe -l -v` and can block for many seconds
    /// on Windows -- but `Mux` itself has no WSL-specific knowledge here).
    ///
    /// The `discover` closure returns `anyhow::Result`: on success, the
    /// discovered domains are registered and the state transitions to
    /// `Done`. On error (including panics), the state transitions to
    /// `NotStarted` instead, allowing a later config reload to retry.
    ///
    /// This exists so that callers on the GUI/mux startup path never have
    /// to block on that discovery: the domains `discover` returns are
    /// registered directly against this `Mux` from the background thread
    /// once available. If `pending_default` is set, that domain also
    /// becomes the mux default domain as soon as it is discovered -- but
    /// only if the default domain hasn't changed since this call, so an
    /// intentional later choice (eg. `wezterm connect`/`wezterm ssh`
    /// setting their own default) isn't clobbered once discovery finally
    /// finishes; see `FinishDomainDiscoveryOnDrop`.
    ///
    /// A second call while discovery is already in progress does not start
    /// a redundant discovery thread (the caller is expected to gate this
    /// on there being nothing left to discover; see
    /// `update_mux_domains_impl`); it always replaces whatever
    /// `pending_default` the in-flight discovery was going to apply --
    /// including with `None` -- so a config reload's updated default
    /// domain (or its explicit choice to no longer want one) isn't
    /// silently overridden by a stale value from before the reload once
    /// the first discovery finishes.
    ///
    /// Precondition if `pending_default` is used: this `Mux` should
    /// already have *some* default domain set before calling this (every
    /// production caller does, seeding one into `Mux::new` before
    /// `update_mux_domains` ever runs). If it doesn't,
    /// `FinishDomainDiscoveryOnDrop`'s "the default hasn't changed since
    /// this call" check compares against `None`; if `discover` finds any
    /// domain, `add_domain` auto-assigns the first one as the default
    /// (its own, unrelated fallback for "no default yet"), which then
    /// reads as "the default changed" and suppresses applying
    /// `pending_default` -- leaving an arbitrary discovered domain as the
    /// default instead of the one actually requested.
    pub fn begin_domain_discovery(
        self: &Arc<Self>,
        discover: impl FnOnce() -> anyhow::Result<Vec<Arc<dyn Domain>>> + Send + 'static,
        pending_default: Option<String>,
    ) {
        self.begin_domain_discovery_with_default_filter(discover, pending_default, |_| true)
    }

    /// Like `begin_domain_discovery`, but `accepts_as_default` gets a say
    /// in whether the domain `pending_default` names is actually
    /// acceptable to apply as the mux default once discovery finds it --
    /// eg. so a standalone mux-server caller can keep enforcing "a client
    /// domain can't be the mux-server default", the same invariant it
    /// already enforces synchronously elsewhere, even when that domain is
    /// only found asynchronously. See `FinishDomainDiscoveryOnDrop`.
    ///
    /// `accepts_as_default` is invoked from three places, all while holding
    /// `domain_discovery`'s lock *and* `default_domain`'s write lock, and
    /// all wrapping the call in `catch_unwind` so a panic in it can't
    /// strand the state machine or unwind into a config-reload task:
    ///
    /// 1. `FinishDomainDiscoveryOnDrop`'s `Drop` impl, when a background
    ///    run finishes and applies its `pending_default`;
    /// 2. the `Done` shortcut in this function, when a previous run already
    ///    registered the requested domain and no new run is needed;
    /// 3. `adopt_orphaned_discovery`, when a superseded run turns out to be
    ///    the only one that ever produced a result. Note that the predicate
    ///    invoked there is the one from the *most recent* call to this
    ///    function, not the one passed alongside the run being adopted --
    ///    it is applied together with that call's `pending_default`, and the
    ///    two have to agree on which config they came from. See
    ///    `domain_discovery_latest_default`.
    ///
    /// In all three cases it must **not** call back into `Mux`'s
    /// domain-discovery API (`begin_domain_discovery*`,
    /// `cancel_domain_discovery_and_replace`, `await_domain_discovery*`,
    /// etc.), nor take `default_domain`'s lock (eg. via
    /// `set_default_domain`/`default_domain()`) -- either would deadlock.
    /// Every current caller passes a trivial predicate (`|_| true`, or a
    /// plain `downcast`/type check) that satisfies this; keep it that way.
    pub fn begin_domain_discovery_with_default_filter(
        self: &Arc<Self>,
        discover: impl FnOnce() -> anyhow::Result<Vec<Arc<dyn Domain>>> + Send + 'static,
        pending_default: Option<String>,
        accepts_as_default: impl Fn(&Arc<dyn Domain>) -> bool + Send + Sync + 'static,
    ) {
        let generation;
        let cancel_epoch_at_start;
        let accepts_as_default: Arc<dyn Fn(&Arc<dyn Domain>) -> bool + Send + Sync> =
            Arc::new(accepts_as_default);
        let mut stuck_wakers: Vec<Waker> = vec![];
        {
            let mut state = self.domain_discovery.lock();
            // Snapshotted *after* acquiring `domain_discovery`'s lock
            // (rather than before it) so it's consistent with everything
            // else decided under that lock below. `domain_discovery` is
            // always the outer lock relative to `default_domain` in this
            // file (see eg. the `Done`-shortcut branch a few lines down,
            // which takes `default_domain`'s write lock while still
            // holding this one), so nesting a `default_domain.read()`
            // snapshot inside an already-held `domain_discovery` lock here
            // follows that same established order. Taking the snapshot
            // before acquiring `domain_discovery` would leave a gap in
            // which a different in-flight run's `FinishDomainDiscoveryOnDrop`
            // could apply its own `pending_default` and transition to
            // `Done`, making this snapshot stale relative to what it's
            // about to be compared against below.
            let default_domain_id_at_start =
                self.default_domain.read().as_ref().map(|d| d.domain_id());
            // Record this call as the latest word on what should be applied
            // as the default, unconditionally and before any of the
            // branching below: `adopt_orphaned_discovery` reads this slot
            // instead of values captured when the adopting run was spawned,
            // so a reload landing while a run is stuck updates what a late
            // adoption will apply even though it can't reach into the
            // already-running thread. (Written under `domain_discovery`'s
            // lock so it can't interleave against another `begin_*` call's
            // state transition; its own lock is a leaf, see the field.)
            *self.domain_discovery_latest_default.lock() = Some(DomainDiscoveryDefaultRequest {
                pending_default: pending_default.clone(),
                default_domain_id_at_start,
                accepts_as_default: Arc::clone(&accepts_as_default),
            });
            if let DomainDiscoveryState::InProgress {
                pending_default: in_flight_pending_default,
                default_domain_id_at_start: in_flight_default_domain_id_at_start,
                accepts_as_default: in_flight_accepts_as_default,
                wakers: in_flight_wakers,
                started_at,
                ..
            } = &mut *state
            {
                if started_at.elapsed() < DOMAIN_DISCOVERY_STUCK_THRESHOLD {
                    // Refresh `accepts_as_default` together with the other
                    // fields on every repeat call, following the same pattern as
                    // `pending_default` and `default_domain_id_at_start`.
                    *in_flight_pending_default = pending_default;
                    *in_flight_default_domain_id_at_start = default_domain_id_at_start;
                    *in_flight_accepts_as_default = accepts_as_default;
                    return;
                }
                // This run has been `InProgress` for longer than
                // `DOMAIN_DISCOVERY_STUCK_THRESHOLD` without finishing:
                // treat it as permanently stuck (see that constant's doc
                // comment for why) and fall through to supersede it with a
                // fresh run below, instead of the usual no-op refresh.
                //
                // Its own discovery thread, if it's still alive, keeps
                // running in the background and is simply abandoned here,
                // not cancelled -- there's no way to cancel a blocked
                // `wsl.exe` subprocess call from here. `add_discovered_domain`
                // and `FinishDomainDiscoveryOnDrop` both compare against
                // the *current* generation under this same lock before
                // doing anything, so whatever that stale thread eventually
                // produces (including much later, once its `wsl.exe`
                // finally returns) is silently discarded as superseded the
                // moment we bump the generation below -- it can never
                // clobber the fresh run's state or double-register a
                // domain.
                log::warn!(
                    "background WSL domain discovery has been running for over {:?} \
                     without finishing; treating it as stuck and starting a new attempt",
                    DOMAIN_DISCOVERY_STUCK_THRESHOLD
                );
                stuck_wakers = std::mem::take(in_flight_wakers)
                    .drain()
                    .map(|(_, w)| w)
                    .collect();
            }

            if matches!(&*state, DomainDiscoveryState::Done { .. }) {
                // A previous run already completed successfully;
                // `discover` is expected to be idempotent/cached at the
                // caller's level (eg. config::WslDomain::default_domains()
                // memoizes its own result -- see config/src/wsl.rs), so a
                // domain this run would find has *already* been
                // registered by the prior run. If there's nothing to
                // apply as a default, or the requested default is
                // already known, there's nothing a fresh discovery
                // thread could add: skip spawning one entirely, rather
                // than doing that on every single config reload just to
                // re-derive the same answer.
                match &pending_default {
                    None => return,
                    Some(name) => {
                        if let Some(dom) = self.get_domain_by_name(name) {
                            // Same "hasn't the default changed since we
                            // decided to apply this" guard as
                            // FinishDomainDiscoveryOnDrop, for the same
                            // reason: an intentional default set between
                            // this call starting and reaching this point
                            // (eg. `wezterm connect` picking its own
                            // domain) must not be clobbered. Compare and
                            // set under one write guard (not a `.read()`
                            // followed by a separately-acquired
                            // `.write()` inside `set_default_domain`), so
                            // a concurrent writer can't land in the gap --
                            // same fix as `FinishDomainDiscoveryOnDrop`.
                            let mut default = self.default_domain.write();
                            let current_default_domain_id = default.as_ref().map(|d| d.domain_id());
                            if current_default_domain_id == default_domain_id_at_start {
                                // Same `catch_unwind` treatment as the
                                // `Drop` path: this is caller-supplied
                                // code running under two of our locks
                                // (`domain_discovery` *and*
                                // `default_domain`), and a panic escaping
                                // here would unwind through
                                // `update_mux_domains` -- on the GUI, that
                                // is the main-thread executor. parking_lot
                                // locks don't poison, so not applying the
                                // default is the whole of the damage.
                                let accepted =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        (*accepts_as_default)(&dom)
                                    }));
                                match accepted {
                                    Ok(true) => {
                                        *default = Some(Arc::clone(&dom));
                                    }
                                    Ok(false) => {}
                                    Err(_) => {
                                        log::error!(
                                            "panic while deciding whether \"{name}\" is an \
                                             acceptable default domain; not applying it"
                                        );
                                    }
                                }
                            }
                            drop(default);
                            return;
                        }
                        // Domain not found. Check if this name was already
                        // looked for by a previous run and not found.
                        // If so, skip spawning a new thread that will
                        // just re-derive the same "not found" answer.
                        if let DomainDiscoveryState::Done {
                            not_found_pending_default,
                            ..
                        } = &*state
                        {
                            if *not_found_pending_default == Some(name.clone()) {
                                return;
                            }
                        }
                        // Not found yet: fall through and actually
                        // (re-)run discovery, in case this is a
                        // genuinely new name discovery hasn't seen
                        // before.
                    }
                }
            }

            // Only allocated (and only ever compared) while holding this
            // same lock, so two runs can never share a generation and a
            // run can never be started with a generation that's already
            // stale by the time it's published.
            generation = self
                .next_domain_discovery_generation
                .fetch_add(1, Ordering::SeqCst);
            // Read under the same lock, for the same reason: it's only
            // meaningful compared against a later read that is also taken
            // under it. See `adopt_orphaned_discovery`.
            cancel_epoch_at_start = self.domain_discovery_cancel_epoch.load(Ordering::SeqCst);
            *state = DomainDiscoveryState::InProgress {
                generation,
                pending_default,
                default_domain_id_at_start,
                wakers: HashMap::new(),
                next_waiter_id: 0,
                accepts_as_default,
                started_at: Instant::now(),
            };
        }

        // Wake anything that was still waiting on a stuck run we just
        // superseded, after releasing the lock -- matching the
        // `FinishDomainDiscoveryOnDrop::drop` / `cancel_domain_discovery_and_replace`
        // convention elsewhere in this file. A no-op (empty vec, harmless
        // `notify_all` on a condvar nothing is parked on) on the far more
        // common path where the previous state wasn't a stuck `InProgress`.
        self.domain_discovery_cond.notify_all();
        for waker in stuck_wakers {
            waker.wake();
        }

        let mux = Arc::clone(self);
        if std::thread::Builder::new()
            .name("mux-domain-discovery".to_string())
            .spawn(move || {
                // Run the discover closure and capture the result
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| discover()));

                let result = match result {
                    Ok(domains) => domains,
                    Err(_) => {
                        // Panic in discover: treat as error
                        Err(anyhow!("discover closure panicked"))
                    }
                };

                // If discover succeeded, register the domains.
                // Continue on AlreadyPresent, only abort on Superseded.
                //
                // Registration runs *before* the finish guard exists and
                // under its own `catch_unwind`, so that a panic escaping
                // any `Domain` method reached from here (`domain_name()`
                // and friends -- these are trait objects, so this crate
                // doesn't control what they do) is folded into `result`
                // as an error rather than unwinding past a guard that's
                // already holding `Ok(..)`. Getting that wrong is not
                // just untidy: the guard would publish `Done` despite the
                // registration having only partially happened, and a
                // later config reload carrying `pending_default: None`
                // would then take the `Done` shortcut and never retry
                // discovery for the rest of the process's life. Failing
                // to `NotStarted` instead makes that reload re-run it,
                // which is safe to repeat -- `add_discovered_domain`
                // reports the already-registered ones as `AlreadyPresent`,
                // which this loop deliberately tolerates.
                let result = match result {
                    Err(err) => Err(err),
                    Ok(domains) => {
                        // `false` means "superseded, stop registering",
                        // distinct from a panic (`Err`) and from running
                        // to completion (`true`).
                        let registered =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                for domain in &domains {
                                    match mux.add_discovered_domain(generation, domain) {
                                        AddDiscoveredDomainOutcome::Inserted => {}
                                        AddDiscoveredDomainOutcome::AlreadyPresent => {
                                            // Name collision is expected and should continue.
                                        }
                                        AddDiscoveredDomainOutcome::Superseded => {
                                            // Superseded (cancelled, or a newer discovery run
                                            // started): generation only ever increases, so
                                            // once this run is stale it can't become current
                                            // again, and none of the remaining domains should
                                            // be registered either.
                                            return false;
                                        }
                                    }
                                }
                                true
                            }));
                        match registered {
                            Ok(true) => Ok(domains),
                            Ok(false) => {
                                // Superseded: skip the finish guard
                                // entirely. Constructing it would be a
                                // no-op anyway (it compares generations
                                // and finds itself stale), so returning
                                // here is equivalent and clearer.
                                //
                                // First, though: `discover` has by now
                                // published this list to the config-level
                                // cache, which no generation check governs.
                                // If nothing else is going to register it,
                                // dropping it here would leave that cache
                                // describing domains the mux doesn't have.
                                // `adopt_orphaned_discovery` decides whether
                                // this is that case; it is a no-op otherwise.
                                mux.adopt_orphaned_discovery(cancel_epoch_at_start, &domains);
                                return;
                            }
                            Err(_) => {
                                log::error!(
                                    "panic while registering discovered domains; \
                                     treating this discovery run as failed so a later \
                                     config reload retries it"
                                );
                                Err(anyhow!("panic while registering discovered domains"))
                            }
                        }
                    }
                };

                // Guarantees the state transitions and wakes any
                // waiters even if `discover()` panics (eg. a malformed
                // `wsl -l -v` output tripping an indexing panic in
                // parse_wsl_distro_list). Without this, a panic here
                // would leave `InProgress` in place forever, and every
                // future domain resolution would silently pay the full
                // DOMAIN_DISCOVERY_TIMEOUT for the rest of the process's
                // life.
                let finish_on_drop = FinishDomainDiscoveryOnDrop {
                    mux: Arc::clone(&mux),
                    generation,
                    result: Some(result),
                };

                // Keep the guard alive until the end of the scope
                let _ = finish_on_drop;
            })
            .is_err()
        {
            // Couldn't spawn the discovery thread: this run never
            // happened, so reset to `NotStarted` (only if this is still
            // the run we just published -- an extremely unlikely, but
            // possible, race against a concurrent cancel/newer `begin`
            // landing in between) rather than `Done`, so a later call
            // retries for real instead of being fooled into thinking
            // discovery already completed. See `cancel_domain_discovery`'s
            // doc comment for why `Done` and "nothing was ever discovered"
            // must stay distinguishable.
            let mut state = self.domain_discovery.lock();
            let is_current = matches!(
                &*state,
                DomainDiscoveryState::InProgress { generation: g, .. } if *g == generation
            );
            let mut wakers_to_wake: Vec<Waker> = vec![];
            if is_current {
                // Wake any wakers that may have been registered (now a HashMap)
                if let DomainDiscoveryState::InProgress { wakers, .. } = &mut *state {
                    wakers_to_wake = wakers.drain().map(|(_, w)| w).collect();
                }
                *state = DomainDiscoveryState::NotStarted;
            }
            drop(state);
            self.domain_discovery_cond.notify_all();

            // Wake all async waiters *after* releasing the lock, matching
            // `FinishDomainDiscoveryOnDrop::drop` and
            // `cancel_domain_discovery_and_replace`. Waking under the lock
            // happens to be safe with the executor actually in use
            // (`async_task` never polls or drops the woken future inline on
            // this thread, so `DomainDiscoveryWaitFuture::drop` can't reenter
            // `domain_discovery`), but that's an implicit dependency on an
            // executor implementation detail, and it costs nothing to not
            // depend on it.
            for waker in wakers_to_wake {
                waker.wake();
            }
        }
    }

    /// Atomically cancels in-flight discovery, runs the replacement
    /// registration closure while still holding the lock, then publishes.
    /// This closes the TOCTOU race where a new waiter arriving between
    /// `cancel_domain_discovery` and `notify_cancelled_domain_discovery_waiters`
    /// would see `NotStarted` and do its lookup before replacement domains are
    /// registered.
    ///
    /// The `register_replacements` closure runs synchronously under the
    /// `domain_discovery` lock. It must only do cheap, in-memory work like
    /// calling `mux.add_domain` for already-known domain objects (no I/O,
    /// no subprocess calls), and must NOT call back into `Mux`'s discovery API
    /// (would deadlock on this same lock).
    ///
    /// Cancellation semantics are identical to `cancel_domain_discovery`,
    /// including leaving an already-`Done` state alone: see that function's
    /// doc comment for why `Done` and "cancelled without ever completing"
    /// must stay distinguishable.
    pub fn cancel_domain_discovery_and_replace(
        &self,
        register_replacements: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        // Guarantees the "publish" half of this function -- notifying the
        // condvar and waking the async waiters -- runs on *every* exit
        // path, including a panic escaping the caller-supplied
        // `register_replacements` closure below. Without it such a panic
        // unwinds straight past the notify/wake calls while the wakers
        // this function has already drained out of the state get dropped,
        // leaving async waiters parked with their registration gone and
        // nothing left that could ever wake them: `Pending` for the rest
        // of the process's life. (Sync waiters survive it, but only
        // because `await_domain_discovery`/`_unbounded` re-check on a
        // timer.) The production closure is trivial today, but this is a
        // public API taking an arbitrary caller closure, and the same
        // hazard on the discovery-completion path is already handled the
        // same way by `FinishDomainDiscoveryOnDrop`.
        //
        // Declared *before* the state lock below so that it drops *after*
        // it: locals drop in reverse declaration order, which keeps the
        // "never wake while holding `domain_discovery`" rule the rest of
        // this file follows, on the unwind path as well as the normal one.
        struct NotifyDiscoveryWaitersOnDrop<'a> {
            mux: &'a Mux,
            wakers: Vec<Waker>,
        }
        impl Drop for NotifyDiscoveryWaitersOnDrop<'_> {
            fn drop(&mut self) {
                self.mux.domain_discovery_cond.notify_all();
                for waker in std::mem::take(&mut self.wakers) {
                    waker.wake();
                }
            }
        }
        let mut notify_on_drop = NotifyDiscoveryWaitersOnDrop {
            mux: self,
            wakers: vec![],
        };

        let mut state = self.domain_discovery.lock();
        // Bumped under the lock, unconditionally: see the field's doc comment.
        // A superseded-but-successful run that observes a changed epoch must
        // not adopt its result, because this config no longer wants
        // auto-discovered domains at all.
        self.domain_discovery_cancel_epoch
            .fetch_add(1, Ordering::SeqCst);
        if let DomainDiscoveryState::InProgress { wakers, .. } = &mut *state {
            notify_on_drop.wakers = std::mem::take(wakers).drain().map(|(_, w)| w).collect();
        }
        // Only an in-flight run gets cancelled. Clobbering `Done` here
        // would throw away the knowledge that a run already completed, so
        // a later reload back to auto-discovery would pointlessly spawn a
        // fresh discovery thread instead of taking `begin_domain_discovery`'s
        // already-completed shortcut -- and it would diverge from
        // `cancel_domain_discovery`, which this is otherwise supposed to be
        // a drop-in, race-free replacement for.
        if matches!(&*state, DomainDiscoveryState::InProgress { .. }) {
            *state = DomainDiscoveryState::NotStarted;
        }

        // Run replacement registration while STILL HOLDING domain_discovery's
        // lock. This is the crux of the fix: a new waiter (sync via
        // await_domain_discovery/_unbounded, or async via
        // DomainDiscoveryWaitFuture::poll) has to acquire this same lock
        // before it can inspect state at all, so it can't observe the
        // intermediate "cancelled to NotStarted, but replacements not
        // registered yet" window -- it simply blocks here until we're done
        // and release the lock below. `register_replacements` must stay
        // fast, in-memory work (see the doc comment above) precisely so
        // that this blocking window stays short, unlike the `wsl.exe`
        // subprocess call this lock is deliberately never held across.
        let result = register_replacements();

        // Releases the lock before `notify_on_drop` fires, same as the
        // unwind path gets for free from drop order.
        drop(state);
        result
    }

    /// Marks any in-progress background domain discovery as no longer
    /// relevant and immediately unblocks anything waiting on it, resetting
    /// back to `NotStarted` so a later `begin_domain_discovery` call
    /// starts a fresh, real discovery run rather than being fooled into
    /// thinking one already completed. A no-op if nothing is in flight --
    /// in particular, this deliberately does *not* touch an already-`Done`
    /// state: if discovery genuinely completed successfully before, the
    /// domains it found are still registered in the mux (domains are
    /// never removed on reload), so there is nothing to "cancel" or lose
    /// by leaving `Done` as `Done`.
    ///
    /// Resetting to `NotStarted` here (rather than the `Done` this used to
    /// transition to) is what fixes a real bug: `Done` and "cancelled
    /// without ever completing" used to be indistinguishable, so a config
    /// that started with explicit `wsl_domains` (calling this from
    /// `NotStarted`, which used to unconditionally land on `Done`), or
    /// that cancelled a still-running auto-discovery mid-flight, and later
    /// reloaded back to auto-discovery, would see `Done` and skip
    /// discovery entirely via `begin_domain_discovery`'s
    /// already-completed shortcut -- even though nothing had actually
    /// ever been discovered, silently losing the auto-discovered domains
    /// until the process restarted.
    ///
    /// Called when a config reload moves to an explicit domain list that
    /// no longer wants discovery: from that point on, a still-running
    /// discovery thread from before the reload must not be allowed to
    /// register extra domains or override the default that the explicit
    /// configuration is about to set synchronously. The discovery thread
    /// itself, if any, keeps running to completion (it can't be killed
    /// mid-subprocess-call), but its results are discarded: resetting
    /// state away from its `InProgress { generation, .. }` here is what
    /// makes that thread's captured generation stop matching --
    /// `add_discovered_domain` and `FinishDomainDiscoveryOnDrop` both
    /// require the state to still be `InProgress` with their generation,
    /// which is no longer true once this runs.
    ///
    /// Deliberately **not** `pub`: `cancel_domain_discovery_and_replace` is
    /// the supported entry point, and it exists precisely because this
    /// two-phase split is easy to misuse. The hazard can't be designed out
    /// while the two halves stay separate -- a caller that drops the
    /// returned `Waker`s, returns early, or panics between the two calls
    /// silently parks every async waiter forever, and `#[must_use]` only
    /// produces a warning. Restricting visibility instead of papering over
    /// it with more documentation keeps the footgun out of this crate's
    /// public surface; the tests below still exercise the split directly,
    /// since that is what `cancel_domain_discovery_and_replace` is built
    /// on top of.
    ///
    /// Returns the list of async waiters (`Waker`s) that were waiting on
    /// this discovery. Deliberately does **not** wake anything itself --
    /// neither the returned `Waker`s nor the synchronous `Condvar` that
    /// `await_domain_discovery`/`await_domain_discovery_unbounded` block
    /// on (both loop on the state, so notifying either kind of waiter here
    /// would let it observe "no longer `InProgress`" and return before a
    /// caller that's about to register replacement domains -- eg. explicit
    /// `wsl_domains` -- has actually done so, reopening the exact race this
    /// two-phase cancel/publish split exists to close). Call
    /// `notify_cancelled_domain_discovery_waiters` once any replacement
    /// domains are visible, passing the `Waker`s returned here, so every
    /// waiter -- sync or async -- observes either the pre-cancellation
    /// state or the fully-registered replacement, never an in-between one.
    ///
    /// Dropping the returned `Waker`s instead of passing them on is a
    /// silent hang: their slots have already been removed from the state,
    /// so nothing else will ever wake those futures. Hence `#[must_use]`.
    #[must_use = "the returned Wakers must be passed to \
                  notify_cancelled_domain_discovery_waiters, or the futures \
                  waiting on this discovery will never be woken again"]
    pub(crate) fn cancel_domain_discovery(&self) -> Vec<Waker> {
        let mut state = self.domain_discovery.lock();
        // Same unconditional bump as `cancel_domain_discovery_and_replace`;
        // see `domain_discovery_cancel_epoch`'s doc comment.
        self.domain_discovery_cancel_epoch
            .fetch_add(1, Ordering::SeqCst);
        if matches!(&*state, DomainDiscoveryState::InProgress { .. }) {
            let wakers = if let DomainDiscoveryState::InProgress { wakers, .. } = &mut *state {
                std::mem::take(wakers).drain().map(|(_, w)| w).collect()
            } else {
                vec![]
            };
            *state = DomainDiscoveryState::NotStarted;
            wakers
        } else {
            vec![]
        }
    }

    /// Publishes a cancellation performed by `cancel_domain_discovery`:
    /// wakes the synchronous `Condvar` waiters and the given async
    /// `Waker`s (as returned by that call). Call this only once any
    /// replacement domains the cancellation was making way for are already
    /// registered -- see `cancel_domain_discovery`'s doc comment for why
    /// the two are split. A no-op (beyond the harmless `notify_all` with no
    /// parked waiters) if nothing was cancelled.
    ///
    /// Not `pub` for the same reason as its counterpart: only useful as the
    /// second half of that split, which isn't part of this crate's public
    /// surface.
    pub(crate) fn notify_cancelled_domain_discovery_waiters(&self, wakers: Vec<Waker>) {
        self.domain_discovery_cond.notify_all();
        for waker in wakers {
            waker.wake();
        }
    }

    /// Blocks the calling thread until background domain discovery (if
    /// any is in progress) has finished, or `timeout` elapses, whichever
    /// is sooner. A no-op if discovery hasn't started or has already
    /// completed.
    pub fn await_domain_discovery(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = self.domain_discovery.lock();
        while matches!(*state, DomainDiscoveryState::InProgress { .. }) {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(remaining) => remaining,
                None => return,
            };
            let result = self.domain_discovery_cond.wait_for(&mut state, remaining);
            if result.timed_out() {
                return;
            }
        }
    }

    /// Like `await_domain_discovery`, but with no timeout: blocks until
    /// any in-progress background domain discovery finishes (a no-op if
    /// none is in progress). Used only where the caller has already
    /// committed to needing the outcome of discovery -- namely, resolving
    /// a `default_domain`/`default_mux_server_domain` that was explicitly
    /// configured to name a domain discovery hasn't found yet
    /// (`pending_default` is set). An arbitrary timeout on that specific
    /// path would incorrectly treat "discovery is still slow" as "the
    /// domain doesn't exist" and fail GUI/mux-server startup outright;
    /// before background discovery existed, this same case simply blocked
    /// for as long as the (then-synchronous) discovery took, with no
    /// cutoff, so this preserves that guarantee for the narrow case that
    /// actually needs it while every other domain resolution keeps the
    /// bounded wait.
    pub fn await_domain_discovery_unbounded(&self) {
        // How often this re-checks and, if still waiting, logs again.
        // "Unbounded" describes the overall wait (no deadline after which
        // this gives up), not this polling granularity: waking up
        // periodically instead of parking on a single indefinite
        // `Condvar::wait` costs nothing observable (the loop just goes
        // straight back to sleep if the state hasn't changed) and turns a
        // stuck wait from "silent forever" into "logged every 30s", which
        // matters most in exactly the case `DOMAIN_DISCOVERY_STUCK_THRESHOLD`
        // exists for: a genuinely hung `wsl.exe` that this same caller may
        // itself never resolve without an external event (eg. a config
        // reload) triggering the mux-level supersede.
        const LOG_INTERVAL: Duration = Duration::from_secs(30);
        let mut state = self.domain_discovery.lock();
        let started_waiting = Instant::now();
        let mut logged = false;
        while matches!(*state, DomainDiscoveryState::InProgress { .. }) {
            // Only reached when discovery is genuinely still in progress
            // and we're about to actually block on it, not when this call
            // is a no-op (`NotStarted`/`Done`). No deadline on this wait
            // -- see the doc comment above -- so this periodic log is the
            // only signal a caller gets that it's stuck here rather than
            // merely slow.
            if !logged {
                log::info!("waiting for WSL domain discovery to finish (unbounded)");
            } else {
                log::info!(
                    "still waiting for WSL domain discovery to finish (unbounded), {:?} elapsed",
                    started_waiting.elapsed()
                );
            }
            logged = true;
            self.domain_discovery_cond
                .wait_for(&mut state, LOG_INTERVAL);
        }
    }

    /// Async-friendly counterpart to `await_domain_discovery`: resolves
    /// once background domain discovery (if any is in progress) finishes,
    /// or `timeout` elapses, without blocking the calling thread while it
    /// waits. `await_domain_discovery`/`await_domain_discovery_unbounded`
    /// park the *calling* thread on a condvar, which is fine for genuinely
    /// synchronous callers (Lua API functions, `setup_mux` before the GUI
    /// event loop starts) but wrong to `.await` from an `async fn` running
    /// on the single-threaded main-thread executor (`Mux::split_pane`,
    /// `Mux::spawn_tab_or_window`): polling a blocking wait there would
    /// freeze that whole executor -- no repaints, no input, no other
    /// spawns -- for the entire wait, unlike a real `.await` point, which
    /// lets the executor run other work while this is pending.
    ///
    /// Returns a `DomainDiscoveryWaitFuture`, which registers its waker
    /// directly in `DomainDiscoveryState` rather than parking a thread per
    /// waiter, and which carries its own timer so the timeout is part of the
    /// future itself instead of a second wait layered on top of it.
    pub fn await_domain_discovery_async(
        self: &Arc<Self>,
        timeout: Duration,
    ) -> DomainDiscoveryWaitFuture {
        DomainDiscoveryWaitFuture::new(Arc::clone(self), Some(timeout))
    }

    /// Async-friendly counterpart to `await_domain_discovery_unbounded`;
    /// see `await_domain_discovery_async` for why this doesn't just block
    /// the calling thread directly.
    pub fn await_domain_discovery_unbounded_async(self: &Arc<Self>) -> DomainDiscoveryWaitFuture {
        DomainDiscoveryWaitFuture::new(Arc::clone(self), None)
    }

    /// Which flavor of discovery wait (if any) `await_domain_resolution_async`
    /// needs for a given `resolve_spawn_tab_domain` target. Split out from
    /// `await_domain_resolution_async` as a synchronous, thread/executor-free
    /// decision so it can be unit tested directly, without needing the
    /// global `promise::spawn` scheduler configured (as it would need to
    /// be to actually drive `await_domain_discovery_async`'s `Task` to
    /// completion in a test).
    fn domain_resolution_wait_kind(
        &self,
        pane_id: Option<PaneId>,
        domain: &SpawnTabDomain,
    ) -> DomainResolutionWaitKind {
        let is_discovery_in_progress = || {
            matches!(
                &*self.domain_discovery.lock(),
                DomainDiscoveryState::InProgress { .. }
            )
        };
        let needs_unbounded_wait_for_default = || {
            matches!(
                &*self.domain_discovery.lock(),
                DomainDiscoveryState::InProgress {
                    pending_default: Some(_),
                    ..
                }
            )
        };
        match domain {
            // Only worth spawning the async wait helper if discovery is
            // actually in progress: if it's NotStarted or Done, there's
            // nothing that could still register this name, and
            // resolve_spawn_tab_domain will report "invalid domain"
            // immediately either way.
            SpawnTabDomain::DomainName(name)
                if self.get_domain_by_name(name).is_none() && is_discovery_in_progress() =>
            {
                DomainResolutionWaitKind::Bounded
            }
            SpawnTabDomain::DefaultDomain if needs_unbounded_wait_for_default() => {
                DomainResolutionWaitKind::Unbounded
            }
            SpawnTabDomain::CurrentPaneDomain
                if pane_id.is_none() && needs_unbounded_wait_for_default() =>
            {
                DomainResolutionWaitKind::Unbounded
            }
            _ => DomainResolutionWaitKind::None,
        }
    }

    /// Waits (asynchronously; see `await_domain_discovery_async`) for any
    /// background domain discovery that resolving `domain` via
    /// `resolve_spawn_tab_domain` would otherwise have to block on
    /// synchronously. Intended to be called from `async fn` call sites
    /// running on the main-thread executor (`split_pane`,
    /// `spawn_tab_or_window`) *before* the synchronous
    /// `resolve_spawn_tab_domain`, so that method's own (thread-blocking)
    /// waits become no-ops by the time it's actually called: discovery
    /// will already be resolved one way or the other. Mirrors the "should
    /// I wait" conditions inside `resolve_spawn_tab_domain` and
    /// `resolve_default_domain` -- a name not yet known, or a
    /// `DefaultDomain`/pane-less-`CurrentPaneDomain` resolution while a
    /// `pending_default` is still outstanding -- using the *unbounded*
    /// async wait for the latter, matching `resolve_default_domain`'s own
    /// no-arbitrary-cutoff guarantee. See `domain_resolution_wait_kind`
    /// for the (separately tested) decision logic.
    pub async fn await_domain_resolution_async(
        self: &Arc<Self>,
        pane_id: Option<PaneId>,
        domain: &SpawnTabDomain,
    ) {
        match self.domain_resolution_wait_kind(pane_id, domain) {
            DomainResolutionWaitKind::None => {}
            DomainResolutionWaitKind::Bounded => {
                let _ = self
                    .await_domain_discovery_async(DOMAIN_DISCOVERY_TIMEOUT)
                    .await;
            }
            DomainResolutionWaitKind::Unbounded => {
                let _ = self.await_domain_discovery_unbounded_async().await;
            }
        }
    }

    pub fn set_mux(mux: &Arc<Mux>) {
        MUX.lock().replace(Arc::clone(mux));
    }

    pub fn shutdown() {
        MUX.lock().take();
    }

    pub fn get() -> Arc<Mux> {
        Self::try_get().unwrap()
    }

    pub fn try_get() -> Option<Arc<Mux>> {
        MUX.lock().as_ref().map(Arc::clone)
    }

    pub fn get_pane(&self, pane_id: PaneId) -> Option<Arc<dyn Pane>> {
        self.panes.read().get(&pane_id).map(Arc::clone)
    }

    pub fn get_tab(&self, tab_id: TabId) -> Option<Arc<Tab>> {
        self.tabs.read().get(&tab_id).map(Arc::clone)
    }

    pub fn add_pane(&self, pane: &Arc<dyn Pane>) -> Result<(), Error> {
        if self.panes.read().contains_key(&pane.pane_id()) {
            return Ok(());
        }

        let clipboard: Arc<dyn Clipboard> = Arc::new(MuxClipboard {
            pane_id: pane.pane_id(),
        });
        pane.set_clipboard(&clipboard);

        let downloader: Arc<dyn DownloadHandler> = Arc::new(MuxDownloader {});
        pane.set_download_handler(&downloader);

        self.panes.write().insert(pane.pane_id(), Arc::clone(pane));
        let pane_id = pane.pane_id();
        if let Some(reader) = pane.reader()? {
            let banner = self.banner.read().clone();
            let pane = Arc::downgrade(pane);
            thread::spawn(move || read_from_pane_pty(pane, banner, reader));
        }
        self.recompute_pane_count();
        self.notify(MuxNotification::PaneAdded(pane_id));
        Ok(())
    }

    pub fn add_tab_no_panes(&self, tab: &Arc<Tab>) {
        self.tabs.write().insert(tab.tab_id(), Arc::clone(tab));
        self.recompute_pane_count();
    }

    pub fn add_tab_and_active_pane(&self, tab: &Arc<Tab>) -> Result<(), Error> {
        self.tabs.write().insert(tab.tab_id(), Arc::clone(tab));
        let pane = tab
            .get_active_pane()
            .ok_or_else(|| anyhow!("tab MUST have an active pane"))?;
        self.add_pane(&pane)
    }

    fn remove_pane_internal(&self, pane_id: PaneId) {
        log::debug!("removing pane {}", pane_id);
        let mut changed = false;
        if let Some(pane) = self.panes.write().remove(&pane_id).clone() {
            log::debug!("killing pane {}", pane_id);
            pane.kill();
            self.notify(MuxNotification::PaneRemoved(pane_id));
            changed = true;
        }

        if changed {
            self.recompute_pane_count();
        }
    }

    fn remove_tab_internal(&self, tab_id: TabId) -> Option<Arc<Tab>> {
        log::debug!("remove_tab_internal tab {}", tab_id);

        let tab = self.tabs.write().remove(&tab_id)?;

        if let Some(mut windows) = self.windows.try_write() {
            for w in windows.values_mut() {
                w.remove_by_id(tab_id);
            }
        }

        let mut pane_ids = vec![];
        for pos in tab.iter_panes_ignoring_zoom() {
            pane_ids.push(pos.pane.pane_id());
        }
        log::debug!("panes to remove: {pane_ids:?}");
        for pane_id in pane_ids {
            self.remove_pane_internal(pane_id);
        }
        self.recompute_pane_count();

        Some(tab)
    }

    fn remove_window_internal(&self, window_id: WindowId) {
        log::debug!("remove_window_internal {}", window_id);

        let window = self.windows.write().remove(&window_id);
        if let Some(window) = window {
            // Gather all the domains referenced by this window
            let mut domains_of_window = HashSet::new();
            for tab in window.iter() {
                for pane in tab.iter_panes_ignoring_zoom() {
                    domains_of_window.insert(pane.pane.domain_id());
                }
            }

            for domain_id in domains_of_window {
                if let Some(domain) = self.get_domain(domain_id) {
                    if domain.detachable() {
                        log::info!("detaching domain");
                        if let Err(err) = domain.detach() {
                            log::error!(
                                "while detaching domain {domain_id} {}: {err:#}",
                                domain.domain_name()
                            );
                        }
                    }
                }
            }

            for tab in window.iter() {
                self.remove_tab_internal(tab.tab_id());
            }
            self.notify(MuxNotification::WindowRemoved(window_id));
        }
        self.recompute_pane_count();
    }

    pub fn remove_pane(&self, pane_id: PaneId) {
        self.remove_pane_internal(pane_id);
        self.prune_dead_windows();
    }

    pub fn remove_tab(&self, tab_id: TabId) -> Option<Arc<Tab>> {
        let tab = self.remove_tab_internal(tab_id);
        self.prune_dead_windows();
        tab
    }

    pub fn prune_dead_windows(&self) {
        if Activity::count() > 0 {
            log::trace!("prune_dead_windows: Activity::count={}", Activity::count());
            return;
        }
        let live_tab_ids: Vec<TabId> = self.tabs.read().keys().cloned().collect();
        let mut dead_windows = vec![];
        let dead_tab_ids: Vec<TabId>;

        {
            let mut windows = match self.windows.try_write() {
                Some(w) => w,
                None => {
                    // It's ok if our caller already locked it; we can prune later.
                    log::trace!("prune_dead_windows: self.windows already borrowed");
                    return;
                }
            };
            for (window_id, win) in windows.iter_mut() {
                win.prune_dead_tabs(&live_tab_ids);
                if win.is_empty() {
                    log::trace!("prune_dead_windows: window is now empty");
                    dead_windows.push(*window_id);
                }
            }

            dead_tab_ids = self
                .tabs
                .read()
                .iter()
                .filter_map(|(&id, tab)| if tab.is_dead() { Some(id) } else { None })
                .collect();
        }

        for tab_id in dead_tab_ids {
            log::trace!("tab {} is dead", tab_id);
            self.remove_tab_internal(tab_id);
        }

        for window_id in dead_windows {
            log::trace!("window {} is dead", window_id);
            self.remove_window_internal(window_id);
        }

        if self.is_empty() {
            log::trace!("prune_dead_windows: is_empty, send MuxNotification::Empty");
            self.notify(MuxNotification::Empty);
        } else {
            log::trace!("prune_dead_windows: not empty");
        }
    }

    pub fn kill_window(&self, window_id: WindowId) {
        self.remove_window_internal(window_id);
        self.prune_dead_windows();
    }

    pub fn get_window(&self, window_id: WindowId) -> Option<MappedRwLockReadGuard<'_, Window>> {
        if !self.windows.read().contains_key(&window_id) {
            return None;
        }
        Some(RwLockReadGuard::map(self.windows.read(), |windows| {
            windows.get(&window_id).unwrap()
        }))
    }

    pub fn get_window_mut(
        &self,
        window_id: WindowId,
    ) -> Option<MappedRwLockWriteGuard<'_, Window>> {
        if !self.windows.read().contains_key(&window_id) {
            return None;
        }
        Some(RwLockWriteGuard::map(self.windows.write(), |windows| {
            windows.get_mut(&window_id).unwrap()
        }))
    }

    pub fn get_active_tab_for_window(&self, window_id: WindowId) -> Option<Arc<Tab>> {
        let window = self.get_window(window_id)?;
        window.get_active().map(Arc::clone)
    }

    pub fn new_empty_window(
        &self,
        workspace: Option<String>,
        position: Option<GuiPosition>,
    ) -> MuxWindowBuilder {
        let window = Window::new(workspace, position);
        let window_id = window.window_id();
        self.windows.write().insert(window_id, window);
        MuxWindowBuilder {
            window_id,
            activity: Some(Activity::new()),
            notified: false,
        }
    }

    pub fn add_tab_to_window(&self, tab: &Arc<Tab>, window_id: WindowId) -> anyhow::Result<()> {
        let tab_id = tab.tab_id();
        {
            let mut window = self
                .get_window_mut(window_id)
                .ok_or_else(|| anyhow!("add_tab_to_window: no such window_id {}", window_id))?;
            window.push(tab);
        }
        self.recompute_pane_count();
        self.notify(MuxNotification::TabAddedToWindow { tab_id, window_id });
        Ok(())
    }

    /// Returns the ID of the window containing the given tab ID, if any.
    pub fn window_containing_tab(&self, tab_id: TabId) -> Option<WindowId> {
        for w in self.windows.read().values() {
            for t in w.iter() {
                if t.tab_id() == tab_id {
                    return Some(w.window_id());
                }
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.panes.read().is_empty()
    }

    pub fn is_workspace_empty(&self, workspace: &str) -> bool {
        *self
            .num_panes_by_workspace
            .read()
            .get(workspace)
            .unwrap_or(&0)
            == 0
    }

    pub fn is_active_workspace_empty(&self) -> bool {
        let workspace = self.active_workspace();
        self.is_workspace_empty(&workspace)
    }

    pub fn iter_panes(&self) -> Vec<Arc<dyn Pane>> {
        self.panes
            .read()
            .iter()
            .map(|(_, v)| Arc::clone(v))
            .collect()
    }

    pub fn iter_windows_in_workspace(&self, workspace: &str) -> Vec<WindowId> {
        let mut windows: Vec<WindowId> = self
            .windows
            .read()
            .iter()
            .filter_map(|(k, w)| {
                if w.get_workspace() == workspace {
                    Some(k)
                } else {
                    None
                }
            })
            .cloned()
            .collect();
        windows.sort();
        windows
    }

    pub fn iter_windows(&self) -> Vec<WindowId> {
        self.windows.read().keys().cloned().collect()
    }

    pub fn iter_domains(&self) -> Vec<Arc<dyn Domain>> {
        self.domains.read().values().cloned().collect()
    }

    pub fn resolve_pane_id(&self, pane_id: PaneId) -> Option<(DomainId, WindowId, TabId)> {
        let mut ids = None;
        for tab in self.tabs.read().values() {
            for p in tab.iter_panes_ignoring_zoom() {
                if p.pane.pane_id() == pane_id {
                    ids = Some((tab.tab_id(), p.pane.domain_id()));
                    break;
                }
            }
        }
        let (tab_id, domain_id) = ids?;
        let window_id = self.window_containing_tab(tab_id)?;
        Some((domain_id, window_id, tab_id))
    }

    pub fn domain_was_detached(&self, domain: DomainId) {
        let mut dead_panes = vec![];
        for pane in self.panes.read().values() {
            if pane.domain_id() == domain {
                dead_panes.push(pane.pane_id());
            }
        }

        {
            let mut windows = self.windows.write();
            for (_, win) in windows.iter_mut() {
                for tab in win.iter() {
                    tab.kill_panes_in_domain(domain);
                }
            }
        }

        log::info!("domain detached panes: {:?}", dead_panes);
        for pane_id in dead_panes {
            self.remove_pane_internal(pane_id);
        }

        self.prune_dead_windows();
    }

    pub fn set_banner(&self, banner: Option<String>) {
        *self.banner.write() = banner;
    }

    /// Wait-free variant of resolve_default_domain for use after
    /// an async pre-wait (await_domain_resolution_async). Does not block,
    /// even if a brand new discovery run started in the gap - that's the
    /// correct trade-off to avoid blocking the executor a second time.
    /// Documented trade-off: if a genuinely new run would have found the
    /// requested domain, this may report "not found" instead.
    ///
    /// `pub` for the same reason as `resolve_spawn_tab_domain_no_wait`
    /// below: `async fn` call sites outside this crate that already did
    /// their own `await_domain_resolution_async` pre-wait (the mux server's
    /// startup, the `wezterm.mux` lua module) must not fall back to the
    /// blocking `resolve_default_domain`, or they reintroduce exactly the
    /// double-wait this was written to close.
    pub fn resolve_default_domain_no_wait(&self) -> anyhow::Result<Arc<dyn Domain>> {
        Ok(self.default_domain())
    }

    /// Wait-free variant of resolve_spawn_tab_domain for use after
    /// an async pre-wait (await_domain_resolution_async). Does not block,
    /// even if a brand new discovery run started in the gap - that's the
    /// correct trade-off to avoid blocking the executor a second time.
    /// Documented trade-off: if a genuinely new run would have found the
    /// requested domain, this may report "not found" instead.
    ///
    /// `pub` (not just crate-internal) so that other `async fn` call
    /// sites outside this crate that already do their own
    /// `await_domain_resolution_async` pre-wait (eg. the GUI's `--domain`
    /// startup handling in `wezterm-gui/src/main.rs`) can avoid the same
    /// double-wait/TOCTOU this was written to close for `split_pane`/
    /// `spawn_tab_or_window`.
    pub fn resolve_spawn_tab_domain_no_wait(
        &self,
        pane_id: Option<PaneId>,
        domain: &config::keyassignment::SpawnTabDomain,
    ) -> anyhow::Result<Arc<dyn Domain>> {
        let domain = match domain {
            SpawnTabDomain::DefaultDomain => self.resolve_default_domain_no_wait()?,
            SpawnTabDomain::CurrentPaneDomain => match pane_id {
                Some(pane_id) => {
                    let (pane_domain_id, _window_id, _tab_id) = self
                        .resolve_pane_id(pane_id)
                        .ok_or_else(|| anyhow!("pane_id {} invalid", pane_id))?;
                    self.get_domain(pane_domain_id)
                        .ok_or_else(|| anyhow!("domain id {} is invalid", pane_domain_id))?
                }
                None => self.resolve_default_domain_no_wait()?,
            },
            SpawnTabDomain::DomainId(domain_id) => self
                .get_domain(*domain_id)
                .ok_or_else(|| anyhow!("domain id {} is invalid", domain_id))?,
            SpawnTabDomain::DomainName(name) => {
                self.get_domain_by_name(&name).ok_or_else(|| {
                    let names: Vec<String> = self
                        .domains_by_name
                        .read()
                        .keys()
                        .map(|name| format!("\"{name}\""))
                        .collect();
                    anyhow!(
                        "domain name \"{name}\" is invalid. Possible names are {}.",
                        names.join(", ")
                    )
                })?
            }
        };
        Ok(domain)
    }

    /// Resolves the mux default domain, waiting for an in-progress
    /// background domain discovery first if (and only if) that discovery
    /// was started because the configured default domain name matched a
    /// domain that wasn't known yet (see `pending_default` on
    /// `DomainDiscoveryState`). This keeps the common case (default domain
    /// already known, or discovery irrelevant to it) from ever blocking.
    ///
    /// Note: For async contexts, use `await_domain_resolution_async` before
    /// calling this method to avoid double-waiting.
    ///
    /// **This blocks the calling thread**, potentially without bound, and
    /// so must never be called synchronously from the GUI/main thread --
    /// doing so reintroduces exactly the freeze that moving discovery off
    /// the startup path was meant to eliminate. Such call sites should
    /// `await_domain_resolution_async` and then use
    /// `resolve_default_domain_no_wait`. As of this writing no production
    /// call site outside this module calls this or
    /// `resolve_spawn_tab_domain`; both are retained for external
    /// consumers of the crate.
    pub fn resolve_default_domain(&self) -> Arc<dyn Domain> {
        let should_wait = matches!(
            &*self.domain_discovery.lock(),
            DomainDiscoveryState::InProgress {
                pending_default: Some(_),
                ..
            }
        );
        if should_wait {
            // The configured default domain names a domain that
            // discovery hasn't found yet: wait for discovery to finish
            // rather than a bounded timeout, since giving up early would
            // mean silently falling back to the wrong domain (or, in
            // wezterm-gui's case, failing startup outright) just because
            // discovery was slower than an arbitrary cutoff. See
            // `await_domain_discovery_unbounded`.
            self.await_domain_discovery_unbounded();
        }
        self.default_domain()
    }

    /// Resolves a spawn tab domain, waiting for in-progress discovery
    /// when needed. Callers in async contexts should use
    /// `await_domain_resolution_async` before calling this method to avoid
    /// double-waiting.
    ///
    /// **This blocks the calling thread** (bounded by
    /// `DOMAIN_DISCOVERY_TIMEOUT` for a named domain, unbounded for
    /// `DefaultDomain`), so the same GUI/main-thread caveat as
    /// `resolve_default_domain` applies: use
    /// `resolve_spawn_tab_domain_no_wait` there instead.
    pub fn resolve_spawn_tab_domain(
        &self,
        // TODO: disambiguate with TabId
        pane_id: Option<PaneId>,
        domain: &config::keyassignment::SpawnTabDomain,
    ) -> anyhow::Result<Arc<dyn Domain>> {
        let domain = match domain {
            SpawnTabDomain::DefaultDomain => self.resolve_default_domain(),
            SpawnTabDomain::CurrentPaneDomain => match pane_id {
                Some(pane_id) => {
                    let (pane_domain_id, _window_id, _tab_id) = self
                        .resolve_pane_id(pane_id)
                        .ok_or_else(|| anyhow!("pane_id {} invalid", pane_id))?;
                    self.get_domain(pane_domain_id)
                        .expect("resolve_pane_id to give valid domain_id")
                }
                None => self.resolve_default_domain(),
            },
            SpawnTabDomain::DomainId(domain_id) => self
                .get_domain(*domain_id)
                .ok_or_else(|| anyhow!("domain id {} is invalid", domain_id))?,
            SpawnTabDomain::DomainName(name) => {
                if self.get_domain_by_name(name).is_none() {
                    // Not found (yet): if a background domain discovery is
                    // in flight, it may still register this domain, so
                    // give it a bounded chance to do so before declaring
                    // the name invalid.
                    self.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
                }
                self.get_domain_by_name(&name).ok_or_else(|| {
                    let names: Vec<String> = self
                        .domains_by_name
                        .read()
                        .keys()
                        .map(|name| format!("\"{name}\""))
                        .collect();
                    anyhow!(
                        "domain name \"{name}\" is invalid. Possible names are {}.",
                        names.join(", ")
                    )
                })?
            }
        };
        Ok(domain)
    }

    fn resolve_cwd(
        &self,
        command_dir: Option<String>,
        pane: Option<Arc<dyn Pane>>,
        target_domain: DomainId,
        policy: CachePolicy,
    ) -> Option<String> {
        command_dir.or_else(|| {
            match pane {
                Some(pane) if pane.domain_id() == target_domain => pane
                    .get_current_working_dir(policy)
                    .and_then(|url| {
                        percent_decode_str(url.path())
                            .decode_utf8()
                            .ok()
                            .map(|path| path.into_owned())
                    })
                    .map(|path| {
                        // On Windows the file URI can produce a path like:
                        // `/C:\Users` which is valid in a file URI, but the leading slash
                        // is not liked by the windows file APIs, so we strip it off here.
                        let bytes = path.as_bytes();
                        if bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b':' {
                            path[1..].to_owned()
                        } else {
                            path
                        }
                    }),
                _ => None,
            }
        })
    }

    pub async fn split_pane(
        self: &Arc<Self>,
        // TODO: disambiguate with TabId
        pane_id: PaneId,
        request: SplitRequest,
        source: SplitSource,
        domain: config::keyassignment::SpawnTabDomain,
    ) -> anyhow::Result<(Arc<dyn Pane>, TerminalSize)> {
        let (_pane_domain_id, window_id, tab_id) = self
            .resolve_pane_id(pane_id)
            .ok_or_else(|| anyhow!("pane_id {} invalid", pane_id))?;

        // Waits asynchronously (without blocking this executor) for any
        // in-flight domain discovery that resolve_spawn_tab_domain below
        // would otherwise have to block this thread on synchronously.
        self.await_domain_resolution_async(Some(pane_id), &domain)
            .await;

        // Use wait-free variant after async pre-wait to avoid
        // blocking the executor a second time if a brand new discovery run
        // started in the gap. Trade-off: we may report "not found" for a
        // domain that a genuinely new run would have found, but that's
        // acceptable for not blocking the executor.
        let domain = self
            .resolve_spawn_tab_domain_no_wait(Some(pane_id), &domain)
            .context("resolve_spawn_tab_domain")?;

        if domain.state() == DomainState::Detached {
            domain.attach(Some(window_id)).await?;
        }

        let current_pane = self
            .get_pane(pane_id)
            .ok_or_else(|| anyhow!("pane_id {} is invalid", pane_id))?;
        let term_config = current_pane.get_config();

        let source = match source {
            SplitSource::Spawn {
                command,
                command_dir,
            } => SplitSource::Spawn {
                command,
                command_dir: self.resolve_cwd(
                    command_dir,
                    Some(Arc::clone(&current_pane)),
                    domain.domain_id(),
                    CachePolicy::FetchImmediate,
                ),
            },
            other => other,
        };

        let pane = domain.split_pane(source, tab_id, pane_id, request).await?;
        if let Some(config) = term_config {
            pane.set_config(config);
        }

        // FIXME: clipboard

        let dims = pane.get_dimensions();

        let size = TerminalSize {
            cols: dims.cols,
            rows: dims.viewport_rows,
            pixel_height: 0, // FIXME: split pane pixel dimensions
            pixel_width: 0,
            dpi: dims.dpi,
        };

        Ok((pane, size))
    }

    pub async fn move_pane_to_new_tab(
        &self,
        pane_id: PaneId,
        window_id: Option<WindowId>,
        workspace_for_new_window: Option<String>,
    ) -> anyhow::Result<(Arc<Tab>, WindowId)> {
        let (domain_id, _src_window, src_tab) = self
            .resolve_pane_id(pane_id)
            .ok_or_else(|| anyhow::anyhow!("pane {} not found", pane_id))?;

        let domain = self
            .get_domain(domain_id)
            .ok_or_else(|| anyhow::anyhow!("domain {domain_id} of pane {pane_id} not found"))?;

        if let Some((tab, window_id)) = domain
            .move_pane_to_new_tab(pane_id, window_id, workspace_for_new_window.clone())
            .await?
        {
            return Ok((tab, window_id));
        }

        let src_tab = match self.get_tab(src_tab) {
            Some(t) => t,
            None => anyhow::bail!("Invalid tab id {}", src_tab),
        };

        let window_builder;
        let (window_id, size) = if let Some(window_id) = window_id {
            let window = self
                .get_window_mut(window_id)
                .ok_or_else(|| anyhow!("window_id {} not found on this server", window_id))?;
            let tab = window
                .get_active()
                .ok_or_else(|| anyhow!("window {} has no tabs", window_id))?;
            let size = tab.get_size();

            (window_id, size)
        } else {
            window_builder = self.new_empty_window(workspace_for_new_window, None);
            (*window_builder, src_tab.get_size())
        };

        let pane = src_tab
            .remove_pane(pane_id)
            .ok_or_else(|| anyhow::anyhow!("pane {} wasn't in its containing tab!?", pane_id))?;

        let tab = Arc::new(Tab::new(&size));
        tab.assign_pane(&pane);
        pane.resize(size)?;
        self.add_tab_and_active_pane(&tab)?;
        self.add_tab_to_window(&tab, window_id)?;

        if src_tab.is_dead() {
            self.remove_tab(src_tab.tab_id());
        }

        Ok((tab, window_id))
    }

    pub async fn spawn_tab_or_window(
        self: &Arc<Self>,
        window_id: Option<WindowId>,
        domain: SpawnTabDomain,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
        size: TerminalSize,
        current_pane_id: Option<PaneId>,
        workspace_for_new_window: String,
        window_position: Option<GuiPosition>,
    ) -> anyhow::Result<(Arc<Tab>, Arc<dyn Pane>, WindowId)> {
        // Waits asynchronously (without blocking this executor) for any
        // in-flight domain discovery that resolve_spawn_tab_domain below
        // would otherwise have to block this thread on synchronously.
        self.await_domain_resolution_async(current_pane_id, &domain)
            .await;

        // Use wait-free variant after async pre-wait to avoid
        // blocking the executor a second time if a brand new discovery run
        // started in the gap. Trade-off: we may report "not found" for a
        // domain that a genuinely new run would have found, but that's
        // acceptable for not blocking the executor.
        let domain = self
            .resolve_spawn_tab_domain_no_wait(current_pane_id, &domain)
            .context("resolve_spawn_tab_domain")?;

        let window_builder;
        let term_config;

        let (window_id, size) = if let Some(window_id) = window_id {
            let window = self
                .get_window_mut(window_id)
                .ok_or_else(|| anyhow!("window_id {} not found on this server", window_id))?;
            let tab = window
                .get_active()
                .ok_or_else(|| anyhow!("window {} has no tabs", window_id))?;
            let pane = tab
                .get_active_pane()
                .ok_or_else(|| anyhow!("active tab in window {} has no panes", window_id))?;
            term_config = pane.get_config();

            let size = tab.get_size();

            (window_id, size)
        } else {
            term_config = None;
            window_builder = self.new_empty_window(Some(workspace_for_new_window), window_position);
            (*window_builder, size)
        };

        if domain.state() == DomainState::Detached {
            domain.attach(Some(window_id)).await?;
        }

        let cwd = self.resolve_cwd(
            command_dir,
            match current_pane_id {
                Some(id) => {
                    // Only use the cwd from the current pane if the domain
                    // is the same as the one we are spawning into
                    let (current_domain_id, _, _) = self
                        .resolve_pane_id(id)
                        .ok_or_else(|| anyhow!("pane_id {} invalid", id))?;
                    if current_domain_id == domain.domain_id() {
                        self.get_pane(id)
                    } else {
                        None
                    }
                }
                None => None,
            },
            domain.domain_id(),
            CachePolicy::FetchImmediate,
        );

        let tab = domain
            .spawn(size, command.clone(), cwd.clone(), window_id)
            .await
            .with_context(|| {
                format!(
                    "Spawning in domain `{}`: {size:?} command={command:?} cwd={cwd:?}",
                    domain.domain_name()
                )
            })?;

        let pane = tab
            .get_active_pane()
            .ok_or_else(|| anyhow!("missing active pane on tab!?"))?;

        if let Some(config) = term_config {
            pane.set_config(config);
        }

        // FIXME: clipboard?

        let mut window = self
            .get_window_mut(window_id)
            .ok_or_else(|| anyhow!("no such window!?"))?;
        if let Some(idx) = window.idx_by_id(tab.tab_id()) {
            window.save_and_then_set_active(idx);
        }

        Ok((tab, pane, window_id))
    }
}

pub struct IdentityHolder {
    prior: Option<Arc<ClientId>>,
}

impl Drop for IdentityHolder {
    fn drop(&mut self) {
        if let Some(mux) = Mux::try_get() {
            mux.replace_identity(self.prior.take());
        }
    }
}

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum SessionTerminated {
    #[error("Process exited: {:?}", status)]
    ProcessStatus { status: ExitStatus },
    #[error("Error: {:?}", err)]
    Error { err: Error },
    #[error("Window Closed")]
    WindowClosed,
}

pub(crate) fn terminal_size_to_pty_size(size: TerminalSize) -> anyhow::Result<PtySize> {
    Ok(PtySize {
        rows: size.rows.try_into()?,
        cols: size.cols.try_into()?,
        pixel_height: size.pixel_height.try_into()?,
        pixel_width: size.pixel_width.try_into()?,
    })
}

struct MuxClipboard {
    pane_id: PaneId,
}

impl Clipboard for MuxClipboard {
    fn set_contents(
        &self,
        selection: ClipboardSelection,
        clipboard: Option<String>,
    ) -> anyhow::Result<()> {
        let mux =
            Mux::try_get().ok_or_else(|| anyhow::anyhow!("MuxClipboard::set_contents: no Mux?"))?;
        mux.notify(MuxNotification::AssignClipboard {
            pane_id: self.pane_id,
            selection,
            clipboard,
        });
        Ok(())
    }
}

struct MuxDownloader {}

impl wezterm_term::DownloadHandler for MuxDownloader {
    fn save_to_downloads(&self, name: Option<String>, data: Vec<u8>) {
        if let Some(mux) = Mux::try_get() {
            mux.notify(MuxNotification::SaveToDownloads {
                name,
                data: Arc::new(data),
            });
        }
    }
}

#[cfg(test)]
mod domain_discovery_tests {
    use super::*;
    use domain::{alloc_domain_id, LocalDomain};

    /// A freestanding `Mux` (not registered via `Mux::set_mux`), so these
    /// tests don't share global state with each other or with anything
    /// else that might call `Mux::get()`.
    fn new_test_mux() -> Arc<Mux> {
        Arc::new(Mux::new(None))
    }

    /// A cheap domain to seed as the initial mux default in tests that
    /// need to distinguish "default changed because pending_default
    /// fired" from "default was auto-assigned by `add_domain` because
    /// there wasn't one yet" (`add_domain` assigns the first domain added
    /// as the default if none is set), and also to stand in for whatever
    /// `discover` closures in these tests pretend to have "discovered".
    fn dummy_domain(name: &str) -> Arc<dyn Domain> {
        Arc::new(LocalDomain::new(name).expect("LocalDomain::new is infallible in tests"))
    }

    /// Regression test for wezterm#7782: `begin_domain_discovery` must
    /// hand the (potentially slow, `wsl.exe`-shaped) `discover` closure off
    /// to a background thread and return immediately, rather than running
    /// it inline. `discover` here blocks on a channel that the test never
    /// closes until after the assertion, standing in for a `wsl.exe`
    /// invocation that hasn't returned yet: if `begin_domain_discovery`
    /// were still synchronous, this test would itself block past the
    /// assertion.
    #[test]
    fn begin_domain_discovery_does_not_block_caller() {
        let mux = new_test_mux();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        let start = Instant::now();
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![])
            },
            None,
        );
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "begin_domain_discovery must return immediately, not wait for `discover` to finish"
        );
        drop(gate_tx);
    }

    /// Regression test for the race that moving discovery off the startup
    /// path introduces: spawning directly into a domain by name
    /// while discovery is still in flight must wait for that discovery
    /// (bounded by DOMAIN_DISCOVERY_TIMEOUT) and succeed once it
    /// completes, instead of immediately failing with "domain name is
    /// invalid". Also covers that a `default_domain` naming a
    /// not-yet-discovered domain gets applied once discovery finds it.
    #[test]
    fn resolve_spawn_tab_domain_waits_for_in_flight_discovery() {
        // Seeded with a distinct default so that, below, `default_domain()`
        // reading back "Discovered" can only be explained by the
        // `pending_default` codepath actually firing -- not by
        // `add_domain`'s "first domain becomes the default" fallback,
        // which would make that assertion pass vacuously.
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                // Stands in for `wsl.exe -l -v` not having returned yet;
                // released by the test below once resolution has had a
                // chance to start waiting on it.
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("Discovered")])
            },
            Some("Discovered".to_string()),
        );

        // Keep the gate closed until this thread is about to resolve, so
        // that in the common case the resolution really does start while
        // discovery is still in flight -- which is the situation under
        // test. It is a bias, not a guarantee: nothing stops the releaser
        // from winning the handoff and letting discovery finish before
        // `resolve_spawn_tab_domain` is even entered. That only ever makes
        // this test *miss* a regression (a resolution that stopped waiting
        // would then still find the domain), never fail spuriously, so the
        // looseness is tolerable here; that a waiter really does block on
        // an unfinished discovery is pinned down deterministically by
        // `await_domain_discovery_respects_timeout` below, which holds its
        // gate shut for the whole of the wait it measures.
        let (start_tx, start_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = start_rx.recv();
            let _ = gate_tx.send(());
        });

        let start = Instant::now();
        let _ = start_tx.send(());
        let resolved = mux
            .resolve_spawn_tab_domain(None, &SpawnTabDomain::DomainName("Discovered".to_string()))
            .expect("domain should be found once discovery completes");
        let elapsed = start.elapsed();
        assert_eq!(resolved.domain_name(), "Discovered");
        assert!(
            elapsed < DOMAIN_DISCOVERY_TIMEOUT,
            "should resolve once discovery completes, not fall through to the timeout path"
        );

        assert_eq!(
            mux.default_domain().domain_name(),
            "Discovered",
            "pending_default should be applied once the named domain is discovered"
        );
    }

    /// `await_domain_discovery` must not wait past the requested timeout,
    /// even if discovery never completes (e.g. a genuinely hung `wsl.exe`).
    #[test]
    fn await_domain_discovery_respects_timeout() {
        let mux = new_test_mux();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                // Never released within this test, simulating a hung
                // `wsl.exe`; kept alive until the end of the test function
                // so `discover` stays blocked for the whole timeout window.
                let _ = gate_rx.recv();
                Ok(vec![])
            },
            None,
        );

        let start = Instant::now();
        mux.await_domain_discovery(Duration::from_millis(50));
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50),
            "must wait at least the requested timeout"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must not wait far past the requested timeout, got {:?}",
            elapsed
        );
        drop(gate_tx);
    }

    /// Regression test: `DomainDiscoveryWaitFuture` must resolve to `false` at (or shortly
    /// after) its deadline when driven by a *real* executor against a
    /// permanently-stuck discovery run -- not just when a test manually
    /// re-polls it in a loop (which `domain_discovery_wait_future_does_not_block_executor`
    /// already does, and so cannot catch a missing independent timer wake
    /// source). Uses `promise::spawn::block_on` (a real single-threaded
    /// executor) to prove the future itself wakes its task at the
    /// deadline, rather than relying on this test to keep polling it.
    #[test]
    fn domain_discovery_wait_future_bounded_wait_times_out_on_a_real_executor() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (_gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        // `_gate_tx` is deliberately never sent to: this discovery run is
        // permanently stuck, standing in for a hung `wsl.exe`.
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![])
            },
            None,
        );

        let future = mux.await_domain_discovery_async(Duration::from_millis(200));
        let start = Instant::now();
        let resolved = promise::spawn::block_on(future);
        let elapsed = start.elapsed();

        assert!(
            !resolved,
            "a bounded wait against a permanently-stuck discovery run must resolve to \
             false (timed out), not hang forever waiting only for discovery completion"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must resolve at or shortly after its own deadline when driven by a real \
             executor, not hang until this test's process exits; took {:?}",
            elapsed
        );
    }

    /// When no discovery is in flight at all (the common case: either
    /// domains were configured explicitly, or discovery already
    /// finished), resolving an unknown domain name must fail immediately,
    /// exactly as before this change.
    #[test]
    fn resolve_spawn_tab_domain_reports_missing_domain_immediately_without_discovery() {
        let mux = new_test_mux();

        let start = Instant::now();
        let err = mux
            .resolve_spawn_tab_domain(None, &SpawnTabDomain::DomainName("nope".to_string()))
            .err()
            .expect("unknown domain name must be an error");
        assert!(err.to_string().contains("nope"));
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    /// Regression test for a reload race: a config reload
    /// landing while auto-discovery is still in flight, whose new choice
    /// no longer wants a discovered default (`pending_default = None`),
    /// must clear whatever `pending_default` an earlier
    /// `begin_domain_discovery` call was going to apply -- not just ignore
    /// the update because it's `None`. Otherwise the stale value wins once
    /// the original discovery finishes, silently overriding the reload's
    /// choice.
    #[test]
    fn begin_domain_discovery_repeat_call_with_none_clears_stale_pending_default() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("A")])
            },
            Some("A".to_string()),
        );

        // Simulates a reload landing mid-discovery that no longer wants a
        // discovered default. Must not start a second discovery thread
        // (the closure below must never run) since one is already in
        // flight. Note: if it *did* run, `unreachable!` would panic on a
        // detached background thread, which the test harness wouldn't
        // observe as a failure by itself -- what actually catches a
        // regression here is `gate_tx.send(...).expect(...)` below: if a
        // second thread were spawned, both the (bad) second call
        // resetting state and this test's own gate handling would no
        // longer line up the way the assertions below expect, and/or the
        // `expect` would fail because the first thread's `discover`
        // never got a chance to consume this same `gate_rx`.
        mux.begin_domain_discovery(
            || unreachable!("must not start a second discovery thread while one is in flight"),
            None,
        );

        gate_tx
            .send(())
            .expect("discovery thread should still be waiting on the gate");
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert_eq!(
            mux.default_domain().domain_name(),
            "dummy-default",
            "the stale pending_default from the first call must not resurrect A"
        );
    }

    /// Regression test: a repeat
    /// `begin_domain_discovery` call landing while a run is already
    /// `InProgress` must refresh *both* `pending_default` and the "did the
    /// default change" baseline (`default_domain_id_at_start`) together --
    /// not just `pending_default`, leaving the baseline pinned to whichever
    /// call happened to be first to actually spawn the discovery thread.
    ///
    /// Scenario: discovery starts with default = "dummy-default"
    /// (baseline captured then). Before it finishes, something sets an
    /// intentional new default ("Explicit", eg. `wezterm connect`) and
    /// *then* a repeat call asks discovery to apply "Discovered" once
    /// found. From that repeat call's point of view, the default hasn't
    /// changed since *it* asked -- it was already "Explicit" -- so once
    /// discovery finds "Discovered" with nothing else having changed
    /// in between, it must be applied. A stale baseline from the
    /// original call would wrongly see "Explicit != dummy-default" as
    /// "the default changed after all" and skip applying it.
    #[test]
    fn begin_domain_discovery_repeat_call_refreshes_default_baseline() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("Discovered")])
            },
            None,
        );

        // An intentional default change lands while discovery is still in
        // flight -- eg. `wezterm connect`/`wezterm ssh` picking its own
        // domain. This must not be clobbered later.
        let explicit = dummy_domain("Explicit");
        mux.set_default_domain(&explicit);

        // A repeat call now asks discovery to apply "Discovered" once
        // found. Its baseline must be "Explicit" (the default *now*, at
        // this call), not "dummy-default" (the default when the first
        // call started the still-running thread).
        mux.begin_domain_discovery(
            || unreachable!("must not start a second discovery thread while one is in flight"),
            Some("Discovered".to_string()),
        );

        gate_tx
            .send(())
            .expect("discovery thread should still be waiting on the gate");
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert_eq!(
            mux.default_domain().domain_name(),
            "Discovered",
            "nothing changed the default between the repeat call and discovery \
             finishing, so pending_default must be applied -- a stale baseline \
             from the original call would wrongly see this as a default change \
             and skip it"
        );
    }

    /// Regression test for a reload race: switching to an
    /// explicitly configured domain list must invalidate any
    /// auto-discovery still running from before the reload, so it can't
    /// register extra domains or override the default the explicit
    /// config is about to set, once it eventually finishes running its
    /// (uninterruptible) `discover` closure.
    #[test]
    fn cancel_domain_discovery_unblocks_waiters_and_ignores_stale_results() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("Stale")])
            },
            Some("Stale".to_string()),
        );

        let start = Instant::now();
        mux.notify_cancelled_domain_discovery_waiters(mux.cancel_domain_discovery());

        // A waiter arriving right after cancellation (not one that was
        // already parked beforehand -- see
        // `cancel_domain_discovery_defers_sync_wakeup_until_notify_is_called`
        // for that case) must not block at all: `cancel_domain_discovery`
        // already flipped the state away from `InProgress` synchronously,
        // so a fresh `await_domain_discovery_unbounded` call sees that on
        // its very first check, before ever reaching the condvar wait --
        // this doesn't depend on `notify_cancelled_domain_discovery_waiters`
        // having been called, and this test deliberately never calls it.
        mux.await_domain_discovery_unbounded();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a waiter arriving after cancel_domain_discovery must not block"
        );

        // Let the now-stale thread finish and give it plenty of time to
        // run its (per-item generation-checked) loop and drop its finish
        // guard. Polls repeatedly instead of a single fixed sleep: this
        // fails immediately if the domain appears at any point in the
        // window (a single point-in-time check after a fixed delay could
        // still race and miss it), while a generous total window (rather
        // than a short single sleep) keeps this from flaking under
        // scheduler pressure on a loaded CI machine -- the tail work
        // after `discover()` returns is a couple of uncontended lock
        // operations, so it either shows up almost immediately or not at
        // all; this isn't waiting for something slow, just being patient
        // about scheduling jitter.
        gate_tx
            .send(())
            .expect("discovery thread should still be waiting on the gate");
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            assert!(
                mux.get_domain_by_name("Stale").is_none(),
                "a cancelled discovery must not register domains it finds after being cancelled"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            mux.default_domain().domain_name(),
            "dummy-default",
            "a cancelled discovery's pending_default must not be applied once it finishes late"
        );
    }

    /// Regression test: `cancel_domain_discovery` must
    /// not itself wake a waiter that was already parked *before* it ran --
    /// only `notify_cancelled_domain_discovery_waiters`, called once any
    /// replacement domains are registered, may do that. Otherwise a thread
    /// already blocked in `await_domain_discovery_unbounded` (eg. via
    /// `resolve_spawn_tab_domain`) could wake between a config reload's
    /// `cancel_domain_discovery()` call and its subsequent explicit-domain
    /// registration loop, observe discovery is no longer `InProgress`, and
    /// look up a domain that hasn't been registered yet -- the same
    /// intermediate-state race the cancel/notify split exists to close,
    /// just for the synchronous `Condvar` waiters instead of the async
    /// `Waker` ones.
    #[test]
    fn cancel_domain_discovery_defers_sync_wakeup_until_notify_is_called() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![])
            },
            None,
        );

        let waiter_mux = Arc::clone(&mux);
        let waiter_returned = Arc::new(AtomicBool::new(false));
        let waiter_returned2 = Arc::clone(&waiter_returned);
        let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
        let waiter = std::thread::spawn(move || {
            let _ = parked_tx.send(()); // Signal we're about to wait
            waiter_mux.await_domain_discovery_unbounded();
            waiter_returned2.store(true, Ordering::SeqCst);
        });

        // Wait for the waiter thread to signal it's about to park, then
        // for it to actually reach the condvar.
        //
        // "About to park" is not good enough on its own: if we cancelled
        // while the waiter was still between the signal and the condvar,
        // it would find the state already flipped away from `InProgress`,
        // return immediately, and fail the assertion below -- reporting a
        // bug that isn't there. So rather than sleeping a fixed 50ms and
        // hoping, hold the discovery lock ourselves and only release it
        // once the waiter is blocked *on that lock or on the condvar*. A
        // parked waiter is precisely one that is inside
        // `await_domain_discovery_unbounded` and therefore either holds or
        // is queued for this mutex, so once we observe contention here and
        // hand the lock over, cancellation cannot race ahead of it.
        let _ = parked_rx.recv();
        {
            let _hold = mux.domain_discovery.lock();
            // Give the waiter a chance to queue up behind this lock. Any
            // delay works: it changes only how long we wait for the
            // handshake, never whether the assertion is valid.
            std::thread::sleep(Duration::from_millis(50));
        }
        // In practice, parking_lot's eventual-fairness heuristic hands the lock
        // to the waiter here once our hold has outlasted its internal fairness
        // timeout (which our 50ms hold reliably does) -- but this is a `parking_lot`
        // implementation detail, not a guarantee: a barging lock can in principle
        // let this thread re-acquire the lock first. If this test ever becomes
        // flaky, that's the first thing to suspect; the fix would be an explicit
        // channel handshake signalled from inside `await_domain_discovery_unbounded`
        // immediately before it parks on the condvar, rather than lock-timing.
        let wakers = mux.cancel_domain_discovery();

        // The waiter must still be parked: cancel alone must not wake it.
        // Use a short poll loop rather than a single fixed sleep to reduce
        // flakiness -- we only need to verify it hasn't woken within a
        // reasonable window.
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            assert!(
                !waiter_returned.load(Ordering::SeqCst),
                "a waiter parked before cancel_domain_discovery must not wake until \
                 notify_cancelled_domain_discovery_waiters is called"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        mux.notify_cancelled_domain_discovery_waiters(wakers);
        waiter.join().expect("waiter thread must not panic");
        assert!(
            waiter_returned.load(Ordering::SeqCst),
            "notify_cancelled_domain_discovery_waiters must wake the previously-parked waiter"
        );

        let _ = gate_tx.send(());
    }

    /// A no-op `Waker` for manually driving a `Future::poll` from a test
    /// without a real executor. Never dereferences its data pointer (it's
    /// always null), so this is sound.
    fn test_waker() -> Waker {
        fn clone(_: *const ()) -> std::task::RawWaker {
            std::task::RawWaker::new(std::ptr::null(), &VTABLE)
        }
        fn noop(_: *const ()) {}
        static VTABLE: std::task::RawWakerVTable =
            std::task::RawWakerVTable::new(clone, noop, noop, noop);
        unsafe { Waker::from_raw(std::task::RawWaker::new(std::ptr::null(), &VTABLE)) }
    }

    /// Regression test for a lost-wakeup bug: `DomainDiscoveryWaitFuture`
    /// originally tracked "have I ever registered a waker" as a plain
    /// `bool`. If a run it had registered against got cancelled and a
    /// *new* run started before the future was polled again, the stale
    /// `true` flag made it skip registering with the new run's (separate,
    /// freshly-empty) `wakers` list -- leaving the future parked nowhere,
    /// pending forever. Registration must be tracked per-generation so a
    /// new run is always detected and re-registered against.
    #[test]
    fn domain_discovery_wait_future_reregisters_across_a_new_discovery_generation() {
        use std::future::Future;

        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx_a, gate_rx_a) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx_a.recv();
                Ok(vec![])
            },
            None,
        );

        let mut future = mux.await_domain_discovery_unbounded_async();
        let waker = test_waker();
        let mut cx = std::task::Context::from_waker(&waker);

        assert_eq!(
            std::pin::Pin::new(&mut future).poll(&mut cx),
            Poll::Pending,
            "must be pending while run A is in progress"
        );
        {
            let state = mux.domain_discovery.lock();
            match &*state {
                DomainDiscoveryState::InProgress { wakers, .. } => {
                    assert_eq!(
                        wakers.len(),
                        1,
                        "future should have registered exactly once against run A"
                    );
                }
                _ => panic!("expected InProgress after begin_domain_discovery"),
            }
        }

        // Cancel run A -- its drained wakers are discarded here rather
        // than published, standing in for a caller that's about to start
        // a fresh run instead of registering replacement domains.
        let _ = mux.cancel_domain_discovery();

        // Start a brand-new run B before the future is ever polled again.
        let (gate_tx_b, gate_rx_b) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx_b.recv();
                Ok(vec![])
            },
            None,
        );

        assert_eq!(
            std::pin::Pin::new(&mut future).poll(&mut cx),
            Poll::Pending,
            "must still be pending while run B is in progress"
        );
        {
            let state = mux.domain_discovery.lock();
            match &*state {
                DomainDiscoveryState::InProgress { wakers, .. } => {
                    assert_eq!(
                        wakers.len(),
                        1,
                        "future must re-register against run B's generation, not silently \
                         stay unregistered because it already registered against run A"
                    );
                }
                _ => panic!("expected InProgress after the second begin_domain_discovery"),
            }
        }

        // Let both gated discovery threads finish so they don't outlive
        // the test.
        let _ = gate_tx_a.send(());
        let _ = gate_tx_b.send(());
        mux.await_domain_discovery_unbounded();
    }

    /// Regression test: `cancel_domain_discovery`
    /// used to set `Done` unconditionally, which is indistinguishable from
    /// a discovery that actually completed successfully.
    /// `begin_domain_discovery`'s "nothing new to discover" shortcut for
    /// `Done` would then also fire after a cancel, so a config that starts
    /// with explicit `wsl_domains` (calling `cancel_domain_discovery` from
    /// `NotStarted` on every reload) would never run auto-discovery even
    /// after `wsl_domains` was later removed from the config, until the
    /// process restarted. Cancelling from `NotStarted` must be a no-op
    /// that leaves a later call free to actually discover.
    #[test]
    fn cancel_domain_discovery_from_not_started_does_not_prevent_a_later_real_discovery() {
        let mux = new_test_mux();

        mux.notify_cancelled_domain_discovery_waiters(mux.cancel_domain_discovery());

        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);
        mux.begin_domain_discovery(
            move || {
                ran2.store(true, Ordering::SeqCst);
                Ok(vec![dummy_domain("Real")])
            },
            None,
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            ran.load(Ordering::SeqCst),
            "a discovery cancelled from NotStarted must not block a later real discovery"
        );
        assert!(mux.get_domain_by_name("Real").is_some());
    }

    /// Regression test: cancelling a genuinely
    /// in-flight discovery must also leave a later call free to actually
    /// (re-)discover, not be silently absorbed into a fake `Done` the way
    /// it used to be.
    #[test]
    fn cancel_domain_discovery_in_flight_does_not_prevent_a_later_real_discovery() {
        let mux = new_test_mux();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("Cancelled")])
            },
            None,
        );
        mux.notify_cancelled_domain_discovery_waiters(mux.cancel_domain_discovery());

        // Let the now-stale thread finish and give it a generous window
        // to clean up before starting a new one, so this test is exercising
        // "a real discovery after a cancelled one finished late", not racing it.
        gate_tx
            .send(())
            .expect("discovery thread should still be waiting on the gate");
        // Give the cancelled thread time to run to completion. There is
        // deliberately nothing to poll for: a cancelled run publishes no
        // state at all -- it sees `Superseded` while registering, returns
        // without ever constructing its finish guard, and leaves
        // `NotStarted` exactly as `cancel_domain_discovery` left it. That
        // silence is the property under test, so a fixed window is the
        // only thing available. The assertions below don't depend on this
        // window having been long enough; it only decides whether the
        // stale thread finishes before or after the second run starts,
        // and both orders must work.
        std::thread::sleep(Duration::from_millis(200));

        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);
        mux.begin_domain_discovery(
            move || {
                ran2.store(true, Ordering::SeqCst);
                Ok(vec![dummy_domain("Real")])
            },
            None,
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            ran.load(Ordering::SeqCst),
            "cancelling an in-flight discovery must not block a later real discovery"
        );
        assert!(mux.get_domain_by_name("Real").is_some());
    }

    /// Regression test: a waiter that
    /// starts *while* `cancel_domain_discovery_and_replace` is running --
    /// after it has cancelled the stale run but before it has finished
    /// registering the replacement domain -- must not observe an
    /// intermediate "nothing in progress, nothing registered yet" state
    /// and report the domain missing. It must either block until the
    /// replacement is visible, or (if it happens to run after
    /// registration completes) find it immediately -- never a hard
    /// "not found" for a domain the new configuration genuinely lists.
    /// This is what `cancel_domain_discovery_and_replace` holding
    /// `domain_discovery`'s lock across the whole cancel -> register ->
    /// publish sequence is for.
    #[test]
    fn cancel_domain_discovery_and_replace_makes_new_waiters_see_replacement_domains() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));

        let (waiter_may_start_tx, waiter_may_start_rx) = std::sync::mpsc::channel::<()>();
        let (waiter_started_tx, waiter_started_rx) = std::sync::mpsc::channel::<()>();

        let replace_mux = Arc::clone(&mux);
        let replacer = std::thread::spawn(move || {
            replace_mux
                .cancel_domain_discovery_and_replace(|| {
                    // Let the waiter thread start its lookup now, while
                    // still inside this closure (ie. still holding
                    // domain_discovery's lock), then wait for confirmation
                    // it has actually issued the lookup call before
                    // finishing registration -- maximizing the chance it
                    // lands in the exact window this test is about.
                    let _ = waiter_may_start_tx.send(());
                    let _ = waiter_started_rx.recv();
                    let domain = dummy_domain("Explicit");
                    replace_mux.add_domain(&domain);
                    Ok(())
                })
                .expect("register_replacements must not fail in this test");
        });

        let _ = waiter_may_start_rx.recv();
        let waiter_mux = Arc::clone(&mux);
        let waiter = std::thread::spawn(move || {
            let _ = waiter_started_tx.send(());
            waiter_mux
                .resolve_spawn_tab_domain(None, &SpawnTabDomain::DomainName("Explicit".to_string()))
        });

        replacer.join().expect("replacer thread must not panic");
        let result = waiter.join().expect("waiter thread must not panic");

        assert_eq!(
            result
                .expect("Explicit must be found, not raced past an intermediate state")
                .domain_name(),
            "Explicit",
            "a waiter starting during cancel_domain_discovery_and_replace must see \
             the replacement domain once registered, not an intermediate state"
        );
    }

    /// Regression test: a bounded timeout on the
    /// default-domain resolution path incorrectly treats "discovery is
    /// still slow" as "the domain doesn't exist": `resolve_default_domain`
    /// must wait for an in-flight discovery to actually finish when it
    /// has a `pending_default` to apply, not give up after
    /// `DOMAIN_DISCOVERY_TIMEOUT` and silently keep the old default.
    #[test]
    fn resolve_default_domain_waits_unbounded_for_pending_default() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("Slow")])
            },
            Some("Slow".to_string()),
        );

        // Signal that we're about to start resolution. This ensures the
        // releaser thread doesn't open the gate until after we've had a
        // chance to actually start waiting.
        let (start_tx, start_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = start_rx.recv();
            std::thread::sleep(Duration::from_millis(80));
            let _ = gate_tx.send(());
        });

        let _ = start_tx.send(());
        let resolved = mux.resolve_default_domain();
        assert_eq!(resolved.domain_name(), "Slow");
    }

    /// Regression test: if the default domain
    /// changes (eg. a `wezterm connect`/`wezterm ssh` caller explicitly
    /// sets one) *after* discovery started but *before* it finishes, the
    /// finish guard applying `pending_default` must not clobber that
    /// intentional choice once discovery completes late.
    #[test]
    fn finish_guard_does_not_clobber_default_domain_changed_after_discovery_started() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("WSL:Ubuntu")])
            },
            Some("WSL:Ubuntu".to_string()),
        );

        // Simulates eg. `wezterm connect my-remote`, which sets its own
        // default domain after `begin_domain_discovery` was kicked off by
        // `update_mux_domains_impl` but before discovery finishes.
        let intentional_default = dummy_domain("my-remote");
        mux.add_domain(&intentional_default);
        mux.set_default_domain(&intentional_default);

        gate_tx
            .send(())
            .expect("discovery thread should still be waiting on the gate");
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert_eq!(
            mux.default_domain().domain_name(),
            "my-remote",
            "an intentional default domain change during discovery must survive discovery finishing"
        );
    }

    /// Regression test for panic safety: a
    /// `discover` closure that panics (eg. a malformed `wsl -l -v` output
    /// tripping an indexing panic in the real parser) must not strand the
    /// state machine in `InProgress` forever -- `FinishDomainDiscoveryOnDrop`
    /// finishes the run either way -- and a later discovery must be able to
    /// run normally afterwards. Note: the panicking thread prints its panic
    /// message to stderr via the default panic hook; that's expected
    /// noise from this test, not a failure.
    ///
    /// The panic is caught and reported as an error, so the resulting state
    /// is `NotStarted` (retryable), *not* `Done`: a run that blew up hasn't
    /// discovered anything, and treating it as completed would let
    /// `begin_domain_discovery`'s already-completed shortcut skip the real
    /// work later. This test asserts that state explicitly, because both
    /// `Done` and `NotStarted` satisfy every other assertion here and an
    /// implicit check would silently pass on either.
    #[test]
    fn begin_domain_discovery_panicking_discover_still_finishes_state() {
        let mux = new_test_mux();

        mux.begin_domain_discovery(|| panic!("simulated wsl.exe / parser panic"), None);
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::NotStarted
            ),
            "a panicking discover must finish the run as retryable NotStarted, \
             neither stranded InProgress nor falsely Done"
        );

        let start = Instant::now();
        let err = mux
            .resolve_spawn_tab_domain(None, &SpawnTabDomain::DomainName("nope".to_string()))
            .err()
            .expect("unknown domain name must still be an error");
        assert!(err.to_string().contains("nope"));
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "the run must be finished after the panic, not still InProgress"
        );

        // A `pending_default` naming a domain not yet known forces a real
        // discovery run rather than the `Done`-with-nothing-to-do
        // shortcut (see begin_domain_discovery_repeat_call_after_done_
        // with_no_new_work_skips_thread below), so this proves discovery
        // genuinely still works, not just that the state machine reports
        // `Done`.
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);
        mux.begin_domain_discovery(
            move || {
                ran2.store(true, Ordering::SeqCst);
                Ok(vec![dummy_domain("AfterPanic")])
            },
            Some("AfterPanic".to_string()),
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            ran.load(Ordering::SeqCst),
            "discovery must be able to run again after a prior panic"
        );
    }

    /// A `Domain` whose `domain_name()` panics, to exercise the
    /// registration loop's own panic path. `add_discovered_domain` calls
    /// `domain_name()` while inserting into `domains_by_name`, so this
    /// panics from *inside* the loop rather than from `discover()`.
    #[derive(Debug)]
    struct PanicOnNameDomain {
        id: DomainId,
    }

    #[async_trait::async_trait(?Send)]
    impl Domain for PanicOnNameDomain {
        async fn spawn_pane(
            &self,
            _size: TerminalSize,
            _command: Option<CommandBuilder>,
            _command_dir: Option<String>,
        ) -> anyhow::Result<Arc<dyn Pane>> {
            anyhow::bail!("not used in this test")
        }
        fn detachable(&self) -> bool {
            false
        }
        fn domain_id(&self) -> DomainId {
            self.id
        }
        fn domain_name(&self) -> &str {
            panic!("simulated panic from a Domain trait impl during registration")
        }
        async fn attach(&self, _window_id: Option<WindowId>) -> anyhow::Result<()> {
            Ok(())
        }
        fn detach(&self) -> anyhow::Result<()> {
            anyhow::bail!("not used in this test")
        }
        fn state(&self) -> DomainState {
            DomainState::Attached
        }
    }

    /// Regression test: the `catch_unwind` in
    /// `begin_domain_discovery_with_default_filter` originally wrapped only
    /// the `discover()` closure, while the `add_discovered_domain`
    /// registration loop ran after the finish guard had already been
    /// constructed holding `Ok(..)`. A panic out of any `Domain` method
    /// reached from that loop therefore unwound with the guard believing
    /// the run had succeeded, publishing `Done` despite the registration
    /// being only partial -- and a later reload carrying
    /// `pending_default: None` would then take the `Done` shortcut and
    /// never retry discovery again. Registration must fail the run to
    /// retryable `NotStarted` instead.
    #[test]
    fn panic_while_registering_discovered_domains_stays_retryable() {
        let mux = new_test_mux();

        mux.begin_domain_discovery(
            || {
                Ok(vec![Arc::new(PanicOnNameDomain {
                    id: alloc_domain_id(),
                }) as Arc<dyn Domain>])
            },
            None,
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::NotStarted
            ),
            "a panic during registration must finish the run as retryable NotStarted, \
             not as Done -- Done would make a later reload skip discovery entirely"
        );

        // Proves the state really is retryable, not merely labelled so:
        // with `pending_default: None` a `Done` state takes the shortcut
        // and never invokes the closure at all.
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);
        mux.begin_domain_discovery(
            move || {
                ran2.store(true, Ordering::SeqCst);
                Ok(vec![dummy_domain("AfterRegistrationPanic")])
            },
            None,
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            ran.load(Ordering::SeqCst),
            "discovery must run again after a registration panic"
        );
        assert!(mux.get_domain_by_name("AfterRegistrationPanic").is_some());
    }

    /// Regression test: `cancel_domain_discovery_and_replace`
    /// drains the async wakers out of the state *before* running the
    /// caller-supplied `register_replacements` closure under the lock. If
    /// that closure panics, the notify/wake calls that used to sit after it
    /// were skipped and the drained wakers were simply dropped, leaving any
    /// async waiter parked with no registration and nothing able to wake it
    /// -- `Pending` forever. The publish half must happen on the unwind
    /// path too.
    #[test]
    fn panicking_replacement_closure_still_wakes_async_waiters() {
        let mux = new_test_mux();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![])
            },
            None,
        );

        // Register a waker against the in-flight run, standing in for an
        // async waiter parked in `DomainDiscoveryWaitFuture`.
        let woken = Arc::new(AtomicBool::new(false));
        {
            let mut state = mux.domain_discovery.lock();
            match &mut *state {
                DomainDiscoveryState::InProgress {
                    wakers,
                    next_waiter_id,
                    ..
                } => {
                    wakers.insert(*next_waiter_id, tracking_waker(Arc::clone(&woken)));
                    *next_waiter_id += 1;
                }
                _ => panic!("discovery should still be in flight behind the gate"),
            }
        }

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            mux.cancel_domain_discovery_and_replace(|| {
                panic!("simulated panic from a caller-supplied replacement closure")
            })
        }));
        assert!(
            panicked.is_err(),
            "the closure's panic must still propagate to the caller"
        );

        assert!(
            woken.load(Ordering::SeqCst),
            "wakers drained before the panicking closure must still be woken, or the \
             async waiters they belong to stay Pending forever"
        );
        // The lock must not have been left held either: parking_lot doesn't
        // poison, and the guard is released on unwind, so this must not hang.
        assert!(matches!(
            &*mux.domain_discovery.lock(),
            DomainDiscoveryState::NotStarted
        ));

        let _ = gate_tx.send(());
    }

    /// Regression test: once a discovery run
    /// has completed and there is nothing new to discover (no
    /// `pending_default`, or one that's already registered), a repeat
    /// call must not spawn another discovery thread at all --
    /// `update_mux_domains_impl` calls this on every config reload, and
    /// wezterm reloads config on every file save, so spawning a fresh OS
    /// thread each time just to re-derive the same (cached) answer would
    /// add up.
    #[test]
    fn begin_domain_discovery_after_done_skips_redundant_thread_when_nothing_new() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));

        // First run: completes with a domain registered, nothing pending.
        mux.begin_domain_discovery(|| Ok(vec![dummy_domain("Known")]), None);
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        // pending_default = None: nothing to discover or apply, so this
        // must return synchronously without spawning a thread at all.
        // Uses a flag rather than `unreachable!` in the closure: a panic
        // on a wrongly-spawned *detached* thread wouldn't fail the test
        // by itself, whereas checking the flag directly is a definitive
        // "was this closure ever invoked" signal.
        //
        // "No thread was spawned" is asserted on the state machine rather
        // than on a timing window: `begin_domain_discovery` publishes
        // `InProgress` *before* spawning, and only ever from the state
        // lock, so a state that is still `Done` the moment the call
        // returns is proof no run was started -- no sleeping, no
        // scheduler-dependent grace period, and no way for a slow-but-real
        // spawn to sneak past. The closure flag is then just a
        // belt-and-braces cross-check with a short window.
        let ran_first = Arc::new(AtomicBool::new(false));
        let ran_first2 = Arc::clone(&ran_first);
        let start = Instant::now();
        mux.begin_domain_discovery(
            move || {
                ran_first2.store(true, Ordering::SeqCst);
                Ok(vec![])
            },
            None,
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "must return synchronously when there is nothing new to discover"
        );
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::Done { .. }
            ),
            "must not start a run when there is nothing new to discover"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !ran_first.load(Ordering::SeqCst),
            "must not spawn a thread when there is nothing new to discover"
        );

        // pending_default names a domain already known from the first
        // run: must apply it synchronously too, no thread spawned.
        let ran_second = Arc::new(AtomicBool::new(false));
        let ran_second2 = Arc::clone(&ran_second);
        let start = Instant::now();
        mux.begin_domain_discovery(
            move || {
                ran_second2.store(true, Ordering::SeqCst);
                Ok(vec![])
            },
            Some("Known".to_string()),
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::Done { .. }
            ),
            "must not start a run when the pending_default is already known"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !ran_second.load(Ordering::SeqCst),
            "must not spawn a thread when the pending_default is already known"
        );
        assert_eq!(mux.default_domain().domain_name(), "Known");
    }

    /// A `Waker` that records whether `wake`/`wake_by_ref` was ever called,
    /// via the `Arc<AtomicBool>` it's constructed from. Unlike `test_waker`
    /// (below), this one is actually observable, for tests that need to
    /// prove a waker was really woken rather than just drive a `poll` by
    /// hand.
    fn tracking_waker(woken: Arc<AtomicBool>) -> Waker {
        fn clone(data: *const ()) -> std::task::RawWaker {
            let arc = unsafe { Arc::from_raw(data as *const AtomicBool) };
            let cloned = Arc::into_raw(Arc::clone(&arc)) as *const ();
            std::mem::forget(arc);
            std::task::RawWaker::new(cloned, &VTABLE)
        }
        fn wake(data: *const ()) {
            let arc = unsafe { Arc::from_raw(data as *const AtomicBool) };
            arc.store(true, Ordering::SeqCst);
        }
        fn wake_by_ref(data: *const ()) {
            let arc = unsafe { Arc::from_raw(data as *const AtomicBool) };
            arc.store(true, Ordering::SeqCst);
            std::mem::forget(arc);
        }
        fn drop_fn(data: *const ()) {
            unsafe { drop(Arc::from_raw(data as *const AtomicBool)) };
        }
        static VTABLE: std::task::RawWakerVTable =
            std::task::RawWakerVTable::new(clone, wake, wake_by_ref, drop_fn);
        let ptr = Arc::into_raw(woken) as *const ();
        unsafe { Waker::from_raw(std::task::RawWaker::new(ptr, &VTABLE)) }
    }

    /// Regression test: a discovery run
    /// that never returns at all (eg. a permanently hung `wsl.exe`, not
    /// just a slow one) must not leave `InProgress` stuck for the rest of
    /// the process's life. A later `begin_domain_discovery` call -- eg.
    /// from a config reload -- must notice the run has been `InProgress`
    /// for longer than `DOMAIN_DISCOVERY_STUCK_THRESHOLD` and supersede it
    /// with a fresh run instead of the usual no-op "refresh the fields and
    /// return", waking anything parked on the stale run so it re-checks
    /// against the new one instead of a generation nothing will ever
    /// finish.
    #[test]
    fn begin_domain_discovery_supersedes_a_permanently_stuck_run() {
        let mux = new_test_mux();

        // Installs a stuck `InProgress` state directly rather than
        // spawning a thread that really never returns (nothing in this
        // test could ever unblock it): a `started_at` already past
        // `DOMAIN_DISCOVERY_STUCK_THRESHOLD` is exactly the state a
        // genuinely hung `wsl.exe` would eventually be observed in by a
        // later caller. A waker is registered too, standing in for an
        // async caller already parked on this run, to prove it gets woken
        // rather than left registered forever against a generation
        // nothing will ever finish.
        let woken = Arc::new(AtomicBool::new(false));
        let stuck_generation = {
            let mut state = mux.domain_discovery.lock();
            let generation = mux
                .next_domain_discovery_generation
                .fetch_add(1, Ordering::SeqCst);
            let mut wakers = HashMap::new();
            wakers.insert(0, tracking_waker(Arc::clone(&woken)));
            *state = DomainDiscoveryState::InProgress {
                generation,
                pending_default: None,
                default_domain_id_at_start: None,
                wakers,
                next_waiter_id: 1,
                accepts_as_default: Arc::new(|_| true),
                // `checked_sub` rather than plain `-`, matching
                // `config::wsl`'s `abandoned_discovery_triggers_new_attempt`:
                // subtracting from `Instant::now()` panics on underflow if
                // the monotonic clock hasn't reached this far yet (a host
                // less than ~91s past whatever it counts from, eg. a CI
                // runner or container started moments ago). Unlike that
                // test's fallback, this one degrades to a *failed
                // assertion* rather than a silently-weaker check -- a
                // non-backdated `started_at` reads as a healthy in-flight
                // run, so `begin_domain_discovery` would take its normal
                // refresh-and-return path and the assertions below would
                // catch it. That's the right failure mode: visibly wrong
                // for an inapplicable environment, rather than a confusing
                // panic from inside a struct literal.
                started_at: Instant::now()
                    .checked_sub(DOMAIN_DISCOVERY_STUCK_THRESHOLD + Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            };
            generation
        };

        mux.begin_domain_discovery(|| Ok(vec![dummy_domain("Fresh")]), None);
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert!(
            mux.get_domain_by_name("Fresh").is_some(),
            "a repeat call against a stuck run must actually start and finish a fresh \
             discovery, not treat it as a normal in-flight run and no-op"
        );
        assert!(
            woken.load(Ordering::SeqCst),
            "a waker registered against the stuck run's generation must be woken so it \
             re-polls and re-registers against the fresh run, instead of being left \
             registered against a generation nothing will ever finish"
        );
        let finished_generation = match &*mux.domain_discovery.lock() {
            DomainDiscoveryState::Done { _generation, .. } => *_generation,
            DomainDiscoveryState::NotStarted => {
                panic!("expected Done after the fresh run finishes, got NotStarted")
            }
            DomainDiscoveryState::InProgress { .. } => {
                panic!("expected Done after the fresh run finishes, got InProgress")
            }
        };
        assert_ne!(
            finished_generation, stuck_generation,
            "the fresh run must publish under a newer generation than the one it superseded"
        );
    }

    /// Drives the "slow run finishes after the run that replaced it already
    /// failed" sequence deterministically, and returns the mux so each caller
    /// can assert on the outcome it cares about.
    ///
    /// The ordering is forced with channels rather than real timing: run A is
    /// installed as an already-stuck `InProgress` (so `begin_domain_discovery`
    /// supersedes it without waiting out `DOMAIN_DISCOVERY_STUCK_THRESHOLD`),
    /// A's thread is then released only *after* the replacement run B has
    /// failed, and A returns success. `cancel` runs, if provided, between B
    /// failing and A finishing -- standing in for a config reload switching to
    /// an explicit `wsl_domains` list in exactly that window.
    ///
    /// `a_pending_default` is what run A was originally asked to apply;
    /// `b_pending_default` is what the reload that superseded it asks for,
    /// ie. the config as it stands by the time anything actually gets
    /// applied. Passing different values models a reload changing or
    /// dropping `default_domain` while A was stuck.
    fn run_late_success_after_replacement_failed(
        cancel: Option<Box<dyn FnOnce(&Arc<Mux>)>>,
        a_pending_default: Option<&str>,
        b_pending_default: Option<&str>,
    ) -> Arc<Mux> {
        let mux = new_test_mux();
        // Seed a default so the "is there one at all" fallback in
        // `register_discovered_domain_locked` can't be mistaken for
        // `pending_default` having been applied.
        let seeded_default = dummy_domain("seeded-default");
        mux.add_domain(&seeded_default);
        mux.set_default_domain(&seeded_default);

        // Run A: a real thread, holding a generation that is about to be
        // superseded, blocked until we release it.
        let (release_a_tx, release_a_rx) = std::sync::mpsc::channel::<()>();
        let (a_done_tx, a_done_rx) = std::sync::mpsc::channel::<()>();
        {
            let mut state = mux.domain_discovery.lock();
            let generation = mux
                .next_domain_discovery_generation
                .fetch_add(1, Ordering::SeqCst);
            let default_domain_id_at_start = Some(seeded_default.domain_id());
            *state = DomainDiscoveryState::InProgress {
                generation,
                pending_default: a_pending_default.map(|s| s.to_string()),
                default_domain_id_at_start,
                wakers: HashMap::new(),
                next_waiter_id: 0,
                accepts_as_default: Arc::new(|_| true),
                started_at: Instant::now()
                    .checked_sub(DOMAIN_DISCOVERY_STUCK_THRESHOLD + Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            };
            // What a real `begin_*` call would have recorded when it
            // published run A. Run B's `begin_domain_discovery` below
            // overwrites this, which is the point: adoption must apply
            // what was asked for *last*, not what A was spawned with.
            *mux.domain_discovery_latest_default.lock() = Some(DomainDiscoveryDefaultRequest {
                pending_default: a_pending_default.map(|s| s.to_string()),
                default_domain_id_at_start,
                accepts_as_default: Arc::new(|_| true),
            });

            // Mirrors what `begin_domain_discovery`'s spawned thread does once
            // its `discover` closure returns: try to register, and on
            // `Superseded` offer the result up for adoption.
            let mux_a = Arc::clone(&mux);
            let cancel_epoch_at_start = mux.domain_discovery_cancel_epoch.load(Ordering::SeqCst);
            std::thread::spawn(move || {
                let _ = release_a_rx.recv();
                let domains = vec![dummy_domain("LateSuccess")];
                let superseded = domains.iter().any(|domain| {
                    matches!(
                        mux_a.add_discovered_domain(generation, domain),
                        AddDiscoveredDomainOutcome::Superseded
                    )
                });
                if superseded {
                    mux_a.adopt_orphaned_discovery(cancel_epoch_at_start, &domains);
                }
                let _ = a_done_tx.send(());
            });
        }

        // Run B supersedes the stuck A and fails outright, carrying the
        // pending default of the config as it stands at *its* reload.
        mux.begin_domain_discovery(
            || Err(anyhow!("simulated wsl.exe failure")),
            b_pending_default.map(|s| s.to_string()),
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::NotStarted
            ),
            "a failed replacement run must leave the state retryable"
        );

        if let Some(cancel) = cancel {
            cancel(&mux);
        }

        // Only now does A finish.
        let _ = release_a_tx.send(());
        a_done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("run A must finish");

        mux
    }

    /// Regression test: a slow run that succeeds only
    /// after the run which superseded it has already failed must not leave the
    /// config-level cache populated with nothing registered against it.
    ///
    /// The cache write happens inside the `discover` closure, before the run
    /// learns it was superseded, and no generation check governs it -- so
    /// dropping the mux half on the floor leaves the two describing different
    /// sets of distributions. Since pane spawn resolves a domain's live
    /// settings against that cache, a domain present in only one of them is
    /// how a command ends up running on the Windows host instead of inside
    /// WSL.
    #[test]
    fn late_success_after_replacement_failed_is_adopted() {
        let mux = run_late_success_after_replacement_failed(None, None, None);

        assert!(
            mux.get_domain_by_name("LateSuccess").is_some(),
            "the late result must be adopted: nothing else is going to register it, and \
             its list is already in the config-level cache"
        );
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::Done { .. }
            ),
            "adopting must finish the run, so a later reload takes the Done shortcut \
             instead of rediscovering what is already registered"
        );
    }

    /// Regression test: adopting a late result must also apply the
    /// `pending_default` that run was carrying.
    ///
    /// `update_mux_domains_impl` already tried to apply the configured
    /// `default_domain` before this domain existed, and found nothing; if
    /// adoption registers the domain without also applying it, an explicitly
    /// configured `default_domain` is silently ignored -- new tabs keep
    /// opening in whatever the previous default was -- until some later reload
    /// happens along.
    #[test]
    fn adopting_a_late_result_applies_its_pending_default() {
        let mux = run_late_success_after_replacement_failed(
            None,
            Some("LateSuccess"),
            Some("LateSuccess"),
        );

        assert!(mux.get_domain_by_name("LateSuccess").is_some());
        assert_eq!(
            mux.default_domain().domain_name(),
            "LateSuccess",
            "the configured default named the domain only this run could supply, and \
             nothing changed the default in the meantime, so adoption must apply it"
        );
    }

    /// Adoption must apply the default as *last* requested, not as requested
    /// when the adopting run was spawned. Here the reload that superseded run
    /// A dropped `default_domain` from the config (run B carries `None`), so
    /// even though A was originally asked to apply "LateSuccess" -- and is
    /// the run that finally supplies that domain -- adopting its result must
    /// register the domain without touching the default. Before discovery
    /// went asynchronous, a `default_domain` removed from the config could
    /// never be applied again under any circumstances; applying A's
    /// spawn-time copy would quietly break that, and no later reload could
    /// undo it (a config with no `default_domain` has nothing to apply).
    #[test]
    fn adopting_a_late_result_does_not_apply_a_default_the_config_dropped() {
        let mux = run_late_success_after_replacement_failed(None, Some("LateSuccess"), None);

        assert!(
            mux.get_domain_by_name("LateSuccess").is_some(),
            "the domains themselves must still be adopted; only the stale default request \
             is dropped"
        );
        assert_eq!(
            mux.default_domain().domain_name(),
            "seeded-default",
            "the reload that superseded run A no longer asks for a default, so adoption \
             must not apply the one A was spawned with"
        );
    }

    /// The other half of the contract: adoption must *not* happen when the
    /// reason there is no live run is that the config switched to an explicit
    /// `wsl_domains` list. `NotStarted` alone cannot tell that apart from "the
    /// replacement run failed" -- both produce it -- so this pins the
    /// cancellation-epoch check that does.
    ///
    /// Getting this wrong resurrects auto-discovered domains that the user's
    /// new config deliberately dropped.
    #[test]
    fn late_success_after_cancellation_is_not_adopted() {
        let mux = run_late_success_after_replacement_failed(
            Some(Box::new(|mux: &Arc<Mux>| {
                mux.cancel_domain_discovery_and_replace(|| Ok(()))
                    .expect("cancellation must succeed");
            })),
            None,
            None,
        );

        assert!(
            mux.get_domain_by_name("LateSuccess").is_none(),
            "a cancelled discovery's late result must be discarded: this config asked for \
             an explicit domain list and no longer wants discovered ones"
        );
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::NotStarted
            ),
            "a discarded adoption must not move the state either"
        );
    }

    /// Regression test: a `Done` run that failed
    /// to find a domain named by `pending_default` must remember that name
    /// and skip spawning a new thread on repeat calls for the same name,
    /// rather than re-running discovery just to re-derive the same "not
    /// found" answer. A *different* name must still spawn a fresh run.
    #[test]
    fn begin_domain_discovery_with_not_found_pending_default_skips_redundant_repeats() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));

        // First run: completes with no domains, pending_default="NeverFound".
        let ran_first = Arc::new(AtomicBool::new(false));
        let ran_first2 = Arc::clone(&ran_first);
        mux.begin_domain_discovery(
            move || {
                ran_first2.store(true, Ordering::SeqCst);
                Ok(vec![]) // No domains registered
            },
            Some("NeverFound".to_string()),
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            ran_first.load(Ordering::SeqCst),
            "first run must have executed"
        );
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::Done {
                    not_found_pending_default,
                    ..
                } if not_found_pending_default.as_deref() == Some("NeverFound")
            ),
            "Done state must remember the not-found pending_default name"
        );

        // Repeat call with the same name: must NOT spawn a new thread.
        let ran_second = Arc::new(AtomicBool::new(false));
        let ran_second2 = Arc::clone(&ran_second);
        let start = Instant::now();
        mux.begin_domain_discovery(
            move || {
                ran_second2.store(true, Ordering::SeqCst);
                Ok(vec![])
            },
            Some("NeverFound".to_string()),
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "must return synchronously when same not-found name is requested again"
        );
        assert!(
            matches!(
                &*mux.domain_discovery.lock(),
                DomainDiscoveryState::Done { .. }
            ),
            "must not start a run when same not-found name is requested again"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !ran_second.load(Ordering::SeqCst),
            "must not spawn a thread when same not-found name is requested again"
        );

        // Different name: MUST spawn a fresh run (this is the "genuinely new
        // name" case the comment describes).
        let ran_third = Arc::new(AtomicBool::new(false));
        let ran_third2 = Arc::clone(&ran_third);
        mux.begin_domain_discovery(
            move || {
                ran_third2.store(true, Ordering::SeqCst);
                Ok(vec![])
            },
            Some("DifferentNeverFound".to_string()),
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            ran_third.load(Ordering::SeqCst),
            "must spawn a fresh run for a different never-found name"
        );
    }

    /// Regression test: decides the right wait
    /// flavor for each `SpawnTabDomain` shape without needing the async
    /// machinery (`promise::spawn` requires a configured global scheduler
    /// to actually drive a `Task` to completion, which isn't set up in
    /// this test binary). This is what keeps `split_pane`/
    /// `spawn_tab_or_window`'s async pre-await from either over-waiting
    /// (blocking on discovery unnecessarily for the common case, right
    /// back to what this whole fix is for) or under-waiting (still
    /// racing discovery for the cases `resolve_spawn_tab_domain`/
    /// `resolve_default_domain` themselves wait for).
    #[test]
    fn domain_resolution_wait_kind_matches_resolve_methods() {
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));

        // No discovery in flight at all: never wait, regardless of target.
        assert_eq!(
            mux.domain_resolution_wait_kind(None, &SpawnTabDomain::DefaultDomain),
            DomainResolutionWaitKind::None
        );
        assert_eq!(
            mux.domain_resolution_wait_kind(
                None,
                &SpawnTabDomain::DomainName("Unregistered".to_string())
            ),
            DomainResolutionWaitKind::None,
            "no discovery in flight, so nothing could still register this name"
        );
        assert_eq!(
            mux.domain_resolution_wait_kind(Some(1), &SpawnTabDomain::CurrentPaneDomain),
            DomainResolutionWaitKind::None,
            "CurrentPaneDomain with a pane_id never goes through discovery at all"
        );
        assert_eq!(
            mux.domain_resolution_wait_kind(None, &SpawnTabDomain::CurrentPaneDomain),
            DomainResolutionWaitKind::None,
            "no pending_default outstanding, so nothing to wait for even though pane_id is None"
        );

        // Start a discovery with a pending_default: DefaultDomain / a
        // pane-less CurrentPaneDomain must wait unbounded (matching
        // resolve_default_domain); an explicit name lookup still only
        // waits bounded (matching resolve_spawn_tab_domain), even for
        // the same name as pending_default.
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![])
            },
            Some("WSL:Ubuntu".to_string()),
        );

        assert_eq!(
            mux.domain_resolution_wait_kind(None, &SpawnTabDomain::DefaultDomain),
            DomainResolutionWaitKind::Unbounded
        );
        assert_eq!(
            mux.domain_resolution_wait_kind(None, &SpawnTabDomain::CurrentPaneDomain),
            DomainResolutionWaitKind::Unbounded
        );
        assert_eq!(
            mux.domain_resolution_wait_kind(Some(1), &SpawnTabDomain::CurrentPaneDomain),
            DomainResolutionWaitKind::None,
            "a concrete pane_id resolves via resolve_pane_id, never touching pending_default"
        );
        assert_eq!(
            mux.domain_resolution_wait_kind(
                None,
                &SpawnTabDomain::DomainName("WSL:Ubuntu".to_string())
            ),
            DomainResolutionWaitKind::Bounded,
            "an explicit name lookup always uses the bounded wait, even for the same name \
             as pending_default"
        );
        drop(gate_tx);
    }

    /// Test that `add_domain` unconditionally registers domains, even when
    /// they share the same name. The mux model does not guarantee globally-unique
    /// domain names -- only `DomainId`s are unique. This matches the real-world
    /// pattern used by `TermWizTerminalDomain` (always name "TermWizTerminalDomain")
    /// and `TmuxDomain` (always name "tmux"), where multiple domain instances with
    /// the same constant name but different `DomainId`s are created and must all
    /// be reachable via `get_domain(id)`.
    #[test]
    fn add_domain_allows_same_name_different_ids() {
        let mux = new_test_mux();
        let first = dummy_domain("Dup");
        let second = dummy_domain("Dup");
        assert_ne!(first.domain_id(), second.domain_id());

        mux.add_domain(&first);
        mux.add_domain(&second);

        // Both domains should be reachable by their unique IDs.
        let resolved_first = mux
            .get_domain(first.domain_id())
            .expect("first domain should be registered by ID");
        let resolved_second = mux
            .get_domain(second.domain_id())
            .expect("second domain should be registered by ID");
        assert_eq!(resolved_first.domain_id(), first.domain_id());
        assert_eq!(resolved_second.domain_id(), second.domain_id());

        // `get_domain_by_name` returns one of them (the most recently
        // inserted, which is the second in this case).
        let resolved_by_name = mux
            .get_domain_by_name("Dup")
            .expect("domain should be registered by name");
        assert_eq!(
            resolved_by_name.domain_id(),
            second.domain_id(),
            "get_domain_by_name returns the most recent insertion for that name"
        );
    }

    /// Test that background discovery deduplicates by name within a single
    /// discovery run: if a domain with a given name is already registered
    /// (by a config reload, or by an earlier discovery in the same run), the
    /// discovery thread should not register a duplicate. This is tested
    /// indirectly by having the discovery closure return two domains with the
    /// same name and asserting that only one ends up registered.
    #[test]
    fn discovery_dedup_by_name_within_same_run() {
        let mux = Arc::new(Mux::new(None));

        // Start discovery with two domains having the same name.
        mux.begin_domain_discovery(
            || Ok(vec![dummy_domain("WSL:Ubuntu"), dummy_domain("WSL:Ubuntu")]),
            None,
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        // Only one domain with name "WSL:Ubuntu" should be registered.
        let registered = mux
            .iter_domains()
            .into_iter()
            .filter(|d| d.domain_name() == "WSL:Ubuntu")
            .collect::<Vec<_>>();
        assert_eq!(
            registered.len(),
            1,
            "discovery should deduplicate domains with the same name within the same run"
        );
    }

    /// Regression test: `discovery_dedup_by_name_within_same_run` above only proves a
    /// duplicate name gets deduplicated -- it can't distinguish "the
    /// worker correctly skipped the dup and kept going" from "the worker
    /// treated the dup as the whole run being superseded and gave up",
    /// because there's nothing else in the list to notice going missing.
    /// This test puts a domain with a *different* name right after the
    /// duplicate: if `add_discovered_domain` conflates "name already
    /// taken" with "generation superseded" and the worker loop `return`s
    /// on either, the domain after the duplicate never gets registered.
    #[test]
    fn discovery_continues_registering_domains_after_a_name_collision() {
        let mux = Arc::new(Mux::new(None));

        mux.begin_domain_discovery(
            || {
                Ok(vec![
                    dummy_domain("WSL:Ubuntu"),
                    dummy_domain("WSL:Ubuntu"), // collides with the one above
                    dummy_domain("WSL:Debian"), // must still be registered
                ])
            },
            None,
        );
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);

        assert!(
            mux.get_domain_by_name("WSL:Ubuntu").is_some(),
            "the first, non-colliding registration must still succeed"
        );
        assert!(
            mux.get_domain_by_name("WSL:Debian").is_some(),
            "a name collision earlier in the discovered list must not abort \
             registration of a later, differently-named domain"
        );
    }

    /// Regression test: a discovery run superseded by cancellation *and* a
    /// subsequent fresh discovery run must not have its late-finishing
    /// guard mistake the newer run's `InProgress` state for its own and
    /// finish it early.
    #[test]
    fn stale_finish_guard_does_not_finish_a_newer_discovery_run() {
        let mux = new_test_mux();
        let (first_gate_tx, first_gate_rx) = std::sync::mpsc::channel::<()>();

        // First (soon-to-be-cancelled) run.
        mux.begin_domain_discovery(
            move || {
                let _ = first_gate_rx.recv();
                Ok(vec![])
            },
            None,
        );
        mux.notify_cancelled_domain_discovery_waiters(mux.cancel_domain_discovery());

        // Second run starts while the first thread is still parked on its
        // gate, so its finish guard hasn't dropped yet.
        let (second_gate_tx, second_gate_rx) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                let _ = second_gate_rx.recv();
                Ok(vec![dummy_domain("Second")])
            },
            Some("Second".to_string()),
        );

        // Let the stale first thread finish while the second run's gate
        // is still closed. If its finish guard mistakes the second run's
        // `InProgress` state for its own, it will mark it `Done` here --
        // before the second run has actually produced anything.
        first_gate_tx
            .send(())
            .expect("first discovery thread should still be waiting on its gate");
        // Give the stale thread time to reach and drop its finish guard.
        // Correct behavior leaves no trace to poll for -- the guard finds
        // itself generation-stale and returns without touching the state --
        // so this is a fixed window that only *exits early* in the failure
        // case it is guarding against: a stale guard that wrongly moved the
        // second run out of `InProgress`.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            let state = mux.domain_discovery.lock();
            if !matches!(&*state, DomainDiscoveryState::InProgress { .. }) {
                break;
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(10));
        }

        // A short bounded wait here must still time out (the second run
        // must still be InProgress), not return immediately as it would
        // if the stale first guard had wrongly finished it.
        let start = Instant::now();
        mux.await_domain_discovery(Duration::from_millis(80));
        assert!(
            start.elapsed() >= Duration::from_millis(70),
            "the second run must still be InProgress; a stale guard must not finish it early"
        );
        assert!(
            mux.get_domain_by_name("Second").is_none(),
            "the second run hasn't produced its domain yet"
        );

        // Now let the second (genuine) run finish and confirm it
        // completes normally, applying its own pending_default.
        second_gate_tx
            .send(())
            .expect("second discovery thread should still be waiting on its gate");
        mux.await_domain_discovery(DOMAIN_DISCOVERY_TIMEOUT);
        assert!(
            mux.get_domain_by_name("Second").is_some(),
            "the second run's domain must be registered once it completes"
        );
        assert_eq!(
            mux.default_domain().domain_name(),
            "Second",
            "the second run's pending_default must still be applied by its own guard"
        );
    }

    /// Test that `DomainDiscoveryWaitFuture` genuinely doesn't block
    /// the executor while pending: when run on a real executor with a
    /// concurrently-executing second future that makes progress on each
    /// poll, the counter should advance while discovery is gated closed,
    /// proving the executor wasn't blocked waiting for discovery to complete.
    #[test]
    fn domain_discovery_wait_future_does_not_block_executor() {
        use std::future::Future;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use std::time::{Duration, Instant};

        // Create a mux and gate discovery closed.
        let mux = Arc::new(Mux::new(Some(dummy_domain("dummy-default"))));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        mux.begin_domain_discovery(
            move || {
                let _ = gate_rx.recv();
                Ok(vec![dummy_domain("Discovered")])
            },
            Some("Discovered".to_string()),
        );

        // A simple future that increments a counter on each poll.
        struct CounterFuture {
            counter: Arc<AtomicUsize>,
        }

        impl CounterFuture {
            fn new(counter: Arc<AtomicUsize>) -> Self {
                Self { counter }
            }
        }

        impl Future for CounterFuture {
            type Output = ();

            fn poll(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Self::Output> {
                self.counter.fetch_add(1, Ordering::SeqCst);
                // Never complete -- we just want to observe it being polled.
                Poll::Pending
            }
        }

        // Run both futures together with a simple executor (select-like).
        // We'll manually drive the executor to simulate concurrent polling.
        let counter = Arc::new(AtomicUsize::new(0));
        let mut wait_future = mux.await_domain_discovery_unbounded_async();
        let mut counter_future = CounterFuture::new(Arc::clone(&counter));
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);

        // Poll both futures several times while discovery is gated.
        // If the wait future blocks the executor, the counter won't advance.
        let start = Instant::now();
        let deadline = start + Duration::from_millis(200);
        while Instant::now() < deadline {
            let _ = Pin::new(&mut wait_future).poll(&mut cx);
            let _ = Pin::new(&mut counter_future).poll(&mut cx);
            std::thread::sleep(Duration::from_millis(5));
        }

        let initial_counter = counter.load(Ordering::SeqCst);
        assert!(
            initial_counter > 0,
            "counter should advance while discovery is gated, proving executor wasn't blocked"
        );

        // Now open the gate and let discovery complete.
        let _ = gate_tx.send(());
        let mut completed = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match Pin::new(&mut wait_future).poll(&mut cx) {
                Poll::Ready(_) => {
                    completed = true;
                    break;
                }
                Poll::Pending => {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        assert!(
            completed,
            "wait future should complete once discovery finishes"
        );

        assert_eq!(
            mux.default_domain().domain_name(),
            "Discovered",
            "pending_default should be applied once discovery completes"
        );
    }
}
