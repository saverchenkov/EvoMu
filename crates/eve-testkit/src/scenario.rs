//! Player-driven scenario builder, presets, and deterministic RNG helpers (`#1540`).
//!
//! Constructs test actors through the same operations real EVE players perform:
//! 1. Provision & authenticate account (`authentication.Login`).
//! 2. Create and select character (`charUnboundMgr.CreateCharacterWithDoll` + `SelectCharacterID`).
//! 3. Fund ISK (treasury transfer) and optionally upgrade to Omega.
//! 4. Inject and train required ship, module, corp, or industry skills (`skillMgr2`).
//! 5. Found or join a player corporation with requested roles (`corpRegistry`).
//! 6. Purchase hull, fitted modules, and charges on the station market (`marketProxy.PlaceBuyOrder`).
//! 7. Assemble, board, fit modules (`ship.AssembleShip`, `ship.Board`, `invbroker.Add`), and load
//!    compatible ammunition (`dogmaIM.LoadAmmo`).
//! 8. Undock into space when requested (`stationSvc.Undock`), form/join fleets (`fleetObjectHandler`).
//! 9. Validate every built actor via [`crate::legal_fixture::assert_legal_fixture`].

#![forbid(unsafe_code)]

use crate::client_harness::{ClientHarness, ClientWireTransport};
use crate::legal_fixture::{
    assert_legal_fixture, FixtureSessionView, LegalFixtureExpectation,
};
use eve_db::{corp_roles, InventoryFlag, WorldRepository};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

/// Canonical fixed seed used by [`DeterministicRng`] unless a test opts into an explicit seed (`#1540`).
pub const DEFAULT_TEST_SEED: u64 = 0x00C5_5EED_0000_0001;

/// SplitMix64 deterministic PRNG so tests never rely on process hash seeds or ambient entropy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
}

impl Default for DeterministicRng {
    fn default() -> Self { Self::new(DEFAULT_TEST_SEED) }
}

impl DeterministicRng {
    /// Creates a deterministic RNG initialized from `seed`.
    pub fn new(seed: u64) -> Self { Self { state: seed } }

    /// Reads `EVOMU_TEST_SEED` if explicitly provided (`0x...` hex or decimal), otherwise uses [`DEFAULT_TEST_SEED`].
    pub fn from_env_or_default() -> Self {
        if let Ok(raw) = std::env::var("EVOMU_TEST_SEED") {
            let trimmed = raw.trim();
            let parsed = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
                .map_or_else(|| trimmed.parse::<u64>().ok(), |h| u64::from_str_radix(h, 16).ok());
            if let Some(seed) = parsed {
                return Self::new(seed);
            }
        }
        Self::default()
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_u32(&mut self) -> u32 { (self.next_u64() >> 32) as u32 }

    pub fn next_usize(&mut self, bound: usize) -> usize {
        if bound <= 1 { 0 } else { (self.next_u64() % (bound as u64)) as usize }
    }

    pub fn next_unit_f64(&mut self) -> f64 {
        const INV_TWO_POW_53: f64 = 1.0 / ((1_u64 << 53) as f64);
        ((self.next_u64() >> 11) as f64) * INV_TWO_POW_53
    }

    pub fn range_i64(&mut self, low_inclusive: i64, high_exclusive: i64) -> i64 {
        if high_exclusive <= low_inclusive { return low_inclusive; }
        low_inclusive.wrapping_add_unsigned(self.next_u64() % high_exclusive.abs_diff(low_inclusive))
    }

    /// Deterministically selects one item from `slice` (never depends on hash randomization).
    pub fn pick_slice<'a, T>(&mut self, slice: &'a [T]) -> Option<&'a T> {
        (!slice.is_empty()).then(|| &slice[self.next_usize(slice.len())])
    }

    /// Sorts matching map entries by ascending key `K`, then deterministically picks one entry via RNG.
    pub fn pick_sorted_map_entry<'a, K: Ord + Copy, V, S: std::hash::BuildHasher>(
        &mut self,
        map: &'a HashMap<K, V, S>,
        pred: impl FnMut(&K, &V) -> bool,
    ) -> Option<(K, &'a V)> {
        let sorted = SdeRegistry::pick_lowest_entries_sorted(map, pred);
        (!sorted.is_empty()).then(|| sorted[self.next_usize(sorted.len())])
    }

    /// Always returns the lowest-ID SDE type satisfying `pred` (`SdeRegistry::lowest_matching_type`).
    pub fn pick_lowest_type<'a>(&self, sde: &'a SdeRegistry, pred: impl FnMut(&InvType) -> bool) -> Option<&'a InvType> {
        sde.lowest_matching_type(pred)
    }
}

