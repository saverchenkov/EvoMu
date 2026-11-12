//! Persistent Copy-on-Write (`CoW`) map and cell primitives for `WorldStateSnapshot` (`#899` §3.1, `#914` `P2-1`).
//!
//! # Verified `imbl 7.0.2` Properties
//!
//! Static inspection of `imbl 7.0.2` (`src/config.rs`, `src/ord/map.rs`, `src/nodes/btree.rs`) confirms:
//! 1. **B-tree leaf chunk width (`ORD_CHUNK_SIZE`):** `16` entries per leaf (`src/config.rs:22`,
//!    `MIN_SIZE = 8` in `src/nodes/btree.rs:25-26`).
//! 2. **Value trait bound on `OrdMap::diff`:** `impl<'a, 'b, K, V, P> Iterator for DiffIter<'a, 'b, K, V, P>`
//!    requires only `K: Ord, V: PartialEq, P: SharedPointerKind` (`src/ord/map.rs:1921-1925`) — `V: Eq` is
//!    **not** required.
//! 3. **Subtree pointer-equality skipping in `OrdMap::diff`:**
//!    - Root short-circuit: `if self.ptr_eq(other) { return diff; }` (`src/ord/map.rs:304`) finishes in `O(1)`.
//!    - Interior/leaf skip: `Cursor::advance_skipping_shared` (`src/nodes/btree.rs:1205-1243`) checks
//!      `std::ptr::eq(this.1, that.1) && this.0 == that.0` on leaves and pops contiguous shared `Branch`
//!      ancestors (`lines 1223-1233`), visiting only the `O(k log N)` path to the `k` modified leaves.
//! 4. **Why `Row<V>(Arc<V>)` is required over bare `Arc<V>`:**
//!    - Within a modified 16-entry leaf (and at the initial `seek_to_first()` cursor peek before the first
//!      `advance_skipping_shared` call), `DiffIter::next` evaluates `if v1 != v2` (`src/ord/map.rs:1940`).
//!    - In Rust's standard library (`alloc::sync::Arc`), `<Arc<T> as PartialEq>::eq` only specializes to
//!      `Arc::ptr_eq(self, other) || **self == **other` when `T: Eq` (`ArcEqIdent`). Domain records containing
//!      `f64` (`InventoryItemRecord`, `CharacterRecord`, `MarketOrderRecord`, `TrainedSkillRecord`,
//!      `SkillQueueEntry`) implement `PartialEq` but **not** `Eq`. With bare `Arc<V>`, every untouched sibling
//!      in a modified leaf would run a full struct comparison `*v1 == *v2`.
//!    - [`Row<V>`] wraps `Arc<V>` and implements `PartialEq` as
//!      `Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0` for all `V: PartialEq`, guaranteeing `O(1)`
//!      pointer-equality short-circuiting on untouched rows and cloning `V` at most once per `get_mut`.

use imbl::OrdMap;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, Serializer};
use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::iter::FusedIterator;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Verified B+ tree leaf node capacity (`ORD_CHUNK_SIZE`) in `imbl 7.0.2` (`src/config.rs:22`).
pub const IMBL_ORD_CHUNK_SIZE: usize = 16;

static COW_FULL_TOUCH_TOTAL: AtomicU64 = AtomicU64::new(0);
static COW_FULL_TOUCH_BY_SITE: OnceLock<Mutex<BTreeMap<String, u64>>> = OnceLock::new();

#[inline]
fn record_full_touch(loc: &'static Location<'static>) {
    COW_FULL_TOUCH_TOTAL.fetch_add(1, Ordering::Relaxed);
    let mutex = COW_FULL_TOUCH_BY_SITE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut map) = mutex.lock() {
        let key = format!("{}:{}", loc.file(), loc.line());
        *map.entry(key).or_insert(0) += 1;
    }
}

/// Returns the total number of full-table mutable touches (`iter_mut`, `values_mut`, `retain`)
/// recorded across all [`CowMap`] and [`CowMapInline`] instances.
pub fn cow_full_touch_total() -> u64 {
    COW_FULL_TOUCH_TOTAL.load(Ordering::Relaxed)
}

/// Returns the per-callsite breakdown (`"file:line" -> count`) of full-table mutable touches
/// (`iter_mut`, `values_mut`, `retain`) recorded across all [`CowMap`] and [`CowMapInline`] instances.
pub fn cow_full_touch_by_site() -> BTreeMap<String, u64> {
    let mutex = COW_FULL_TOUCH_BY_SITE.get_or_init(|| Mutex::new(BTreeMap::new()));
    mutex.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Resets the full-touch telemetry counters (for deterministic unit testing).
pub fn reset_cow_full_touch_for_test() {
    COW_FULL_TOUCH_TOTAL.store(0, Ordering::Relaxed);
    if let Some(mutex) = COW_FULL_TOUCH_BY_SITE.get() {
        if let Ok(mut map) = mutex.lock() {
            map.clear();
        }
    }
}

/// Private per-row `Arc<V>` wrapper that short-circuits `PartialEq` via `Arc::ptr_eq` even when
/// `V: PartialEq + !Eq` (e.g. structs containing `f64` fields).
#[derive(Debug)]
struct Row<V>(Arc<V>);

impl<V> Row<V> {
    #[inline]
    fn new(value: V) -> Self {
        Self(Arc::new(value))
    }

    #[inline]
    fn from_arc(arc: Arc<V>) -> Self {
        Self(arc)
    }
}

impl<V> Clone for Row<V> {
    #[inline]
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<V: PartialEq> PartialEq for Row<V> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl<V: Eq> Eq for Row<V> {}

impl<V: Default> Default for Row<V> {
    #[inline]
    fn default() -> Self {
        Self::new(V::default())
    }
}

/// Difference item yielded by [`CowMap::diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CowDiffItem<'a, 'b, K, V> {
    /// Entry was added in the new map.
    Add(&'b K, &'b Arc<V>),
    /// Entry existed in both maps and its value changed (`!Arc::ptr_eq` and `*old != *new`).
    Update {
        old: (&'a K, &'a Arc<V>),
        new: (&'b K, &'b Arc<V>),
    },
    /// Entry was present in the old map and removed from the new map.
    Remove(&'a K, &'a Arc<V>),
}

impl<'a, 'b, K, V> CowDiffItem<'a, 'b, K, V> {
    /// Returns the key affected by this diff item.
    #[inline]
    pub fn key(&self) -> &K {
        match self {
            Self::Add(k, _) => k,
            Self::Update { new: (k, _), .. } => k,
            Self::Remove(k, _) => k,
        }
    }

    /// Returns the pre-change `Arc<V>` if the entry existed before (`Update` or `Remove`).
    #[inline]
    pub fn before(&self) -> Option<&'a Arc<V>> {
        match self {
            Self::Add(_, _) => None,
            Self::Update { old: (_, v), .. } | Self::Remove(_, v) => Some(v),
        }
    }

    /// Returns the post-change `Arc<V>` if the entry exists after (`Add` or `Update`).
    #[inline]
    pub fn after(&self) -> Option<&'b Arc<V>> {
        match self {
            Self::Add(_, v) | Self::Update { new: (_, v), .. } => Some(v),
            Self::Remove(_, _) => None,
        }
    }

    /// Returns the pre-change `&V` if the entry existed before.
    #[inline]
    pub fn before_val(&self) -> Option<&'a V> {
        self.before().map(|v| &**v)
    }

    /// Returns the post-change `&V` if the entry exists after.
    #[inline]
    pub fn after_val(&self) -> Option<&'b V> {
        self.after().map(|v| &**v)
    }
}

/// Persistent Copy-on-Write ordered map backed by `imbl::OrdMap<K, Row<V>>`.
///
/// - `Clone` is `O(1)` (increments the root B-tree node refcount).
/// - Point lookup (`get`, `contains_key`) is `O(log N)`.
/// - Point mutation (`get_mut`, `insert`, `remove`, `entry`) is `O(log N)` and clones at most the
///   single target `V` via `Arc::make_mut` (sibling values in the 16-entry B-tree leaf only clone
///   their `Arc<V>` pointer).
/// - Structural diff (`before.diff(&after)`) is `O(k log N)` for `k` modified rows, skipping
///   pointer-identical B-tree subtrees and pointer-identical `Row<V>` (`Arc<V>`) leaves.
pub struct CowMap<K, V> {
    ord: OnceLock<OrdMap<K, Row<V>>>,
    dirty: Option<Box<BTreeMap<K, Row<V>>>>,
}

impl<K, V> CowMap<K, V> {
    /// Creates an empty `CowMap`.
    #[inline]
    pub fn new() -> Self {
        Self {
            ord: OnceLock::from(OrdMap::new()),
            dirty: None,
        }
    }

    /// Creates an empty `CowMap` (capacity hint is accepted for `HashMap` call-site compatibility).
    #[inline]
    pub fn with_capacity(_capacity: usize) -> Self {
        Self::new()
    }

