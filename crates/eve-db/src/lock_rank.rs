//! Runtime lock-rank enforcement and ratchet checking (`RFC-1 §4`).
//!
//! Assigns static numeric ranks to synchronization primitives (`Ranked<L, RANK>`)
//! and maintains a thread-local stack of currently held lock ranks when compiled
//! with `test`, `debug_assertions`, or feature `lock-rank`. Acquiring a lock whose
//! rank is less than or equal to any currently held lock on the same thread is
//! reported with both the outer held call site and the inner acquisition call site.

use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::{LockResult, Mutex, PoisonError, RwLock, TryLockError, TryLockResult};

/// Rank `20`: per-system `Ballpark` (`SystemHandle::ballpark`).
pub const RANK_BALLPARK: u16 = 20;
/// Rank `21`: per-system `ActorState` (`SystemHandle::actor_state`).
pub const RANK_ACTOR_STATE: u16 = 21;
/// Rank `30`: transitional CrimeWatch coordinator lock (`CrimeWatchLock::lock`).
pub const RANK_CRIMEWATCH: u16 = 30;
/// Rank `40`: `WorldRepository::inner` snapshot lock.
pub const RANK_WORLD_INNER: u16 = 40;
/// Rank `41`: `WorldRepository::sqlite_conn`.
pub const RANK_SQLITE_CONN: u16 = 41;
/// Rank `42`: `WorldRepository::sqlite_fp`.
pub const RANK_SQLITE_FP: u16 = 42;
/// Rank `43`: `WorldRepository::wal_lock`.
pub const RANK_WAL_LOCK: u16 = 43;
/// Rank `60`: leaf locks (`SystemRegistry::systems`, runtime maps, mirrors, notification queues,
/// `bound_objects`, `fleet_extras`, `social_extras`, `auth_runtime`, `esi_state.*`, and Quasar meta).
pub const RANK_LEAF: u16 = 60;
/// Rank `70`: terminal clock pointer lock (`WorldRepository::clock`, `ServerContext::clock`);
/// never acquires any inner lock while held, so leaf locks may read the clock.
pub const RANK_CLOCK: u16 = 70;

/// Generated ratchet baseline of known temporary lock-rank violations (`(held, acquired, site)`).
/// When empty, default enforcement switches to `panic`.
pub const LOCK_RANK_RATCHET_RAW: &str = include_str!("../lock_rank_ratchet.txt");

/// Runtime checking mode controlled by `EVOMU_LOCK_RANK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockRankMode {
    /// Panic immediately on any lock-rank inversion or assertion failure.
    Panic,
    /// Allow triples already present in `lock_rank_ratchet.txt`, panic on any new triple.
    EnforceRatchet,
    /// Record unique `(held, acquired, site)` triples to the ratchet file without panicking.
    Record,
    /// Emit warnings to stderr/tracing without panicking.
    Warn,
}

#[cfg(any(test, debug_assertions, feature = "lock-rank"))]
mod imp {
    use super::{Location, LockRankMode, LOCK_RANK_RATCHET_RAW};
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    #[derive(Clone, Copy)]
    struct HeldEntry {
        id: u64,
        rank: u16,
        site: &'static Location<'static>,
    }

    #[derive(Default)]
    struct HeldStack {
        next_id: u64,
        entries: Vec<HeldEntry>,
    }

    pub(crate) struct HeldToken {
        id: u64,
    }

    impl HeldToken {
        pub(crate) const fn none() -> Self {
            Self { id: 0 }
        }
    }

    impl Drop for HeldToken {
        fn drop(&mut self) {
            if self.id == 0 {
                return;
            }
            let _ = HELD.try_with(|cell| {
                if let Ok(mut stack) = cell.try_borrow_mut() {
                    if let Some(pos) = stack.entries.iter().rposition(|e| e.id == self.id) {
                        stack.entries.remove(pos);
                    }
                }
            });
        }
    }

