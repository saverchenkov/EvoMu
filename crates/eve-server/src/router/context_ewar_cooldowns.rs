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
        let Some(sid) = sys_opt else {
            return (1.0, false);
        };
        let tidi = self.systems.get_system_tidi_factor(sid);
        let has_anchor = self
            .systems
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sim_clock_anchoring
            .contains_key(&sid);
        (tidi, has_anchor || tidi < 0.999_999)
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

    fn resolve_module_cooldown_entry(
        &self,
        ship_id: i64,
        module_id: i64,
        type_id: u32,
        sys_opt: Option<u32>,
    ) -> Option<ModuleCooldownEntry> {
        let key = (ship_id, module_id, type_id);
        if let Some(sid) = sys_opt {
            if let Some(handle) = self.systems.get_handle(sid) {
                if let Some(entry) = handle.module_reactivation_cooldown(&key) {
                    return Some(entry);
                }
            }
        }
        for (sid, handle) in self.systems.active_system_handles() {
            if Some(sid) != sys_opt {
                if let Some(entry) = handle.module_reactivation_cooldown(&key) {
                    return Some(entry);
                }
            }
        }
        crate::system_actor::find_cold_module_cooldown_entry_in_db(&self.db, sys_opt, key)
            .map(|(_, entry)| entry)
    }

    /// Resolves the Unix expiry timestamp (`f64`) of `(ship_id, module_id)`'s active reactivation
    /// cooldown, returning `None` if no cooldown exists or it has already elapsed.
    fn resolve_active_reactivation_cooldown(&self, ship_id: i64, module_id: i64) -> Option<f64> {
        let now = self.now_unix_secs() as f64;
        let (type_id, sys_opt, cur_sys_sim_s) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        let (tidi_factor, has_tidi_history) = self.resolve_sys_tidi_mode(sys_opt);
        self.resolve_module_cooldown_entry(ship_id, module_id, type_id, sys_opt)
            .and_then(|entry| {
                let rem = Self::remaining_cooldown_secs_with_tidi(
                    &entry,
                    now,
                    sys_opt,
                    cur_sys_sim_s,
                    tidi_factor,
                    has_tidi_history,
                );
                if rem <= 1e-6 {
                    None
                } else {
                    let eff_tidi = tidi_factor.clamp(0.10, 1.0);
                    if eff_tidi < 0.999_999 {
                        Some(now + rem / eff_tidi)
                    } else if has_tidi_history {
                        Some(now + rem)
                    } else {
                        Some(entry.recorded_unix_f64 + entry.delay_s)
                    }
                }
            })
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
        let (tidi_factor, has_tidi_history) = self.resolve_sys_tidi_mode(Some(sys_id));
        if let Some(handle) = self.systems.get_handle(sys_id) {
            let _ = handle.record_module_cooldown_sync(
                ship_id,
                module_id,
                type_id,
                now,
                delay_s,
                tidi_factor,
                has_tidi_history,
            );
        } else {
            crate::system_actor::record_cold_system_module_cooldown_in_db(
                &self.db,
                sys_id,
                ship_id,
                module_id,
                type_id,
                now,
                delay_s,
                tidi_factor,
                has_tidi_history,
            );
        }
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

    /// Removes a specific module reactivation cooldown across active system actors and checkpoints.
    pub fn remove_module_reactivation_delay(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
    ) -> bool {
        let key = (ship_id, module_id, module_type_id);
        let mut removed = false;
        for (_, handle) in self.systems.active_system_handles() {
            if handle.module_reactivation_cooldown(&key).is_some()
                && handle.remove_module_cooldown_sync(key).unwrap_or(false)
            {
                removed = true;
            }
        }
        if let Some(sys_id) = self.resolve_ship_system_opt(ship_id) {
            if self.systems.get_handle(sys_id).is_none()
                && crate::system_actor::remove_cold_system_module_cooldown_in_db(
                    &self.db, sys_id, key,
                )
            {
                removed = true;
            }
        }
        removed
    }

    /// Clears all reactivation cooldowns associated with `ship_id`.
    pub fn clear_ship_reactivation_delays(&self, ship_id: i64) {
        for (_, handle) in self.systems.active_system_handles() {
            let _ = handle.clear_ship_module_cooldowns_sync(ship_id);
        }
        crate::system_actor::clear_cold_ship_module_cooldowns_in_db(&self.db, ship_id);
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn clear_all_reactivation_delays(&self) {
        for (_, handle) in self.systems.active_system_handles() {
            let _ = handle.clear_all_module_cooldowns_sync();
        }
        crate::system_actor::clear_all_cold_module_cooldowns_in_db(&self.db);
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
        let key = (ship_id, module_id, module_type_id);
        if let Some(handle) = self.systems.get_handle(sys_id) {
            let _ = handle.seed_module_cooldown_entry_sync(key, entry);
        } else {
            crate::system_actor::seed_cold_system_module_cooldown_in_db(
                &self.db, sys_id, key, entry,
            );
        }
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