/// Ship, modules, compatible charges, and cargo/hangar items to acquire and fit for an actor.
#[derive(Debug, Clone, Default)]
pub struct ShipLoadoutSpec {
    pub hull_type_id: TypeId,
    pub modules: Vec<(InventoryFlag, TypeId)>,
    pub charges: Vec<(InventoryFlag, TypeId, i32)>,
    pub ship_cargo: Vec<(TypeId, i64)>,
    pub station_hangar_items: Vec<(TypeId, i64)>,
}

/// Corporation setup action for a scenario actor.
#[derive(Debug, Clone, Default)]
pub enum CorpSetup {
    #[default]
    NpcStarter,
    FoundPlayerCorp { name: String, ticker: String, tax_rate: f64, required_roles: u64 },
    JoinActorCorp { founder_actor_index: usize, required_roles: u64 },
}

/// Fleet setup action for a scenario actor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FleetSetup {
    #[default]
    None,
    CreateFleet,
    JoinActorFleet { leader_actor_index: usize },
}

/// Declarative recipe for one pilot created through real player operations.
#[derive(Debug, Clone)]
pub struct ActorSpec {
    pub label: String,
    pub username: String,
    pub password: String,
    pub character_name: String,
    pub bloodline_id: u32,
    pub starting_isk: f64,
    pub require_omega: bool,
    pub extra_skills: Vec<TypeId>,
    pub corp: CorpSetup,
    pub ship: Option<ShipLoadoutSpec>,
    pub station_hangar_items: Vec<(TypeId, i64)>,
    pub in_space: bool,
    pub fleet: FleetSetup,
}

impl Default for ActorSpec {
    fn default() -> Self {
        Self {
            label: "pilot".into(),
            username: "scenario_pilot".into(),
            password: "scenario-password".into(),
            character_name: "Scenario Pilot".into(),
            bloodline_id: 1,
            starting_isk: 50_000_000.0,
            require_omega: false,
            extra_skills: Vec::new(),
            corp: CorpSetup::NpcStarter,
            ship: None,
            station_hangar_items: Vec::new(),
            in_space: false,
            fleet: FleetSetup::None,
        }
    }
}

/// Built actor returned by [`ScenarioBuilder::build`] alongside its live [`ClientHarness`].
pub struct BuiltActor<T: ClientWireTransport> {
    pub label: String,
    pub username: String,
    pub user_id: u64,
    pub character_id: i64,
    pub corporation_id: i64,
    pub station_id: Option<i64>,
    pub solar_system_id: u32,
    pub ship_id: Option<i64>,
    pub module_item_ids: Vec<(InventoryFlag, i64, TypeId)>,
    pub fleet_id: Option<i64>,
    pub hangar_item_ids: BTreeMap<TypeId, Vec<i64>>,
    pub harness: ClientHarness<T>,
    pub expectation: LegalFixtureExpectation,
}

/// Collection of constructed actors returned by [`ScenarioBuilder::build`].
pub struct BuiltScenario<T: ClientWireTransport> {
    pub seed: u64,
    pub actors: Vec<BuiltActor<T>>,
}

impl<T: ClientWireTransport> BuiltScenario<T> {
    pub fn actor(&self, idx: usize) -> &BuiltActor<T> { &self.actors[idx] }
    pub fn actor_mut(&mut self, idx: usize) -> &mut BuiltActor<T> { &mut self.actors[idx] }
}

/// Server/DB runtime bridge implemented by test harnesses (e.g. `ServerScenarioBackend`)
/// so [`ScenarioBuilder`] lives in `eve-testkit` without depending on `eve-server`.
pub trait ScenarioWorldBackend {
    type Transport: ClientWireTransport + Send;

    fn sde(&self) -> &SdeRegistry;
    fn db(&self) -> &WorldRepository;
    fn provision_player_account(&self, username: &str, password: &str) -> u64;
    fn next_session_id(&mut self) -> u64;
    fn create_actor_transport(&mut self, session_id: u64) -> Self::Transport;
    fn actor_session_view(&self, transport: &Self::Transport) -> FixtureSessionView;
    fn sync_actor_session(&mut self, transport: &mut Self::Transport);
    fn fund_character_isk<'a>(&'a mut self, char_id: i64, amount_isk: f64) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
    fn upgrade_actor_to_omega<'a>(&'a mut self, transport: &'a mut Self::Transport) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
    fn inject_unavailable_skillbook_id(&mut self, char_id: i64, station_id: i64, skill_id: TypeId) -> i64;
    fn ensure_station_market_liquidity(&mut self, station_id: i64, requirements: &[(TypeId, i64)]);
    fn ship_in_space_ballpark<'a>(&'a mut self, solar_system_id: u32, ship_id: i64) -> Pin<Box<dyn Future<Output = Option<bool>> + Send + 'a>>;
    fn advance_secs(&self, secs: u64);
}

