//! Celestial population, asteroid belt extraction, and slim item synchronization for `system_actor`.

#![forbid(unsafe_code)]

use super::{
    AsteroidState, MiningExtractionResult, TickBroadcast, next_downtime_unix_secs,
};
use eve_destiny::wire::{encode_destiny_event, encode_destiny_updates, ordered_batch_indices};
use eve_destiny::{
    Ball, BallId, Ballpark, DestinyEvent, MovementMode, SlimItemMeta,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hasher;
use std::sync::Arc;

/// Computes V24 `dunRotation` `[yaw_deg, pitch_deg, roll_deg]` so a stargate model's `+Z`
/// forward vector aligns with `dest.galactic_pos_ly - origin.galactic_pos_ly` (`#1088`).
///
/// Matches V24 `SpaceObject.SetStaticRotation` (`math.radians` -> `geo2.QuaternionRotationSetYawPitchRoll`,
/// i.e. `EulerRot::YXZ`) and `JumpTransitionCamera` (`QuaternionTransformVector(rot, (0, 0, 1))`).
pub fn compute_stargate_dun_rotation(
    origin_sys_id: SolarSystemId,
    dest_sys_id: SolarSystemId,
    sde: &SdeRegistry,
) -> Option<[f64; 3]> {
    let origin = sde.solar_systems.get(&origin_sys_id)?;
    let dest = sde.solar_systems.get(&dest_sys_id)?;
    let delta = DVec3::from_array(dest.galactic_pos_ly) - DVec3::from_array(origin.galactic_pos_ly);
    if !delta.is_finite() || delta.length_squared() <= 1e-24 {
        return None;
    }
    let dir = delta.normalize();
    let pitch_rad = (-dir.y).clamp(-1.0, 1.0).asin();
    let yaw_rad = if dir.x * dir.x + dir.z * dir.z > 1e-24 {
        dir.x.atan2(dir.z)
    } else {
        0.0
    };
    let yaw_deg = yaw_rad.to_degrees();
    let pitch_deg = pitch_rad.to_degrees();
    if yaw_deg.is_finite() && pitch_deg.is_finite() {
        Some([yaw_deg, pitch_deg, 0.0])
    } else {
        None
    }
}

/// Evaluates the world-space `+Z` forward unit vector for V24 `dunRotation` `[yaw_deg, pitch_deg, roll_deg]`.
pub fn stargate_forward_vector_from_dun_rotation(dun_rotation: [f64; 3]) -> DVec3 {
    let [yaw_deg, pitch_deg, roll_deg] = dun_rotation;
    let q = glam::DQuat::from_euler(
        glam::EulerRot::YXZ,
        yaw_deg.to_radians(),
        pitch_deg.to_radians(),
        roll_deg.to_radians(),
    );
    (q * DVec3::Z).normalize_or(DVec3::Z)
}

/// Populates a solar system's `Ballpark` with its real SDE stars, planets, moons, asteroid belts,
/// stargates, and stations as static `MovementMode::Rigid` balls (`SP-1`).
pub fn populate_system_celestials(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
) {
    ballpark.bind_sde(sde);
    if let Some(sys) = sde.solar_systems.get(&solar_system_id) {
        ballpark.constellation_id = sys.constellation_id;
    }
    // 1. Stars (group 6), Planets (group 7), Moons (group 8), Asteroid Belts (group 9), Stargates (group 10)
    for cel in sde.celestials_in_system_slice(solar_system_id) {
        let type_rec = sde.types.get(&cel.type_id);
        let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
        let radius = if cel.radius > 0.0 {
            cel.radius
        } else {
            type_rec
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(1_000.0)
        };
        let mass = type_rec
            .map(|t| t.mass)
            .filter(|&m| m > 0.0)
            .unwrap_or(1.0e18);
        let jump_target = if cel.group_id == 10 {
            sde.stargate(cel.celestial_id)
                .map(|j| (j.destination_stargate_id, j.to_solar_system_id))
        } else {
            None
        };
        let (origin_system_owner_id, dest_system_owner_id, dun_rotation) = if cel.group_id == 10 {
            (
                sde.solar_system_faction_id(solar_system_id).map(i64::from),
                jump_target
                    .and_then(|(_, dest_sys_id)| sde.solar_system_faction_id(dest_sys_id))
                    .map(i64::from),
                jump_target.and_then(|(_, dest_sys_id)| {
                    compute_stargate_dun_rotation(solar_system_id, dest_sys_id, sde)
                }),
            )
        } else {
            (None, None, None)
        };
        ballpark.add_static_ball_with_meta(
            Ball {
                id: cel.celestial_id,
                type_id: cel.type_id,
                position: cel.position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            SlimItemMeta {
                group_id: cel.group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                jump_target,
                name: (!cel.name.is_empty()).then(|| cel.name.clone()),
                origin_system_owner_id,
                dest_system_owner_id,
                dun_rotation,
                ..Default::default()
            },
        );
    }

    // 2. Ensure all stargates in `sde.jumps` originating in `solar_system_id` are present (`group_id = 10`)
    for jump in sde.jumps.values() {
        if jump.from_solar_system_id == solar_system_id
            && !ballpark.balls.contains_key(&jump.stargate_id)
        {
            let (gate_type_id, gate_name) = sde
                .stargate_type_and_name(jump.stargate_id)
                .unwrap_or((16, ""));
            let type_rec = sde
                .types
                .get(&gate_type_id)
                .or_else(|| sde.types.get(&16));
            let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
            let radius = type_rec
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0);
            ballpark.add_static_ball_with_meta(
                Ball {
                    id: jump.stargate_id,
                    type_id: gate_type_id,
                    position: jump.position,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e12,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                },
                SlimItemMeta {
                    group_id: 10,
                    category_id,
                    owner_id: 1,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    jump_target: Some((jump.destination_stargate_id, jump.to_solar_system_id)),
                    name: (!gate_name.is_empty()).then(|| gate_name.to_string()),
                    origin_system_owner_id: sde
                        .solar_system_faction_id(solar_system_id)
                        .map(i64::from),
                    dest_system_owner_id: sde
                        .solar_system_faction_id(jump.to_solar_system_id)
                        .map(i64::from),
                    dun_rotation: compute_stargate_dun_rotation(
                        solar_system_id,
                        jump.to_solar_system_id,
                        sde,
                    ),
                    ..Default::default()
                },
            );
        }
    }

    // 3. Stations (`group_id = 15`) from `sde.solar_systems[solar_system_id].stations`
    if let Some(sys) = sde.solar_systems.get(&solar_system_id) {
        for st in &sys.stations {
            if !ballpark.balls.contains_key(&st.station_id) {
                let Some(type_rec) = sde.types.get(&st.station_type_id) else {
                    continue;
                };
                let group_id = type_rec.group_id;
                let category_id = type_rec.category_id;
                let owner_id = st.owner_corporation_id;
                let radius = type_rec.radius;
                let mass = type_rec.mass;
                ballpark.add_static_ball_with_meta(
                    Ball {
                        id: st.station_id,
                        type_id: st.station_type_id,
                        position: st.position,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 0.0,
                        mass,
                        radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    },
                    SlimItemMeta {
                        group_id,
                        category_id,
                        owner_id,
                        char_id: None,
                        corp_id: owner_id,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false, ..Default::default()
                    },
                );
            }
        }
    }

    crate::services::cluster_macho_misc_services::populate_shipcaster_balls_with_sde(
        solar_system_id,
        ballpark,
        Some(sde),
        None,
    );
}

/// Computes the deterministic base asteroid entity ID for `solar_system_id` (`750_000_000..800_000_000`),
/// allocating a `6,000`-ID block per solar system so even the largest V24.01 belt systems (`5,807` rocks)
/// never collide across solar systems or overlap living-world convoy IDs (`800_100_000+`).
pub fn belt_asteroid_base_id(solar_system_id: SolarSystemId) -> i64 {
    let sys_offset = if solar_system_id == 30_000_144 {
        0
    } else if solar_system_id >= 31_000_000 {
        (5_600 + i64::from(solar_system_id - 31_000_000)) * 6_000
    } else {
        (i64::from(solar_system_id % 10_000_000).max(1)) * 6_000
    };
    750_000_000 + sys_offset
}

/// Inserts a belt celestial (`group_id == 9`) into `ballpark` when `populate_asteroid_belt_entities`
/// is called standalone without a prior `populate_system_celestials` pass.
fn ensure_belt_celestial_ball(
    ballpark: &mut Ballpark,
    cel: &eve_sde::CelestialInfo,
    sde: &SdeRegistry,
) {
    if ballpark.balls.contains_key(&cel.celestial_id) {
        return;
    }
    let type_rec = sde.types.get(&cel.type_id);
    let category_id = type_rec.map(|t| t.category_id).unwrap_or(2);
    let radius = if cel.radius > 0.0 {
        cel.radius
    } else {
        type_rec
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(1_000.0)
    };
    let mass = type_rec
        .map(|t| t.mass)
        .filter(|&m| m > 0.0 && m < 1.0e30)
        .unwrap_or(1.0e12);
    ballpark.add_static_ball_with_meta(
        Ball {
            id: cel.celestial_id,
            type_id: cel.type_id,
            position: cel.position,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass,
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        SlimItemMeta {
            group_id: cel.group_id,
            category_id,
            owner_id: 1,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
            name: (!cel.name.is_empty()).then(|| cel.name.clone()),
            ..Default::default()
        },
    );
}

/// Computes deterministic per-asteroid ore quantities (aligned to SDE `portion_size`),
/// spherical radius, finite rock mass, and semicircular belt arc coordinates.
fn build_belt_asteroid_rock(
    asteroid_id: i64,
    belt_id: i64,
    idx: usize,
    belt_pos: DVec3,
    ore_type: &eve_sde::InvType,
) -> (AsteroidState, Ball, SlimItemMeta) {
    // DERIVED: Client SDE (`asteroidcountbytypeidbeltid.static`) supplies per-belt counts by `type_id`,
    // while `sde_types` supplies per-unit `volume` and `portion_size` (and stores a uniform `radius = 1.0`
    // and `1.0e35` sentinel mass on ores). Individual asteroid rock volumes (`1,800..3,400 m³`), semicircular
    // belt arc placement within 8.5 km, scaled grid radius (`>= 60 m`), and silicate/metallic bulk mass
    // (`3,000 kg/m³`) are server-only deterministic seeding bounds.
    let unit_vol = ore_type.volume.max(0.01);
    let batch = ore_type.portion_size.max(1);
    let mix = ((belt_id as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ ((idx as u64)
            .wrapping_add(1)
            .wrapping_mul(0xbf58_476d_1ce4_e5b9))
        ^ u64::from(ore_type.type_id).wrapping_mul(0x94d0_49bb_1331_11eb))
    .rotate_left(17);
    let size_factor = 0.9 + ((mix % 801) as f64) / 1_000.0;
    let target_m3 = 2_000.0 * size_factor;
    let raw_units = ((target_m3 / unit_vol).round() as u64).max(batch);
    let units = (raw_units / batch).max(1) * batch;
    let total_m3 = (units as f64) * unit_vol;
    let equiv_sphere_r = (3.0 * total_m3 / (4.0 * std::f64::consts::PI)).cbrt();
    let rock_radius = (equiv_sphere_r * 12.0).clamp(60.0, 2_500.0);
    let rock_mass = (total_m3 * 3_000.0).max(1.0e6);

    let offset = if idx == 0 {
        DVec3::new(200.0, -100.0, 150.0)
    } else {
        let angle = (idx as f64) * 0.28;
        let arc_r = 2_000.0 + ((idx % 10) as f64) * 650.0;
        DVec3::new(
            arc_r * angle.cos(),
            (((idx % 5) as f64) - 2.0) * 180.0,
            arc_r * angle.sin(),
        )
    };

    let ast_state = AsteroidState {
        asteroid_id,
        belt_id,
        type_id: ore_type.type_id,
        initial_units: units,
        remaining_units: units,
        unit_volume: unit_vol,
        depleted: false,
        respawn_at_tick: None,
    };
    let ball = Ball {
        id: asteroid_id,
        type_id: ore_type.type_id,
        position: belt_pos + offset,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: rock_mass,
        radius: rock_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };
    let meta = SlimItemMeta {
        group_id: ore_type.group_id,
        category_id: ore_type.category_id,
        owner_id: 1,
        char_id: None,
        corp_id: -1,
        alliance_id: -1,
        security_status: 0.0,
        is_cloaked: false,
        name: (!ore_type.name.is_empty()).then(|| ore_type.name.clone()),
        ..Default::default()
    };
    (ast_state, ball, meta)
}

/// Populates asteroid belt entities around asteroid belt celestials.
/// Uses exact per-belt asteroid counts from `res:/staticdata/asteroidcountbytypeidbeltid.static`
/// when present in `SdeRegistry`, falling back to `SdeRegistry::belt_ores_for_system`
/// for synthetic belt celestials in unit tests.
pub fn populate_asteroid_belt_entities(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    sde: &SdeRegistry,
) -> Vec<AsteroidState> {
    let mut states = Vec::new();
    let mut ast_id_counter = belt_asteroid_base_id(solar_system_id);

    if sde.solar_systems.get(&solar_system_id).is_none() {
        return states;
    }
    let band_ores = sde.belt_ores_for_system(solar_system_id);

    for cel in sde.celestials_in_system_slice(solar_system_id) {
        if cel.group_id == 9 {
            ensure_belt_celestial_ball(ballpark, cel, sde);
            let belt_pos = cel.position;

            let mut belt_spawn_seq: Vec<&eve_sde::InvType> = Vec::new();
            if let Some(counts_map) = sde.belt_asteroid_counts(cel.celestial_id) {
                let mut typed_counts: Vec<(&eve_sde::InvType, u32)> = counts_map
                    .iter()
                    .filter(|(_, &cnt)| cnt > 0)
                    .filter_map(|(&tid, &cnt)| sde.types.get(&tid).map(|t| (t, cnt)))
                    .collect();
                if !typed_counts.is_empty() {
                    typed_counts.sort_by(|(a, _), (b, _)| {
                        a.volume
                            .partial_cmp(&b.volume)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| a.type_id.cmp(&b.type_id))
                    });
                    let max_cnt = typed_counts.iter().map(|(_, c)| *c).max().unwrap_or(0);
                    let total_cnt: usize = typed_counts.iter().map(|(_, c)| *c as usize).sum();
                    belt_spawn_seq.reserve(total_cnt);
                    for round in 0..max_cnt {
                        for &(ore_t, cnt) in &typed_counts {
                            if round < cnt {
                                belt_spawn_seq.push(ore_t);
                            }
                        }
                    }
                }
            }
            if belt_spawn_seq.is_empty() {
                belt_spawn_seq.extend(band_ores.iter().copied());
            }

            for (idx, ore_type) in belt_spawn_seq.into_iter().enumerate() {
                let id = ast_id_counter;
                ast_id_counter += 1;
                let (ast_state, ball, meta) =
                    build_belt_asteroid_rock(id, cel.celestial_id, idx, belt_pos, ore_type);
                ballpark.add_static_ball_with_meta(ball, meta);
                states.push(ast_state);
            }
        }
    }
    states
}

/// Returns `true` if `ball` is a stationary space object (wreck, jetcan, loot/secure container,
/// corpse, or anchored deployable/structure) that must omit `BALL_FLAG_IS_FREE` in `AddBalls2` /
/// `SetState` (`DestinyManager.cpp:1780`).
pub fn should_pin_stationary_ball(ball: &Ball, meta: Option<&SlimItemMeta>) -> bool {
    if !matches!(ball.mode, MovementMode::Stop)
        || ball.max_velocity > 0.0
        || ball.velocity.length_squared() > 0.0
    {
        return false;
    }
    if matches!(
        ball.type_id,
        23 | 25 | 3293 | 26_468..=27_030 | 33_474..=33_500
    ) {
        return true;
    }
    meta.is_some_and(|m| matches!(m.category_id, 2 | 3 | 22 | 23 | 40 | 46 | 65))
}

pub(super) fn ensure_default_slim_meta(
    ballpark: &mut Ballpark,
    ball: &Ball,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    if ballpark.ball_slim_meta.contains_key(&ball.id) {
        return;
    }
    let (group_id, category_id, name) = sde_opt
        .as_ref()
        .and_then(|sde| sde.types.get(&ball.type_id))
        .map(|t| {
            (
                t.group_id,
                t.category_id,
                (!t.name.is_empty()).then(|| t.name.clone()),
            )
        })
        .unwrap_or_else(|| {
            let fallback = eve_destiny::wire::resolve_slim_item_meta(None, ball);
            (fallback.group_id, fallback.category_id, None)
        });
    ballpark.set_ball_slim_meta(
        ball.id,
        SlimItemMeta {
            group_id,
            category_id,
            owner_id: 1,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
            name,
            ..Default::default()
        },
    );
}

#[derive(Debug)]
struct SerHashErr;

impl std::fmt::Display for SerHashErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("hash error")
    }
}