    thread_local! {
        static HELD: RefCell<HeldStack> = RefCell::new(HeldStack::default());
        #[cfg(test)]
        static TEST_CONFIG: RefCell<Option<(LockRankMode, Option<PathBuf>)>> = const { RefCell::new(None) };
    }

    pub(crate) fn normalize_site(site: &'static Location<'static>) -> String {
        let file = site.file().replace('\\', "/");
        format!("{file}:{}:{}", site.line(), site.column())
    }

    pub(crate) fn parse_ratchet(raw: &str) -> HashSet<(u16, u16, String)> {
        let mut out = HashSet::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut parts = trimmed.splitn(3, '\t');
            let (Some(h), Some(a), Some(s)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if let (Ok(held), Ok(acq)) = (h.parse::<u16>(), a.parse::<u16>()) {
                out.insert((held, acq, s.to_string()));
            }
        }
        out
    }

    fn baseline_ratchet() -> &'static HashSet<(u16, u16, String)> {
        static BASELINE: OnceLock<HashSet<(u16, u16, String)>> = OnceLock::new();
        BASELINE.get_or_init(|| parse_ratchet(LOCK_RANK_RATCHET_RAW))
    }

    fn current_mode_and_path() -> (LockRankMode, Option<PathBuf>) {
        #[cfg(test)]
        if let Some(cfg) = TEST_CONFIG.with(|c| c.borrow().clone()) {
            return cfg;
        }
        let mode = match std::env::var("EVOMU_LOCK_RANK") {
            Ok(val) => match val.trim().to_ascii_lowercase().as_str() {
                "" | "default" => {
                    if baseline_ratchet().is_empty() {
                        LockRankMode::Panic
                    } else {
                        LockRankMode::EnforceRatchet
                    }
                }
                "panic" | "strict" => LockRankMode::Panic,
                "ratchet" | "enforce" => {
                    if baseline_ratchet().is_empty() {
                        LockRankMode::Panic
                    } else {
                        LockRankMode::EnforceRatchet
                    }
                }
                "record" => LockRankMode::Record,
                "warn" => LockRankMode::Warn,
                _ => LockRankMode::Panic,
            },
            Err(_) => {
                if baseline_ratchet().is_empty() {
                    LockRankMode::Panic
                } else {
                    LockRankMode::EnforceRatchet
                }
            }
        };
        let path = std::env::var_os("EVOMU_LOCK_RANK_FILE").map(PathBuf::from);
        (mode, path)
    }

    fn record_triple(triple_line: &str, override_path: Option<PathBuf>) {
        static RECORDED: OnceLock<Mutex<BTreeMap<PathBuf, BTreeSet<String>>>> = OnceLock::new();
        let target = override_path.unwrap_or_else(|| {
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/lock_rank_ratchet.txt"))
        });
        let map_lock = RECORDED.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut by_path = map_lock.lock().unwrap_or_else(|e| e.into_inner());
        let seen = by_path.entry(target.clone()).or_insert_with(|| {
            let mut set = BTreeSet::new();
            if let Ok(existing) = std::fs::read_to_string(&target) {
                for line in existing.lines() {
                    let t = line.trim();
                    if !t.is_empty() && !t.starts_with('#') {
                        set.insert(t.to_string());
                    }
                }
            }
            set
        });
        if seen.insert(triple_line.to_string()) {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::File::create(&target) {
                for line in seen.iter() {
                    let _ = writeln!(file, "{line}");
                }
            }
        }
    }

    fn stack_summary(entries: &[HeldEntry]) -> String {
        entries
            .iter()
            .map(|e| format!("{}@{}", e.rank, normalize_site(e.site)))
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    fn report_violation(
        held_rank: u16,
        held_site: &'static Location<'static>,
        acquired_rank: u16,
        acquired_site: &'static Location<'static>,
        stack: &str,
        context: &str,
    ) {
        let held_str = normalize_site(held_site);
        let acq_str = normalize_site(acquired_site);
        let triple = format!("{held_rank}\t{acquired_rank}\t{acq_str}");
        let (mode, record_path) = current_mode_and_path();
        match mode {
            LockRankMode::Record => {
                record_triple(&triple, record_path);
                tracing::warn!(
                    held_rank,
                    held_site = %held_str,
                    acquired_rank,
                    acquired_site = %acq_str,
                    held_stack = %stack,
                    "{context} (recorded in lock_rank_ratchet.txt)"
                );
            }
            LockRankMode::Warn => {
                tracing::warn!(
                    held_rank,
                    held_site = %held_str,
                    acquired_rank,
                    acquired_site = %acq_str,
                    held_stack = %stack,
                    "{context}"
                );
            }
            LockRankMode::EnforceRatchet => {
                if baseline_ratchet().contains(&(held_rank, acquired_rank, acq_str.clone())) {
                    tracing::warn!(
                        held_rank,
                        held_site = %held_str,
                        acquired_rank,
                        acquired_site = %acq_str,
                        "{context} (allowed by lock_rank_ratchet.txt)"
                    );
                } else {
                    panic!(
                        "lock-rank violation ({context}): cannot acquire rank {acquired_rank} at {acq_str} while holding rank {held_rank} at {held_str} [held: {stack}]"
                    );
                }
            }
            LockRankMode::Panic => {
                panic!(
                    "lock-rank violation ({context}): cannot acquire rank {acquired_rank} at {acq_str} while holding rank {held_rank} at {held_str} [held: {stack}]"
                );
            }
        }
    }

    pub(crate) fn enter(rank: u16, site: &'static Location<'static>) -> HeldToken {
        let violation = HELD.with(|cell| {
            let stack = cell.borrow();
            let violating = stack.entries.iter().rev().find(|e| e.rank >= rank).copied();
            violating.map(|v| (v.rank, v.site, stack_summary(&stack.entries)))
        });
        if let Some((held_rank, held_site, summary)) = violation {
            report_violation(held_rank, held_site, rank, site, &summary, "out-of-order lock acquisition");
        }
        HELD.with(|cell| {
            let mut stack = cell.borrow_mut();
            stack.next_id = stack.next_id.saturating_add(1).max(1);
            let id = stack.next_id;
            stack.entries.push(HeldEntry { id, rank, site });
            HeldToken { id }
        })
    }

    pub(crate) fn assert_no_locks_held(site: &'static Location<'static>) {
        let violation = HELD.with(|cell| {
            let stack = cell.borrow();
            stack
                .entries
                .last()
                .copied()
                .map(|top| (top.rank, top.site, stack_summary(&stack.entries)))
        });
        if let Some((held_rank, held_site, summary)) = violation {
            report_violation(held_rank, held_site, 0, site, &summary, "assert_no_locks_held");
        }
    }

    pub(crate) fn assert_locks_below(max_rank: u16, site: &'static Location<'static>) {
        let violation = HELD.with(|cell| {
            let stack = cell.borrow();
            stack
                .entries
                .iter()
                .rev()
                .find(|e| e.rank >= max_rank)
                .copied()
                .map(|v| (v.rank, v.site, stack_summary(&stack.entries)))
        });
        if let Some((held_rank, held_site, summary)) = violation {
            report_violation(
                held_rank,
                held_site,
                max_rank,
                site,
                &summary,
                "assert_locks_below",
            );
        }
    }

    pub(crate) fn held_ranks() -> Vec<u16> {
        HELD.with(|cell| cell.borrow().entries.iter().map(|e| e.rank).collect())
    }

    #[cfg(test)]
    pub(crate) fn with_test_mode<R>(
        mode: LockRankMode,
        record_path: Option<PathBuf>,
        f: impl FnOnce() -> R,
    ) -> R {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                let _ = TEST_CONFIG.try_with(|c| {
                    *c.borrow_mut() = None;
                });
            }
        }
        TEST_CONFIG.with(|c| {
            *c.borrow_mut() = Some((mode, record_path));
        });
        let _reset = Reset;
        f()
    }
}