    /// Returns the number of entries in the map.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.dirty {
            Some(d) => d.len(),
            None => self.ord.get().map_or(0, OrdMap::len),
        }
    }

    /// Returns `true` if the map contains no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes all entries from the map in `O(1)`.
    #[inline]
    pub fn clear(&mut self) {
        self.dirty = None;
        self.ord = OnceLock::from(OrdMap::new());
    }
}

impl<K: Ord, V> CowMap<K, V> {
    /// Returns a reference to the value corresponding to the key.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(ord) = self.ord.get() {
            return ord.get(key).map(|r| &*r.0);
        }
        self.dirty.as_ref()?.get(key).map(|r| &*r.0)
    }

    /// Returns a reference to the underlying `Arc<V>` for `key` without cloning `V`.
    #[inline]
    pub fn get_arc<Q>(&self, key: &Q) -> Option<&Arc<V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(ord) = self.ord.get() {
            return ord.get(key).map(|r| &r.0);
        }
        self.dirty.as_ref()?.get(key).map(|r| &r.0)
    }

    /// Returns the key-value pair corresponding to the supplied key.
    #[inline]
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(ord) = self.ord.get() {
            return ord.get_key_value(key).map(|(k, r)| (k, &*r.0));
        }
        self.dirty.as_ref()?.get_key_value(key).map(|(k, r)| (k, &*r.0))
    }

    /// Returns `true` if the map contains a value for the specified key.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    /// Returns `true` if both maps contain `key` and their row `Arc<V>` allocations are pointer-equal.
    #[inline]
    pub fn row_ptr_eq<Q>(&self, other: &Self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match (self.get_arc(key), other.get_arc(key)) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Returns an iterator over `(&K, &V)` pairs in ascending key order.
    #[inline]
    pub fn iter(&self) -> Iter<'_, K, V> {
        let inner = if let Some(ord) = self.ord.get() {
            IterInner::Ord(ord.iter())
        } else if let Some(dirty) = &self.dirty {
            IterInner::Dirty(dirty.iter())
        } else {
            IterInner::Empty
        };
        Iter { inner }
    }

    /// Returns an iterator over `&K` keys in ascending key order.
    #[inline]
    pub fn keys(&self) -> Keys<'_, K, V> {
        Keys { inner: self.iter() }
    }

    /// Returns an iterator over `&V` values in ascending key order.
    #[inline]
    pub fn values(&self) -> Values<'_, K, V> {
        Values { inner: self.iter() }
    }
}

impl<K: Ord + Clone, V: Clone> CowMap<K, V> {
    #[inline]
    fn ensure_ord(&self) -> &OrdMap<K, Row<V>> {
        self.ord.get_or_init(|| match &self.dirty {
            Some(dirty) => dirty.iter().map(|(k, r)| (k.clone(), r.clone())).collect(),
            None => OrdMap::new(),
        })
    }

    #[inline]
    fn ensure_ord_mut(&mut self) -> &mut OrdMap<K, Row<V>> {
        if let Some(dirty) = self.dirty.take() {
            if self.ord.get().is_none() {
                self.ord = OnceLock::from(dirty.into_iter().collect::<OrdMap<K, Row<V>>>());
            }
        }
        if self.ord.get().is_none() {
            self.ord = OnceLock::from(OrdMap::new());
        }
        self.ord
            .get_mut()
            .expect("CowMap::ord is always initialized after ensure_ord_mut")
    }

    /// Returns `true` if both `CowMap` instances share the exact same root B-tree node pointer (`O(1)`).
    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.ensure_ord().ptr_eq(other.ensure_ord())
    }

    /// Returns a mutable reference to the value corresponding to `key`, cloning `V` at most once
    /// via `Arc::make_mut` if the row is currently shared with an older snapshot.
    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.ensure_ord_mut()
            .get_mut(key)
            .map(|row| Arc::make_mut(&mut row.0))
    }

    /// Inserts a key-value pair into the map, returning the previous value if the key was present.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.ensure_ord_mut()
            .insert(key, Row::new(value))
            .map(|old| Arc::unwrap_or_clone(old.0))
    }

    /// Inserts a pre-wrapped `Arc<V>` into the map, returning the previous `Arc<V>` if present.
    #[inline]
    pub fn insert_arc(&mut self, key: K, value: Arc<V>) -> Option<Arc<V>> {
        self.ensure_ord_mut()
            .insert(key, Row::from_arc(value))
            .map(|old| old.0)
    }

    /// Removes a key from the map, returning the value at the key if the key was previously in the map.
    #[inline]
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.ensure_ord_mut()
            .remove(key)
            .map(|old| Arc::unwrap_or_clone(old.0))
    }

    /// Removes a key from the map, returning the stored key and value if the key was previously in the map.
    #[inline]
    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.ensure_ord_mut()
            .remove_with_key(key)
            .map(|(k, old)| (k, Arc::unwrap_or_clone(old.0)))
    }

    /// Gets the given key's corresponding entry in the map for in-place manipulation.
    #[inline]
    pub fn entry(&mut self, key: K) -> CowMapEntry<'_, K, V> {
        let ord = self.ensure_ord_mut();
        match ord.entry(key) {
            imbl::ordmap::Entry::Occupied(occ) => {
                CowMapEntry::Occupied(CowMapOccupiedEntry { inner: occ })
            }
            imbl::ordmap::Entry::Vacant(vac) => {
                CowMapEntry::Vacant(CowMapVacantEntry { inner: vac })
            }
        }
    }

    /// Returns a mutable iterator over `(&K, &mut V)` pairs in ascending key order.
    ///
    /// Increments the `cow_full_touch_total{site}` counter (`#[track_caller]`) so full-table
    /// scans can be tracked and burned down (`#914`, `#919`).
    #[track_caller]
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V> {
        record_full_touch(Location::caller());
        self.prepare_dirty_iter_mut()
    }

    /// Returns a mutable iterator over `&mut V` values in ascending key order.
    ///
    /// Increments the `cow_full_touch_total{site}` counter (`#[track_caller]`).
    #[track_caller]
    pub fn values_mut(&mut self) -> ValuesMut<'_, K, V> {
        record_full_touch(Location::caller());
        ValuesMut {
            inner: self.prepare_dirty_iter_mut(),
        }
    }

    fn prepare_dirty_iter_mut(&mut self) -> IterMut<'_, K, V> {
        if self.dirty.is_none() {
            let ord = self.ord.take().unwrap_or_default();
            let map: BTreeMap<K, Row<V>> = ord.into_iter().collect();
            self.dirty = Some(Box::new(map));
        } else {
            self.ord.take();
        }
        IterMut {
            inner: self
                .dirty
                .as_mut()
                .expect("CowMap::dirty is initialized")
                .iter_mut(),
        }
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// Increments the `cow_full_touch_total{site}` counter (`#[track_caller]`).
    #[track_caller]
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        record_full_touch(Location::caller());
        let ord = self.ensure_ord_mut();
        let keys: Vec<K> = ord.keys().cloned().collect();
        for k in keys {
            let keep = if let Some(row) = ord.get_mut(&k) {
                f(&k, Arc::make_mut(&mut row.0))
            } else {
                true
            };
            if !keep {
                ord.remove(&k);
            }
        }
    }

    /// Returns an `O(k log N)` iterator over the differences from `self` (old) to `other` (new),
    /// skipping pointer-equal B-tree subtrees and pointer-equal `Row<V>` (`Arc<V>`) leaves.
    #[inline]
    pub fn diff<'a, 'b>(&'a self, other: &'b Self) -> CowDiffIter<'a, 'b, K, V>
    where
        V: PartialEq,
    {
        CowDiffIter {
            inner: self.ensure_ord().diff(other.ensure_ord()),
        }
    }
}

/// Iterator over differences between two [`CowMap`] instances.
pub struct CowDiffIter<'a, 'b, K, V> {
    inner: imbl::ordmap::DiffIter<'a, 'b, K, Row<V>, imbl::shared_ptr::DefaultSharedPtr>,
}

impl<'a, 'b, K: Ord, V: PartialEq> Iterator for CowDiffIter<'a, 'b, K, V> {
    type Item = CowDiffItem<'a, 'b, K, V>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|item| match item {
            imbl::ordmap::DiffItem::Add(k, r) => CowDiffItem::Add(k, &r.0),
            imbl::ordmap::DiffItem::Update {
                old: (k1, r1),
                new: (k2, r2),
            } => CowDiffItem::Update {
                old: (k1, &r1.0),
                new: (k2, &r2.0),
            },
            imbl::ordmap::DiffItem::Remove(k, r) => CowDiffItem::Remove(k, &r.0),
        })
    }
}

impl<'a, 'b, K: Ord, V: PartialEq> FusedIterator for CowDiffIter<'a, 'b, K, V> {}

/// A view into a single entry in a [`CowMap`], which may either be vacant or occupied.
pub enum CowMapEntry<'a, K: Ord + Clone, V: Clone> {
    Occupied(CowMapOccupiedEntry<'a, K, V>),
    Vacant(CowMapVacantEntry<'a, K, V>),
}

/// Occupied entry in a [`CowMap`].
pub struct CowMapOccupiedEntry<'a, K: Ord + Clone, V: Clone> {
    inner: imbl::ordmap::OccupiedEntry<'a, K, Row<V>, imbl::shared_ptr::DefaultSharedPtr>,
}