/// Fluent player-driven scenario builder with deterministic defaults and preset builders (`#1540`).
#[derive(Debug, Clone)]
pub struct ScenarioBuilder {
    pub rng: DeterministicRng,
    pub seed: u64,
    pub actors: Vec<ActorSpec>,
}

const RIFTER_HULL_TYPE_ID: TypeId = 587;
const AUTOCANNON_125MM_TYPE_ID: TypeId = 484;
const SMALL_PROJ_AMMO_LOWEST_ID: TypeId = 178; // Carbonized Lead S
const BADGER_HAULER_TYPE_ID: TypeId = 648;
const SKILL_CORP_MANAGEMENT: TypeId = 3363;
const SKILL_INDUSTRY: TypeId = 3380;

impl ScenarioBuilder {
    pub fn new() -> Self { Self::with_seed(DEFAULT_TEST_SEED) }

    pub fn with_seed(seed: u64) -> Self {
        Self { rng: DeterministicRng::new(seed), seed, actors: Vec::new() }
    }

    pub fn add_actor(mut self, actor: ActorSpec) -> Self {
        self.actors.push(actor);
        self
    }

    fn combat_loadout() -> ShipLoadoutSpec {
        ShipLoadoutSpec {
            hull_type_id: RIFTER_HULL_TYPE_ID,
            modules: vec![(InventoryFlag::HiSlot0, AUTOCANNON_125MM_TYPE_ID), (InventoryFlag::HiSlot1, AUTOCANNON_125MM_TYPE_ID)],
            charges: vec![(InventoryFlag::HiSlot0, SMALL_PROJ_AMMO_LOWEST_ID, 40), (InventoryFlag::HiSlot1, SMALL_PROJ_AMMO_LOWEST_ID, 40)],
            ..Default::default()
        }
    }

    /// Preset: single combat pilot flying a fitted Rifter loaded with Carbonized Lead S in space.
    pub fn combat_pilot_in_space() -> Self {
        let mut b = Self::new();
        let tag = (b.seed & 0xFFFF) as u16;
        b.actors.push(ActorSpec {
            label: "combat_pilot".into(), username: format!("combat_{tag:04x}"), character_name: format!("Combat Pilot {tag:04X}"),
            ship: Some(Self::combat_loadout()), in_space: true, ..ActorSpec::default()
        });
        b
    }

    /// Preset: docked industrialist in a Badger hauler with Industry skill trained and manufacturing supplies.
    pub fn industrialist_docked(sde: &SdeRegistry) -> Self {
        let mut b = Self::new();
        let tag = (b.seed & 0xFFFF) as u16;
        let bp = sde
            .lowest_matching_blueprint(|bp| {
                sde.types.get(&bp.blueprint_type_id).is_some_and(|t| t.published && t.market_group_id.is_some())
                    && sde.types.get(&bp.product_type_id).is_some_and(|p| p.published && p.category_id == 7)
                    && !bp.materials.is_empty()
                    && bp.materials.iter().all(|m| (34..=40).contains(&m.type_id) && m.quantity > 0 && m.quantity <= 60)
            })
            .expect("published mineral module blueprint in SDE");
        let mut hangar_items = vec![(bp.blueprint_type_id, 1)];
        hangar_items.extend(bp.materials.iter().map(|m| (m.type_id, i64::try_from(m.quantity).expect("material qty fits i64"))));
        b.actors.push(ActorSpec {
            label: "industrialist".into(), username: format!("indy_{tag:04x}"), character_name: format!("Indy Pilot {tag:04X}"),
            extra_skills: vec![SKILL_INDUSTRY],
            ship: Some(ShipLoadoutSpec { hull_type_id: BADGER_HAULER_TYPE_ID, ..Default::default() }),
            station_hangar_items: hangar_items, in_space: false, ..ActorSpec::default()
        });
        b
    }

    /// Preset: character who founds a real player corporation and holds `ROLE_DIRECTOR`.
    pub fn corporation_director() -> Self {
        let mut b = Self::new();
        let tag = (b.seed & 0xFFFF) as u16;
        b.actors.push(ActorSpec {
            label: "corp_director".into(), username: format!("dir_{tag:04x}"), character_name: format!("Corp Dir {tag:04X}"),
            starting_isk: 100_000_000.0, extra_skills: vec![SKILL_CORP_MANAGEMENT],
            corp: CorpSetup::FoundPlayerCorp {
                name: format!("Scenario Holdings {tag:04X}"), ticker: format!("S{:04X}", tag & 0x0FFF),
                tax_rate: 0.05, required_roles: corp_roles::ROLE_DIRECTOR,
            },
            ship: Some(ShipLoadoutSpec { hull_type_id: RIFTER_HULL_TYPE_ID, ..Default::default() }),
            in_space: false, ..ActorSpec::default()
        });
        b
    }

