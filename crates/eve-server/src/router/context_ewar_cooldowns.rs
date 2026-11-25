//! Per-system EWAR and module reactivation cooldown helpers (`impl ServerContext`).

use super::context::ServerContext;
use crate::system_actor::ModuleCooldownEntry;

impl ServerContext {
    pub(crate) fn resolve_ship_system_opt(&self, ship_id: i64) -> Option<u32> {
        if let Some(sys_id) = self.systems.find_ball_system_sync(ship_id) {
            return Some(sys_id);
        }
        self.db.with_snapshot(|s| {
            let char_sys = s
                .characters
                .values()
                .find(|c| c.active_ship_id == ship_id && c.solar_system_id > 0)
                .map(|c| c.solar_system_id);
            let item_sys = s.items.get(&ship_id).and_then(|it| {
                if (30_000_000..=33_000_000).contains(&it.location_id) {
                    Some(it.location_id as u32)
                } else if let Some(st) = s.structures.get(&it.location_id) {
                    Some(st.solar_system_id)
                } else {
                    self.sde
                        .stations
                        .get(&it.location_id)
                        .map(|st| st.solar_system_id)
                        .filter(|&sid| sid > 0)
                }
            });
            char_sys.or(item_sys)
        })
    }

    fn resolve_ship_cooldown_clock(&self, ship_id: i64, module_id: i64) -> (u32, Option<u32>, f64) {
        let type_id = self
            .db
            .with_snapshot(|s| s.items.get(&module_id).map(|i| i.type_id).unwrap_or(0));
        let sys_opt = self.resolve_ship_system_opt(ship_id);
        let cur_sys_sim_s = sys_opt
            .map(|sid| self.systems.get_system_sim_elapsed(sid))
            .unwrap_or(0.0);
        (type_id, sys_opt, cur_sys_sim_s)
    }

