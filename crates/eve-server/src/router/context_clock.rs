//! Typed simulation and wall-clock domain helpers (`impl ServerContext`).

use super::context::ServerContext;

impl ServerContext {
    /// Returns the server virtual clock's current wall-clock timestamp as a typed
    /// [`eve_db::clock::WallFileTime`].
    pub fn now_blueos_wall_filetime(&self) -> eve_db::clock::WallFileTime {
        eve_db::clock::WallFileTime::from_ticks(self.now_blueos_filetime_ticks())
    }

    /// Converts a wall-clock Unix timestamp (`>= 0` seconds) into a typed
    /// [`eve_db::clock::WallFileTime`] without mixing domains.
    pub fn wall_unix_to_filetime(&self, wall_unix_secs: u64) -> eve_db::clock::WallFileTime {
        eve_db::clock::WallUnixMs::from_secs_saturating(wall_unix_secs)
            .to_wall_filetime_saturating()
    }

    /// Converts a server-virtual Unix expiry/target timestamp into client-aligned
    /// [`eve_db::clock::WallFileTime`], subtracting un-rebased `/advancetime` lead
    /// (`#1586` — used for notifications compared against `blue.os.GetWallclockTimeNow()`).
    pub fn virtual_unix_to_machonet_wall_filetime(
        &self,
        virtual_unix_secs: u64,
    ) -> eve_db::clock::WallFileTime {
        let rebased_secs = virtual_unix_secs.saturating_sub(self.unrebased_clock_offset_secs());
        self.wall_unix_to_filetime(rebased_secs)
    }

    /// Returns [`Self::now_client_sim_filetime_ticks`] wrapped in [`eve_db::clock::SimFileTime`].
    pub fn now_client_sim_filetime(&self) -> eve_db::clock::SimFileTime {
        eve_db::clock::SimFileTime::from_ticks(self.now_client_sim_filetime_ticks())
    }

    /// Returns the typed [`eve_db::clock::SimFileTime`] for `solar_system_id` via
    /// [`Self::now_system_sim_filetime_ticks`].
    pub fn now_system_sim_filetime(
        &self,
        solar_system_id: Option<u32>,
    ) -> eve_db::clock::SimFileTime {
        eve_db::clock::SimFileTime::from_ticks(self.now_system_sim_filetime_ticks(solar_system_id))
    }