    /// Preset: two combat-fitted pilots in space in the same solar system and active fleet.
    pub fn two_pilots_in_fleet() -> Self {
        let mut b = Self::new();
        let tag = (b.seed & 0xFFFF) as u16;
        b.actors.push(ActorSpec {
            label: "fleet_commander".into(), username: format!("fc_{tag:04x}"), character_name: format!("Fleet Lead {tag:04X}"),
            ship: Some(Self::combat_loadout()), in_space: true, fleet: FleetSetup::CreateFleet, ..ActorSpec::default()
        });
        b.actors.push(ActorSpec {
            label: "fleet_wingman".into(), username: format!("wing_{tag:04x}"), character_name: format!("Fleet Wing {tag:04X}"),
            ship: Some(Self::combat_loadout()), in_space: true, fleet: FleetSetup::JoinActorFleet { leader_actor_index: 0 }, ..ActorSpec::default()
        });
        b
    }

    /// Builds every configured actor through real player RPC flows and verifies legal-fixture invariants.
    pub async fn build<B: ScenarioWorldBackend>(&self, backend: &mut B) -> BuiltScenario<B::Transport> {
        let mut built: Vec<BuiltActor<B::Transport>> = Vec::with_capacity(self.actors.len());

        // Step 1: Account provisioning, login, character creation, selection, funding, and skills.
        for (idx, spec) in self.actors.iter().enumerate() {
            let username = unique_username(backend.db(), &spec.username, idx);
            let char_name = unique_char_name(backend.db(), &spec.character_name, idx);
            let user_id = backend.provision_player_account(&username, &spec.password);
            let sess_id = backend.next_session_id();
            let mut harness = ClientHarness::new(backend.create_actor_transport(sess_id));

            rpc_ok(&mut harness, "authentication", "Login", &[PyRep::String(username.clone()), PyRep::String(spec.password.clone())]).await;

            let (ancestry_id, school_id) = {
                let sde = backend.sde();
                let anc = sde.lowest_matching_ancestry(|a| a.bloodline_id == spec.bloodline_id).map(|a| a.ancestry_id).unwrap_or(11);
                let race = sde.bloodlines.get(&spec.bloodline_id).map(|b| b.race_id).unwrap_or(1);
                let school = sde.lowest_matching_school(|sc| sc.race_id == race).map(|sc| sc.school_id).unwrap_or(17);
                (anc, school)
            };
            let doll = PyRep::keyval([("costume", PyRep::Integer(301)), ("gender", PyRep::Integer(1)), ("hair", PyRep::Integer(101)), ("skin", PyRep::Integer(201))]);
            let portrait = PyRep::keyval([("background", PyRep::Integer(42)), ("camera", PyRep::Integer(1)), ("pose", PyRep::Integer(7))]);
            let cid_rep = rpc_ok(&mut harness, "charUnboundMgr", "CreateCharacterWithDoll", &[
                PyRep::String(char_name), PyRep::Integer(i64::from(spec.bloodline_id)), PyRep::Integer(1),
                PyRep::Integer(i64::from(ancestry_id)), doll, portrait, PyRep::Integer(i64::from(school_id)),
            ]).await;
            let character_id = cid_rep.as_i64().expect("CreateCharacterWithDoll must return characterID");

            rpc_ok(&mut harness, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(character_id)]).await;
            backend.advance_secs(12);
            backend.sync_actor_session(harness.transport_mut());

            backend.fund_character_isk(character_id, spec.starting_isk).await;
            if spec.require_omega { backend.upgrade_actor_to_omega(harness.transport_mut()).await; }

            let mut skill_source_types = spec.extra_skills.clone();
            if matches!(spec.corp, CorpSetup::FoundPlayerCorp { .. }) { skill_source_types.push(SKILL_CORP_MANAGEMENT); }
            if let Some(ship) = &spec.ship {
                skill_source_types.push(ship.hull_type_id);
                skill_source_types.extend(ship.modules.iter().map(|&(_, mid)| mid));
            }
            let view = backend.actor_session_view(harness.transport());
            let station_id = view.station_id.expect("new character must spawn docked");
            let solar_system_id = view.solar_system_id.expect("new character solar_system_id");
            train_required_skills(backend, &mut harness, character_id, station_id, &skill_source_types).await;

            built.push(BuiltActor {
                label: spec.label.clone(), username, user_id, character_id,
                corporation_id: view.corporation_id.unwrap_or(0), station_id: Some(station_id),
                solar_system_id, ship_id: view.ship_id, module_item_ids: Vec::new(), fleet_id: None,
                hangar_item_ids: BTreeMap::new(), harness, expectation: LegalFixtureExpectation::default(),
            });
        }

