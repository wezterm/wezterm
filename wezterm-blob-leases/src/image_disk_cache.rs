#![cfg(feature = "image_disk_cache")]

//! A bounded, content-addressed on-disk cache for image data.
//!
//! The cache is the spill target for in-memory image caches that
//! exceeded their budget: entries released from memory can be
//! stored here under their content hash and looked up again later.
//!
//! * It is inert until [`configure`] is called with `enabled: true`;
//!   the default configuration never touches the disk.
//! * The backing directory is a per-process temp dir (under the
//!   system temp dir, or a configured parent directory) that is
//!   removed when the cache is dropped via [`clear`], so nothing
//!   outlives the process.
//! * A byte budget bounds the total payload; when it is exceeded the
//!   least recently used entries are removed.  A single entry larger
//!   than the whole budget is allowed to stay, mirroring the in-memory
//!   image caches' keep-at-least-one behavior.
//!
//! Read/write latency and size, hit/miss accounting and byte totals
//! are recorded via the `metrics` crate under `image_disk_cache.*`
//! so that users can decide whether to keep the feature enabled.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;
use tempfile::TempDir;

/// Cumulative activity counters, kept alongside the metrics both for
/// the exit summary log and for tests.
#[derive(Default)]
struct Stats {
    spills: u64,
    spill_failed: u64,
    restores: u64,
    restore_misses: u64,
    evicted: u64,
    errors: u64,
    bytes_written: u64,
    bytes_read: u64,
}

struct Entry {
    bytes: usize,
    last_access: u64,
}

struct ImageDiskCache {
    /// Lazily created on the first successful store; dropping it
    /// removes the directory tree.
    root: Option<TempDir>,
    enabled: bool,
    budget: usize,
    parent_dir: Option<PathBuf>,
    entries: HashMap<[u8; 32], Entry>,
    total_bytes: usize,
    /// Monotonic counter minting `last_access` stamps
    next_access: u64,
    stats: Stats,
}

static CACHE: OnceLock<Mutex<Option<ImageDiskCache>>> = OnceLock::new();

fn lock_cache() -> MutexGuard<'static, Option<ImageDiskCache>> {
    CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap()
}

/// Run `f` against the cache when one has been configured.
fn with_cache<R>(f: impl FnOnce(&mut ImageDiskCache) -> R) -> Option<R> {
    lock_cache().as_mut().map(f)
}

fn hex_name(hash: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut name = String::with_capacity(64);
    for byte in hash {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

impl ImageDiskCache {
    fn next_access(&mut self) -> u64 {
        self.next_access += 1;
        self.next_access
    }

    /// Create the cache directory on first use.  A failure to create
    /// the directory disables the cache until the next `configure`
    /// call so that we don't retry (and re-log) on every spill.
    fn ensure_root(&mut self) -> bool {
        if self.root.is_some() {
            return true;
        }
        let mut builder = tempfile::Builder::new();
        builder.prefix("wezterm-image-cache-").rand_bytes(8);
        let made = match &self.parent_dir {
            Some(parent) => match std::fs::create_dir_all(parent) {
                Ok(()) => builder.tempdir_in(parent),
                Err(err) => {
                    log::error!(
                        "failed to create image disk cache parent dir {}: {err:#}",
                        parent.display()
                    );
                    self.enabled = false;
                    self.stats.errors += 1;
                    metrics::counter!("image_disk_cache.error.count").increment(1);
                    return false;
                }
            },
            None => builder.tempdir(),
        };
        match made {
            Ok(root) => {
                log::info!(
                    "image disk cache enabled at {} (budget {} bytes); \
                     observe it via the periodic_stat_logging option or \
                     wezterm.metrics in lua",
                    root.path().display(),
                    self.budget
                );
                self.root = Some(root);
                true
            }
            Err(err) => {
                log::error!("failed to create image disk cache directory: {err:#}");
                self.enabled = false;
                self.stats.errors += 1;
                metrics::counter!("image_disk_cache.error.count").increment(1);
                false
            }
        }
    }

    fn update_gauges(&self) {
        metrics::counter!("image_disk_cache.entries.current").absolute(self.entries.len() as u64);
        metrics::counter!("image_disk_cache.bytes.current").absolute(self.total_bytes as u64);
    }

    fn remove_entry(&mut self, hash: &[u8; 32]) {
        if let Some(entry) = self.entries.remove(hash) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
            if let Some(root) = &self.root {
                let path = root.path().join(hex_name(hash));
                if let Err(err) = std::fs::remove_file(&path) {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        log::error!("failed to remove {}: {err}", path.display());
                    }
                }
            }
        }
        self.update_gauges();
    }

    /// Drop least-recently-used entries until the budget is met.
    /// A lone entry is always kept, even when it alone exceeds the
    /// budget, so that an oversized image is still cached.
    fn evict_over_budget(&mut self) {
        while self.total_bytes > self.budget && self.entries.len() > 1 {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(hash, _)| *hash);
            let Some(victim) = victim else {
                break;
            };
            self.remove_entry(&victim);
            self.stats.evicted += 1;
            metrics::counter!("image_disk_cache.evict.count").increment(1);
        }
    }
}