impl std::error::Error for SerHashErr {}

impl serde::ser::Error for SerHashErr {
    fn custom<T: std::fmt::Display>(_msg: T) -> Self {
        Self
    }
}

struct HashSerializer<'a, H: Hasher>(&'a mut H);

impl<'a, H: Hasher> serde::Serializer for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, v: bool) -> Result<(), Self::Error> {
        self.0.write_u8(v as u8);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Self::Error> {
        self.0.write_i8(v);
        Ok(())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Self::Error> {
        self.0.write_i16(v);
        Ok(())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Self::Error> {
        self.0.write_i32(v);
        Ok(())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Self::Error> {
        self.0.write_i64(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Self::Error> {
        self.0.write_u8(v);
        Ok(())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Self::Error> {
        self.0.write_u16(v);
        Ok(())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Self::Error> {
        self.0.write_u32(v);
        Ok(())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Self::Error> {
        self.0.write_u64(v);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Self::Error> {
        self.0.write_u32(if v == 0.0 { 0 } else { v.to_bits() });
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Self::Error> {
        self.0.write_u64(if v == 0.0 { 0 } else { v.to_bits() });
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Self::Error> {
        self.0.write_u32(v as u32);
        Ok(())
    }
    fn serialize_str(self, v: &str) -> Result<(), Self::Error> {
        self.0.write_usize(v.len());
        self.0.write(v.as_bytes());
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Self::Error> {
        self.0.write_usize(v.len());
        self.0.write(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Self::Error> {
        self.0.write_u8(0);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Self::Error> {
        self.0.write_u8(1);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Self::Error> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result<(), Self::Error> {
        self.0.write_u32(variant_index);
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.0.write_u32(variant_index);
        value.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        if let Some(n) = len {
            self.0.write_usize(n);
        }
        Ok(self)
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.0.write_usize(len);
        Ok(self)
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.0.write_usize(len);
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.0.write_u32(variant_index);
        self.0.write_usize(len);
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        if let Some(n) = len {
            self.0.write_usize(n);
        }
        Ok(self)
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.0.write_usize(len);
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.0.write_u32(variant_index);
        self.0.write_usize(len);
        Ok(self)
    }
}

impl<'a, H: Hasher> serde::ser::SerializeSeq for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, H: Hasher> serde::ser::SerializeTuple for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, H: Hasher> serde::ser::SerializeTupleStruct for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, H: Hasher> serde::ser::SerializeTupleVariant for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, H: Hasher> serde::ser::SerializeMap for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Self::Error> {
        key.serialize(&mut **self)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, H: Hasher> serde::ser::SerializeStruct for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, H: Hasher> serde::ser::SerializeStructVariant for &mut HashSerializer<'a, H> {
    type Ok = ();
    type Error = SerHashErr;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn hash_destiny_event(ev: &DestinyEvent) -> u64 {
    let mut h = DefaultHasher::new();
    let _ = ev.serialize(&mut HashSerializer(&mut h));
    h.finish()
}

pub(super) fn drain_tick_events_without_encode(ballpark: &mut Ballpark) {
    ballpark.pending_events.clear();
    let ready_obs_ids: Vec<BallId> = ballpark
        .observer_events
        .iter()
        .filter_map(|(&obs_id, evs)| {
            if evs.is_empty()
                || ballpark.pending_handoff_observers.contains(&obs_id)
                || evs
                    .iter()
                    .any(|ev| matches!(ev, DestinyEvent::SetState { .. }))
            {
                None
            } else {
                Some(obs_id)
            }
        })
        .collect();
    for obs_id in ready_obs_ids {
        let _ = ballpark.drain_observer_events(obs_id);
    }
    let balls = &ballpark.balls;
    ballpark
        .observer_events
        .retain(|id, evs| !evs.is_empty() || balls.contains_key(id));
}

/// Encodes a Destiny event batch while ensuring `OnSpecialFX` stamps never backstep behind
/// `fx_floor_stamp` (max of owner-visible / prime update stamp) while scene `stamp` is unchanged (`#1081`).
pub(super) fn encode_destiny_updates_with_fx_floor(
    stamp: u32,
    fx_floor_stamp: u32,
    events: &[DestinyEvent],
    ballpark: Option<&Ballpark>,
    last_fx_stamp_opt: Option<&std::sync::atomic::AtomicU64>,
) -> PyRep {
    // `#1268`: Keep a single uniform Destiny stamp across the entire update batch whenever
    // `fx_floor_stamp > stamp`, so mixed `(OnSpecialFX, OnDamageStateChange)` or standalone
    // non-FX observer flushes (e.g. `/heal`) never mix stamps or regress behind `last_fx_stamp`.
    let has_set_state = events
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::SetState { .. }));
    let batch_stamp = if has_set_state {
        ballpark.map(|p| p.current_tick as u32).unwrap_or(stamp)
    } else {
        stamp.max(fx_floor_stamp)
    };
    if events
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::OnSpecialFX { .. }))
    {
        if let Some(a) = last_fx_stamp_opt {
            a.fetch_max(u64::from(batch_stamp), std::sync::atomic::Ordering::AcqRel);
        }
    }
    encode_destiny_updates(batch_stamp, events, ballpark)
}

pub(super) fn build_tick_broadcast(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    events: Vec<DestinyEvent>,
) -> TickBroadcast {
    build_tick_broadcast_with_fx_floor(solar_system_id, ballpark, events, 0, None, None)
}

pub(super) fn build_tick_broadcast_with_fx_floor(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    mut events: Vec<DestinyEvent>,
    fx_floor_stamp: u32,
    last_visible_stamp_opt: Option<&std::sync::atomic::AtomicU64>,
    last_fx_stamp_opt: Option<&std::sync::atomic::AtomicU64>,
) -> TickBroadcast {
    events.append(&mut ballpark.pending_events);
    let ready_obs_ids: Vec<BallId> = ballpark
        .observer_events
        .iter()
        .filter_map(|(&obs_id, evs)| {
            if evs.is_empty()
                || ballpark.pending_handoff_observers.contains(&obs_id)
                || evs
                    .iter()
                    .any(|ev| matches!(ev, DestinyEvent::SetState { .. }))
            {
                None
            } else {
                Some(obs_id)
            }
        })
        .collect();
    let mut observer_events = BTreeMap::new();
    for obs_id in ready_obs_ids {
        let obs_evs = ballpark.drain_observer_events(obs_id);
        if !obs_evs.is_empty() {
            observer_events.insert(obs_id, obs_evs);
        }
    }
    let balls = &ballpark.balls;
    ballpark
        .observer_events
        .retain(|id, evs| !evs.is_empty() || balls.contains_key(id));
    let stamp = ballpark.current_tick as u32;
    let effective_fx_floor = stamp.max(fx_floor_stamp);
    let mut observer_updates_pyrep = BTreeMap::new();
    let mut event_cache: HashMap<u64, Vec<(&DestinyEvent, PyRep)>> = HashMap::new();
    let mut batch_cache: HashMap<u64, Vec<(&[DestinyEvent], PyRep)>> = HashMap::new();
    for (&obs_id, obs_evs) in &observer_events {
        let has_set_state = obs_evs
            .iter()
            .any(|ev| matches!(ev, DestinyEvent::SetState { .. }));
        if has_set_state {
            if let Some(a) = last_visible_stamp_opt {
                a.fetch_max(ballpark.current_tick, std::sync::atomic::Ordering::AcqRel);
            }
        }
        let has_fx = obs_evs
            .iter()
            .any(|ev| matches!(ev, DestinyEvent::OnSpecialFX { .. }));
        if !has_set_state && effective_fx_floor > stamp {
            observer_updates_pyrep.insert(
                obs_id,
                encode_destiny_updates_with_fx_floor(
                    stamp,
                    effective_fx_floor,
                    obs_evs,
                    Some(ballpark),
                    last_fx_stamp_opt,
                ),
            );
            continue;
        }
        let mut batch_hasher = DefaultHasher::new();
        batch_hasher.write_usize(obs_evs.len());
        let mut ev_hashes = Vec::with_capacity(obs_evs.len());
        for ev in obs_evs {
            let eh = hash_destiny_event(ev);
            batch_hasher.write_u64(eh);
            ev_hashes.push(eh);
        }
        let bh = batch_hasher.finish();
        if let Some(cached) = batch_cache
            .get(&bh)
            .and_then(|bucket| bucket.iter().find(|(s, _)| *s == obs_evs.as_slice()))
        {
            if has_fx {
                if let Some(a) = last_fx_stamp_opt {
                    a.fetch_max(u64::from(effective_fx_floor), std::sync::atomic::Ordering::AcqRel);
                }
            }
            observer_updates_pyrep.insert(obs_id, cached.1.clone());
            continue;
        }
        let batch_indices = ordered_batch_indices(obs_evs);
        let mut encoded = Vec::with_capacity(batch_indices.len());
        for idx in batch_indices {
            let ev = &obs_evs[idx];
            if matches!(ev, DestinyEvent::OnSpecialFX { .. }) {
                if let Some(a) = last_fx_stamp_opt {
                    a.fetch_max(u64::from(effective_fx_floor), std::sync::atomic::Ordering::AcqRel);
                }
            }
            let eh = ev_hashes[idx];
            let pyrep = if let Some(hit) = event_cache
                .get(&eh)
                .and_then(|bucket| bucket.iter().find(|(c_ev, _)| *c_ev == ev))
            {
                hit.1.clone()
            } else {
                let rep = encode_destiny_event(stamp, ev, Some(ballpark));
                event_cache.entry(eh).or_default().push((ev, rep.clone()));
                rep
            };
            encoded.push(pyrep);
        }
        let list_rep = PyRep::List(encoded);
        batch_cache
            .entry(bh)
            .or_default()
            .push((obs_evs.as_slice(), list_rep.clone()));
        observer_updates_pyrep.insert(obs_id, list_rep);
    }
    TickBroadcast {
        solar_system_id,
        tick: ballpark.current_tick,
        broadcast_seq: 0,
        events,
        observer_events,
        observer_updates_pyrep,
    }
}

pub(super) fn restore_asteroid_from_template(
    id: BallId,
    orig_ast: &AsteroidState,
    orig_ball: &Ball,
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    let mut reset_ast = orig_ast.clone();
    reset_ast.remaining_units = reset_ast.initial_units;
    reset_ast.depleted = false;
    reset_ast.respawn_at_tick = None;
    asteroids.insert(id, reset_ast);
    if !ballpark.balls.contains_key(&id) {
        let (group_id, category_id, name) = sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&orig_ball.type_id))
            .map(|t| {
                (
                    t.group_id,
                    t.category_id,
                    (!t.name.is_empty()).then(|| t.name.clone()),
                )
            })
            .unwrap_or((25, 25, None));
        let mut restored_ball = orig_ball.clone();
        restored_ball.mode = MovementMode::Rigid;
        ballpark.add_static_ball_with_meta(
            restored_ball.clone(),
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                name,
                ..Default::default()
            },
        );
        ballpark.queue_event_for_all_grid_observers(
            Some(id),
            &DestinyEvent::AddBalls2 {
                balls: vec![restored_ball],
            },
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReconcileBeltAction {
    RespawnNow,
    WaitUntil(u64),
    ScheduleNew(u64),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_spawned_belt_asteroids_collecting(
    ballpark: &mut Ballpark,
    solar_system_id: SolarSystemId,
    spawned: Vec<AsteroidState>,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &mut HashMap<BallId, (AsteroidState, Ball)>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    belt_interval_secs: u64,
    downtime_utc_secs: u64,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> Vec<AsteroidState> {
    let (existing_qtys, persisted_due, now_unix): (
        HashMap<i64, i64>,
        HashMap<i64, u64>,
        u64,
    ) = if let Some(db) = db_opt {
        let now_unix = db.clock().now_unix_secs();
        let (qtys, due) = db.with_snapshot(|snap| {
            let qtys = spawned
                .iter()
                .filter_map(|ast| {
                    snap.asteroids
                        .get(&ast.asteroid_id)
                        .map(|rec| (ast.asteroid_id, rec.quantity))
                })
                .collect();
            let due = snap.mining_and_ess_state.belt_respawn_due_unix.clone();
            (qtys, due)
        });
        (qtys, due, now_unix)
    } else {
        (HashMap::new(), HashMap::new(), 0)
    };

    let mut belt_ids: Vec<BallId> = spawned
        .iter()
        .filter(|ast| ast.belt_id > 0)
        .map(|ast| ast.belt_id)
        .collect();
    belt_ids.sort_unstable();
    belt_ids.dedup();

    let mut belt_actions: HashMap<BallId, ReconcileBeltAction> = HashMap::new();
    let mut persist_due_belts: Vec<(BallId, u64)> = Vec::new();
    let mut clear_due_belts: Vec<BallId> = Vec::new();
    if db_opt.is_some() {
        for belt_id in belt_ids {
            let belt_empty_in_db = spawned
                .iter()
                .filter(|a| a.belt_id == belt_id)
                .all(|a| existing_qtys.get(&a.asteroid_id).is_some_and(|&qty| qty <= 0));
            if belt_empty_in_db {
                match persisted_due.get(&belt_id).copied() {
                    Some(due_unix) if now_unix >= due_unix => {
                        belt_actions.insert(belt_id, ReconcileBeltAction::RespawnNow);
                        clear_due_belts.push(belt_id);
                    }
                    Some(due_unix) => {
                        belt_actions.insert(belt_id, ReconcileBeltAction::WaitUntil(due_unix));
                    }
                    None => {
                        let due_unix = next_downtime_unix_secs(
                            now_unix,
                            belt_interval_secs,
                            downtime_utc_secs,
                        );
                        belt_actions.insert(belt_id, ReconcileBeltAction::ScheduleNew(due_unix));
                        persist_due_belts.push((belt_id, due_unix));
                    }
                }
            } else if persisted_due.contains_key(&belt_id) {
                clear_due_belts.push(belt_id);
            }
        }
    }

    let mut to_spawn: Vec<eve_db::AsteroidRecord> = Vec::new();
    let mut to_respawn: Vec<eve_db::AsteroidRecord> = Vec::new();
    let mut populated = Vec::new();
    for mut ast in spawned {
        if let Some(ball) = ballpark.balls.get(&ast.asteroid_id).cloned() {
            asteroid_templates.insert(ast.asteroid_id, (ast.clone(), ball.clone()));
            if db_opt.is_some() {
                let belt_action = belt_actions.get(&ast.belt_id).copied();
                if matches!(belt_action, Some(ReconcileBeltAction::RespawnNow)) {
                    let init_qty = i64::try_from(ast.initial_units).unwrap_or(i64::MAX);
                    ast.remaining_units = ast.initial_units;
                    ast.depleted = false;
                    ast.respawn_at_tick = None;
                    to_respawn.push(eve_db::AsteroidRecord {
                        asteroid_id: ast.asteroid_id,
                        belt_id: ast.belt_id,
                        type_id: ast.type_id,
                        solar_system_id,
                        position: (ball.position.x, ball.position.y, ball.position.z),
                        quantity: init_qty,
                        initial_quantity: init_qty,
                        radius: ball.radius,
                    });
                } else if let Some(&existing_qty) = existing_qtys.get(&ast.asteroid_id) {
                    if existing_qty <= 0 {
                        ast.remaining_units = 0;
                        ast.depleted = true;
                        let _ = ballpark.remove_ball(ast.asteroid_id);
                        if let Some(
                            ReconcileBeltAction::WaitUntil(due_unix)
                            | ReconcileBeltAction::ScheduleNew(due_unix),
                        ) = belt_action
                        {
                            ast.respawn_at_tick = Some(due_unix);
                        }
                    } else {
                        ast.remaining_units = existing_qty as u64;
                    }
                } else {
                    let init_qty = i64::try_from(ast.initial_units).unwrap_or(i64::MAX);
                    to_spawn.push(eve_db::AsteroidRecord {
                        asteroid_id: ast.asteroid_id,
                        belt_id: ast.belt_id,
                        type_id: ast.type_id,
                        solar_system_id,
                        position: (ball.position.x, ball.position.y, ball.position.z),
                        quantity: i64::try_from(ast.remaining_units).unwrap_or(i64::MAX),
                        initial_quantity: init_qty,
                        radius: ball.radius,
                    });
                }
            }
            asteroids.insert(ast.asteroid_id, ast.clone());
            populated.push(ast);
        }
    }
    if db_opt.is_some() {
        for rec in to_spawn {
            post_unlock.push(super::post_unlock::PostUnlockIntent::BeltAsteroidInsertIfAbsent(rec));
        }
        for rec in to_respawn {
            post_unlock.push(super::post_unlock::PostUnlockIntent::BeltAsteroidUpsert(rec));
        }
        for (belt_id, due_unix) in persist_due_belts {
            post_unlock.push(super::post_unlock::PostUnlockIntent::BeltRespawnDueUpsert {
                belt_id,
                due_unix,
            });
        }
        for belt_id in clear_due_belts {
            post_unlock.push(super::post_unlock::PostUnlockIntent::BeltRespawnDueClear(belt_id));
        }
    }
    populated
}

pub(super) fn respawn_due_asteroids_collecting(
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &HashMap<BallId, (AsteroidState, Ball)>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> Vec<BallId> {
    let Some(db) = db_opt else {
        return Vec::new();
    };
    let now_unix = db.clock().now_unix_secs();
    let mut belt_ids: Vec<BallId> = asteroid_templates
        .values()
        .filter(|(ast, _)| ast.belt_id > 0)
        .map(|(ast, _)| ast.belt_id)
        .collect();
    belt_ids.sort_unstable();
    belt_ids.dedup();

    let mut respawned = Vec::new();
    for belt_id in belt_ids {
        let belt_empty_and_due = asteroid_templates
            .iter()
            .filter(|(_, (orig_ast, _))| orig_ast.belt_id == belt_id)
            .all(|(&id, _)| {
                asteroids.get(&id).is_some_and(|a| {
                    a.remaining_units == 0
                        && a.respawn_at_tick.is_some_and(|due_unix| now_unix >= due_unix)
                })
            });
        if belt_empty_and_due {
            for (&id, (orig_ast, orig_ball)) in asteroid_templates {
                if orig_ast.belt_id == belt_id {
                    restore_asteroid_from_template(
                        id,
                        orig_ast,
                        orig_ball,
                        ballpark,
                        asteroids,
                        sde_opt,
                    );
                    let init_qty = i64::try_from(orig_ast.initial_units).unwrap_or(i64::MAX);
                    post_unlock.push(super::post_unlock::PostUnlockIntent::BeltAsteroidUpsert(
                        eve_db::AsteroidRecord {
                            asteroid_id: id,
                            belt_id: orig_ast.belt_id,
                            type_id: orig_ast.type_id,
                            solar_system_id: ballpark.solar_system_id,
                            position: (
                                orig_ball.position.x,
                                orig_ball.position.y,
                                orig_ball.position.z,
                            ),
                            quantity: init_qty,
                            initial_quantity: init_qty,
                            radius: orig_ball.radius,
                        },
                    ));
                    respawned.push(id);
                }
            }
            post_unlock.push(super::post_unlock::PostUnlockIntent::BeltRespawnDueClear(
                belt_id,
            ));
        }
    }
    respawned
}

/// Daily 11:00 UTC downtime maintenance respawn for an active solar system:
/// - Respawns any asteroid belt (`belt_id > 0`) where all asteroids in that belt are depleted (`Asteroids_and_ore.md:69`).
/// - Respawns any depleted ice asteroid (`group_id == 465`) back to `initial_units`.
pub(crate) fn respawn_depleted_belts_on_downtime_inner_collecting(
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    asteroid_templates: &HashMap<BallId, (AsteroidState, Ball)>,
    sde_opt: &Option<Arc<SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> usize {
    let mut belt_ids: Vec<BallId> = asteroid_templates
        .values()
        .filter(|(ast, _)| ast.belt_id > 0)
        .map(|(ast, _)| ast.belt_id)
        .collect();
    belt_ids.sort_unstable();
    belt_ids.dedup();

    let mut to_restore: Vec<BallId> = Vec::new();
    let mut restored_belts: Vec<BallId> = Vec::new();
    for belt_id in belt_ids {
        let belt_fully_depleted = asteroid_templates
            .iter()
            .filter(|(_, (orig_ast, _))| orig_ast.belt_id == belt_id)
            .all(|(&id, _)| {
                asteroids
                    .get(&id)
                    .is_some_and(|a| a.remaining_units == 0 || a.depleted)
            });
        if belt_fully_depleted {
            restored_belts.push(belt_id);
            for (&id, (orig_ast, _)) in asteroid_templates {
                if orig_ast.belt_id == belt_id {
                    to_restore.push(id);
                }
            }
        }
    }

    // Ice belts (`group_id == 465`) respawn depleted ice blocks at downtime
    for (&id, (orig_ast, _)) in asteroid_templates {
        let is_ice = sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&orig_ast.type_id))
            .is_some_and(|t| t.group_id == 465);
        if is_ice
            && asteroids
                .get(&id)
                .is_some_and(|a| a.remaining_units < orig_ast.initial_units || a.depleted)
            && !to_restore.contains(&id)
        {
            to_restore.push(id);
        }
    }

    to_restore.sort_unstable();
    to_restore.dedup();
    let count = to_restore.len();
    for id in to_restore {
        if let Some((orig_ast, orig_ball)) = asteroid_templates.get(&id) {
            restore_asteroid_from_template(id, orig_ast, orig_ball, ballpark, asteroids, sde_opt);
            if db_opt.is_some() {
                let init_qty = i64::try_from(orig_ast.initial_units).unwrap_or(i64::MAX);
                post_unlock.push(super::post_unlock::PostUnlockIntent::BeltAsteroidUpsert(
                    eve_db::AsteroidRecord {
                        asteroid_id: id,
                        belt_id: orig_ast.belt_id,
                        type_id: orig_ast.type_id,
                        solar_system_id: ballpark.solar_system_id,
                        position: (orig_ball.position.x, orig_ball.position.y, orig_ball.position.z),
                        quantity: init_qty,
                        initial_quantity: init_qty,
                        radius: orig_ball.radius,
                    },
                ));
            }
        }
    }
    if db_opt.is_some() && !restored_belts.is_empty() {
        for belt_id in restored_belts {
            post_unlock.push(super::post_unlock::PostUnlockIntent::BeltRespawnDueClear(
                belt_id,
            ));
        }
    }
    count
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_mine_asteroid_collecting(
    solar_system_id: SolarSystemId,
    asteroid_id: BallId,
    requested_units: u64,
    waste_probability: f64,
    persist_db: bool,
    ballpark: &mut Ballpark,
    asteroids: &mut HashMap<BallId, AsteroidState>,
    mechanic_rng: &crate::rng::MechanicRng,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    belt_interval_atomic: &std::sync::atomic::AtomicU64,
    downtime_utc_atomic: &std::sync::atomic::AtomicU64,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> Result<MiningExtractionResult, String> {
    if let Some(ast) = asteroids.get_mut(&asteroid_id) {
        if ast.remaining_units == 0 {
            Err("Asteroid is already depleted".to_string())
        } else {
            let to_mine = requested_units.min(ast.remaining_units);
            ast.remaining_units -= to_mine;

            let wasted_units = if waste_probability > 0.0 && ast.remaining_units > 0 {
                let rolls = waste_probability >= 1.0 || mechanic_rng.roll(waste_probability);
                if rolls {
                    to_mine.min(ast.remaining_units)
                } else {
                    0
                }
            } else {
                0
            };
            ast.remaining_units -= wasted_units;

            let depleted = ast.remaining_units == 0;
            ast.depleted = depleted;
            let remaining_units = ast.remaining_units;
            let ore_type_id = ast.type_id;
            let belt_id = ast.belt_id;
            let belt_fully_depleted = depleted
                && belt_id > 0
                && asteroids
                    .values()
                    .filter(|a| a.belt_id == belt_id)
                    .all(|a| a.remaining_units == 0);
            let belt_due_unix = if belt_fully_depleted {
                db_opt.as_ref().map(|db| {
                    let interval_secs =
                        belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed);
                    let downtime_secs =
                        downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed);
                    next_downtime_unix_secs(
                        db.clock().now_unix_secs(),
                        interval_secs,
                        downtime_secs,
                    )
                })
            } else {
                None
            };
            if (persist_db || depleted || belt_due_unix.is_some()) && db_opt.is_some() {
                post_unlock.push(super::post_unlock::PostUnlockIntent::MineAsteroid(
                    super::post_unlock::MineAsteroidDbUpdate {
                        asteroid_id,
                        belt_id,
                        remaining_units,
                        belt_due_unix,
                    },
                ));
            }
            if depleted {
                let _ = ballpark.remove_ball(asteroid_id);
                if let Some(due_unix) = belt_due_unix {
                    for belt_ast in asteroids.values_mut().filter(|a| a.belt_id == belt_id) {
                        belt_ast.respawn_at_tick = Some(due_unix);
                    }
                }
            }

            Ok(MiningExtractionResult {
                mined_units: to_mine,
                wasted_units,
                remaining_units,
                ore_type_id,
                depleted,
            })
        }
    } else {
        Err(format!(
            "Asteroid {asteroid_id} not found in system {solar_system_id}"
        ))
    }
}

// parity: destiny:OnSlimItemChange
pub(super) fn sync_player_slim_items(
    ballpark: &mut Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) -> bool {
    let Some(db) = db_opt else {
        return false;
    };
    let updates: Vec<(BallId, SlimItemMeta)> = ballpark
        .ball_slim_meta
        .iter()
        .filter_map(|(&ball_id, meta)| {
            if meta.category_id != 6 {
                return None;
            }
            let char_id = meta.char_id?;
            let ch = db.get_character(char_id).ok()?;
            let new_corp_id = ch.corporation_id;
            let new_alliance_id = ch.alliance_id.unwrap_or(0);
            let new_sec = ch.security_status;
            if meta.corp_id != new_corp_id
                || meta.alliance_id != new_alliance_id
                || (meta.security_status - new_sec).abs() > 1e-9
            {
                let mut updated = meta.clone();
                updated.corp_id = new_corp_id;
                updated.alliance_id = new_alliance_id;
                updated.security_status = new_sec;
                Some((ball_id, updated))
            } else {
                None
            }
        })
        .collect();
    let changed = !updates.is_empty();
    for (ball_id, updated_meta) in updates {
        ballpark.change_slim_item(ball_id, updated_meta);
    }
    changed
}

#[cfg(test)]
#[path = "belts_tests.rs"]
mod belts_tests;

#[cfg(test)]
pub(crate) use belts_tests::respawn_depleted_belts_on_downtime_inner;
#[cfg(test)]
pub(super) use belts_tests::{handle_mine_asteroid, respawn_due_asteroids};