/// Vacant entry in a [`CowMap`].
pub struct CowMapVacantEntry<'a, K: Ord + Clone, V: Clone> {
    inner: imbl::ordmap::VacantEntry<'a, K, Row<V>, imbl::shared_ptr::DefaultSharedPtr>,
}

impl<'a, K: Ord + Clone, V: Clone> CowMapEntry<'a, K, V> {
    /// Ensures a value is in the entry by inserting `default` if empty, and returns a mutable
    /// reference to the value in the entry.
    #[inline]
    pub fn or_insert(self, default: V) -> &'a mut V {
        match self {
            Self::Occupied(occ) => occ.into_mut(),
            Self::Vacant(vac) => vac.insert(default),
        }
    }

    /// Ensures a value is in the entry by inserting the result of the `default` function if empty,
    /// and returns a mutable reference to the value in the entry.
    #[inline]
    pub fn or_insert_with<F: FnOnce() -> V>(self, default: F) -> &'a mut V {
        match self {
            Self::Occupied(occ) => occ.into_mut(),
            Self::Vacant(vac) => vac.insert(default()),
        }
    }

    /// Ensures a value is in the entry by inserting the result of `default(key)` if empty,
    /// and returns a mutable reference to the value in the entry.
    #[inline]
    pub fn or_insert_with_key<F: FnOnce(&K) -> V>(self, default: F) -> &'a mut V {
        match self {
            Self::Occupied(occ) => occ.into_mut(),
            Self::Vacant(vac) => {
                let val = default(vac.key());
                vac.insert(val)
            }
        }
    }

    /// Ensures a value is in the entry by inserting `V::default()` if empty, and returns a mutable
    /// reference to the value in the entry.
    #[inline]
    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }

    /// Provides in-place mutable access to an occupied entry before any potential inserts into the map.
    #[inline]
    pub fn and_modify<F: FnOnce(&mut V)>(mut self, f: F) -> Self {
        if let Self::Occupied(ref mut occ) = self {
            f(occ.get_mut());
        }
        self
    }

    /// Returns a reference to this entry's key.
    #[inline]
    pub fn key(&self) -> &K {
        match self {
            Self::Occupied(occ) => occ.key(),
            Self::Vacant(vac) => vac.key(),
        }
    }
}

impl<'a, K: Ord + Clone, V: Clone> CowMapOccupiedEntry<'a, K, V> {
    #[inline]
    pub fn key(&self) -> &K {
        self.inner.key()
    }

    #[inline]
    pub fn get(&self) -> &V {
        &self.inner.get().0
    }

    #[inline]
    pub fn get_arc(&self) -> &Arc<V> {
        &self.inner.get().0
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut V {
        Arc::make_mut(&mut self.inner.get_mut().0)
    }

    #[inline]
    pub fn into_mut(self) -> &'a mut V {
        Arc::make_mut(&mut self.inner.into_mut().0)
    }

    #[inline]
    pub fn insert(&mut self, value: V) -> V {
        Arc::unwrap_or_clone(self.inner.insert(Row::new(value)).0)
    }

    #[inline]
    pub fn remove(self) -> V {
        Arc::unwrap_or_clone(self.inner.remove().0)
    }

    #[inline]
    pub fn remove_entry(self) -> (K, V) {
        let (k, r) = self.inner.remove_entry();
        (k, Arc::unwrap_or_clone(r.0))
    }
}

impl<'a, K: Ord + Clone, V: Clone> CowMapVacantEntry<'a, K, V> {
    #[inline]
    pub fn key(&self) -> &K {
        self.inner.key()
    }

    #[inline]
    pub fn into_key(self) -> K {
        self.inner.into_key()
    }

    #[inline]
    pub fn insert(self, value: V) -> &'a mut V {
        Arc::make_mut(&mut self.inner.insert(Row::new(value)).0)
    }
}

enum IterInner<'a, K, V> {
    Ord(imbl::ordmap::Iter<'a, K, Row<V>, imbl::shared_ptr::DefaultSharedPtr>),
    Dirty(std::collections::btree_map::Iter<'a, K, Row<V>>),
    Empty,
}

impl<'a, K, V> Clone for IterInner<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::Ord(it) => Self::Ord(it.clone()),
            Self::Dirty(it) => Self::Dirty(it.clone()),
            Self::Empty => Self::Empty,
        }
    }
}

/// An iterator over the entries of a [`CowMap`] in ascending key order.
pub struct Iter<'a, K, V> {
    inner: IterInner<'a, K, V>,
}

impl<'a, K, V> Clone for Iter<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::Ord(it) => it.next().map(|(k, r)| (k, &*r.0)),
            IterInner::Dirty(it) => it.next().map(|(k, r)| (k, &*r.0)),
            IterInner::Empty => None,
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            IterInner::Ord(it) => it.size_hint(),
            IterInner::Dirty(it) => it.size_hint(),
            IterInner::Empty => (0, Some(0)),
        }
    }
}

impl<'a, K, V> DoubleEndedIterator for Iter<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::Ord(it) => it.next_back().map(|(k, r)| (k, &*r.0)),
            IterInner::Dirty(it) => it.next_back().map(|(k, r)| (k, &*r.0)),
            IterInner::Empty => None,
        }
    }
}

impl<'a, K, V> ExactSizeIterator for Iter<'a, K, V> {}
impl<'a, K, V> FusedIterator for Iter<'a, K, V> {}

/// An iterator over the keys of a [`CowMap`] in ascending key order.
pub struct Keys<'a, K, V> {
    inner: Iter<'a, K, V>,
}

impl<'a, K, V> Clone for Keys<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for Keys<'a, K, V> {
    type Item = &'a K;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, _)| k)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, K, V> DoubleEndedIterator for Keys<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(k, _)| k)
    }
}

impl<'a, K, V> ExactSizeIterator for Keys<'a, K, V> {}
impl<'a, K, V> FusedIterator for Keys<'a, K, V> {}

/// An iterator over the values of a [`CowMap`] in ascending key order.
pub struct Values<'a, K, V> {
    inner: Iter<'a, K, V>,
}

impl<'a, K, V> Clone for Values<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for Values<'a, K, V> {
    type Item = &'a V;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, v)| v)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, K, V> DoubleEndedIterator for Values<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, v)| v)
    }
}

impl<'a, K, V> ExactSizeIterator for Values<'a, K, V> {}
impl<'a, K, V> FusedIterator for Values<'a, K, V> {}

/// A mutable iterator over `(&'a K, &'a mut V)` pairs in a [`CowMap`].
///
/// Wraps `std::collections::btree_map::IterMut` so `std::mem::needs_drop::<IterMut<'a, K, V>>()`
/// is `false`, preserving Non-Lexical Lifetimes (NLL) borrow release at last use.
pub struct IterMut<'a, K, V> {
    inner: std::collections::btree_map::IterMut<'a, K, Row<V>>,
}

impl<'a, K, V: Clone> Iterator for IterMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(k, row)| (k, Arc::make_mut(&mut row.0)))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, K, V: Clone> DoubleEndedIterator for IterMut<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner
            .next_back()
            .map(|(k, row)| (k, Arc::make_mut(&mut row.0)))
    }
}

impl<'a, K, V: Clone> ExactSizeIterator for IterMut<'a, K, V> {}
impl<'a, K, V: Clone> FusedIterator for IterMut<'a, K, V> {}

/// A mutable iterator over `&'a mut V` values in a [`CowMap`].
///
/// `std::mem::needs_drop::<ValuesMut<'a, K, V>>()` is `false`, preserving NLL borrow release.
pub struct ValuesMut<'a, K, V> {
    inner: IterMut<'a, K, V>,
}

impl<'a, K, V: Clone> Iterator for ValuesMut<'a, K, V> {
    type Item = &'a mut V;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, v)| v)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, K, V: Clone> DoubleEndedIterator for ValuesMut<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, v)| v)
    }
}

impl<'a, K, V: Clone> ExactSizeIterator for ValuesMut<'a, K, V> {}
impl<'a, K, V: Clone> FusedIterator for ValuesMut<'a, K, V> {}

enum IntoIterInner<K, V> {
    Ord(imbl::ordmap::ConsumingIter<K, Row<V>, imbl::shared_ptr::DefaultSharedPtr>),
    Dirty(std::collections::btree_map::IntoIter<K, Row<V>>),
}

/// An owning iterator over `(K, V)` entries of a [`CowMap`].
pub struct IntoIter<K, V> {
    inner: IntoIterInner<K, V>,
}

impl<K: Clone, V: Clone> Iterator for IntoIter<K, V> {
    type Item = (K, V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterInner::Ord(it) => it.next().map(|(k, r)| (k, Arc::unwrap_or_clone(r.0))),
            IntoIterInner::Dirty(it) => it.next().map(|(k, r)| (k, Arc::unwrap_or_clone(r.0))),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            IntoIterInner::Ord(it) => it.size_hint(),
            IntoIterInner::Dirty(it) => it.size_hint(),
        }
    }
}