        // Step 2: Found and join player corporations while docked.
        for idx in 0..self.actors.len() {
            if let CorpSetup::FoundPlayerCorp { name, ticker, tax_rate, required_roles } = &self.actors[idx].corp {
                let (c_name, c_ticker) = unique_corp_name_ticker(backend.db(), name, ticker, idx);
                let corp_rep = rpc_ok(&mut built[idx].harness, "corpRegistry", "AddCorporation", &[
                    PyRep::String(c_name), PyRep::String(c_ticker), PyRep::Real(*tax_rate),
                ]).await;
                let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64())
                    .or_else(|| corp_rep.as_i64()).expect("AddCorporation must return corporationID");
                backend.advance_secs(12);
                backend.sync_actor_session(built[idx].harness.transport_mut());
                built[idx].corporation_id = corp_id;
                built[idx].expectation.require_player_corp = true;
                built[idx].expectation.expected_corp_roles = *required_roles;
            }
        }
        for idx in 0..self.actors.len() {
            if let CorpSetup::JoinActorCorp { founder_actor_index, required_roles } = self.actors[idx].corp {
                let (corp_id, char_id, base_station) = (built[founder_actor_index].corporation_id, built[idx].character_id, built[idx].station_id.unwrap_or(0));
                let app_rep = rpc_ok(&mut built[idx].harness, "corpRegistry", "InsertApplication", &[
                    PyRep::Integer(corp_id), PyRep::String("Applying to player corp".into()),
                ]).await;
                let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("InsertApplication applicationID");
                rpc_ok(&mut built[founder_actor_index].harness, "corpRegistry", "UpdateApplicationOffer", &[
                    PyRep::Integer(app_id), PyRep::String("Accepted".into()),
                ]).await;
                if required_roles != 0 {
                    let r = PyRep::Integer(required_roles as i64);
                    rpc_ok(&mut built[founder_actor_index].harness, "corpRegistry", "UpdateMember", &[
                        PyRep::Integer(char_id), PyRep::String("Officer".into()), PyRep::Integer(1), PyRep::Integer(0),
                        r.clone(), PyRep::Integer(0), r.clone(), PyRep::Integer(0), r,
                        PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(base_station), PyRep::Integer(0),
                    ]).await;
                }
                backend.advance_secs(12);
                backend.sync_actor_session(built[idx].harness.transport_mut());
                built[idx].corporation_id = corp_id;
                built[idx].expectation.require_player_corp = true;
                built[idx].expectation.expected_corp_roles = required_roles;
            }
        }

        // Step 3: Acquire hangar items, assemble & board ship, fit modules, load ammo, undock.
        for idx in 0..self.actors.len() {
            let (station_id, char_id) = (built[idx].station_id.expect("actor docked before fitting"), built[idx].character_id);
            let mut market_reqs = self.actors[idx].station_hangar_items.clone();
            if let Some(ship) = &self.actors[idx].ship {
                market_reqs.push((ship.hull_type_id, 1));
                market_reqs.extend(ship.modules.iter().map(|&(_, mid)| (mid, 1)));
                market_reqs.extend(ship.charges.iter().map(|&(_, cid, qty)| (cid, i64::from(qty))));
                market_reqs.extend(ship.ship_cargo.iter().copied());
                market_reqs.extend(ship.station_hangar_items.iter().copied());
            }
            if !market_reqs.is_empty() { backend.ensure_station_market_liquidity(station_id, &market_reqs); }

            for &(tid, qty) in &self.actors[idx].station_hangar_items {
                let ids = buy_at_station(backend, &mut built[idx].harness, char_id, station_id, tid, qty).await;
                built[idx].hangar_item_ids.entry(tid).or_default().extend(ids);
            }

            if let Some(ship) = &self.actors[idx].ship {
                for &(tid, qty) in &ship.station_hangar_items {
                    let ids = buy_at_station(backend, &mut built[idx].harness, char_id, station_id, tid, qty).await;
                    built[idx].hangar_item_ids.entry(tid).or_default().extend(ids);
                }

                let ship_id = buy_at_station(backend, &mut built[idx].harness, char_id, station_id, ship.hull_type_id, 1).await[0];
                rpc_ok(&mut built[idx].harness, "ship", "AssembleShip", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await;
                backend.advance_secs(12);
                rpc_ok(&mut built[idx].harness, "ship", "Board", &[PyRep::Integer(ship_id)]).await;
                backend.advance_secs(12);
                backend.sync_actor_session(built[idx].harness.transport_mut());
                built[idx].ship_id = Some(ship_id);

                let mut fitted_by_flag: HashMap<InventoryFlag, i64> = HashMap::new();
                for &(flag, mod_tid) in &ship.modules {
                    let src_id = buy_at_station(backend, &mut built[idx].harness, char_id, station_id, mod_tid, 1).await[0];
                    rpc_ok(&mut built[idx].harness, "invbroker", "Add", &[
                        PyRep::Integer(src_id), PyRep::Integer(ship_id), PyRep::Integer(i64::from(flag as i32)), PyRep::Integer(1),
                    ]).await;
                    let mod_item_id = find_ship_slot_item(backend.db(), ship_id, flag, mod_tid);
                    fitted_by_flag.insert(flag, mod_item_id);
                    built[idx].module_item_ids.push((flag, mod_item_id, mod_tid));
                }

                for &(flag, charge_tid, qty) in &ship.charges {
                    let mod_item_id = *fitted_by_flag.get(&flag).unwrap_or_else(|| panic!("no fitted module in {flag:?} to load charge {charge_tid}"));
                    let ammo_hangar_id = buy_at_station(backend, &mut built[idx].harness, char_id, station_id, charge_tid, i64::from(qty)).await[0];
                    rpc_ok(&mut built[idx].harness, "invbroker", "Add", &[
                        PyRep::Integer(ammo_hangar_id), PyRep::Integer(ship_id), PyRep::Integer(i64::from(InventoryFlag::Cargo as i32)), PyRep::Integer(i64::from(qty)),
                    ]).await;
                    let cargo_ammo_id = find_ship_slot_item(backend.db(), ship_id, InventoryFlag::Cargo, charge_tid);
                    rpc_ok(&mut built[idx].harness, "dogmaIM", "LoadAmmo", &[
                        PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_item_id)]), PyRep::List(vec![PyRep::Integer(cargo_ammo_id)]),
                    ]).await;
                }

                for &(tid, qty) in &ship.ship_cargo {
                    let cargo_id = buy_at_station(backend, &mut built[idx].harness, char_id, station_id, tid, qty).await[0];
                    rpc_ok(&mut built[idx].harness, "invbroker", "Add", &[
                        PyRep::Integer(cargo_id), PyRep::Integer(ship_id), PyRep::Integer(i64::from(InventoryFlag::Cargo as i32)), PyRep::Integer(qty),
                    ]).await;
                }
            }

            if self.actors[idx].in_space {
                rpc_ok(&mut built[idx].harness, "stationSvc", "Undock", &[]).await;
                backend.advance_secs(15);
                backend.sync_actor_session(built[idx].harness.transport_mut());
                let view = backend.actor_session_view(built[idx].harness.transport());
                built[idx].station_id = view.station_id;
                if let Some(sys) = view.solar_system_id { built[idx].solar_system_id = sys; }
                built[idx].expectation.expect_in_space = Some(true);
            } else {
                built[idx].expectation.expect_in_space = Some(false);
                built[idx].expectation.expected_dock_location_id = built[idx].station_id;
            }
        }

        // Step 4: Form and join fleets.
        for idx in 0..self.actors.len() {
            if matches!(self.actors[idx].fleet, FleetSetup::CreateFleet) {
                let rep = rpc_ok(&mut built[idx].harness, "fleetObjectHandler", "CreateFleet", &[]).await;
                backend.advance_secs(2);
                backend.sync_actor_session(built[idx].harness.transport_mut());
                let view = backend.actor_session_view(built[idx].harness.transport());
                let fleet_id = rep.as_i64().or(view.fleet_id).expect("CreateFleet must produce fleet_id");
                built[idx].fleet_id = Some(fleet_id);
                built[idx].expectation.require_fleet_membership = true;
            }
        }
        // Join via Fleet Finder auto-join advert rather than Invite/AcceptInvite so ClientHarness wire checks stay clean
        // (fleetObjectHandler.Invite currently emits a pre-existing 5-arg OnFleetInvite vs client's 4-arg expectation).
        let mut advertised_leaders = BTreeSet::new();
        for idx in 0..self.actors.len() {
            if let FleetSetup::JoinActorFleet { leader_actor_index } = self.actors[idx].fleet {
                let fleet_id = built[leader_actor_index].fleet_id.expect("leader must create fleet first");
                if advertised_leaders.insert(leader_actor_index) {
                    let advert = PyRep::keyval([
                        ("fleetName", PyRep::String(format!("Scenario Fleet {fleet_id}"))),
                        ("description", PyRep::String("Scenario builder fleet".into())),
                        ("inviteScope", PyRep::Integer(8)),
                        ("joinNeedsApproval", PyRep::Bool(false)),
                        ("minStanding", PyRep::Real(-10.0)),
                        ("minSecurity", PyRep::Real(-10.0)),
                    ]);
                    rpc_ok(&mut built[leader_actor_index].harness, "fleetProxy", "AddFleetFinderAdvert", &[advert]).await;
                }
                rpc_ok(&mut built[idx].harness, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id), PyRep::Bool(false)]).await;
                backend.advance_secs(2);
                backend.sync_actor_session(built[idx].harness.transport_mut());
                built[idx].fleet_id = Some(fleet_id);
                built[idx].expectation.require_fleet_membership = true;
            }
        }
        for leader_idx in advertised_leaders {
            rpc_ok(&mut built[leader_idx].harness, "fleetProxy", "RemoveFleetFinderAdvert", &[]).await;
        }

        // Step 5: Mandatory end-of-setup legal-fixture verification on every built actor.
        for actor in &mut built {
            let view = backend.actor_session_view(actor.harness.transport());
            if actor.expectation.expect_in_space == Some(true) {
                if let Some(ship_id) = view.ship_id {
                    actor.expectation.ship_in_space_ballpark = backend.ship_in_space_ballpark(actor.solar_system_id, ship_id).await;
                }
            }
            let (expectation, label, sde) = (&actor.expectation, &actor.label, backend.sde());
            backend.db().with_snapshot(|snap| assert_legal_fixture(label, snap, sde, &view, expectation));
        }

        BuiltScenario { seed: self.seed, actors: built }
    }
}

