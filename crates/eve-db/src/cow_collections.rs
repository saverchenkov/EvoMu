//! Persistent Copy-on-Write indexed containers for cold/append collections (`#1181`).
//!
//! Provides:
//! - [`CowIdVec<T>`]: persistent index-keyed `OrdMap` sequence with `O(1)` clone, `O(log N)`
//!   append/lookup/truncate/entity-ID containment, `O(log N)` cumulative prefix hash, and
//!   `O(k log N)` `Unchanged`/`AppendOnly`/`Rewritten` diff classification (`wallet_journal`, `mails`).
//! - [`CowLpLogs`]: indexed wrapper around `CowMap<i64, Vec<LpRewardLogEntry>>` with an
//!   incremental `OrdMap<i64, u32>` `log_id` index (`char_lp_logs`).
//! - [`CowActiveMissions`]: `Arc`-backed active mission map with `O(1)` clone, `Deref` to
//!   `HashMap<(i64, u32), ActiveMissionRecord>`, and an incremental `OrdMap<i64, u32>` index over
//!   `spawned_npc_ball_ids` (`active_missions`).

use crate::cow::{CowMap, CowMapEntry};
use crate::snapshot_ext::LpRewardLogEntry;
use crate::sqlite_store::fingerprints::canonical_fp;
use crate::types::{ActiveMissionRecord, MailRecord, WalletJournalEntry};
use imbl::OrdMap;
use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use std::collections::{hash_map, HashMap};
use std::fmt;
use std::iter::FusedIterator;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::sync::Arc;

const EMPTY_VEC_HASH: u64 = 0xCBF2_9CE4_8422_2325;

/// Extracts the entity identifier indexed for O(log N) allocation collision checks.
pub trait HasEntityId {
    fn entity_id(&self) -> i64;
}

impl HasEntityId for WalletJournalEntry {
    #[inline]
    fn entity_id(&self) -> i64 {
        self.entry_id
    }
}

impl HasEntityId for MailRecord {
    #[inline]
    fn entity_id(&self) -> i64 {
        self.message_id
    }
}

#[inline]
fn inc_id(counts: &mut OrdMap<i64, u32>, id: i64) {
    if id <= 0 {
        return;
    }
    let next = counts.get(&id).copied().unwrap_or(0).saturating_add(1);
    counts.insert(id, next);
}

#[inline]
fn dec_id(counts: &mut OrdMap<i64, u32>, id: i64) {
    if id <= 0 {
        return;
    }
    if let Some(&prev) = counts.get(&id) {
        if prev <= 1 {
            counts.remove(&id);
        } else {
            counts.insert(id, prev - 1);
        }
    }
}

#[inline]
fn step_vec_hash<T: Serialize>(prev_hash: u64, idx: usize, val: &T) -> u64 {
    let elem_fp = canonical_fp(val);
    let mut h = prev_hash ^ ((idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    h ^= elem_fp;
    h = h.wrapping_mul(0x100_0000_01B3);
    h ^= h >> 33;
    if h == 0 {
        1
    } else {
        h
    }
}

/// Result of classifying changes between two [`CowIdVec`] snapshots in `O(k log N)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowVecDiff {
    Unchanged,
    AppendOnly { old_len: usize },
    Rewritten,
}

#[derive(Debug)]
struct CowVecEntry<T> {
    row: Arc<T>,
    cum_hash: u64,
}

impl<T> Clone for CowVecEntry<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            row: Arc::clone(&self.row),
            cum_hash: self.cum_hash,
        }
    }
}

impl<T: PartialEq> PartialEq for CowVecEntry<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.row, &other.row) || *self.row == *other.row
    }
}

impl<T: Eq> Eq for CowVecEntry<T> {}

/// Persistent Copy-on-Write sequence indexed by position and `HasEntityId`.
#[derive(Clone)]
pub struct CowIdVec<T> {
    ord: OrdMap<usize, CowVecEntry<T>>,
    id_counts: OrdMap<i64, u32>,
    dirty_from: usize,
    stale_id_checks: Vec<(usize, i64)>,
}