impl<K: Clone, V: Clone> DoubleEndedIterator for IntoIter<K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterInner::Ord(it) => it.next_back().map(|(k, r)| (k, Arc::unwrap_or_clone(r.0))),
            IntoIterInner::Dirty(it) => it.next_back().map(|(k, r)| (k, Arc::unwrap_or_clone(r.0))),
        }
    }
}

impl<K: Clone, V: Clone> ExactSizeIterator for IntoIter<K, V> {}
impl<K: Clone, V: Clone> FusedIterator for IntoIter<K, V> {}

impl<K: Ord + Clone, V: Clone> Clone for CowMap<K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            ord: OnceLock::from(self.ensure_ord().clone()),
            dirty: None,
        }
    }
}

impl<K, V> Default for CowMap<K, V> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + fmt::Debug, V: fmt::Debug> fmt::Debug for CowMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Ord, V: PartialEq> PartialEq for CowMap<K, V> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        if let (Some(a), Some(b)) = (self.ord.get(), other.ord.get()) {
            return a == b;
        }
        self.iter()
            .zip(other.iter())
            .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
    }
}

impl<K: Ord + Eq, V: Eq> Eq for CowMap<K, V> {}

impl<K: Ord + Borrow<Q>, Q: Ord + ?Sized, V> Index<&Q> for CowMap<K, V> {
    type Output = V;

    #[inline]
    fn index(&self, key: &Q) -> &Self::Output {
        self.get(key).expect("CowMap::index: key not found")
    }
}

impl<K: Ord + Clone + Borrow<Q>, Q: Ord + ?Sized, V: Clone> IndexMut<&Q> for CowMap<K, V> {
    #[inline]
    fn index_mut(&mut self, key: &Q) -> &mut Self::Output {
        self.get_mut(key).expect("CowMap::index_mut: key not found")
    }
}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for CowMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut ord = OrdMap::new();
        for (k, v) in iter {
            ord.insert(k, Row::new(v));
        }
        Self {
            ord: OnceLock::from(ord),
            dirty: None,
        }
    }
}

impl<K: Ord + Clone, V: Clone> Extend<(K, V)> for CowMap<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        let ord = self.ensure_ord_mut();
        for (k, v) in iter {
            ord.insert(k, Row::new(v));
        }
    }
}

impl<K: Ord + Clone, V: Clone> IntoIterator for CowMap<K, V> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V>;

    #[inline]
    fn into_iter(mut self) -> Self::IntoIter {
        if let Some(dirty) = self.dirty.take() {
            IntoIter {
                inner: IntoIterInner::Dirty(dirty.into_iter()),
            }
        } else {
            let ord = self.ord.take().unwrap_or_default();
            IntoIter {
                inner: IntoIterInner::Ord(ord.into_iter()),
            }
        }
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a CowMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K: Ord + Clone, V: Clone> IntoIterator for &'a mut CowMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V>;

    #[track_caller]
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K: Ord + Clone, V: Clone> From<HashMap<K, V>> for CowMap<K, V> {
    fn from(map: HashMap<K, V>) -> Self {
        map.into_iter().collect()
    }
}

impl<K: Ord + Clone, V: Clone> From<BTreeMap<K, V>> for CowMap<K, V> {
    fn from(map: BTreeMap<K, V>) -> Self {
        map.into_iter().collect()
    }
}

impl<K: Ord + Clone, V: Clone, const N: usize> From<[(K, V); N]> for CowMap<K, V> {
    fn from(arr: [(K, V); N]) -> Self {
        arr.into_iter().collect()
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for CowMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (k, v) in self.iter() {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de, K, V> Deserialize<'de> for CowMap<K, V>
where
    K: Ord + Clone + Deserialize<'de>,
    V: Clone + Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CowMapVisitor<K, V>(std::marker::PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for CowMapVisitor<K, V>
        where
            K: Ord + Clone + Deserialize<'de>,
            V: Clone + Deserialize<'de>,
        {
            type Value = CowMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut ord = OrdMap::new();
                while let Some((k, v)) = access.next_entry::<K, V>()? {
                    ord.insert(k, Row::new(v));
                }
                Ok(CowMap {
                    ord: OnceLock::from(ord),
                    dirty: None,
                })
            }
        }

        deserializer.deserialize_map(CowMapVisitor(std::marker::PhantomData))
    }
}

// ============================================================================
// CowMapInline<K, V> — persistent CoW map storing V inline (no per-row Arc)
// ============================================================================

/// Difference item yielded by [`CowMapInline::diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CowInlineDiffItem<'a, 'b, K, V> {
    /// Entry was added in the new map.
    Add(&'b K, &'b V),
    /// Entry existed in both maps and its value changed (`*old != *new`).
    Update {
        old: (&'a K, &'a V),
        new: (&'b K, &'b V),
    },
    /// Entry was present in the old map and removed from the new map.
    Remove(&'a K, &'a V),
}

impl<'a, 'b, K, V> CowInlineDiffItem<'a, 'b, K, V> {
    #[inline]
    pub fn key(&self) -> &K {
        match self {
            Self::Add(k, _) => k,
            Self::Update { new: (k, _), .. } => k,
            Self::Remove(k, _) => k,
        }
    }

    #[inline]
    pub fn before(&self) -> Option<&'a V> {
        match self {
            Self::Add(_, _) => None,
            Self::Update { old: (_, v), .. } | Self::Remove(_, v) => Some(v),
        }
    }

    #[inline]
    pub fn after(&self) -> Option<&'b V> {
        match self {
            Self::Add(_, v) | Self::Update { new: (_, v), .. } => Some(v),
            Self::Remove(_, _) => None,
        }
    }
}

/// Persistent Copy-on-Write ordered map backed directly by `imbl::OrdMap<K, V>` without per-row
/// `Arc<V>` wrapping, suited for small copy-ish values (e.g. `TrainedSkillRecord`, scalar digests).
pub struct CowMapInline<K, V> {
    ord: OnceLock<OrdMap<K, V>>,
    dirty: Option<Box<BTreeMap<K, V>>>,
}

impl<K, V> CowMapInline<K, V> {
    #[inline]
    pub fn new() -> Self {
        Self {
            ord: OnceLock::from(OrdMap::new()),
            dirty: None,
        }
    }

    #[inline]
    pub fn with_capacity(_capacity: usize) -> Self {
        Self::new()
    }

    #[inline]
    pub fn len(&self) -> usize {
        match &self.dirty {
            Some(d) => d.len(),
            None => self.ord.get().map_or(0, OrdMap::len),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn clear(&mut self) {
        self.dirty = None;
        self.ord = OnceLock::from(OrdMap::new());
    }
}

impl<K: Ord, V> CowMapInline<K, V> {
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(ord) = self.ord.get() {
            return ord.get(key);
        }
        self.dirty.as_ref()?.get(key)
    }

    #[inline]
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(ord) = self.ord.get() {
            return ord.get_key_value(key);
        }
        self.dirty.as_ref()?.get_key_value(key)
    }

    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    #[inline]
    pub fn iter(&self) -> InlineIter<'_, K, V> {
        let inner = if let Some(ord) = self.ord.get() {
            InlineIterInner::Ord(ord.iter())
        } else if let Some(dirty) = &self.dirty {
            InlineIterInner::Dirty(dirty.iter())
        } else {
            InlineIterInner::Empty
        };
        InlineIter { inner }
    }

    #[inline]
    pub fn keys(&self) -> InlineKeys<'_, K, V> {
        InlineKeys { inner: self.iter() }
    }

    #[inline]
    pub fn values(&self) -> InlineValues<'_, K, V> {
        InlineValues { inner: self.iter() }
    }
}

impl<K: Ord + Clone, V: Clone> CowMapInline<K, V> {
    #[inline]
    fn ensure_ord(&self) -> &OrdMap<K, V> {
        self.ord.get_or_init(|| match &self.dirty {
            Some(dirty) => dirty.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => OrdMap::new(),
        })
    }