async fn rpc_ok<T: ClientWireTransport>(harness: &mut ClientHarness<T>, service: &str, method: &str, args: &[PyRep]) -> PyRep {
    harness.call_ok(service, None, method, args, &HashMap::new()).await
}

impl Default for ScenarioBuilder {
    fn default() -> Self { Self::new() }
}

fn unique_username(db: &WorldRepository, base: &str, idx: usize) -> String {
    db.with_snapshot(|snap| {
        if !snap.accounts.contains_key(base) { return base.to_string(); }
        (1..10_000).map(|n| format!("{base}_{idx}_{n}")).find(|c| !snap.accounts.contains_key(c)).unwrap_or_else(|| format!("{base}_{idx}_overflow"))
    })
}

fn unique_char_name(db: &WorldRepository, base: &str, idx: usize) -> String {
    db.with_snapshot(|snap| {
        let taken = |name: &str| snap.characters.values().any(|c| c.name.eq_ignore_ascii_case(name));
        if !taken(base) { return base.to_string(); }
        (1..10_000).map(|n| format!("{base} {idx}{n}")).find(|c| c.len() <= 36 && !taken(c)).unwrap_or_else(|| format!("Scenario Actor {idx}"))
    })
}

fn unique_corp_name_ticker(db: &WorldRepository, base_name: &str, base_ticker: &str, idx: usize) -> (String, String) {
    db.with_snapshot(|snap| {
        let name_taken = |n: &str| snap.corporations.values().any(|c| c.name.eq_ignore_ascii_case(n));
        let tick_taken = |t: &str| snap.corporations.values().any(|c| c.ticker.eq_ignore_ascii_case(t));
        if !name_taken(base_name) && !tick_taken(base_ticker) { return (base_name.to_string(), base_ticker.to_string()); }
        (1..1_000).map(|n| (format!("{base_name} {idx}{n}"), format!("S{idx}{n:02X}")))
            .find(|(cn, ct)| !name_taken(cn) && !tick_taken(ct))
            .unwrap_or_else(|| (format!("{base_name} {idx}"), format!("SC{idx}X")))
    })
}