impl<T> CowIdVec<T> {
    /// Creates an empty [`CowIdVec`].
    #[inline]
    pub fn new() -> Self {
        Self {
            ord: OrdMap::new(),
            id_counts: OrdMap::new(),
            dirty_from: usize::MAX,
            stale_id_checks: Vec::new(),
        }
    }

    /// Creates an empty [`CowIdVec`] (capacity hint accepted for `Vec` call-site compatibility).
    #[inline]
    pub fn with_capacity(_capacity: usize) -> Self {
        Self::new()
    }

    /// Capacity hint no-op for `Vec` call-site compatibility.
    #[inline]
    pub fn reserve(&mut self, _additional: usize) {}

    /// Returns the number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.ord.len()
    }

    /// Returns `true` if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ord.is_empty()
    }

    /// Returns `true` if `self` and `other` share the exact underlying `OrdMap` root (`O(1)`).
    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.ord.ptr_eq(&other.ord)
    }

    /// Returns the element at `index`, if in bounds (`O(log N)`).
    #[inline]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.ord.get(&index).map(|e| &*e.row)
    }

    /// Returns the underlying `Arc<T>` at `index` (for zero-clone pointer assertions in tests).
    #[inline]
    pub fn get_arc(&self, index: usize) -> Option<&Arc<T>> {
        self.ord.get(&index).map(|e| &e.row)
    }

    /// Returns the first element (`O(log N)`).
    #[inline]
    pub fn first(&self) -> Option<&T> {
        self.get(0)
    }

    /// Returns the last element (`O(log N)`).
    #[inline]
    pub fn last(&self) -> Option<&T> {
        let len = self.ord.len();
        if len == 0 {
            None
        } else {
            self.get(len - 1)
        }
    }

    /// Returns an iterator over elements in index order.
    #[inline]
    pub fn iter(&self) -> CowIdVecIter<'_, T> {
        CowIdVecIter {
            inner: self.ord.iter(),
        }
    }

    /// Iterates `(index, &T)` starting at `start` in `O(k log N)` for `k = len - start`.
    #[inline]
    pub fn iter_from(&self, start: usize) -> impl Iterator<Item = (usize, &T)> {
        self.ord
            .range(start..)
            .map(|(&idx, entry)| (idx, &*entry.row))
    }

    /// Clears all elements in `O(1)`.
    #[inline]
    pub fn clear(&mut self) {
        self.ord.clear();
        self.id_counts.clear();
        self.dirty_from = usize::MAX;
        self.stale_id_checks.clear();
    }
}

impl<T: HasEntityId> CowIdVec<T> {
    fn reconcile_stale_ids(&mut self) {
        if self.stale_id_checks.is_empty() {
            return;
        }
        let checks = std::mem::take(&mut self.stale_id_checks);
        for (idx, old_id) in checks {
            if let Some(entry) = self.ord.get(&idx) {
                let new_id = entry.row.entity_id();
                if new_id != old_id {
                    dec_id(&mut self.id_counts, old_id);
                    inc_id(&mut self.id_counts, new_id);
                }
            } else {
                dec_id(&mut self.id_counts, old_id);
            }
        }
    }

    /// Returns `true` if any element has `entity_id() == id` in `O(log N)` (or `O(log N + stale)`).
    #[inline]
    pub fn contains_id(&self, id: i64) -> bool {
        if id <= 0 {
            return false;
        }
        if self.stale_id_checks.is_empty() {
            return self.id_counts.contains_key(&id);
        }
        let mut net = self.id_counts.get(&id).copied().unwrap_or(0) as isize;
        for &(idx, old_id) in &self.stale_id_checks {
            if old_id == id {
                net -= 1;
            }
            if self.ord.get(&idx).is_some_and(|e| e.row.entity_id() == id) {
                net += 1;
            }
        }
        net > 0
    }
}

impl<T: HasEntityId + Serialize> CowIdVec<T> {
    /// Appends `value` in `O(log N)` without cloning existing elements.
    pub fn push(&mut self, value: T) {
        if !self.stale_id_checks.is_empty() {
            self.reconcile_stale_ids();
        }
        let idx = self.ord.len();
        inc_id(&mut self.id_counts, value.entity_id());
        let cum_hash = if self.dirty_from >= idx {
            let prev = if idx == 0 {
                EMPTY_VEC_HASH
            } else {
                self.ord
                    .get(&(idx - 1))
                    .map_or(EMPTY_VEC_HASH, |e| e.cum_hash)
            };
            step_vec_hash(prev, idx, &value)
        } else {
            0
        };
        self.ord.insert(
            idx,
            CowVecEntry {
                row: Arc::new(value),
                cum_hash,
            },
        );
    }