#[cfg(not(any(test, debug_assertions, feature = "lock-rank")))]
mod imp {
    use super::Location;

    pub(crate) struct HeldToken;

    impl HeldToken {
        pub(crate) const fn none() -> Self {
            Self
        }
    }

    #[inline(always)]
    pub(crate) fn enter(_rank: u16, _site: &'static Location<'static>) -> HeldToken {
        HeldToken
    }

    #[inline(always)]
    pub(crate) fn assert_no_locks_held(_site: &'static Location<'static>) {}

    #[inline(always)]
    pub(crate) fn assert_locks_below(_max_rank: u16, _site: &'static Location<'static>) {}

    #[inline(always)]
    pub(crate) fn held_ranks() -> Vec<u16> {
        Vec::new()
    }
}

/// Asserts that the current thread holds zero ranked locks.
///
/// Called before actor `ask`/`tell` channel sends and `.await` boundaries (`L-2`).
#[inline]
#[track_caller]
pub fn assert_no_locks_held() {
    imp::assert_no_locks_held(Location::caller());
}

/// Asserts that every lock currently held on this thread has rank strictly below `max_rank`.
#[inline]
#[track_caller]
pub fn assert_locks_below(max_rank: u16) {
    imp::assert_locks_below(max_rank, Location::caller());
}