/// Configure the cache.  Inert unless called with `enabled == true`.
/// The directory is created lazily on the first store.  Budget
/// changes apply immediately (shrinking evicts right away); changing
/// the parent directory only takes effect before the directory has
/// been created, or after a restart.
pub fn configure(enabled: bool, budget: usize, parent_dir: Option<&Path>) {
    let mut guard = lock_cache();
    match guard.as_mut() {
        Some(cache) => {
            if !enabled {
                // Disabling tears the cache down, but only when it ever
                // created a directory: repeated no-op calls stay cheap.
                cache.enabled = false;
                if cache.root.is_some() {
                    take_cache(&mut guard);
                }
                return;
            }
            cache.enabled = true;
            if cache.budget != budget {
                cache.budget = budget;
                cache.evict_over_budget();
            }
            if cache.root.is_none() {
                cache.parent_dir = parent_dir.map(Path::to_path_buf);
            } else if cache.parent_dir.as_deref() != parent_dir {
                log::info!(
                    "image disk cache directory change to {parent_dir:?} takes effect \
                     after a restart; keeping the current directory"
                );
            }
        }
        None => {
            *guard = Some(ImageDiskCache {
                root: None,
                enabled,
                budget,
                parent_dir: parent_dir.map(Path::to_path_buf),
                entries: HashMap::new(),
                total_bytes: 0,
                next_access: 0,
                stats: Stats::default(),
            });
        }
    }
}

/// Remove and drop the cache, logging a summary of its activity.
/// Dropping the cache removes the temp directory tree.
fn take_cache(guard: &mut Option<ImageDiskCache>) {
    if let Some(cache) = guard.take() {
        let stats = &cache.stats;
        if stats.spills + stats.restores + stats.spill_failed > 0 {
            log::info!(
                "image disk cache summary: {} spill(s) writing {} bytes, \
                 {} restore(s) reading {} bytes, {} restore miss(es), \
                 {} failed spill(s), {} quota eviction(s)",
                stats.spills,
                stats.bytes_written,
                stats.restores,
                stats.bytes_read,
                stats.restore_misses,
                stats.spill_failed,
                stats.evicted
            );
        }
    }
}

/// Tear the cache down, removing its directory.  Called at process exit.
pub fn clear() {
    if let Some(mutex) = CACHE.get() {
        take_cache(&mut mutex.lock().unwrap());
    }
}