    /// Repairs any deferred prefix hashes / ID counters from `IndexMut` writes.
    pub fn reconcile_indexes(&mut self) {
        self.reconcile_stale_ids();
        let len = self.ord.len();
        if self.dirty_from >= len {
            self.dirty_from = usize::MAX;
            return;
        }
        let start = self.dirty_from;
        let mut prev = if start == 0 {
            EMPTY_VEC_HASH
        } else {
            self.ord
                .get(&(start - 1))
                .map_or(EMPTY_VEC_HASH, |e| e.cum_hash)
        };
        for idx in start..len {
            if let Some(entry) = self.ord.get_mut(&idx) {
                let h = step_vec_hash(prev, idx, &*entry.row);
                entry.cum_hash = h;
                prev = h;
            }
        }
        self.dirty_from = usize::MAX;
    }

    /// Retains only elements satisfying `pred`, preserving order and rebuilding indexes.
    pub fn retain<F>(&mut self, mut pred: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.reconcile_stale_ids();
        let mut next_ord = OrdMap::new();
        let mut next_ids = OrdMap::new();
        let mut prev = EMPTY_VEC_HASH;
        let mut out_idx = 0usize;
        for (_, entry) in self.ord.iter() {
            if pred(&*entry.row) {
                inc_id(&mut next_ids, entry.row.entity_id());
                let cum_hash = step_vec_hash(prev, out_idx, &*entry.row);
                prev = cum_hash;
                next_ord.insert(
                    out_idx,
                    CowVecEntry {
                        row: Arc::clone(&entry.row),
                        cum_hash,
                    },
                );
                out_idx += 1;
            }
        }
        self.ord = next_ord;
        self.id_counts = next_ids;
        self.dirty_from = usize::MAX;
    }
}

impl<T: Clone + HasEntityId> CowIdVec<T> {
    /// Pops and returns the last element in `O(log N)`.
    pub fn pop(&mut self) -> Option<T> {
        self.reconcile_stale_ids();
        let len = self.ord.len();
        if len == 0 {
            return None;
        }
        let last_idx = len - 1;
        let entry = self.ord.remove(&last_idx)?;
        dec_id(&mut self.id_counts, entry.row.entity_id());
        if self.dirty_from >= last_idx {
            self.dirty_from = usize::MAX;
        }
        Some(Arc::try_unwrap(entry.row).unwrap_or_else(|a| (*a).clone()))
    }

    /// Truncates the sequence to `new_len` elements in `O(k log N)` for `k` removed tail items.
    pub fn truncate(&mut self, new_len: usize) {
        if new_len == 0 {
            self.clear();
            return;
        }
        if new_len >= self.ord.len() {
            return;
        }
        while self.ord.len() > new_len {
            let _ = self.pop();
        }
    }
}

impl<T: Serialize> CowIdVec<T> {
    /// Returns the deterministic cumulative hash of the `0..prefix_len` slice in `O(log N)`
    /// whenever `dirty_from >= prefix_len`.
    pub fn prefix_hash(&self, prefix_len: usize) -> u64 {
        let n = prefix_len.min(self.ord.len());
        if n == 0 {
            return EMPTY_VEC_HASH;
        }
        if self.dirty_from >= n {
            return self
                .ord
                .get(&(n - 1))
                .map_or(EMPTY_VEC_HASH, |e| e.cum_hash);
        }
        let start = self.dirty_from;
        let mut prev = if start == 0 {
            EMPTY_VEC_HASH
        } else {
            self.ord
                .get(&(start - 1))
                .map_or(EMPTY_VEC_HASH, |e| e.cum_hash)
        };
        for (idx, entry) in self.ord.range(start..n) {
            prev = step_vec_hash(prev, *idx, &*entry.row);
        }
        prev
    }
}