    pub(crate) fn resolve_sys_tidi_mode(&self, sys_opt: Option<u32>) -> (f64, bool) {
        let Some(sid) = sys_opt.filter(|&s| s > 0) else {
            return (1.0, false);
        };
        let db_read_safe = !eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_WORLD_INNER);
        self.systems.resolve_effective_tidi_mode(sid, db_read_safe)
    }

    pub(crate) fn remaining_cooldown_secs_with_tidi(
        entry: &ModuleCooldownEntry,
        now_unix_f64: f64,
        sys_opt: Option<u32>,
        cur_sys_sim_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) -> f64 {
        crate::system_actor::remaining_cooldown_secs_with_tidi(
            entry,
            now_unix_f64,
            sys_opt,
            cur_sys_sim_s,
            tidi_factor,
            has_tidi_history,
        )
    }

    fn resolve_best_active_cooldown(
        &self,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
        now_unix_f64: f64,
    ) -> Option<(Option<u32>, ModuleCooldownEntry, f64, f64, bool)> {
        let key = (ship_id, module_id, type_id);
        let mut best: Option<(Option<u32>, ModuleCooldownEntry, f64, f64, bool)> = None;
        let mut consider =
            |sys_opt: Option<u32>,
             entry: ModuleCooldownEntry,
             cur_sys_sim_s: f64,
             tidi_factor: f64,
             has_tidi_history: bool| {
                let rem = Self::remaining_cooldown_secs_with_tidi(
                    &entry,
                    now_unix_f64,
                    sys_opt,
                    cur_sys_sim_s,
                    tidi_factor,
                    has_tidi_history,
                );
                if rem <= 1e-6 {
                    return;
                }
                let replace = best
                    .as_ref()
                    .is_none_or(|(_, _, best_rem, _, _)| rem > *best_rem);
                if replace {
                    best = Some((sys_opt, entry, rem, tidi_factor, has_tidi_history));
                }
            };

        for (sid, handle) in self.systems.active_system_handles() {
            if let Some(entry) = handle.module_reactivation_cooldown(&key) {
                let (tidi_factor, has_tidi_history) = self.resolve_sys_tidi_mode(Some(sid));
                consider(
                    Some(sid),
                    entry,
                    handle.sim_elapsed_secs(),
                    tidi_factor,
                    has_tidi_history,
                );
            }
        }
        let mut seen_non_live = std::collections::HashSet::new();
        for (cold_sid, entry) in
            crate::system_actor::find_all_cold_module_cooldown_entries_in_db(&self.db, key)
        {
            if let Some(live_h) = self.systems.get_handle(cold_sid) {
                let view = live_h.view();
                if !view.module_cooldowns.boot_db_pending_merge
                    || view.module_cooldowns.pending_db_clear_all
                    || view
                        .module_cooldowns
                        .pending_db_cleared_ships
                        .contains(&ship_id)
                    || view.module_cooldowns.pending_db_removed_keys.contains(&key)
                {
                    continue;
                }
            } else {
                seen_non_live.insert(cold_sid);
            }
            let cand_sys = (cold_sid > 0).then_some(cold_sid);
            let effective = if cold_sid > 0 && self.systems.get_handle(cold_sid).is_none() {
                self.systems
                    .overlay_transition_cooldown_for_system(cold_sid, key, Some(entry))
            } else {
                Some(entry)
            };
            if let Some(entry) = effective {
                let (tidi_factor, has_tidi_history) = self.resolve_sys_tidi_mode(cand_sys);
                consider(cand_sys, entry, 0.0, tidi_factor, has_tidi_history);
            }
        }
        for trans_sid in self.systems.transitioning_system_ids(None) {
            if seen_non_live.contains(&trans_sid) || self.systems.get_handle(trans_sid).is_some() {
                continue;
            }
            if let Some(entry) = self
                .systems
                .overlay_transition_cooldown_for_system(trans_sid, key, None)
            {
                let (tidi_factor, has_tidi_history) = self.resolve_sys_tidi_mode(Some(trans_sid));
                consider(Some(trans_sid), entry, 0.0, tidi_factor, has_tidi_history);
            }
        }
        best
    }

    fn resolve_module_cooldown_entry(
        &self,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
        sys_opt: Option<u32>,
    ) -> Option<ModuleCooldownEntry> {
        let now = self.now_unix_secs() as f64;
        if let Some((_, entry, _, _, _)) =
            self.resolve_best_active_cooldown(ship_id, module_id, type_id, now)
        {
            return Some(entry);
        }
        let key = (ship_id, module_id, type_id);
        if let Some(sid) = sys_opt {
            if let Some(handle) = self.systems.get_handle(sid) {
                if let Some(entry) = handle.module_reactivation_cooldown(&key) {
                    return Some(entry);
                }
            }
        }
        crate::system_actor::find_cold_module_cooldown_entry_in_db(&self.db, sys_opt, key)
            .map(|(_, entry)| entry)
    }

    /// Resolves the Unix expiry timestamp (`f64`) of `(ship_id, module_id)`'s active reactivation
    /// cooldown across all live and cold systems, picking the longest active expiry (`G1A-01`).
    pub(crate) fn resolve_active_reactivation_cooldown(
        &self,
        ship_id: i64,
        module_id: i64,
    ) -> Option<f64> {
        let now = self.now_unix_secs() as f64;
        let (type_id, _sys_opt, _) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        let (_, entry, rem, tidi_factor, has_tidi_history) =
            self.resolve_best_active_cooldown(ship_id, module_id, type_id, now)?;
        let eff_tidi = tidi_factor.clamp(0.10, 1.0);
        if eff_tidi < 0.999_999 {
            Some(now + rem / eff_tidi)
        } else if has_tidi_history {
            Some(now + rem)
        } else {
            Some(entry.recorded_unix_f64 + entry.delay_s)
        }
    }

    /// Checks if a module on a ship is currently under reactivation delay (`feat:dogma.reactivation-delay`, `DG-4`).
    pub fn is_module_in_reactivation_delay(&self, ship_id: i64, module_id: i64) -> bool {
        self.resolve_active_reactivation_cooldown(ship_id, module_id)
            .is_some()
    }

    /// Returns the Unix timestamp (`f64`) when `(ship_id, module_id)` finishes its
    /// reactivation cooldown, if active (`§C`).
    #[cfg(any(test, feature = "testkit"))]
    pub fn module_reactivation_cooldown_until_unix(
        &self,
        ship_id: i64,
        module_id: i64,
    ) -> Option<f64> {
        self.resolve_active_reactivation_cooldown(ship_id, module_id)
    }

    /// Enforces single-system cooldown ownership by purging `key` from every other live system
    /// actor and non-owning cold SQLite system checkpoint (`G1A-01`), returning the maximum active
    /// remaining cooldown across those other systems before purge.
    fn collect_and_purge_other_systems_cooldown(
        &self,
        keep_sys_id: u32,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
        now_unix_f64: f64,
        measure_remaining: bool,
    ) -> f64 {
        let key = (ship_id, module_id, type_id);
        let mut max_other_rem = 0.0_f64;
        let mut retry_non_live = self.systems.transitioning_system_ids(Some(keep_sys_id));
        for (other_sid, handle) in self.systems.active_system_handles() {
            if other_sid == keep_sys_id {
                continue;
            }
            let view = handle.view();
            if let Some(entry) = view.module_cooldowns.reactivation_cooldowns.get(&key) {
                if measure_remaining {
                    let (tidi, has_tidi) = self.resolve_sys_tidi_mode(Some(other_sid));
                    let rem = Self::remaining_cooldown_secs_with_tidi(
                        entry,
                        now_unix_f64,
                        Some(other_sid),
                        handle.sim_elapsed_secs(),
                        tidi,
                        has_tidi,
                    );
                    max_other_rem = max_other_rem.max(rem);
                }
                if handle.remove_module_cooldown_sync(key).is_err() {
                    retry_non_live.push(other_sid);
                }
            } else if view.module_cooldowns.boot_db_pending_merge
                && handle.remove_module_cooldown_sync(key).is_err()
            {
                retry_non_live.push(other_sid);
            }
        }
        if measure_remaining {
            let mut seen_non_live = std::collections::HashSet::new();
            for (cold_sid, entry) in
                crate::system_actor::find_all_cold_module_cooldown_entries_in_db(&self.db, key)
            {
                if cold_sid == keep_sys_id || self.systems.get_handle(cold_sid).is_some() {
                    continue;
                }
                seen_non_live.insert(cold_sid);
                let cand_sys = (cold_sid > 0).then_some(cold_sid);
                let eff_entry = if cold_sid > 0 {
                    self.systems
                        .overlay_transition_cooldown_for_system(cold_sid, key, Some(entry))
                } else {
                    Some(entry)
                };
                if let Some(entry) = eff_entry {
                    let (tidi, has_tidi) = self.resolve_sys_tidi_mode(cand_sys);
                    let rem = Self::remaining_cooldown_secs_with_tidi(
                        &entry,
                        now_unix_f64,
                        cand_sys,
                        0.0,
                        tidi,
                        has_tidi,
                    );
                    max_other_rem = max_other_rem.max(rem);
                }
            }
            for trans_sid in self.systems.transitioning_system_ids(Some(keep_sys_id)) {
                if seen_non_live.contains(&trans_sid) || self.systems.get_handle(trans_sid).is_some()
                {
                    continue;
                }
                if let Some(entry) = self
                    .systems
                    .overlay_transition_cooldown_for_system(trans_sid, key, None)
                {
                    let (tidi, has_tidi) = self.resolve_sys_tidi_mode(Some(trans_sid));
                    let rem = Self::remaining_cooldown_secs_with_tidi(
                        &entry,
                        now_unix_f64,
                        Some(trans_sid),
                        0.0,
                        tidi,
                        has_tidi,
                    );
                    max_other_rem = max_other_rem.max(rem);
                }
            }
        }
        let _ = self.systems.remove_cooldown_key_across_non_live_systems(
            &self.db,
            Some(keep_sys_id),
            &retry_non_live,
            key,
        );
        max_other_rem
    }

    pub(crate) fn purge_module_cooldown_other_systems(
        &self,
        keep_sys_id: u32,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
    ) {
        let _ = self.collect_and_purge_other_systems_cooldown(
            keep_sys_id,
            ship_id,
            module_id,
            type_id,
            self.now_unix_secs() as f64,
            false,
        );
    }

    /// Dispatches cross-system cooldown purges asynchronously off the calling `SystemActor`
    /// task (`G1A-R2-P1-02`), avoiding any `block_in_place` actor deadlock or synchronous SQLite
    /// I/O inside `flush_post_unlock_actions`.
    pub(crate) fn spawn_purge_module_cooldown_other_systems(
        &self,
        keep_sys_id: u32,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
    ) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.purge_module_cooldown_other_systems(keep_sys_id, ship_id, module_id, type_id);
            return;
        };
        let key = (ship_id, module_id, type_id);
        let ctx = self.clone();
        let mut retry_non_live = self.systems.transitioning_system_ids(Some(keep_sys_id));
        let mut target_handles = Vec::new();
        for (other_sid, handle) in self.systems.active_system_handles() {
            if other_sid == keep_sys_id {
                continue;
            }
            let view = handle.view();
            if view.module_cooldowns.reactivation_cooldowns.contains_key(&key)
                || view.module_cooldowns.boot_db_pending_merge
            {
                target_handles.push(handle);
            }
        }
        rt.spawn(async move {
            for handle in target_handles {
                if handle.remove_module_cooldown(key).await.is_err() {
                    retry_non_live.push(handle.solar_system_id);
                }
            }
            let _ = tokio::task::spawn_blocking(move || {
                let _ = ctx.systems.remove_cooldown_key_across_non_live_systems(
                    &ctx.db,
                    Some(keep_sys_id),
                    &retry_non_live,
                    key,
                );
            })
            .await;
        });
    }

    pub(crate) fn record_module_reactivation_delay_for_system(
        &self,
        sys_opt: Option<u32>,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
        delay_s: f64,
    ) {
        if !delay_s.is_finite() || delay_s <= 0.0 {
            return;
        }
        let Some(sys_id) = sys_opt.filter(|&s| s > 0) else {
            return;
        };
        let now = self.now_unix_secs() as f64;
        let other_max = self.collect_and_purge_other_systems_cooldown(
            sys_id, ship_id, module_id, type_id, now, true,
        );
        let effective_delay = delay_s.max(other_max);
        let (tidi_factor, has_tidi_history) = self.resolve_sys_tidi_mode(Some(sys_id));
        self.systems.record_cold_system_module_cooldown_in_db(
            &self.db,
            sys_id,
            ship_id,
            module_id,
            type_id,
            now,
            effective_delay,
            tidi_factor,
            has_tidi_history,
        );
    }

    /// Records module reactivation delay until `now + delay_s` (`feat:dogma.reactivation-delay`, `DG-4`).
    pub fn record_module_reactivation_delay(&self, ship_id: i64, module_id: i64, delay_s: f64) {
        if !delay_s.is_finite() || delay_s <= 0.0 {
            return;
        }
        let (type_id, sys_opt, _) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        self.record_module_reactivation_delay_for_system(
            sys_opt, ship_id, module_id, type_id, delay_s,
        );
    }

    /// Removes a specific module reactivation cooldown across ALL active, transitioning
    /// (`Loading`/`Stopping`), and cold SQLite system checkpoints (`G1A-01`).
    pub fn remove_module_reactivation_delay(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
    ) -> bool {
        let key = (ship_id, module_id, module_type_id);
        let mut removed = false;
        let mut retry_non_live = self.systems.transitioning_system_ids(None);
        for (sid, handle) in self.systems.active_system_handles() {
            let view = handle.view();
            let existed = view.module_cooldowns.reactivation_cooldowns.contains_key(&key);
            if existed || view.module_cooldowns.boot_db_pending_merge {
                match handle.remove_module_cooldown_sync(key) {
                    Ok(hit) => removed |= hit,
                    Err(_) => retry_non_live.push(sid),
                }
            }
        }
        if self.systems.remove_cooldown_key_across_non_live_systems(&self.db, None, &retry_non_live, key) {
            removed = true;
        }
        removed
    }

    /// Clears all reactivation cooldowns associated with `ship_id`.
    pub fn clear_ship_reactivation_delays(&self, ship_id: i64) {
        let mut retry_non_live = self.systems.transitioning_system_ids(None);
        for (sid, handle) in self.systems.active_system_handles() {
            if handle.clear_ship_module_cooldowns_sync(ship_id).is_err() {
                retry_non_live.push(sid);
            }
        }
        self.systems.clear_non_live_ship_module_cooldowns(&self.db, &retry_non_live, ship_id);
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn clear_all_reactivation_delays(&self) {
        let mut retry_non_live = self.systems.transitioning_system_ids(None);
        for (sid, handle) in self.systems.active_system_handles() {
            if handle.clear_all_module_cooldowns_sync().is_err() {
                retry_non_live.push(sid);
            }
        }
        self.systems.clear_all_non_live_module_cooldowns(&self.db, &retry_non_live);
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn seed_module_cooldown_entry_for_test(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        entry: ModuleCooldownEntry,
    ) {
        let Some(sys_id) = entry
            .solar_system_id
            .filter(|&s| s > 0)
            .or_else(|| self.resolve_ship_system_opt(ship_id))
        else {
            return;
        };
        self.systems.seed_cold_system_module_cooldown_in_db(
            &self.db,
            sys_id,
            (ship_id, module_id, module_type_id),
            entry,
        );
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn module_cooldown_entry_for_test(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
    ) -> Option<ModuleCooldownEntry> {
        let sys_opt = self.resolve_ship_system_opt(ship_id);
        self.resolve_module_cooldown_entry(ship_id, module_id, module_type_id, sys_opt)
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn has_any_reactivation_delays_for_test(&self) -> bool {
        if self
            .systems
            .active_system_handles()
            .into_iter()
            .any(|(_, h)| !h.view().module_cooldowns.reactivation_cooldowns.is_empty())
        {
            return true;
        }
        self.db
            .with_sqlite_conn(|conn| {
                let _ = eve_db::sqlite_store::system_checkpoint::ensure_system_checkpoint_schema(conn);
                eve_db::sqlite_store::system_checkpoint::load_all_checkpoints_for_kind::<
                    ((i64, i64, u32), ModuleCooldownEntry),
                >(
                    conn,
                    eve_db::sqlite_store::system_checkpoint::KIND_MODULE_COOLDOWNS,
                )
            })
            .is_ok_and(|rows| !rows.is_empty())
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn record_active_ewar_start_for_test(
        &self,
        sys_opt: Option<u32>,
        source_ship_id: i64,
        module_id: i64,
        target_ship_id: i64,
        jamming_type: &'static str,
    ) {
        let handle_opt = sys_opt
            .and_then(|sid| self.systems.get_handle(sid))
            .or_else(|| self.systems.find_ship_live_handle(source_ship_id))
            .or_else(|| self.systems.find_ship_live_handle(target_ship_id))
            .or_else(|| {
                self.resolve_ship_system_opt(source_ship_id)
                    .or_else(|| self.resolve_ship_system_opt(target_ship_id))
                    .and_then(|sid| self.systems.get_handle(sid))
            });
        if let Some(handle) = handle_opt {
            let _ = handle.record_active_ewar_start_sync(
                source_ship_id,
                module_id,
                target_ship_id,
                jamming_type,
            );
        }
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn seed_ewar_source_for_test(
        &self,
        sys_opt: Option<u32>,
        source_module_id: i64,
        source_ship_id: i64,
    ) {
        let resolved_sys = sys_opt.or_else(|| self.resolve_ship_system_opt(source_ship_id));
        self.systems
            .seed_ewar_source_for_test_sync(resolved_sys, source_module_id, source_ship_id);
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn clear_target_ewar_for_test(&self, target_ship_id: i64) {
        self.systems.clear_target_ewar_for_test_sync(target_ship_id);
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn ewar_starts_count_for_test(&self) -> usize {
        self.systems.ewar_starts_count()
    }
}