/// Returns the list of lock ranks currently held on this thread (diagnostic/test helper).
pub fn held_lock_ranks() -> Vec<u16> {
    imp::held_ranks()
}

/// Lock wrapper associating a compile-time `RANK` with an underlying synchronization primitive `L`.
#[derive(Debug)]
pub struct Ranked<L: ?Sized, const RANK: u16>(L);

impl<L: Default, const RANK: u16> Default for Ranked<L, RANK> {
    fn default() -> Self {
        Self(L::default())
    }
}

/// Type alias for a rank-checked [`std::sync::RwLock`].
pub type RankedRwLock<T, const RANK: u16> = Ranked<RwLock<T>, RANK>;

/// Type alias for a rank-checked [`std::sync::Mutex`].
pub type RankedMutex<T, const RANK: u16> = Ranked<Mutex<T>, RANK>;

impl<L: ?Sized, const RANK: u16> Ranked<L, RANK> {
    /// Returns the static lock rank associated with this lock.
    pub const fn rank(&self) -> u16 {
        RANK
    }
}

impl<L, const RANK: u16> Ranked<L, RANK> {
    /// Wraps an already-constructed lock primitive with rank `RANK`.
    pub const fn from_lock(lock: L) -> Self {
        Self(lock)
    }

    /// Consumes the wrapper and returns the underlying lock primitive.
    pub fn into_inner_lock(self) -> L {
        self.0
    }
}

/// Guard returned by [`RankedRwLock::read`]. Releases the read lock before popping the rank stack.
pub struct RankedRwLockReadGuard<'a, T: ?Sized, const RANK: u16> {
    guard: std::sync::RwLockReadGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized, const RANK: u16> Deref for RankedRwLockReadGuard<'_, T, RANK> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized + std::fmt::Debug, const RANK: u16> std::fmt::Debug
    for RankedRwLockReadGuard<'_, T, RANK>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

/// Guard returned by [`RankedRwLock::write`]. Releases the write lock before popping the rank stack.
pub struct RankedRwLockWriteGuard<'a, T: ?Sized, const RANK: u16> {
    guard: std::sync::RwLockWriteGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized, const RANK: u16> Deref for RankedRwLockWriteGuard<'_, T, RANK> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized, const RANK: u16> DerefMut for RankedRwLockWriteGuard<'_, T, RANK> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T: ?Sized + std::fmt::Debug, const RANK: u16> std::fmt::Debug
    for RankedRwLockWriteGuard<'_, T, RANK>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