    #[inline]
    fn ensure_ord_mut(&mut self) -> &mut OrdMap<K, V> {
        if let Some(dirty) = self.dirty.take() {
            if self.ord.get().is_none() {
                self.ord = OnceLock::from(dirty.into_iter().collect::<OrdMap<K, V>>());
            }
        }
        if self.ord.get().is_none() {
            self.ord = OnceLock::from(OrdMap::new());
        }
        self.ord
            .get_mut()
            .expect("CowMapInline::ord is always initialized after ensure_ord_mut")
    }

    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.ensure_ord().ptr_eq(other.ensure_ord())
    }

    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.ensure_ord_mut().get_mut(key)
    }

    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.ensure_ord_mut().insert(key, value)
    }

    #[inline]
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.ensure_ord_mut().remove(key)
    }

    #[inline]
    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.ensure_ord_mut().remove_with_key(key)
    }

    #[inline]
    pub fn entry(
        &mut self,
        key: K,
    ) -> imbl::ordmap::Entry<'_, K, V, imbl::shared_ptr::DefaultSharedPtr> {
        self.ensure_ord_mut().entry(key)
    }

    #[track_caller]
    pub fn iter_mut(&mut self) -> std::collections::btree_map::IterMut<'_, K, V> {
        record_full_touch(Location::caller());
        self.prepare_dirty_iter_mut()
    }

    #[track_caller]
    pub fn values_mut(&mut self) -> std::collections::btree_map::ValuesMut<'_, K, V> {
        record_full_touch(Location::caller());
        if self.dirty.is_none() {
            let ord = self.ord.take().unwrap_or_default();
            let map: BTreeMap<K, V> = ord.into_iter().collect();
            self.dirty = Some(Box::new(map));
        } else {
            self.ord.take();
        }
        self.dirty
            .as_mut()
            .expect("CowMapInline::dirty is initialized")
            .values_mut()
    }

    fn prepare_dirty_iter_mut(&mut self) -> std::collections::btree_map::IterMut<'_, K, V> {
        if self.dirty.is_none() {
            let ord = self.ord.take().unwrap_or_default();
            let map: BTreeMap<K, V> = ord.into_iter().collect();
            self.dirty = Some(Box::new(map));
        } else {
            self.ord.take();
        }
        self.dirty
            .as_mut()
            .expect("CowMapInline::dirty is initialized")
            .iter_mut()
    }

    #[track_caller]
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        record_full_touch(Location::caller());
        let ord = self.ensure_ord_mut();
        let keys: Vec<K> = ord.keys().cloned().collect();
        for k in keys {
            let keep = if let Some(v) = ord.get_mut(&k) {
                f(&k, v)
            } else {
                true
            };
            if !keep {
                ord.remove(&k);
            }
        }
    }

    #[inline]
    pub fn diff<'a, 'b>(&'a self, other: &'b Self) -> CowInlineDiffIter<'a, 'b, K, V>
    where
        V: PartialEq,
    {
        CowInlineDiffIter {
            inner: self.ensure_ord().diff(other.ensure_ord()),
        }
    }
}

/// Iterator over differences between two [`CowMapInline`] instances.
pub struct CowInlineDiffIter<'a, 'b, K, V> {
    inner: imbl::ordmap::DiffIter<'a, 'b, K, V, imbl::shared_ptr::DefaultSharedPtr>,
}

impl<'a, 'b, K: Ord, V: PartialEq> Iterator for CowInlineDiffIter<'a, 'b, K, V> {
    type Item = CowInlineDiffItem<'a, 'b, K, V>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|item| match item {
            imbl::ordmap::DiffItem::Add(k, v) => CowInlineDiffItem::Add(k, v),
            imbl::ordmap::DiffItem::Update { old, new } => CowInlineDiffItem::Update { old, new },
            imbl::ordmap::DiffItem::Remove(k, v) => CowInlineDiffItem::Remove(k, v),
        })
    }
}

impl<'a, 'b, K: Ord, V: PartialEq> FusedIterator for CowInlineDiffIter<'a, 'b, K, V> {}

enum InlineIterInner<'a, K, V> {
    Ord(imbl::ordmap::Iter<'a, K, V, imbl::shared_ptr::DefaultSharedPtr>),
    Dirty(std::collections::btree_map::Iter<'a, K, V>),
    Empty,
}

impl<'a, K, V> Clone for InlineIterInner<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::Ord(it) => Self::Ord(it.clone()),
            Self::Dirty(it) => Self::Dirty(it.clone()),
            Self::Empty => Self::Empty,
        }
    }
}

pub struct InlineIter<'a, K, V> {
    inner: InlineIterInner<'a, K, V>,
}

impl<'a, K, V> Clone for InlineIter<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for InlineIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            InlineIterInner::Ord(it) => it.next(),
            InlineIterInner::Dirty(it) => it.next(),
            InlineIterInner::Empty => None,
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            InlineIterInner::Ord(it) => it.size_hint(),
            InlineIterInner::Dirty(it) => it.size_hint(),
            InlineIterInner::Empty => (0, Some(0)),
        }
    }
}

impl<'a, K, V> DoubleEndedIterator for InlineIter<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            InlineIterInner::Ord(it) => it.next_back(),
            InlineIterInner::Dirty(it) => it.next_back(),
            InlineIterInner::Empty => None,
        }
    }
}

impl<'a, K, V> ExactSizeIterator for InlineIter<'a, K, V> {}
impl<'a, K, V> FusedIterator for InlineIter<'a, K, V> {}

pub struct InlineKeys<'a, K, V> {
    inner: InlineIter<'a, K, V>,
}

impl<'a, K, V> Clone for InlineKeys<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for InlineKeys<'a, K, V> {
    type Item = &'a K;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, _)| k)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, K, V> DoubleEndedIterator for InlineKeys<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(k, _)| k)
    }
}

impl<'a, K, V> ExactSizeIterator for InlineKeys<'a, K, V> {}
impl<'a, K, V> FusedIterator for InlineKeys<'a, K, V> {}

pub struct InlineValues<'a, K, V> {
    inner: InlineIter<'a, K, V>,
}

impl<'a, K, V> Clone for InlineValues<'a, K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, K, V> Iterator for InlineValues<'a, K, V> {
    type Item = &'a V;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, v)| v)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, K, V> DoubleEndedIterator for InlineValues<'a, K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, v)| v)
    }
}

impl<'a, K, V> ExactSizeIterator for InlineValues<'a, K, V> {}
impl<'a, K, V> FusedIterator for InlineValues<'a, K, V> {}

enum InlineIntoIterInner<K, V> {
    Ord(imbl::ordmap::ConsumingIter<K, V, imbl::shared_ptr::DefaultSharedPtr>),
    Dirty(std::collections::btree_map::IntoIter<K, V>),
}

pub struct InlineIntoIter<K, V> {
    inner: InlineIntoIterInner<K, V>,
}

impl<K: Clone, V: Clone> Iterator for InlineIntoIter<K, V> {
    type Item = (K, V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            InlineIntoIterInner::Ord(it) => it.next(),
            InlineIntoIterInner::Dirty(it) => it.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            InlineIntoIterInner::Ord(it) => it.size_hint(),
            InlineIntoIterInner::Dirty(it) => it.size_hint(),
        }
    }
}

impl<K: Clone, V: Clone> DoubleEndedIterator for InlineIntoIter<K, V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            InlineIntoIterInner::Ord(it) => it.next_back(),
            InlineIntoIterInner::Dirty(it) => it.next_back(),
        }
    }
}

impl<K: Clone, V: Clone> ExactSizeIterator for InlineIntoIter<K, V> {}
impl<K: Clone, V: Clone> FusedIterator for InlineIntoIter<K, V> {}

impl<K: Ord + Clone, V: Clone> Clone for CowMapInline<K, V> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            ord: OnceLock::from(self.ensure_ord().clone()),
            dirty: None,
        }
    }
}

impl<K, V> Default for CowMapInline<K, V> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + fmt::Debug, V: fmt::Debug> fmt::Debug for CowMapInline<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Ord, V: PartialEq> PartialEq for CowMapInline<K, V> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        if let (Some(a), Some(b)) = (self.ord.get(), other.ord.get()) {
            return a == b;
        }
        self.iter()
            .zip(other.iter())
            .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
    }
}

impl<K: Ord + Eq, V: Eq> Eq for CowMapInline<K, V> {}

impl<K: Ord + Borrow<Q>, Q: Ord + ?Sized, V> Index<&Q> for CowMapInline<K, V> {
    type Output = V;

    #[inline]
    fn index(&self, key: &Q) -> &Self::Output {
        self.get(key).expect("CowMapInline::index: key not found")
    }
}

impl<K: Ord + Clone + Borrow<Q>, Q: Ord + ?Sized, V: Clone> IndexMut<&Q> for CowMapInline<K, V> {
    #[inline]
    fn index_mut(&mut self, key: &Q) -> &mut Self::Output {
        self.get_mut(key)
            .expect("CowMapInline::index_mut: key not found")
    }
}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for CowMapInline<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut ord = OrdMap::new();
        for (k, v) in iter {
            ord.insert(k, v);
        }
        Self {
            ord: OnceLock::from(ord),
            dirty: None,
        }
    }
}

impl<K: Ord + Clone, V: Clone> Extend<(K, V)> for CowMapInline<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        let ord = self.ensure_ord_mut();
        for (k, v) in iter {
            ord.insert(k, v);
        }
    }
}

impl<K: Ord + Clone, V: Clone> IntoIterator for CowMapInline<K, V> {
    type Item = (K, V);
    type IntoIter = InlineIntoIter<K, V>;