impl<T: PartialEq> CowIdVec<T> {
    /// Classifies how `after` differs from `self` in `O(k log N)` without scanning unchanged prefixes.
    pub fn classify_diff(&self, after: &Self) -> CowVecDiff {
        if self.ptr_eq(after) {
            return CowVecDiff::Unchanged;
        }
        let old_len = self.len();
        let new_len = after.len();
        if new_len < old_len {
            return CowVecDiff::Rewritten;
        }
        for item in self.ord.diff(&after.ord) {
            match item {
                imbl::ordmap::DiffItem::Add(idx, _) => {
                    if *idx < old_len {
                        return CowVecDiff::Rewritten;
                    }
                }
                imbl::ordmap::DiffItem::Remove(_, _) => {
                    return CowVecDiff::Rewritten;
                }
                imbl::ordmap::DiffItem::Update { old: (idx, a), new: (_, b) } => {
                    if *idx < old_len && a != b {
                        return CowVecDiff::Rewritten;
                    }
                }
            }
        }
        if old_len == new_len {
            CowVecDiff::Unchanged
        } else {
            CowVecDiff::AppendOnly { old_len }
        }
    }
}

impl<T> Default for CowIdVec<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for CowIdVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: PartialEq> PartialEq for CowIdVec<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        if self.len() != other.len() {
            return false;
        }
        for item in self.ord.diff(&other.ord) {
            match item {
                imbl::ordmap::DiffItem::Add(_, _) | imbl::ordmap::DiffItem::Remove(_, _) => {
                    return false;
                }
                imbl::ordmap::DiffItem::Update {
                    old: (_, a),
                    new: (_, b),
                } => {
                    if a != b {
                        return false;
                    }
                }
            }
        }
        true
    }
}

impl<T: Eq> Eq for CowIdVec<T> {}

impl<T> Index<usize> for CowIdVec<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        self.ord.get(&index).map(|e| &*e.row).unwrap_or_else(|| {
            panic!(
                "index out of bounds: the len is {} but the index is {index}",
                self.ord.len()
            )
        })
    }
}

impl<T: Clone + HasEntityId> IndexMut<usize> for CowIdVec<T> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let len = self.ord.len();
        let old_id = self
            .ord
            .get(&index)
            .map(|e| e.row.entity_id())
            .unwrap_or_else(|| panic!("index out of bounds: the len is {len} but the index is {index}"));
        if !self.stale_id_checks.iter().any(|&(i, _)| i == index) {
            self.stale_id_checks.push((index, old_id));
        }
        self.dirty_from = self.dirty_from.min(index);
        let entry = self.ord.get_mut(&index).expect("index verified in bounds");
        Arc::make_mut(&mut entry.row)
    }
}

impl<T: HasEntityId + Serialize> From<Vec<T>> for CowIdVec<T> {
    fn from(vec: Vec<T>) -> Self {
        let mut ord = OrdMap::new();
        let mut id_counts = OrdMap::new();
        let mut prev = EMPTY_VEC_HASH;
        for (idx, item) in vec.into_iter().enumerate() {
            inc_id(&mut id_counts, item.entity_id());
            let cum_hash = step_vec_hash(prev, idx, &item);
            prev = cum_hash;
            ord.insert(
                idx,
                CowVecEntry {
                    row: Arc::new(item),
                    cum_hash,
                },
            );
        }
        Self {
            ord,
            id_counts,
            dirty_from: usize::MAX,
            stale_id_checks: Vec::new(),
        }
    }
}

impl<T: HasEntityId + Serialize> FromIterator<T> for CowIdVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<Vec<T>>())
    }
}

impl<T: HasEntityId + Serialize> Extend<T> for CowIdVec<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

impl<T: Serialize> Serialize for CowIdVec<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de, T: Deserialize<'de> + HasEntityId + Serialize> Deserialize<'de> for CowIdVec<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(Self::from)
    }
}

/// Iterator over `&T` elements of a [`CowIdVec`] in ascending index order.
pub struct CowIdVecIter<'a, T> {
    inner: imbl::ordmap::Iter<'a, usize, CowVecEntry<T>, imbl::shared_ptr::DefaultSharedPtr>,
}