    /// Resolves `(char_id, ship_id)` to a solar system if in space and returns typed
    /// [`eve_db::clock::SimFileTime`] (`#1081`, `#1196`).
    pub fn now_system_sim_filetime_for_ship(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> eve_db::clock::SimFileTime {
        eve_db::clock::SimFileTime::from_ticks(
            self.now_system_sim_filetime_ticks_for_ship(char_id, ship_id),
        )
    }

    /// Resolves an undocked character's active solar system and returns its typed
    /// [`eve_db::clock::SimFileTime`], falling back to client sim time when docked.
    pub fn now_system_sim_filetime_for_char(&self, char_id: i64) -> eve_db::clock::SimFileTime {
        let sys_opt = (char_id > 0)
            .then(|| {
                self.db.with_snapshot(|s| {
                    s.characters
                        .get(&char_id)
                        .filter(|c| c.station_id.is_none() && c.structure_id.is_none())
                        .map(|c| c.solar_system_id)
                })
            })
            .flatten();
        self.now_system_sim_filetime(sys_opt)
    }

    /// Read-only projection of `solar_system_id`'s anchored simulation clock (`SimFileTime`),
    /// returning `None` when the system has no active entry in `sim_clock_anchoring`.
    pub fn peek_system_sim_filetime(&self, solar_system_id: u32) -> Option<eve_db::clock::SimFileTime> {
        let now_wall_ft = self.now_machonet_wallclock_filetime_ticks();
        let cn = self
            .systems
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let &(anchor_wall_ft, anchor_sim_ft, factor) = cn.sim_clock_anchoring.get(&solar_system_id)?;
        let elapsed_wall_ft = now_wall_ft.saturating_sub(anchor_wall_ft).max(0);
        let dilated_ft = ((elapsed_wall_ft as f64) * factor.clamp(0.10, 1.0)).round() as i64;
        Some(eve_db::clock::SimFileTime::from_ticks(
            anchor_sim_ft.saturating_add(dilated_ft),
        ))
    }

    /// Anchors `solar_system_id`'s simulation clock with an explicit `(sim_time - wallclock)`
    /// millisecond offset and TiDi factor (`0.10..=1.00`) for deterministic multi-domain fixtures
    /// and clock-offset testing (`#1196`), keeping `sim_clock_anchoring`, `manual_tidi_overrides`,
    /// and live `SystemHandle` synchronized.
    pub fn anchor_system_sim_clock_with_offset_ms(
        &self,
        solar_system_id: u32,
        sim_minus_wall_ms: i64,
        tidi_factor: f64,
    ) -> eve_db::clock::SolarSystemSimClock {
        let clamped_tidi =
            eve_db::clock::SolarSystemSimClock::clamp_tidi_factor(tidi_factor).unwrap_or(1.0);
        let now_wall_ft = self.now_machonet_wall_filetime();
        let now_wall_ms = now_wall_ft.to_wall_unix_ms().unwrap_or_default();
        let sim_clock = eve_db::clock::SolarSystemSimClock::new(
            now_wall_ms,
            sim_minus_wall_ms,
            eve_db::clock::DestinyStamp::from_ticks(1),
            clamped_tidi,
        )
        .unwrap_or_else(|| {
            eve_db::clock::SolarSystemSimClock::new(
                now_wall_ms,
                0,
                eve_db::clock::DestinyStamp::from_ticks(1),
                clamped_tidi,
            )
            .expect("zero-offset SolarSystemSimClock is valid")
        });
        let anchor_wall_ft = sim_clock
            .last_wall_filetime()
            .unwrap_or(now_wall_ft)
            .as_ticks();
        let anchor_sim_ft = sim_clock.sim_filetime().as_ticks();
        crate::services::serverops_cluster::sync_anchored_system_tidi(
            self,
            solar_system_id,
            clamped_tidi,
            anchor_wall_ft,
            anchor_sim_ft,
        );
        sim_clock
    }

    /// Maps a virtual Unix timer expiry timestamp into `solar_system_id`'s
    /// [`eve_db::clock::SimFileTime`] domain (`blue.os.GetSimTime()`), preserving zero-skew
    /// equality when the system has no simulation-clock offset or TiDi active, shifting by
    /// `(sim - wall)` when anchored, and scaling remaining wall ticks by the system's active TiDi
    /// factor (`0.10..=1.0`, `#1196`). Falls back to MachoNet client wall filetime when `None`.
    pub fn virtual_unix_expiry_to_sim_filetime_for_system(
        &self,
        solar_system_id: Option<u32>,
        expiry_unix_secs: u64,
    ) -> eve_db::clock::SimFileTime {
        let macho_exp_ft = self.virtual_unix_to_machonet_wall_filetime(expiry_unix_secs);
        if let Some(sys_id) = solar_system_id {
            let now_wall_ft = self.now_machonet_wallclock_filetime_ticks();
            let anchor_opt = {
                let cn = self
                    .systems
                    .cluster_nodes
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                cn.sim_clock_anchoring.get(&sys_id).copied()
            };
            let live_factor =
                crate::services::serverops_cluster::get_system_tidi_factor(self, sys_id);
            let (cur_sim_ft, effective_factor) =
                if let Some((anchor_wall_ft, anchor_sim_ft, anchor_factor)) = anchor_opt {
                    let clamped_anchor_factor = anchor_factor.clamp(0.10, 1.0);
                    let elapsed_wall_ft = now_wall_ft.saturating_sub(anchor_wall_ft).max(0);
                    let dilated_ft =
                        ((elapsed_wall_ft as f64) * clamped_anchor_factor).round() as i64;
                    let sim_ft = anchor_sim_ft.saturating_add(dilated_ft);
                    let eff = if (live_factor - clamped_anchor_factor).abs()
                        >= crate::system_actor::tidi_autoscaler::TIDI_MIN_CHANGE_DELTA
                    {
                        live_factor
                    } else {
                        clamped_anchor_factor
                    };
                    (sim_ft, eff)
                } else {
                    (now_wall_ft, live_factor)
                };
            let clamped_factor = if effective_factor.is_finite() {
                effective_factor.clamp(0.10, 1.0)
            } else {
                1.0
            };
            let remaining_wall_ticks = macho_exp_ft.as_ticks().saturating_sub(now_wall_ft);
            let scaled_remaining_ticks = if clamped_factor < 1.0 {
                ((remaining_wall_ticks as f64) * clamped_factor).round() as i64
            } else {
                remaining_wall_ticks
            };
            return eve_db::clock::SimFileTime::from_ticks(
                cur_sim_ft.saturating_add(scaled_remaining_ticks),
            );
        }
        eve_db::clock::SimFileTime::from_ticks(macho_exp_ft.as_ticks())
    }

    /// Maps a virtual Unix timer expiry timestamp into `char_id`'s current [`eve_db::clock::SimFileTime`]
    /// domain (`blue.os.GetSimTime()`), resolving undocked characters to their active solar system (`#1196`).
    pub fn virtual_unix_expiry_to_sim_filetime_for_char(
        &self,
        char_id: i64,
        expiry_unix_secs: u64,
    ) -> eve_db::clock::SimFileTime {
        let space_sys_opt = (char_id > 0)
            .then(|| {
                self.db.with_snapshot(|s| {
                    s.characters
                        .get(&char_id)
                        .filter(|c| c.station_id.is_none() && c.structure_id.is_none())
                        .map(|c| c.solar_system_id)
                })
            })
            .flatten();
        self.virtual_unix_expiry_to_sim_filetime_for_system(space_sys_opt, expiry_unix_secs)
    }

    /// Returns MachoNet client wall-clock time wrapped in [`eve_db::clock::WallFileTime`].
    pub fn now_machonet_wall_filetime(&self) -> eve_db::clock::WallFileTime {
        eve_db::clock::WallFileTime::from_ticks(self.now_machonet_wallclock_filetime_ticks())
    }
}