    #[inline]
    fn into_iter(mut self) -> Self::IntoIter {
        if let Some(dirty) = self.dirty.take() {
            InlineIntoIter {
                inner: InlineIntoIterInner::Dirty(dirty.into_iter()),
            }
        } else {
            let ord = self.ord.take().unwrap_or_default();
            InlineIntoIter {
                inner: InlineIntoIterInner::Ord(ord.into_iter()),
            }
        }
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a CowMapInline<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = InlineIter<'a, K, V>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K: Ord + Clone, V: Clone> IntoIterator for &'a mut CowMapInline<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = std::collections::btree_map::IterMut<'a, K, V>;

    #[track_caller]
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K: Ord + Clone, V: Clone> From<HashMap<K, V>> for CowMapInline<K, V> {
    fn from(map: HashMap<K, V>) -> Self {
        map.into_iter().collect()
    }
}

impl<K: Ord + Clone, V: Clone> From<BTreeMap<K, V>> for CowMapInline<K, V> {
    fn from(map: BTreeMap<K, V>) -> Self {
        map.into_iter().collect()
    }
}

impl<K: Ord + Clone, V: Clone, const N: usize> From<[(K, V); N]> for CowMapInline<K, V> {
    fn from(arr: [(K, V); N]) -> Self {
        arr.into_iter().collect()
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for CowMapInline<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (k, v) in self.iter() {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de, K, V> Deserialize<'de> for CowMapInline<K, V>
where
    K: Ord + Clone + Deserialize<'de>,
    V: Clone + Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CowMapInlineVisitor<K, V>(std::marker::PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for CowMapInlineVisitor<K, V>
        where
            K: Ord + Clone + Deserialize<'de>,
            V: Clone + Deserialize<'de>,
        {
            type Value = CowMapInline<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut ord = OrdMap::new();
                while let Some((k, v)) = access.next_entry::<K, V>()? {
                    ord.insert(k, v);
                }
                Ok(CowMapInline {
                    ord: OnceLock::from(ord),
                    dirty: None,
                })
            }
        }

        deserializer.deserialize_map(CowMapInlineVisitor(std::marker::PhantomData))
    }
}

// ============================================================================
// CowCell<T> — persistent Copy-on-Write cell over Arc<T>
// ============================================================================

/// Copy-on-Write cell wrapping `Arc<T>` for singleton blobs (`#899` §3.1, `#914`).
///
/// - `Clone` is `O(1)` (`Arc::clone`).
/// - `PartialEq` short-circuits on `Arc::ptr_eq` in `O(1)` even when `T: PartialEq + !Eq`.
/// - `get_mut` unshares the inner `T` via `Arc::make_mut` on first write.
#[derive(Debug)]
pub struct CowCell<T>(Arc<T>);

impl<T> CowCell<T> {
    /// Creates a new `CowCell` wrapping `value`.
    #[inline]
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }

    /// Creates a `CowCell` from an existing `Arc<T>`.
    #[inline]
    pub fn from_arc(arc: Arc<T>) -> Self {
        Self(arc)
    }

    /// Returns a shared reference to the inner value.
    #[inline]
    pub fn get(&self) -> &T {
        &self.0
    }

    /// Returns a shared reference to the backing `Arc<T>`.
    #[inline]
    pub fn as_arc(&self) -> &Arc<T> {
        &self.0
    }

    /// Replaces the inner value with a newly allocated `Arc::new(value)`.
    #[inline]
    pub fn set(&mut self, value: T) {
        self.0 = Arc::new(value);
    }

    /// Returns `true` if `self` and `other` point to the exact same `Arc<T>` allocation (`O(1)`).
    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Returns `Some((old, new))` if `self` and `other` differ (`!ptr_eq` and `**self != **other`),
    /// or `None` if they are identical.
    #[inline]
    pub fn diff<'a, 'b>(&'a self, other: &'b Self) -> Option<(&'a T, &'b T)>
    where
        T: PartialEq,
    {
        if self == other {
            None
        } else {
            Some((&self.0, &other.0))
        }
    }
}

impl<T: Clone> CowCell<T> {
    /// Returns a mutable reference to the inner value, cloning `T` via `Arc::make_mut` only if
    /// the `Arc<T>` is currently shared (`strong_count > 1`).
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }

    /// Unwraps the inner value, cloning only if shared.
    #[inline]
    pub fn into_inner(self) -> T {
        Arc::unwrap_or_clone(self.0)
    }
}

impl<T> Clone for CowCell<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for CowCell<T> {
    #[inline]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: PartialEq> PartialEq for CowCell<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl<T: Eq> Eq for CowCell<T> {}

impl<T> Deref for CowCell<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Clone> DerefMut for CowCell<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<T> AsRef<T> for CowCell<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T> From<T> for CowCell<T> {
    #[inline]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> From<Arc<T>> for CowCell<T> {
    #[inline]
    fn from(arc: Arc<T>) -> Self {
        Self::from_arc(arc)
    }
}

impl<T: Serialize> Serialize for CowCell<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for CowCell<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::WorldRepository;
    use crate::types::{IskCents, MarketOrderRecord, SkillQueueEntry, TrainedSkillRecord};
    use proptest::prelude::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    #[derive(Debug, Default)]
    struct Counter {
        clones: AtomicUsize,
        eqs: AtomicUsize,
    }

    #[derive(Debug)]
    struct TrackedRow {
        id: i64,
        value: i64,
        float_field: f64,
        counter: Arc<Counter>,
    }

    impl Clone for TrackedRow {
        fn clone(&self) -> Self {
            self.counter.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                id: self.id,
                value: self.value,
                float_field: self.float_field,
                counter: Arc::clone(&self.counter),
            }
        }
    }

    impl PartialEq for TrackedRow {
        fn eq(&self, other: &Self) -> bool {
            self.counter.eqs.fetch_add(1, Ordering::Relaxed);
            self.id == other.id
                && self.value == other.value
                && self.float_field == other.float_field
        }
    }

    #[test]
    fn imbl_constants_and_nll_drop_invariants_hold() {
        assert_eq!(IMBL_ORD_CHUNK_SIZE, 16);
        assert!(
            !std::mem::needs_drop::<IterMut<'_, i64, MarketOrderRecord>>(),
            "IterMut must have trivial drop so NLL releases borrows at last use"
        );
        assert!(
            !std::mem::needs_drop::<ValuesMut<'_, i64, MarketOrderRecord>>(),
            "ValuesMut must have trivial drop so NLL releases borrows at last use"
        );
    }

    #[test]
    fn row_wrapper_short_circuits_arc_ptr_eq_when_value_is_not_eq() {
        let counter = Arc::new(Counter::default());
        let row = TrackedRow {
            id: 1,
            value: 100,
            float_field: 3.14,
            counter: Arc::clone(&counter),
        };

        // 1. Plain std::sync::Arc<T> where T: PartialEq + !Eq does NOT short-circuit on Arc::ptr_eq
        //    because alloc::sync::Arc gates pointer-equality specialization on `T: Eq`.
        let plain_a = Arc::new(row);
        let plain_b = Arc::clone(&plain_a);
        assert!(Arc::ptr_eq(&plain_a, &plain_b));
        assert_eq!(plain_a, plain_b);
        assert_eq!(
            counter.eqs.load(Ordering::Relaxed),
            1,
            "plain Arc<T> with T: !Eq invokes T::eq even when Arc::ptr_eq is true"
        );

        // 2. Row<T> and CowCell<T> short-circuit on Arc::ptr_eq with 0 T::eq calls.
        counter.eqs.store(0, Ordering::Relaxed);
        let wrapped_a = Row::from_arc(Arc::clone(&plain_a));
        let wrapped_b = wrapped_a.clone();
        assert_eq!(wrapped_a, wrapped_b);
        assert_eq!(
            counter.eqs.load(Ordering::Relaxed),
            0,
            "Row<T> must short-circuit on Arc::ptr_eq without calling T::eq"
        );

        let cell_a = CowCell::from_arc(Arc::clone(&plain_a));
        let cell_b = cell_a.clone();
        assert!(cell_a.ptr_eq(&cell_b));
        assert_eq!(cell_a, cell_b);
        assert!(cell_a.diff(&cell_b).is_none());
        assert_eq!(
            counter.eqs.load(Ordering::Relaxed),
            0,
            "CowCell<T> must short-circuit on Arc::ptr_eq without calling T::eq"
        );
    }