impl<'a, T> Clone for CowIdVecIter<'a, T> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, T> Iterator for CowIdVecIter<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, entry)| &*entry.row)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a, T> DoubleEndedIterator for CowIdVecIter<'a, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, entry)| &*entry.row)
    }
}

impl<'a, T> ExactSizeIterator for CowIdVecIter<'a, T> {}
impl<'a, T> FusedIterator for CowIdVecIter<'a, T> {}

impl<'a, T> IntoIterator for &'a CowIdVec<T> {
    type Item = &'a T;
    type IntoIter = CowIdVecIter<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Owning iterator over `T` elements of a [`CowIdVec`].
pub struct CowIdVecIntoIter<T> {
    inner: imbl::ordmap::ConsumingIter<usize, CowVecEntry<T>, imbl::shared_ptr::DefaultSharedPtr>,
}

impl<T: Clone> Iterator for CowIdVecIntoIter<T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(_, entry)| Arc::try_unwrap(entry.row).unwrap_or_else(|a| (*a).clone()))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T: Clone> IntoIterator for CowIdVec<T> {
    type Item = T;
    type IntoIter = CowIdVecIntoIter<T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        CowIdVecIntoIter {
            inner: self.ord.into_iter(),
        }
    }
}

// ============================================================================
// CowLpLogs (`char_lp_logs`: indexed CowMap<i64, Vec<LpRewardLogEntry>>)
// ============================================================================

/// Indexed Copy-on-Write LP reward log map with `O(log N)` `log_id` membership checks.
#[derive(Debug, Clone)]
pub struct CowLpLogs {
    map: CowMap<i64, Vec<LpRewardLogEntry>>,
    id_counts: OrdMap<i64, u32>,
    dirty_ids: bool,
}

impl CowLpLogs {
    /// Creates an empty [`CowLpLogs`].
    #[inline]
    pub fn new() -> Self {
        Self {
            map: CowMap::new(),
            id_counts: OrdMap::new(),
            dirty_ids: false,
        }
    }

    /// Creates an empty [`CowLpLogs`] (capacity hint compatibility).
    #[inline]
    pub fn with_capacity(_capacity: usize) -> Self {
        Self::new()
    }

    /// Returns `true` if `self` and `other` share the exact underlying `CowMap` pointer (`O(1)`).
    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.map.ptr_eq(&other.map)
    }

    /// Returns the underlying `&CowMap` for direct persistence diffing.
    #[inline]
    pub fn as_cow_map(&self) -> &CowMap<i64, Vec<LpRewardLogEntry>> {
        &self.map
    }

    /// Returns `true` if any log entry carries `log_id == id` (`O(log N)` when clean).
    #[inline]
    pub fn contains_log_id(&self, id: i64) -> bool {
        if id <= 0 {
            return false;
        }
        if !self.dirty_ids {
            return self.id_counts.contains_key(&id);
        }
        self.map.values().flatten().any(|l| l.log_id == id)
    }

    /// Rebuilds the `log_id` index if bulk mutable iteration (`values_mut`/`iter_mut`) occurred.
    pub fn reconcile_id_index(&mut self) {
        if !self.dirty_ids {
            return;
        }
        let mut counts = OrdMap::new();
        for logs in self.map.values() {
            for entry in logs {
                inc_id(&mut counts, entry.log_id);
            }
        }
        self.id_counts = counts;
        self.dirty_ids = false;
    }

    #[inline]
    pub fn clear(&mut self) {
        self.map.clear();
        self.id_counts.clear();
        self.dirty_ids = false;
    }

    pub fn insert(
        &mut self,
        char_id: i64,
        logs: Vec<LpRewardLogEntry>,
    ) -> Option<Vec<LpRewardLogEntry>> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        for entry in &logs {
            inc_id(&mut self.id_counts, entry.log_id);
        }
        let prev = self.map.insert(char_id, logs);
        if let Some(ref old_logs) = prev {
            for entry in old_logs {
                dec_id(&mut self.id_counts, entry.log_id);
            }
        }
        prev
    }

    pub fn remove(&mut self, char_id: &i64) -> Option<Vec<LpRewardLogEntry>> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        let prev = self.map.remove(char_id);
        if let Some(ref old_logs) = prev {
            for entry in old_logs {
                dec_id(&mut self.id_counts, entry.log_id);
            }
        }
        prev
    }

    pub fn remove_entry(&mut self, char_id: &i64) -> Option<(i64, Vec<LpRewardLogEntry>)> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        let prev = self.map.remove_entry(char_id);
        if let Some((_, ref old_logs)) = prev {
            for entry in old_logs {
                dec_id(&mut self.id_counts, entry.log_id);
            }
        }
        prev
    }

    pub fn get_mut(&mut self, char_id: &i64) -> Option<LpLogVecMut<'_>> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        let old_ids: Vec<i64> = self
            .map
            .get(char_id)
            .map(|v| v.iter().map(|e| e.log_id).collect())?;
        let vec = self.map.get_mut(char_id)?;
        Some(LpLogVecMut {
            vec,
            old_ids,
            id_counts: &mut self.id_counts,
        })
    }

    pub fn entry(&mut self, char_id: i64) -> LpLogEntryMut<'_> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        LpLogEntryMut {
            inner: self.map.entry(char_id),
            id_counts: &mut self.id_counts,
        }
    }

    pub fn values_mut(&mut self) -> crate::cow::ValuesMut<'_, i64, Vec<LpRewardLogEntry>> {
        self.dirty_ids = true;
        self.map.values_mut()
    }

    pub fn iter_mut(&mut self) -> crate::cow::IterMut<'_, i64, Vec<LpRewardLogEntry>> {
        self.dirty_ids = true;
        self.map.iter_mut()
    }

    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&i64, &mut Vec<LpRewardLogEntry>) -> bool,
    {
        self.map.retain(f);
        self.dirty_ids = true;
        self.reconcile_id_index();
    }
}