/// Guard returned by [`RankedMutex::lock`]. Releases the mutex before popping the rank stack.
pub struct RankedMutexGuard<'a, T: ?Sized, const RANK: u16> {
    guard: std::sync::MutexGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized, const RANK: u16> Deref for RankedMutexGuard<'_, T, RANK> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized, const RANK: u16> DerefMut for RankedMutexGuard<'_, T, RANK> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T: ?Sized + std::fmt::Debug, const RANK: u16> std::fmt::Debug
    for RankedMutexGuard<'_, T, RANK>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

fn map_lock_result<G, W>(
    res: LockResult<G>,
    held: imp::HeldToken,
    wrap: impl FnOnce(G, imp::HeldToken) -> W,
) -> LockResult<W> {
    match res {
        Ok(guard) => Ok(wrap(guard, held)),
        Err(poison) => Err(PoisonError::new(wrap(poison.into_inner(), held))),
    }
}

fn map_try_lock_result<G, W>(
    res: TryLockResult<G>,
    rank: u16,
    site: &'static Location<'static>,
    wrap: impl FnOnce(G, imp::HeldToken) -> W,
) -> TryLockResult<W> {
    match res {
        Ok(guard) => {
            let held = imp::enter(rank, site);
            Ok(wrap(guard, held))
        }
        Err(TryLockError::Poisoned(poison)) => {
            let held = imp::enter(rank, site);
            Err(TryLockError::Poisoned(PoisonError::new(wrap(
                poison.into_inner(),
                held,
            ))))
        }
        Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
    }
}

impl<T, const RANK: u16> Ranked<RwLock<T>, RANK> {
    /// Creates a new rank-checked `RwLock` wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }

    /// Consumes this lock, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        self.0.into_inner()
    }
}

impl<T: ?Sized, const RANK: u16> Ranked<RwLock<T>, RANK> {
    /// Acquires shared read access, enforcing `RANK` against locks currently held on this thread.
    #[track_caller]
    pub fn read(&self) -> LockResult<RankedRwLockReadGuard<'_, T, RANK>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.read(), held, |guard, _held| RankedRwLockReadGuard {
            guard,
            _held,
        })
    }

    /// Attempts to acquire shared read access without blocking, enforcing `RANK` when acquired.
    #[track_caller]
    pub fn try_read(&self) -> TryLockResult<RankedRwLockReadGuard<'_, T, RANK>> {
        let site = Location::caller();
        map_try_lock_result(self.0.try_read(), RANK, site, |guard, _held| {
            RankedRwLockReadGuard { guard, _held }
        })
    }

    /// Acquires exclusive write access, enforcing `RANK` against locks currently held on this thread.
    #[track_caller]
    pub fn write(&self) -> LockResult<RankedRwLockWriteGuard<'_, T, RANK>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.write(), held, |guard, _held| RankedRwLockWriteGuard {
            guard,
            _held,
        })
    }

    /// Attempts to acquire exclusive write access without blocking, enforcing `RANK` when acquired.
    #[track_caller]
    pub fn try_write(&self) -> TryLockResult<RankedRwLockWriteGuard<'_, T, RANK>> {
        let site = Location::caller();
        map_try_lock_result(self.0.try_write(), RANK, site, |guard, _held| {
            RankedRwLockWriteGuard { guard, _held }
        })
    }

    /// Returns whether the underlying `RwLock` is currently poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.0.is_poisoned()
    }

    /// Returns a mutable reference to the underlying data with exclusive borrow access.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.0.get_mut()
    }
}

impl<T, const RANK: u16> Ranked<Mutex<T>, RANK> {
    /// Creates a new rank-checked `Mutex` wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    /// Consumes this lock, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        self.0.into_inner()
    }
}