    #[test]
    fn get_mut_clones_exactly_one_row_and_diff_compares_at_most_one_row() {
        let counter = Arc::new(Counter::default());
        let mut map = CowMap::new();
        for id in 0..1_000_i64 {
            map.insert(
                id,
                TrackedRow {
                    id,
                    value: id * 10,
                    float_field: id as f64 * 0.5,
                    counter: Arc::clone(&counter),
                },
            );
        }

        // Snapshot clone is O(1): zero row clones and zero eq comparisons.
        counter.clones.store(0, Ordering::Relaxed);
        counter.eqs.store(0, Ordering::Relaxed);
        let before = map.clone();
        assert!(before.ptr_eq(&map));
        assert_eq!(counter.clones.load(Ordering::Relaxed), 0);
        assert_eq!(counter.eqs.load(Ordering::Relaxed), 0);
        assert_eq!(before.diff(&map).count(), 0);
        assert_eq!(counter.eqs.load(Ordering::Relaxed), 0);

        // Mutate a single row in the middle of the 1,000-entry map.
        let target_key = 517_i64;
        map.get_mut(&target_key).expect("target exists").value += 1;
        assert_eq!(
            counter.clones.load(Ordering::Relaxed),
            1,
            "get_mut must clone only the single mutated row V, never sibling rows in the 16-entry leaf"
        );

        let diffs: Vec<_> = before.diff(&map).collect();
        assert_eq!(diffs.len(), 1);
        assert_eq!(*diffs[0].key(), target_key);
        assert_eq!(diffs[0].before_val().unwrap().value, 5_170);
        assert_eq!(diffs[0].after_val().unwrap().value, 5_171);
        assert_eq!(
            counter.clones.load(Ordering::Relaxed),
            1,
            "diff must not clone any row values"
        );
        assert_eq!(
            counter.eqs.load(Ordering::Relaxed),
            1,
            "diff must invoke V::eq only on the 1 unshared row; all 15 sibling rows in the leaf short-circuit via Arc::ptr_eq"
        );

        // No-op get_mut (dereferencing &mut V without changing value) unshares 1 Arc<V>,
        // runs 1 V::eq during diff, and yields 0 diff items.
        let before_noop = map.clone();
        counter.clones.store(0, Ordering::Relaxed);
        counter.eqs.store(0, Ordering::Relaxed);
        let _noop_ref = map.get_mut(&target_key).expect("target exists");
        assert_eq!(counter.clones.load(Ordering::Relaxed), 1);
        let noop_diffs: Vec<_> = before_noop.diff(&map).collect();
        assert!(
            noop_diffs.is_empty(),
            "no-op get_mut must not emit a spurious Update in diff"
        );
        assert_eq!(counter.eqs.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn world_state_snapshot_clone_shares_all_five_hot_tables_without_copying() {
        let mut snap = WorldRepository::create_demo_snapshot();
        snap.skill_queues.insert(
            90_000_001,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 256_000.0,
                duration_seconds: 3_600.0,
                started_at_unix: Some(1_700_000_000),
            }],
        );
        assert!(!snap.characters.is_empty());
        assert!(!snap.skills.is_empty());
        assert!(!snap.skill_queues.is_empty());
        assert!(!snap.items.is_empty());
        assert!(!snap.market_orders.is_empty());

        let cloned = snap.clone();

        // Root OrdMap pointers are shared across all 5 hot tables.
        assert!(snap.characters.ptr_eq(&cloned.characters));
        assert!(snap.skills.ptr_eq(&cloned.skills));
        assert!(snap.skill_queues.ptr_eq(&cloned.skill_queues));
        assert!(snap.items.ptr_eq(&cloned.items));
        assert!(snap.market_orders.ptr_eq(&cloned.market_orders));

        // Individual row Arc<V> allocations are shared for every key.
        for cid in snap.characters.keys() {
            assert!(snap.characters.row_ptr_eq(&cloned.characters, cid));
        }
        for cid in snap.skills.keys() {
            assert!(snap.skills.row_ptr_eq(&cloned.skills, cid));
        }
        for cid in snap.skill_queues.keys() {
            assert!(snap.skill_queues.row_ptr_eq(&cloned.skill_queues, cid));
        }
        for iid in snap.items.keys() {
            assert!(snap.items.row_ptr_eq(&cloned.items, iid));
        }
        for oid in snap.market_orders.keys() {
            assert!(snap.market_orders.row_ptr_eq(&cloned.market_orders, oid));
        }

        // Mutating 1 item unshares only that item's Arc<InventoryItemRecord>.
        let mut after = cloned;
        let first_item_id = *after.items.keys().next().unwrap();
        after.items.get_mut(&first_item_id).unwrap().quantity += 5;
        assert!(!snap.items.ptr_eq(&after.items));
        assert!(!snap.items.row_ptr_eq(&after.items, &first_item_id));
        for iid in snap.items.keys() {
            if *iid != first_item_id {
                assert!(
                    snap.items.row_ptr_eq(&after.items, iid),
                    "untouched item {iid} must remain Arc-shared"
                );
            }
        }
        assert!(snap.characters.ptr_eq(&after.characters));
        assert!(snap.skills.ptr_eq(&after.skills));
        assert!(snap.skill_queues.ptr_eq(&after.skill_queues));
        assert!(snap.market_orders.ptr_eq(&after.market_orders));
    }

    #[test]
    fn full_touch_counter_tracks_iter_mut_values_mut_and_retain_callsites() {
        let mut map: CowMap<i64, i64> = CowMap::from([(1, 10), (2, 20), (3, 30)]);
        let mut inline_map: CowMapInline<i64, i64> = CowMapInline::from([(1, 10), (2, 20)]);

        let before_total = cow_full_touch_total();
        let line_iter_mut = line!() + 1;
        for (_, v) in map.iter_mut() {
            *v += 1;
        }
        let line_values_mut = line!() + 1;
        for v in map.values_mut() {
            *v += 1;
        }
        let line_retain = line!() + 1;
        map.retain(|&k, _| k != 2);

        let line_inline_values_mut = line!() + 1;
        for v in inline_map.values_mut() {
            *v *= 2;
        }

        let after_total = cow_full_touch_total();
        assert!(
            after_total >= before_total + 4,
            "expected at least 4 full-touch increments, got {before_total} -> {after_total}"
        );

        let sites = cow_full_touch_by_site();
        for expected_line in [
            line_iter_mut,
            line_values_mut,
            line_retain,
            line_inline_values_mut,
        ] {
            let key = format!("{}:{expected_line}", file!());
            assert!(
                sites.get(&key).copied().unwrap_or(0) >= 1,
                "missing full-touch callsite {key} in {sites:?}"
            );
        }
        assert_eq!(map.get(&1), Some(&12));
        assert_eq!(map.get(&2), None);
        assert_eq!(map.get(&3), Some(&32));
        assert_eq!(inline_map.get(&1), Some(&20));
        assert_eq!(inline_map.get(&2), Some(&40));
    }

    #[derive(Debug, Clone)]
    enum MapOp {
        Insert(i64, i64),
        Get(i64),
        GetMutAdd(i64, i64),
        GetMutNoop(i64),
        Remove(i64),
        EntryOrInsert(i64, i64),
        EntryAndModify(i64, i64),
        RetainOddKeys,
        ValuesMutAdd(i64),
    }

    fn arb_map_op() -> impl Strategy<Value = MapOp> {
        prop_oneof![
            4 => (0..40_i64, -1_000..1_000_i64).prop_map(|(k, v)| MapOp::Insert(k, v)),
            3 => (0..40_i64).prop_map(MapOp::Get),
            4 => (0..40_i64, -100..100_i64).prop_map(|(k, d)| MapOp::GetMutAdd(k, d)),
            2 => (0..40_i64).prop_map(MapOp::GetMutNoop),
            3 => (0..40_i64).prop_map(MapOp::Remove),
            2 => (0..40_i64, -500..500_i64).prop_map(|(k, v)| MapOp::EntryOrInsert(k, v)),
            2 => (0..40_i64, -50..50_i64).prop_map(|(k, d)| MapOp::EntryAndModify(k, d)),
            1 => Just(MapOp::RetainOddKeys),
            1 => (-25..25_i64).prop_map(MapOp::ValuesMutAdd),
        ]
    }

    fn apply_op(
        cow: &mut CowMap<i64, i64>,
        inline: &mut CowMapInline<i64, i64>,
        reference: &mut HashMap<i64, i64>,
        op: &MapOp,
    ) {
        match *op {
            MapOp::Insert(k, v) => {
                let r1 = cow.insert(k, v);
                let r2 = inline.insert(k, v);
                let r3 = reference.insert(k, v);
                assert_eq!(r1, r3);
                assert_eq!(r2, r3);
            }
            MapOp::Get(k) => {
                assert_eq!(cow.get(&k), reference.get(&k));
                assert_eq!(inline.get(&k), reference.get(&k));
            }
            MapOp::GetMutAdd(k, delta) => {
                if let Some(v) = cow.get_mut(&k) {
                    *v = v.wrapping_add(delta);
                }
                if let Some(v) = inline.get_mut(&k) {
                    *v = v.wrapping_add(delta);
                }
                if let Some(v) = reference.get_mut(&k) {
                    *v = v.wrapping_add(delta);
                }
            }
            MapOp::GetMutNoop(k) => {
                if let Some(v) = cow.get_mut(&k) {
                    let cur = *v;
                    *v = cur;
                }
                if let Some(v) = inline.get_mut(&k) {
                    let cur = *v;
                    *v = cur;
                }
            }
            MapOp::Remove(k) => {
                let r1 = cow.remove(&k);
                let r2 = inline.remove(&k);
                let r3 = reference.remove(&k);
                assert_eq!(r1, r3);
                assert_eq!(r2, r3);
            }
            MapOp::EntryOrInsert(k, default) => {
                let v1 = *cow.entry(k).or_insert(default);
                let v2 = *inline.entry(k).or_insert(default);
                let v3 = *reference.entry(k).or_insert(default);
                assert_eq!(v1, v3);
                assert_eq!(v2, v3);
            }
            MapOp::EntryAndModify(k, delta) => {
                let v1 = *cow
                    .entry(k)
                    .and_modify(|v| *v = v.wrapping_add(delta))
                    .or_insert(0);
                let v2 = *inline
                    .entry(k)
                    .and_modify(|v| *v = v.wrapping_add(delta))
                    .or_insert(0);
                let v3 = *reference
                    .entry(k)
                    .and_modify(|v| *v = v.wrapping_add(delta))
                    .or_insert(0);
                assert_eq!(v1, v3);
                assert_eq!(v2, v3);
            }
            MapOp::RetainOddKeys => {
                cow.retain(|k, _| k % 2 != 0);
                inline.retain(|k, _| k % 2 != 0);
                reference.retain(|k, _| k % 2 != 0);
            }
            MapOp::ValuesMutAdd(delta) => {
                for v in cow.values_mut() {
                    *v = v.wrapping_add(delta);
                }
                for v in inline.values_mut() {
                    *v = v.wrapping_add(delta);
                }
                for v in reference.values_mut() {
                    *v = v.wrapping_add(delta);
                }
            }
        }
    }