fn collect_skill_topo(sde: &SdeRegistry, tid: TypeId, seen: &mut HashSet<TypeId>, out: &mut Vec<TypeId>) {
    if !seen.insert(tid) { return; }
    let mut direct: Vec<(TypeId, u8)> = sde.required_skills_for_type(tid).into_iter().collect();
    direct.sort_by_key(|&(sk, _)| sk);
    for (req_sk, _) in direct { collect_skill_topo(sde, req_sk, seen, out); }
    if sde.types.get(&tid).is_some_and(|t| t.category_id == 16) { out.push(tid); }
}

async fn train_required_skills<B: ScenarioWorldBackend>(
    backend: &mut B,
    harness: &mut ClientHarness<B::Transport>,
    char_id: i64,
    station_id: i64,
    source_types: &[TypeId],
) {
    let (mut seen, mut topo, mut req_levels) = (HashSet::new(), Vec::new(), BTreeMap::<TypeId, u8>::new());
    {
        let sde = backend.sde();
        for &tid in source_types {
            collect_skill_topo(sde, tid, &mut seen, &mut topo);
            let mut visited = HashSet::new();
            collect_skill_req_levels(sde, tid, &mut req_levels, &mut visited, 0);
        }
    }
    for sid in topo {
        let target_lvl = req_levels.get(&sid).copied().unwrap_or(1).max(1);
        let (cur_lvl, known) = backend.db().with_snapshot(|s| {
            let m = s.skills.get(&char_id);
            (m.and_then(|x| x.get(&sid)).map(|r| r.level).unwrap_or(0), m.is_some_and(|x| x.contains_key(&sid)))
        });
        if cur_lvl >= target_lvl { continue; }
        if !known {
            if let Err(err) = harness.call("skillMgr2", None, "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(i64::from(sid))])], &HashMap::new()).await {
                if err.code == "SkillUnavailableForPurchase" {
                    let book_id = backend.inject_unavailable_skillbook_id(char_id, station_id, sid);
                    rpc_ok(harness, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(book_id)]).await;
                } else {
                    panic!("skillMgr2.PurchaseSkills({sid}) failed: {err:?}");
                }
            }
        }
        let queue: Vec<PyRep> = ((cur_lvl + 1)..=target_lvl)
            .map(|lvl| PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(lvl))]))
            .collect();
        if !queue.is_empty() {
            rpc_ok(harness, "skillMgr2", "SaveNewQueue", &[PyRep::List(queue)]).await;
            backend.advance_secs(90 * 86_400);
            let _rep = rpc_ok(harness, "skillMgr2", "GetSkillQueueAndFreePoints", &[]).await;
        }
    }
}