impl<T: ?Sized, const RANK: u16> Ranked<Mutex<T>, RANK> {
    /// Acquires the mutex, enforcing `RANK` against locks currently held on this thread.
    #[track_caller]
    pub fn lock(&self) -> LockResult<RankedMutexGuard<'_, T, RANK>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.lock(), held, |guard, _held| RankedMutexGuard {
            guard,
            _held,
        })
    }

    /// Attempts to acquire the mutex without blocking, enforcing `RANK` when acquired.
    #[track_caller]
    pub fn try_lock(&self) -> TryLockResult<RankedMutexGuard<'_, T, RANK>> {
        let site = Location::caller();
        map_try_lock_result(self.0.try_lock(), RANK, site, |guard, _held| RankedMutexGuard {
            guard,
            _held,
        })
    }

    /// Returns whether the underlying `Mutex` is currently poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.0.is_poisoned()
    }

    /// Returns a mutable reference to the underlying data with exclusive borrow access.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.0.get_mut()
    }
}

/// Rank-erased shared read guard returned by [`AnyRwLock::read`].
pub struct AnyRwLockReadGuard<'a, T: ?Sized> {
    guard: std::sync::RwLockReadGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized> Deref for AnyRwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

/// Rank-erased exclusive write guard returned by [`AnyRwLock::write`].
pub struct AnyRwLockWriteGuard<'a, T: ?Sized> {
    guard: std::sync::RwLockWriteGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized> Deref for AnyRwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized> DerefMut for AnyRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

/// Object-safe adapter allowing helper functions to accept either a [`RankedRwLock`]
/// (enforcing its static rank) or a raw [`std::sync::RwLock`] (e.g. local unit-test fixtures).
pub trait AnyRwLock<T: ?Sized>: Send + Sync {
    /// Acquires shared read access.
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>>;
    /// Acquires exclusive write access.
    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>>;
}

impl<T: ?Sized + Send + Sync> AnyRwLock<T> for RwLock<T> {
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        map_lock_result(RwLock::read(self), imp::HeldToken::none(), |guard, _held| {
            AnyRwLockReadGuard { guard, _held }
        })
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        map_lock_result(RwLock::write(self), imp::HeldToken::none(), |guard, _held| {
            AnyRwLockWriteGuard { guard, _held }
        })
    }
}

impl<T: ?Sized + Send + Sync> AnyRwLock<T> for std::sync::Arc<RwLock<T>> {
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        <RwLock<T> as AnyRwLock<T>>::read(&**self)
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        <RwLock<T> as AnyRwLock<T>>::write(&**self)
    }
}

impl<T: ?Sized + Send + Sync, const RANK: u16> AnyRwLock<T> for Ranked<RwLock<T>, RANK> {
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.read(), held, |guard, _held| AnyRwLockReadGuard {
            guard,
            _held,
        })
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.write(), held, |guard, _held| AnyRwLockWriteGuard {
            guard,
            _held,
        })
    }
}