impl Default for CowLpLogs {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for CowLpLogs {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.map.ptr_eq(&other.map) || self.map == other.map
    }
}

impl Eq for CowLpLogs {}

impl Deref for CowLpLogs {
    type Target = CowMap<i64, Vec<LpRewardLogEntry>>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl Index<&i64> for CowLpLogs {
    type Output = Vec<LpRewardLogEntry>;

    #[inline]
    fn index(&self, key: &i64) -> &Self::Output {
        &self.map[key]
    }
}

impl<'a> IntoIterator for &'a CowLpLogs {
    type Item = (&'a i64, &'a Vec<LpRewardLogEntry>);
    type IntoIter = crate::cow::Iter<'a, i64, Vec<LpRewardLogEntry>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl From<CowMap<i64, Vec<LpRewardLogEntry>>> for CowLpLogs {
    fn from(map: CowMap<i64, Vec<LpRewardLogEntry>>) -> Self {
        let mut id_counts = OrdMap::new();
        for logs in map.values() {
            for entry in logs {
                inc_id(&mut id_counts, entry.log_id);
            }
        }
        Self {
            map,
            id_counts,
            dirty_ids: false,
        }
    }
}

impl FromIterator<(i64, Vec<LpRewardLogEntry>)> for CowLpLogs {
    fn from_iter<I: IntoIterator<Item = (i64, Vec<LpRewardLogEntry>)>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<CowMap<i64, Vec<LpRewardLogEntry>>>())
    }
}

impl Serialize for CowLpLogs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.map.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CowLpLogs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CowMap::<i64, Vec<LpRewardLogEntry>>::deserialize(deserializer).map(Self::from)
    }
}

/// Scoped mutable borrow of a character's LP log vector that updates `id_counts` on drop.
pub struct LpLogVecMut<'a> {
    vec: &'a mut Vec<LpRewardLogEntry>,
    old_ids: Vec<i64>,
    id_counts: &'a mut OrdMap<i64, u32>,
}

impl<'a> Deref for LpLogVecMut<'a> {
    type Target = Vec<LpRewardLogEntry>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.vec
    }
}

impl<'a> DerefMut for LpLogVecMut<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.vec
    }
}