fn collect_skill_req_levels(sde: &SdeRegistry, type_id: TypeId, out: &mut BTreeMap<TypeId, u8>, visited: &mut HashSet<TypeId>, depth: u8) {
    if depth > 8 || !visited.insert(type_id) { return; }
    for (sk, lvl) in sde.required_skills_for_type(type_id) {
        out.entry(sk).and_modify(|e| *e = (*e).max(lvl)).or_insert(lvl);
        if sde.types.contains_key(&sk) { collect_skill_req_levels(sde, sk, out, visited, depth + 1); }
    }
}

fn round_four_sig_figs(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 { return 1.0; }
    let tick = 10_f64.powi(v.log10().floor() as i32 - 3).max(0.01);
    ((v / tick).ceil() * tick * 100.0).round() / 100.0
}

fn hangar_item_ids_for_type(db: &WorldRepository, owner_id: i64, station_id: i64, type_id: TypeId) -> Vec<i64> {
    db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap.items.values()
            .filter(|i| i.owner_id == owner_id && i.location_id == station_id && i.flag == InventoryFlag::Hangar && i.type_id == type_id && i.quantity > 0)
            .map(|i| i.item_id).collect();
        ids.sort_unstable();
        ids
    })
}

async fn buy_at_station<B: ScenarioWorldBackend>(
    backend: &mut B,
    harness: &mut ClientHarness<B::Transport>,
    char_id: i64,
    station_id: i64,
    type_id: TypeId,
    qty: i64,
) -> Vec<i64> {
    let before: HashSet<i64> = hangar_item_ids_for_type(backend.db(), char_id, station_id, type_id).into_iter().collect();
    let ask = backend.db().with_snapshot(|snap| {
        snap.market_orders.values()
            .filter(|o| !o.is_bid && o.station_id == station_id && o.type_id == type_id && o.vol_remaining > 0)
            .map(|o| o.price.to_wire_f64()).fold(0.0_f64, f64::max)
    });
    assert!(ask > 0.0, "no sell order at station {station_id} for type {type_id}");
    rpc_ok(harness, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(station_id), PyRep::Integer(i64::from(type_id)), PyRep::Real(round_four_sig_figs(ask * 1.05)),
        PyRep::Integer(qty), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await;
    let after = hangar_item_ids_for_type(backend.db(), char_id, station_id, type_id);
    let newly_added: Vec<i64> = after.iter().copied().filter(|id| !before.contains(id)).collect();
    if newly_added.is_empty() { after } else { newly_added }
}

fn find_ship_slot_item(db: &WorldRepository, ship_id: i64, flag: InventoryFlag, type_id: TypeId) -> i64 {
    db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == flag && i.type_id == type_id && i.quantity > 0).map(|i| i.item_id).min()
    }).unwrap_or_else(|| panic!("expected item type {type_id} in ship {ship_id} flag {flag:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_rng_reproduces_identical_picks_across_shuffled_maps() {
        let (mut forward, mut reverse) = (HashMap::new(), HashMap::new());
        let data = [(10_u32, "a"), (20, "b"), (30, "c"), (40, "d"), (50, "e")];
        for &(k, v) in &data { forward.insert(k, v); }
        for &(k, v) in data.iter().rev() { reverse.insert(k, v); }

        let (mut rng1, mut rng2, mut rng3) = (DeterministicRng::default(), DeterministicRng::default(), DeterministicRng::new(DEFAULT_TEST_SEED));
        let picks1: Vec<u32> = (0..16).map(|_| rng1.pick_sorted_map_entry(&forward, |&k, _| k >= 20).unwrap().0).collect();
        let picks2: Vec<u32> = (0..16).map(|_| rng2.pick_sorted_map_entry(&reverse, |&k, _| k >= 20).unwrap().0).collect();
        let picks3: Vec<u32> = (0..16).map(|_| rng3.pick_sorted_map_entry(&forward, |&k, _| k >= 20).unwrap().0).collect();
        assert_eq!(picks1, picks2);
        assert_eq!(picks2, picks3);
        assert!(picks1.iter().all(|&k| k >= 20));
    }
}