/// Store `data` under `hash`.  Returns false when the cache is
/// disabled or the write failed; in the latter case the data is
/// permanently dropped as far as the caller is concerned.
pub fn store(hash: [u8; 32], data: &[u8]) -> bool {
    with_cache(|cache| {
        if !cache.enabled || !cache.ensure_root() {
            cache.stats.spill_failed += 1;
            metrics::counter!("image_disk_cache.spill_failed.count").increment(1);
            return false;
        }
        if cache.entries.contains_key(&hash) {
            // Content addressing means the cached payload is identical
            let tick = cache.next_access();
            if let Some(entry) = cache.entries.get_mut(&hash) {
                entry.last_access = tick;
            }
            return true;
        }

        let Some(root) = cache.root.as_ref() else {
            return false;
        };
        let path = root.path().join(hex_name(&hash));
        let dir = root.path().to_path_buf();
        let started = Instant::now();
        let written: Result<(), std::io::Error> = (|| {
            let mut file = tempfile::Builder::new()
                .prefix("new-")
                .rand_bytes(5)
                .tempfile_in(&dir)?;
            file.write_all(data)?;
            file.persist(&path).map_err(|err| err.error)?;
            Ok(())
        })();
        let elapsed = started.elapsed();
        metrics::histogram!("image_disk_cache.write.latency").record(elapsed.as_secs_f64());
        metrics::histogram!("image_disk_cache.write.size").record(data.len() as f64);

        match written {
            Ok(()) => {
                let tick = cache.next_access();
                cache.entries.insert(
                    hash,
                    Entry {
                        bytes: data.len(),
                        last_access: tick,
                    },
                );
                cache.total_bytes += data.len();
                cache.stats.spills += 1;
                cache.stats.bytes_written += data.len() as u64;
                metrics::counter!("image_disk_cache.spill.count").increment(1);
                metrics::counter!("image_disk_cache.bytes_written").increment(data.len() as u64);
                cache.evict_over_budget();
                true
            }
            Err(err) => {
                log::error!(
                    "failed to store image {} in disk cache: {err:#}",
                    hex_name(&hash)
                );
                cache.stats.errors += 1;
                cache.stats.spill_failed += 1;
                metrics::counter!("image_disk_cache.error.count").increment(1);
                metrics::counter!("image_disk_cache.spill_failed.count").increment(1);
                false
            }
        }
    })
    .unwrap_or(false)
}

/// Look up `hash` and read its payload.  A `None` result means the
/// entry was never stored or has since been evicted by the budget.
pub fn load(hash: &[u8; 32]) -> Option<Vec<u8>> {
    with_cache(|cache| {
        if !cache.enabled || !cache.entries.contains_key(hash) {
            return None;
        }
        let Some(root) = cache.root.as_ref() else {
            return None;
        };
        let path = root.path().join(hex_name(hash));
        let started = Instant::now();
        match std::fs::read(&path) {
            Ok(data) => {
                let elapsed = started.elapsed();
                metrics::histogram!("image_disk_cache.read.latency").record(elapsed.as_secs_f64());
                metrics::histogram!("image_disk_cache.read.size").record(data.len() as f64);
                cache.stats.bytes_read += data.len() as u64;
                metrics::counter!("image_disk_cache.bytes_read").increment(data.len() as u64);
                let tick = cache.next_access();
                if let Some(entry) = cache.entries.get_mut(hash) {
                    entry.last_access = tick;
                }
                Some(data)
            }
            Err(err) => {
                log::error!(
                    "failed to read image {} from disk cache: {err:#}",
                    hex_name(hash)
                );
                cache.stats.errors += 1;
                metrics::counter!("image_disk_cache.error.count").increment(1);
                // The file is gone or unreadable; forget the entry
                cache.remove_entry(hash);
                None
            }
        }
    })
    .flatten()
}

/// Forget `hash`, removing its file.  Used when a client explicitly
/// deletes an image: the data should not survive on disk.
pub fn remove(hash: &[u8; 32]) {
    with_cache(|cache| cache.remove_entry(hash));
}

/// Account a successful restore-from-disk lookup.  The lookup itself
/// is driven by the caller, which knows whether the image id was
/// ever spilled.
pub fn note_restore_hit() {
    metrics::counter!("image_disk_cache.restore.count").increment(1);
    with_cache(|cache| cache.stats.restores += 1);
}