impl<T: ?Sized + Send + Sync, const RANK: u16> AnyRwLock<T>
    for std::sync::Arc<Ranked<RwLock<T>, RANK>>
{
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        <Ranked<RwLock<T>, RANK> as AnyRwLock<T>>::read(&**self)
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        <Ranked<RwLock<T>, RANK> as AnyRwLock<T>>::write(&**self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

    fn temp_ratchet_path(name: &str) -> std::path::PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "evomu_lock_rank_{name}_{}_{seq}.txt",
            std::process::id()
        ))
    }

    #[test]
    fn strictly_ascending_acquisition_succeeds_and_drops_cleanly() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(1);
            let actor: RankedRwLock<u32, RANK_ACTOR_STATE> = RankedRwLock::new(2);
            let cw: RankedRwLock<u32, RANK_CRIMEWATCH> = RankedRwLock::new(3);
            let inner: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(4);
            let conn: RankedMutex<u32, RANK_SQLITE_CONN> = RankedMutex::new(5);
            let fp: RankedMutex<u32, RANK_SQLITE_FP> = RankedMutex::new(6);
            let wal: RankedMutex<u32, RANK_WAL_LOCK> = RankedMutex::new(7);
            let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(8);

            assert!(held_lock_ranks().is_empty());
            assert_no_locks_held();
            {
                let _g20 = bp.read().unwrap();
                let _g21 = actor.write().unwrap();
                let _g30 = cw.read().unwrap();
                let _g40 = inner.write().unwrap();
                let _g41 = conn.lock().unwrap();
                let _g42 = fp.lock().unwrap();
                let _g43 = wal.lock().unwrap();
                let _g60 = leaf.read().unwrap();
                assert_eq!(held_lock_ranks(), vec![20, 21, 30, 40, 41, 42, 43, 60]);
            }
            assert!(held_lock_ranks().is_empty());
            assert_no_locks_held();

            // Re-acquiring lowest rank after drop succeeds.
            let _g20_again = bp.write().unwrap();
            assert_eq!(held_lock_ranks(), vec![20]);
        });
    }

    #[test]
    fn wrong_order_reports_both_call_sites() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(10);
            let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(20);

            let outer_line = line!() + 1;
            let _outer = leaf.read().unwrap();
            let inner_line = line!() + 2;
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _inner = world.write();
            }))
            .expect_err("acquiring rank 40 while holding rank 60 must panic");

            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&'static str>().map(|s| (*s).to_string()))
                .expect("panic payload must be string");
            assert!(msg.contains("rank 40"), "unexpected msg: {msg}");
            assert!(msg.contains("rank 60"), "unexpected msg: {msg}");
            assert!(
                msg.contains(&format!(":{outer_line}:")),
                "missing outer call site line {outer_line} in: {msg}"
            );
            assert!(
                msg.contains(&format!(":{inner_line}:")),
                "missing inner call site line {inner_line} in: {msg}"
            );
        });
        assert!(held_lock_ranks().is_empty());
    }

    #[test]
    fn same_rank_acquisition_panics_with_both_sites() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let a: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(1);
            let b: RankedMutex<u32, RANK_LEAF> = RankedMutex::new(2);

            let line_a = line!() + 1;
            let _ga = a.read().unwrap();
            let line_b = line!() + 2;
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _gb = b.lock();
            }))
            .expect_err("two locks of the same rank must fail");

            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            assert!(msg.contains(&format!(":{line_a}:")), "msg: {msg}");
            assert!(msg.contains(&format!(":{line_b}:")), "msg: {msg}");
        });
        assert!(held_lock_ranks().is_empty());
    }

    #[test]
    fn assert_no_locks_held_reports_held_site_and_caller_site() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(0);
            let held_line = line!() + 1;
            let _g = bp.read().unwrap();
            let assert_line = line!() + 2;
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_no_locks_held();
            }))
            .expect_err("assert_no_locks_held must fail while lock is held");
            let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
            assert!(msg.contains("assert_no_locks_held"), "msg: {msg}");
            assert!(msg.contains(&format!(":{held_line}:")), "msg: {msg}");
            assert!(msg.contains(&format!(":{assert_line}:")), "msg: {msg}");
        });
        assert_no_locks_held();
    }

    #[test]
    fn out_of_lifo_drop_keeps_stack_consistent_and_try_would_block_pops() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let l20: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(0);
            let l40: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(0);
            let l30: RankedRwLock<u32, RANK_CRIMEWATCH> = RankedRwLock::new(0);
            let l60 = std::sync::Arc::new(RankedMutex::<u32, RANK_LEAF>::new(0));

            let g20 = l20.read().unwrap();
            let g40 = l40.read().unwrap();
            drop(g20);
            assert_eq!(held_lock_ranks(), vec![40]);

            // Holding 40 still forbids 30.
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _g30 = l30.read();
            }));
            assert!(res.is_err());
            assert_eq!(held_lock_ranks(), vec![40]);

            // WouldBlock on try_lock must not leave rank 60 on stack.
            let l60_bg = std::sync::Arc::clone(&l60);
            let (tx, rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let _held = l60_bg.lock().unwrap();
                tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            rx.recv().unwrap();
            assert!(matches!(l60.try_lock(), Err(TryLockError::WouldBlock)));
            assert_eq!(held_lock_ranks(), vec![40]);
            release_tx.send(()).unwrap();
            handle.join().unwrap();

            drop(g40);
            assert!(held_lock_ranks().is_empty());
        });
    }

    #[test]
    fn poison_recovery_preserves_balanced_stack() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let l60 = std::sync::Arc::new(RankedMutex::<u32, RANK_LEAF>::new(1));
            let l60_bg = std::sync::Arc::clone(&l60);
            let _ = std::thread::spawn(move || {
                let _g = l60_bg.lock().unwrap();
                panic!("poison mutex intentionally");
            })
            .join();
            {
                let guard = l60.lock().unwrap_or_else(|e| e.into_inner());
                assert_eq!(*guard, 1);
                assert_eq!(held_lock_ranks(), vec![60]);
            }
            assert!(held_lock_ranks().is_empty());
        });
    }

    #[test]
    fn ratchet_file_is_empty_and_record_enforce_roundtrip() {
        let non_comment_lines = LOCK_RANK_RATCHET_RAW
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            })
            .count();
        let parsed = imp::parse_ratchet(LOCK_RANK_RATCHET_RAW);
        assert_eq!(
            parsed.len(),
            non_comment_lines,
            "every non-comment line in crates/eve-db/lock_rank_ratchet.txt must parse as a unique triple"
        );

        let path = temp_ratchet_path("roundtrip");
        let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(1);
        let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(2);

        imp::with_test_mode(LockRankMode::Record, Some(path.clone()), || {
            let _outer = leaf.read().unwrap();
            let _inner = world.read().unwrap();
        });
        let recorded = std::fs::read_to_string(&path).expect("ratchet file created");
        let entries = imp::parse_ratchet(&recorded);
        assert_eq!(entries.len(), 1, "expected 1 recorded triple, got: {recorded}");
        let (h, a, site) = entries.clone().into_iter().next().unwrap();
        assert_eq!((h, a), (RANK_LEAF, RANK_WORLD_INNER));
        assert!(site.contains("lock_rank.rs"));
        assert!(!entries.contains(&(RANK_LEAF, RANK_BALLPARK, site)));

        // Re-recording the same site is deduplicated; assert_locks_below enforces upper rank.
        imp::with_test_mode(LockRankMode::Record, Some(path.clone()), || {
            let _g = leaf.read().unwrap();
            assert_locks_below(RANK_WORLD_INNER);
        });
        let recorded_after = std::fs::read_to_string(&path).expect("ratchet file exists");
        assert_eq!(imp::parse_ratchet(&recorded_after).len(), 2);

        imp::with_test_mode(LockRankMode::Panic, None, || {
            let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(0);
            let _g20 = bp.read().unwrap();
            assert_locks_below(RANK_WORLD_INNER);
            let _g40 = world.read().unwrap();
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_locks_below(RANK_WORLD_INNER);
            }));
            assert!(err.is_err());
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn any_rwlock_enforces_rank_for_ranked_and_allows_raw() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let ranked: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(42);
            let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(7);
            let raw: RwLock<u32> = RwLock::new(99);

            fn lock_via_trait(r: &dyn AnyRwLock<u32>) -> u32 {
                *r.read().unwrap()
            }
            assert_eq!(lock_via_trait(&ranked), 42);
            assert_eq!(lock_via_trait(&raw), 99);

            let _g = (&ranked as &dyn AnyRwLock<u32>).read().unwrap();
            assert_eq!(held_lock_ranks(), vec![RANK_LEAF]);
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _w = world.read();
            }));
            assert!(err.is_err());
        });
    }
}