    proptest! {
        #[test]
        fn prop_cow_map_matches_std_hashmap_and_exact_diff(
            pre_ops in proptest::collection::vec(arb_map_op(), 0..60),
            post_ops in proptest::collection::vec(arb_map_op(), 0..60),
        ) {
            let mut cow = CowMap::new();
            let mut inline = CowMapInline::new();
            let mut reference = HashMap::new();

            for op in &pre_ops {
                apply_op(&mut cow, &mut inline, &mut reference, op);
            }

            let before_cow = cow.clone();
            let before_inline = inline.clone();
            let before_ref = reference.clone();

            for op in &post_ops {
                apply_op(&mut cow, &mut inline, &mut reference, op);
            }

            // 1. State equivalence with HashMap
            prop_assert_eq!(cow.len(), reference.len());
            prop_assert_eq!(inline.len(), reference.len());
            let mut expected_sorted: Vec<(i64, i64)> = reference.iter().map(|(&k, &v)| (k, v)).collect();
            expected_sorted.sort_by_key(|(k, _)| *k);
            let actual_cow: Vec<(i64, i64)> = cow.iter().map(|(&k, &v)| (k, v)).collect();
            let actual_inline: Vec<(i64, i64)> = inline.iter().map(|(&k, &v)| (k, v)).collect();
            prop_assert_eq!(&actual_cow, &expected_sorted);
            prop_assert_eq!(&actual_inline, &expected_sorted);

            // 2. Exact diff equivalence against (before_ref, reference)
            let mut all_keys: Vec<i64> = before_ref
                .keys()
                .chain(reference.keys())
                .copied()
                .collect();
            all_keys.sort_unstable();
            all_keys.dedup();

            let mut expected_diffs = Vec::new();
            for k in all_keys {
                match (before_ref.get(&k), reference.get(&k)) {
                    (None, Some(&new_v)) => expected_diffs.push((k, None, Some(new_v))),
                    (Some(&old_v), Some(&new_v)) if old_v != new_v => {
                        expected_diffs.push((k, Some(old_v), Some(new_v)));
                    }
                    (Some(&old_v), None) => expected_diffs.push((k, Some(old_v), None)),
                    _ => {}
                }
            }

            let cow_diffs: Vec<(i64, Option<i64>, Option<i64>)> = before_cow
                .diff(&cow)
                .map(|d| (*d.key(), d.before_val().copied(), d.after_val().copied()))
                .collect();
            let inline_diffs: Vec<(i64, Option<i64>, Option<i64>)> = before_inline
                .diff(&inline)
                .map(|d| (*d.key(), d.before().copied(), d.after().copied()))
                .collect();

            prop_assert_eq!(cow_diffs, expected_diffs.clone());
            prop_assert_eq!(inline_diffs, expected_diffs);

            // 3. Serde JSON roundtrip preserves equality
            let json = serde_json::to_string(&cow).unwrap();
            let roundtrip: CowMap<i64, i64> = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(roundtrip, cow);
        }
    }

    fn sample_market_order(order_id: i64) -> MarketOrderRecord {
        MarketOrderRecord {
            order_id,
            owner_id: 90_000_001 + (order_id % 100),
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34 + (order_id % 50) as u32,
            is_bid: order_id % 2 == 0,
            price: IskCents(500 + order_id),
            vol_entered: 1_000,
            vol_remaining: 1_000,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(0),
        }
    }

    #[test]
    fn cow_map_vs_cow_map_inline_market_order_latency_and_diff() {
        const N: i64 = 10_000;
        let mut cow: CowMap<i64, MarketOrderRecord> = CowMap::new();
        let mut inline: CowMapInline<i64, MarketOrderRecord> = CowMapInline::new();
        for id in 0..N {
            let ord = sample_market_order(id);
            cow.insert(id, ord.clone());
            inline.insert(id, ord);
        }

        let before_cow = cow.clone();
        let before_inline = inline.clone();

        let write_ids = [7_i64, 512, 1_999, 4_096, 6_543, 7_777, 8_888, 9_901];
        for &id in &write_ids {
            cow.get_mut(&id).unwrap().vol_remaining -= 10;
            inline.get_mut(&id).unwrap().vol_remaining -= 10;
        }

        for &id in &write_ids {
            assert_eq!(cow.get(&id).unwrap().vol_remaining, 990);
            assert_eq!(inline.get(&id).unwrap().vol_remaining, 990);
        }

        let cow_diff_keys: Vec<i64> = before_cow.diff(&cow).map(|d| *d.key()).collect();
        let inline_diff_keys: Vec<i64> = before_inline.diff(&inline).map(|d| *d.key()).collect();
        assert_eq!(cow_diff_keys, write_ids);
        assert_eq!(inline_diff_keys, write_ids);

        // Also verify CowMapInline with TrainedSkillRecord
        let mut sk_inline: CowMapInline<u32, TrainedSkillRecord> = CowMapInline::new();
        sk_inline.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        let sk_before = sk_inline.clone();
        sk_inline.get_mut(&3300).unwrap().level = 5;
        let sk_diffs: Vec<_> = sk_before.diff(&sk_inline).collect();
        assert_eq!(sk_diffs.len(), 1);
        assert_eq!(*sk_diffs[0].key(), 3300);
    }

    #[test]
    #[ignore = "1M-row micro-benchmark for CowMap vs CowMapInline (#914 AC 5); run with --ignored --nocapture"]
    fn bench_1m_market_orders_cow_map_vs_inline() {
        const N: i64 = 1_000_000;
        let mut cow: CowMap<i64, MarketOrderRecord> = CowMap::new();
        let mut inline: CowMapInline<i64, MarketOrderRecord> = CowMapInline::new();
        for id in 0..N {
            let ord = sample_market_order(id);
            cow.insert(id, ord.clone());
            inline.insert(id, ord);
        }

        let t_clone_cow = Instant::now();
        let before_cow = cow.clone();
        let clone_cow_ns = t_clone_cow.elapsed().as_nanos();

        let t_clone_inline = Instant::now();
        let before_inline = inline.clone();
        let clone_inline_ns = t_clone_inline.elapsed().as_nanos();

        let write_ids = [
            17_i64, 14_285, 111_111, 271_828, 314_159, 500_000, 777_777, 999_983,
        ];

        let t_get_cow = Instant::now();
        for &id in &write_ids {
            assert_eq!(cow.get(&id).unwrap().vol_remaining, 1_000);
        }
        let get_cow_us = t_get_cow.elapsed().as_micros();

        let t_get_inline = Instant::now();
        for &id in &write_ids {
            assert_eq!(inline.get(&id).unwrap().vol_remaining, 1_000);
        }
        let get_inline_us = t_get_inline.elapsed().as_micros();

        let t_mut_cow = Instant::now();
        for &id in &write_ids {
            cow.get_mut(&id).unwrap().vol_remaining -= 1;
        }
        let mut_cow_us = t_mut_cow.elapsed().as_micros();

        let t_mut_inline = Instant::now();
        for &id in &write_ids {
            inline.get_mut(&id).unwrap().vol_remaining -= 1;
        }
        let mut_inline_us = t_mut_inline.elapsed().as_micros();

        let t_diff_cow = Instant::now();
        let diff_cow_count = before_cow.diff(&cow).count();
        let diff_cow_us = t_diff_cow.elapsed().as_micros();

        let t_diff_inline = Instant::now();
        let diff_inline_count = before_inline.diff(&inline).count();
        let diff_inline_us = t_diff_inline.elapsed().as_micros();

        assert_eq!(diff_cow_count, write_ids.len());
        assert_eq!(diff_inline_count, write_ids.len());
        println!(
            "1M MarketOrderRecord bench (8 writes): \
             CowMap(clone={clone_cow_ns}ns, 8x_get={get_cow_us}us, 8x_get_mut={mut_cow_us}us, diff={diff_cow_us}us) vs \
             CowMapInline(clone={clone_inline_ns}ns, 8x_get={get_inline_us}us, 8x_get_mut={mut_inline_us}us, diff={diff_inline_us}us)"
        );
    }
}