/// Account a lookup that expected a spilled image but found the
/// payload gone (quota eviction or write failure): the image data
/// is permanently dropped.
pub fn note_restore_miss() {
    metrics::counter!("image_disk_cache.restore_miss.count").increment(1);
    with_cache(|cache| cache.stats.restore_misses += 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is a process-global singleton; hold this lock so
    /// that tests don't fight over it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct Serial(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for Serial {
        fn drop(&mut self) {
            // The cache is a process-global static whose Drop never
            // runs at exit; clear it per test or the temp dir leaks
            clear();
        }
    }

    fn setup(enabled: bool, budget: usize, parent: Option<&Path>) -> Serial {
        // unwrap_or_else(into_inner) keeps a failure in one test from
        // poisoning the lock for the rest
        let guard = TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        clear();
        configure(enabled, budget, parent);
        Serial(guard)
    }

    fn hash_of(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    /// store/load round trip, plus a second store of the same hash
    /// being an idempotent no-op
    #[test]
    fn store_load_roundtrip() {
        let _serial = setup(true, 1024, None);
        let hash = hash_of(1);
        assert!(store(hash, b"hello"));
        assert_eq!(load(&hash).as_deref(), Some(&b"hello"[..]));

        // Re-storing the same content hash leaves the original payload
        assert!(store(hash, b"other"));
        assert_eq!(load(&hash).as_deref(), Some(&b"hello"[..]));
    }

    /// Without configure(enable) nothing is stored or loadable
    #[test]
    fn disabled_cache_is_inert() {
        let _serial = setup(false, 1024, None);
        assert!(!store(hash_of(2), b"hello"));
        assert_eq!(load(&hash_of(2)), None);
    }

    /// Exceeding the budget evicts the least recently used entry,
    /// and loading an entry makes it most recently used
    #[test]
    fn evicts_lru_over_budget() {
        let _serial = setup(true, 30, None);
        let a = hash_of(10);
        let b = hash_of(11);
        let c = hash_of(12);
        let d = hash_of(13);
        assert!(store(a, &[0u8; 10]));
        assert!(store(b, &[0u8; 10]));
        assert!(store(c, &[0u8; 10]));
        // Touch `a` so that `b` becomes the eviction victim
        assert_eq!(load(&a).as_deref(), Some(&[0u8; 10][..]));
        assert!(store(d, &[0u8; 10]));

        assert_eq!(load(&b), None, "least recently used entry was evicted");
        assert!(load(&a).is_some());
        assert!(load(&c).is_some());
        assert!(load(&d).is_some());
    }

    /// A single entry larger than the whole budget is still kept,
    /// mirroring the keep-at-least-one behavior of the memory caches
    #[test]
    fn oversized_lone_entry_survives() {
        let _serial = setup(true, 10, None);
        let big = hash_of(20);
        let small = hash_of(21);
        assert!(store(big, &[0u8; 50]));
        assert!(load(&big).is_some());
        // Bringing in a second entry evicts the big one to fit
        assert!(store(small, &[0u8; 5]));
        assert_eq!(load(&big), None);
        assert!(load(&small).is_some());
    }

    /// An explicitly removed hash is gone, others are unaffected
    #[test]
    fn remove_forgets_entry() {
        let _serial = setup(true, 1024, None);
        let a = hash_of(30);
        let b = hash_of(31);
        assert!(store(a, b"a"));
        assert!(store(b, b"b"));
        remove(&a);
        assert_eq!(load(&a), None);
        assert!(load(&b).is_some());
    }

    /// A configured parent directory hosts the cache temp dir, and
    /// clear() removes the whole directory tree
    #[test]
    fn custom_parent_dir_and_clear() {
        let parent = tempfile::tempdir().expect("make parent dir");
        let _serial = setup(true, 1024, Some(parent.path()));
        assert!(store(hash_of(40), b"data"));

        let children: Vec<PathBuf> = std::fs::read_dir(parent.path())
            .expect("read parent")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        assert_eq!(children.len(), 1, "one cache dir under the parent");
        assert!(
            children[0]
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .starts_with("wezterm-image-cache-"),
            "cache dir is namespaced, got {:?}",
            children[0]
        );

        clear();
        assert!(
            !children[0].exists(),
            "clear() removed the cache directory tree"
        );
    }

    /// The activity counters observed by tests track the operations
    #[test]
    fn stats_counters_track_activity() {
        let _serial = setup(true, 15, None);
        let a = hash_of(50);
        let b = hash_of(51);
        let c = hash_of(52);
        assert!(store(a, &[1u8; 10]));
        assert!(store(b, &[1u8; 10]));
        assert!(store(c, &[1u8; 10]));
        // Each store over the 15 byte budget evicted its predecessor,
        // so only the most recent entry is still resident
        assert_eq!(load(&a), None);
        assert_eq!(load(&b), None);
        assert!(load(&c).is_some());
        note_restore_hit();
        note_restore_miss();

        let stats = with_cache(|cache| {
            (
                cache.stats.spills,
                cache.stats.evicted,
                cache.stats.bytes_written,
                cache.stats.bytes_read,
                cache.stats.restores,
                cache.stats.restore_misses,
            )
        })
        .unwrap();
        assert_eq!(stats.0, 3, "three stores");
        assert_eq!(stats.1, 2, "two quota evictions");
        assert_eq!(stats.2, 30, "bytes written");
        assert_eq!(stats.3, 10, "bytes read");
        assert_eq!(stats.4, 1);
        assert_eq!(stats.5, 1);
    }
}