impl<'a> Drop for LpLogVecMut<'a> {
    fn drop(&mut self) {
        if self.old_ids.len() == self.vec.len()
            && self
                .old_ids
                .iter()
                .zip(self.vec.iter())
                .all(|(&old, new)| old == new.log_id)
        {
            return;
        }
        for &old_id in &self.old_ids {
            dec_id(self.id_counts, old_id);
        }
        for entry in self.vec.iter() {
            inc_id(self.id_counts, entry.log_id);
        }
    }
}

/// Entry view for [`CowLpLogs`].
pub struct LpLogEntryMut<'a> {
    inner: CowMapEntry<'a, i64, Vec<LpRewardLogEntry>>,
    id_counts: &'a mut OrdMap<i64, u32>,
}

impl<'a> LpLogEntryMut<'a> {
    pub fn or_insert(self, default: Vec<LpRewardLogEntry>) -> LpLogVecMut<'a> {
        match self.inner {
            CowMapEntry::Occupied(occ) => {
                let old_ids: Vec<i64> = occ.get().iter().map(|e| e.log_id).collect();
                LpLogVecMut {
                    vec: occ.into_mut(),
                    old_ids,
                    id_counts: self.id_counts,
                }
            }
            CowMapEntry::Vacant(vac) => LpLogVecMut {
                vec: vac.insert(default),
                old_ids: Vec::new(),
                id_counts: self.id_counts,
            },
        }
    }

    pub fn or_insert_with<F: FnOnce() -> Vec<LpRewardLogEntry>>(self, default: F) -> LpLogVecMut<'a> {
        match self.inner {
            CowMapEntry::Occupied(occ) => {
                let old_ids: Vec<i64> = occ.get().iter().map(|e| e.log_id).collect();
                LpLogVecMut {
                    vec: occ.into_mut(),
                    old_ids,
                    id_counts: self.id_counts,
                }
            }
            CowMapEntry::Vacant(vac) => LpLogVecMut {
                vec: vac.insert(default()),
                old_ids: Vec::new(),
                id_counts: self.id_counts,
            },
        }
    }

    pub fn or_default(self) -> LpLogVecMut<'a> {
        self.or_insert_with(Vec::new)
    }
}

// ============================================================================
// CowActiveMissions (`active_missions`: Arc<HashMap> + npc_id_counts index)
// ============================================================================

/// Copy-on-Write active mission map with `O(1)` clone, `Deref` to `HashMap`, and `O(log N)`
/// `spawned_npc_ball_ids` membership checks.
#[derive(Debug, Clone)]
pub struct CowActiveMissions {
    inner: Arc<HashMap<(i64, u32), ActiveMissionRecord>>,
    npc_id_counts: OrdMap<i64, u32>,
    dirty_ids: bool,
}

impl CowActiveMissions {
    /// Creates an empty [`CowActiveMissions`].
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HashMap::new()),
            npc_id_counts: OrdMap::new(),
            dirty_ids: false,
        }
    }

    /// Creates an empty [`CowActiveMissions`].
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(HashMap::with_capacity(capacity)),
            npc_id_counts: OrdMap::new(),
            dirty_ids: false,
        }
    }

    /// Returns `true` if `self` and `other` share the exact backing `Arc<HashMap>` (`O(1)`).
    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Returns `true` if any active mission references `npc_id` in `spawned_npc_ball_ids` (`O(log N)`).
    #[inline]
    pub fn contains_spawned_npc_id(&self, npc_id: i64) -> bool {
        if npc_id <= 0 {
            return false;
        }
        if !self.dirty_ids {
            return self.npc_id_counts.contains_key(&npc_id);
        }
        self.inner
            .values()
            .any(|m| m.spawned_npc_ball_ids.contains(&npc_id))
    }

    /// Returns `true` if mutable access has potentially invalidated the cached NPC ID index.
    #[inline]
    pub fn is_dirty_ids(&self) -> bool {
        self.dirty_ids
    }

    /// Rebuilds the `spawned_npc_ball_ids` index if mutable access (`get_mut`/`entry`/`iter_mut`) occurred.
    pub fn reconcile_id_index(&mut self) {
        if !self.dirty_ids {
            return;
        }
        let mut counts = OrdMap::new();
        for rec in self.inner.values() {
            for &id in &rec.spawned_npc_ball_ids {
                inc_id(&mut counts, id);
            }
        }
        self.npc_id_counts = counts;
        self.dirty_ids = false;
    }

    #[inline]
    pub fn clear(&mut self) {
        self.inner = Arc::new(HashMap::new());
        self.npc_id_counts.clear();
        self.dirty_ids = false;
    }

    pub fn insert(
        &mut self,
        key: (i64, u32),
        value: ActiveMissionRecord,
    ) -> Option<ActiveMissionRecord> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        for &id in &value.spawned_npc_ball_ids {
            inc_id(&mut self.npc_id_counts, id);
        }
        let prev = Arc::make_mut(&mut self.inner).insert(key, value);
        if let Some(ref old_rec) = prev {
            for &id in &old_rec.spawned_npc_ball_ids {
                dec_id(&mut self.npc_id_counts, id);
            }
        }
        prev
    }

    pub fn remove(&mut self, key: &(i64, u32)) -> Option<ActiveMissionRecord> {
        if self.dirty_ids {
            self.reconcile_id_index();
        }
        let prev = Arc::make_mut(&mut self.inner).remove(key);
        if let Some(ref old_rec) = prev {
            for &id in &old_rec.spawned_npc_ball_ids {
                dec_id(&mut self.npc_id_counts, id);
            }
        }
        prev
    }

    pub fn get_mut(&mut self, key: &(i64, u32)) -> Option<&mut ActiveMissionRecord> {
        let rec = Arc::make_mut(&mut self.inner).get_mut(key)?;
        self.dirty_ids = true;
        Some(rec)
    }

    pub fn entry(
        &mut self,
        key: (i64, u32),
    ) -> hash_map::Entry<'_, (i64, u32), ActiveMissionRecord> {
        self.dirty_ids = true;
        Arc::make_mut(&mut self.inner).entry(key)
    }

    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&(i64, u32), &mut ActiveMissionRecord) -> bool,
    {
        Arc::make_mut(&mut self.inner).retain(f);
        self.dirty_ids = true;
        self.reconcile_id_index();
    }

    pub fn iter_mut(&mut self) -> hash_map::IterMut<'_, (i64, u32), ActiveMissionRecord> {
        self.dirty_ids = true;
        Arc::make_mut(&mut self.inner).iter_mut()
    }

    pub fn values_mut(&mut self) -> hash_map::ValuesMut<'_, (i64, u32), ActiveMissionRecord> {
        self.dirty_ids = true;
        Arc::make_mut(&mut self.inner).values_mut()
    }
}

impl Default for CowActiveMissions {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for CowActiveMissions {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner) || *self.inner == *other.inner
    }
}

impl Deref for CowActiveMissions {
    type Target = HashMap<(i64, u32), ActiveMissionRecord>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> IntoIterator for &'a CowActiveMissions {
    type Item = (&'a (i64, u32), &'a ActiveMissionRecord);
    type IntoIter = hash_map::Iter<'a, (i64, u32), ActiveMissionRecord>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

impl From<HashMap<(i64, u32), ActiveMissionRecord>> for CowActiveMissions {
    fn from(map: HashMap<(i64, u32), ActiveMissionRecord>) -> Self {
        let mut npc_id_counts = OrdMap::new();
        for rec in map.values() {
            for &id in &rec.spawned_npc_ball_ids {
                inc_id(&mut npc_id_counts, id);
            }
        }
        Self {
            inner: Arc::new(map),
            npc_id_counts,
            dirty_ids: false,
        }
    }
}

impl FromIterator<((i64, u32), ActiveMissionRecord)> for CowActiveMissions {
    fn from_iter<I: IntoIterator<Item = ((i64, u32), ActiveMissionRecord)>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<HashMap<_, _>>())
    }
}

impl Serialize for CowActiveMissions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CowActiveMissions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HashMap::<(i64, u32), ActiveMissionRecord>::deserialize(deserializer).map(Self::from)
    }
}

pub mod cow_active_missions_tuple_serde {
    use super::CowActiveMissions;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        map: &CowActiveMissions,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        crate::durable_domain_types::tuple_map_serde::serialize(map, serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CowActiveMissions, D::Error> {
        crate::durable_domain_types::tuple_map_serde::deserialize(deserializer)
            .map(CowActiveMissions::from)
    }
}
