//! Extra branch-coverage and mutation-killing unit tests for `fw_gameplay_rpc.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::fw_gameplay_rpc::*;
use crate::services::fw_gameplay::militia_lp_corp_id;
use eve_db::snapshot_ext::{
    FwSystemOccupationRecord, PersistedFwStatEvent, SocialContactRecord,
};
use eve_db::{
    AllianceRecord, CharacterRecord, CorporationRecord, IskCents, StandingTransactionRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let sde = Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())));
    ServerContext::with_sde_and_repository(
        sde,
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

async fn call_facwar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "facWarMgr", method, args, &kw)
        .await
        .expect("facWarMgr method handled")
}

fn make_char(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    fac_id: Option<i64>,
    sys_id: u32,
) -> CharacterRecord {
    let mut c = snap.characters.values().next().unwrap().clone();
    c.character_id = char_id;
    c.name = format!("FwPilot_{char_id}");
    c.corporation_id = corp_id;
    c.alliance_id = alliance_id;
    c.war_faction_id = fac_id;
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.active_ship_id = 0;
    c.balance = IskCents(50_000_000_00);
    c.loyalty_points.clear();
    c.standings.clear();
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supply_sites_ihub_regen_battlefield_wave_and_space_entity_destroyed() {
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let cal_cid = 930_001_i64;
    let gal_cid = 930_002_i64;
    let no_fw_cid = 930_003_i64;
    let depot_id = 710_001_i64;
    let cache_id = 710_002_i64;
    let complex_gid = 55_001_i64;
    let guard_ball_id = 500_000_000 + complex_gid;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 2_900,
                victory_points_threshold: 3_000,
            },
        );
        let mut c1 = make_char(snap, cal_cid, 98_000_001, None, Some(500_001), sys_id);
        c1.active_ship_id = 880_001;
        let c2 = make_char(snap, gal_cid, 98_000_002, None, Some(500_004), sys_id);
        let c3 = make_char(snap, no_fw_cid, 98_000_003, None, None, sys_id);
        snap.characters.insert(cal_cid, c1);
        snap.characters.insert(gal_cid, c2);
        snap.characters.insert(no_fw_cid, c3);

        snap.fw_runtime
            .supply_sites
            .insert(depot_id, (sys_id, "SupplyDepot".into(), 500_001));
        snap.fw_runtime
            .supply_sites
            .insert(cache_id, (sys_id, "SupplyCache".into(), 500_004));
        snap.structures.insert(
            depot_id,
            UpwellStructureRecord {
                structure_id: depot_id,
                solar_system_id: sys_id,
                type_id: 17_621,
                owner_corp_id: 500_001,
                name: "Caldari Supply Depot".into(),
                state: "online".into(),
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            cache_id,
            UpwellStructureRecord {
                structure_id: cache_id,
                solar_system_id: sys_id,
                type_id: 17_621,
                owner_corp_id: 500_004,
                name: "Gallente Supply Cache".into(),
                state: "online".into(),
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                ..Default::default()
            },
        );
        snap.fw_runtime.fw_complexes.insert(
            complex_gid,
            (sys_id, "Novice".into(), 500_001, 0.0, false),
        );
    });

    assert_eq!(
        resolve_friendly_supply_site_defense(&ctx, cal_cid, 999_999)
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    assert_eq!(
        resolve_friendly_supply_site_defense(&ctx, no_fw_cid, depot_id)
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    assert_eq!(
        resolve_friendly_supply_site_defense(&ctx, gal_cid, depot_id)
            .unwrap_err()
            .code,
        "CannotDefendEnemyStructure"
    );

    // Occupying militia defends SupplyDepot -> +5.0 friendly advantage, repairs to 3000 HP
    let dep_res = resolve_friendly_supply_site_defense(&ctx, cal_cid, depot_id).unwrap();
    assert_eq!(dep_res.dict_get("defended"), Some(&PyRep::Bool(true)));
    assert_eq!(dep_res.dict_get("friendlyAdvantage"), Some(&PyRep::Real(5.0)));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&depot_id].hull_hp, 3_000.0);
    });

    // Attacking militia defends its SupplyCache -> +2.0 attacker friendly advantage, repairs to 1500 HP
    let cache_res = resolve_friendly_supply_site_defense(&ctx, gal_cid, cache_id).unwrap();
    assert_eq!(cache_res.dict_get("friendlyAdvantage"), Some(&PyRep::Real(2.0)));
    assert_eq!(cache_res.dict_get("enemyAdvantage"), Some(&PyRep::Real(5.0)));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&cache_id].hull_hp, 1_500.0);
    });

    // Regenerate unattacked FW I-Hub lowers VP to 80% (2400) and contested_pct to 80.0
    assert_eq!(regenerate_unattacked_fw_ihub(&ctx, sys_id), 80.0);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.fw_system_occupations[&sys_id].victory_points, 2_400);
        assert_eq!(s.fw_runtime.ihub_vulnerable.get(&sys_id), Some(&false));
    });

    // on_space_entity_destroyed marks complex guards cleared and destroys supply site when killer is ship_id
    on_space_entity_destroyed(&ctx, sys_id, None, guard_ball_id, 587).await;
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.fw_runtime.complex_guards_cleared.get(&complex_gid), Some(&true));
    });
    on_space_entity_destroyed(&ctx, sys_id, Some(880_001), cache_id, 17_621).await;

    // spawn_battlefield_wave_npc spawns gate + NPC with fallback HP when type has 0 structure HP
    spawn_battlefield_wave_npc(
        &ctx,
        sys_id,
        750_001,
        750_002,
        999_999,
        500_001,
        DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;
    let has_npc = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.contains_key(&750_002)
    });
    assert!(has_npc);
}

#[test]
fn donate_lp_all_tiers_and_neighbor_jump_check() {
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let neighbor_sys = ctx
        .sde
        .jumps
        .values()
        .find_map(|j| {
            if j.from_solar_system_id == sys_id {
                Some(j.to_solar_system_id)
            } else if j.to_solar_system_id == sys_id {
                Some(j.from_solar_system_id)
            } else {
                None
            }
        })
        .unwrap_or(sys_id);
    let far_sys = 30000142_u32;
    let cid = 930_010_i64;

    // NotInWarzone when system has no occupation record
    assert_eq!(
        donate_lp_to_fw_ihub(&ctx, cid, 30000142, 100).unwrap_err().code,
        "NotInWarzone"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        let mut ch = make_char(snap, cid, 98_000_001, None, Some(500_001), far_sys);
        // Seed LP under faction_id fallback key (500_001) below effective_lp (200_000 cap) first
        ch.loyalty_points.insert(500_001, 150_000);
        snap.characters.insert(cid, ch);
    });

    // Far system fails NotInOrNearSystem
    assert_eq!(
        donate_lp_to_fw_ihub(&ctx, cid, sys_id, 100).unwrap_err().code,
        "NotInOrNearSystem"
    );

    // Move character to 1-jump neighbor system -> allowed!
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().solar_system_id = neighbor_sys;
    });
    assert_eq!(
        donate_lp_to_fw_ihub(&ctx, cid, sys_id, 300_000).unwrap_err().code,
        "InsufficientLoyaltyPoints"
    );

    // Top up LP so the tier walk has enough for all 200_000 LP
    ctx.db.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&cid)
            .unwrap()
            .loyalty_points
            .insert(500_001, 250_000);
    });

    // Walk through all tier thresholds: 0 -> 1 -> 2 -> 3 -> 4 -> 5
    let (tot0, t0, r0, _) = donate_lp_to_fw_ihub(&ctx, cid, sys_id, 39_999).unwrap();
    assert_eq!((tot0, t0, r0), (39_999, 0, 0.0));
    let (tot1, t1, r1, _) = donate_lp_to_fw_ihub(&ctx, cid, sys_id, 1).unwrap();
    assert_eq!((tot1, t1, r1), (40_000, 1, 10.0));
    let (tot2, t2, r2, _) = donate_lp_to_fw_ihub(&ctx, cid, sys_id, 20_000).unwrap();
    assert_eq!((tot2, t2, r2), (60_000, 2, 20.0));
    let (tot3, t3, r3, _) = donate_lp_to_fw_ihub(&ctx, cid, sys_id, 30_000).unwrap();
    assert_eq!((tot3, t3, r3), (90_000, 3, 30.0));
    let (tot4, t4, r4, _) = donate_lp_to_fw_ihub(&ctx, cid, sys_id, 50_000).unwrap();
    assert_eq!((tot4, t4, r4), (140_000, 4, 40.0));
    let (tot5, t5, r5, _) = donate_lp_to_fw_ihub(&ctx, cid, sys_id, 60_000).unwrap();
    assert_eq!((tot5, t5, r5), (200_000, 5, 50.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corp_and_alliance_standing_sources_and_error_guards() {
    let ctx = test_ctx();
    let ceo_id = 930_020_i64;
    let ceo2_id = 930_021_i64;
    let corp_id = 98_000_020_i64;
    let sub_corp = 98_000_021_i64;
    let ally_id = 99_000_020_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            ceo_id,
            make_char(snap, ceo_id, corp_id, None, None, 30002505),
        );
        snap.characters.insert(
            ceo2_id,
            make_char(snap, ceo2_id, sub_corp, None, None, 30002505),
        );
        let mut c1 = CorporationRecord::default();
        c1.corporation_id = corp_id;
        c1.ceo_id = ceo_id;
        c1.alliance_id = None;
        c1.members.insert(ceo_id);
        snap.corporations.insert(corp_id, c1);

        let mut c2 = CorporationRecord::default();
        c2.corporation_id = sub_corp;
        c2.ceo_id = ceo2_id;
        c2.alliance_id = None;
        c2.members.insert(ceo2_id);
        snap.corporations.insert(sub_corp, c2);
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(ally_id);

    // Missing faction arg -> InvalidArgs
    assert_eq!(
        call_facwar(&ctx, &mut sess, "JoinFactionAsCorporation", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgs"
    );

    // Negative standing via corp_contacts -> InsufficientStanding
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.insert(
            corp_id,
            vec![SocialContactRecord {
                contact_id: 500_001,
                contact_type: "faction".into(),
                standing: -1.5,
                is_watched: false,
                is_blocked: false,
                label_ids: vec![],
            }],
        );
    });
    assert_eq!(
        call_facwar(&ctx, &mut sess, "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "InsufficientStanding"
    );

    // Clear corp_contacts, test negative standing via standing_transactions
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.remove(&corp_id);
        let dummy_entity = make_char(snap, corp_id, corp_id, None, None, 30002505);
        snap.characters.insert(corp_id, dummy_entity);
        snap.standing_transactions.insert(
            corp_id,
            vec![StandingTransactionRecord {
                event_id: 1,
                from_id: 500_001,
                to_id: corp_id,
                modification: -0.5,
                event_type_id: 1,
            }],
        );
    });
    assert_eq!(
        call_facwar(&ctx, &mut sess, "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "InsufficientStanding"
    );

    // Clear standing_transactions, test negative standing via member character average
    ctx.db.with_mut_snapshot(|snap| {
        snap.standing_transactions.remove(&corp_id);
        snap.characters.remove(&corp_id);
        snap.characters
            .get_mut(&ceo_id)
            .unwrap()
            .standings
            .insert(500_001, -2.0);
    });
    assert_eq!(
        call_facwar(&ctx, &mut sess, "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "InsufficientStanding"
    );

    // LeaveFactionAsCorporation when not enlisted -> CorporationNotEnlisted
    assert_eq!(
        call_facwar(&ctx, &mut sess, "LeaveFactionAsCorporation", &[])
            .await
            .unwrap_err()
            .code,
        "CorporationNotEnlisted"
    );

    // Alliance error branches: AllianceNotFound when alliance record not yet inserted
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&ceo_id)
            .unwrap()
            .standings
            .insert(500_001, 2.0);
    });
    assert_eq!(
        call_facwar(&ctx, &mut sess, "JoinFactionAsAlliance", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "AllianceNotFound"
    );
    assert_eq!(
        call_facwar(&ctx, &mut sess, "LeaveFactionAsAlliance", &[])
            .await
            .unwrap_err()
            .code,
        "AllianceNotFound"
    );

    // Insert alliance with negative sub_corp standing -> InsufficientStanding on JoinFactionAsAlliance
    ctx.db.with_mut_snapshot(|snap| {
        let mut m = HashSet::new();
        m.insert(corp_id);
        m.insert(sub_corp);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test Ally".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: m,
                ..Default::default()
            },
        );
        snap.corporations.get_mut(&corp_id).unwrap().alliance_id = Some(ally_id);
        snap.corporations.get_mut(&sub_corp).unwrap().alliance_id = Some(ally_id);
        snap.characters.get_mut(&ceo_id).unwrap().alliance_id = Some(ally_id);
        snap.corp_contacts.insert(
            sub_corp,
            vec![SocialContactRecord {
                contact_id: 500_001,
                contact_type: "faction".into(),
                standing: -0.2,
                is_watched: false,
                is_blocked: false,
                label_ids: vec![],
            }],
        );
    });
    assert_eq!(
        call_facwar(&ctx, &mut sess, "JoinFactionAsAlliance", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "InsufficientStanding"
    );
    assert_eq!(
        call_facwar(&ctx, &mut sess, "LeaveFactionAsAlliance", &[])
            .await
            .unwrap_err()
            .code,
        "AllianceNotEnlisted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stat_events_leaderboards_and_remaining_rpc_methods() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let cid = 930_030_i64;
    let corp_id = 98_000_030_i64;
    let ally_id = 99_000_030_i64;
    let sys_id = 30002505_u32;
    let lp_corp = militia_lp_corp_id(&ctx.sde, 500_001);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = make_char(snap, cid, corp_id, Some(ally_id), Some(500_001), sys_id);
        ch.loyalty_points.insert(lp_corp, 100_000);
        snap.characters.insert(cid, ch);
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 100,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_runtime.historical_char_totals.insert(cid, (10, 1000));
        snap.fw_runtime.historical_corp_totals.insert(corp_id, (20, 2000));
        snap.fw_runtime.historical_fac_totals.insert(500_001, (30, 3000));
        snap.fw_runtime.historical_ally_totals.insert(ally_id, (40, 4000));
        snap.fw_runtime.stat_events.push(PersistedFwStatEvent {
            character_id: cid,
            corporation_id: corp_id,
            alliance_id: Some(ally_id),
            faction_id: 500_001,
            kills: 3,
            victory_points: 150,
            timestamp_unix: now.saturating_sub(3600), // < 24h (yesterday + last_week + total)
        });
        snap.fw_runtime.stat_events.push(PersistedFwStatEvent {
            character_id: cid,
            corporation_id: corp_id,
            alliance_id: Some(ally_id),
            faction_id: 500_001,
            kills: 5,
            victory_points: 250,
            timestamp_unix: now.saturating_sub(2 * 86_400), // < 7d (last_week + total)
        });
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(ally_id);
    sess.solar_system_id = Some(sys_id);
    sess.war_faction_id = Some(500_001);

    let ch_stats = call_facwar(&ctx, &mut sess, "GetStats_Character", &[])
        .await
        .unwrap();
    assert_eq!(ch_stats.dict_get("killsY"), Some(&PyRep::Integer(3)));
    assert_eq!(ch_stats.dict_get("killsLW"), Some(&PyRep::Integer(8)));
    assert_eq!(ch_stats.dict_get("killsTotal"), Some(&PyRep::Integer(18)));
    assert_eq!(ch_stats.dict_get("vpY"), Some(&PyRep::Integer(150)));
    assert_eq!(ch_stats.dict_get("vpLW"), Some(&PyRep::Integer(400)));
    assert_eq!(ch_stats.dict_get("vpTotal"), Some(&PyRep::Integer(1400)));

    // DonateLPsToSolarSystem RPC
    let don = call_facwar(
        &ctx,
        &mut sess,
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(sys_id as i64), PyRep::Integer(45_000)],
    )
    .await
    .unwrap();
    assert_eq!(don.dict_get("systemTier"), Some(&PyRep::Integer(1)));
    assert_eq!(
        call_facwar(&ctx, &mut sess, "GetSolarSystemLPs", &[PyRep::Integer(sys_id as i64)])
            .await
            .unwrap(),
        PyRep::Integer(45_000)
    );

    // Rank overview, bunker ID, corp status, faction corps, alliance faction ID, withdraw stubs
    assert!(matches!(
        call_facwar(&ctx, &mut sess, "GetMyCharacterRankOverview", &[]).await.unwrap(),
        PyRep::Rowset { .. }
    ));
    assert!(matches!(
        call_facwar(&ctx, &mut sess, "GetControlBunkerIDInSystem", &[]).await.unwrap(),
        PyRep::Integer(_)
    ));
    assert!(call_facwar(&ctx, &mut sess, "GetCorpFactionalWarStatus", &[]).await.is_ok());
    assert!(call_facwar(&ctx, &mut sess, "GetFactionCorporations", &[PyRep::Integer(500_001)]).await.is_ok());
    assert!(call_facwar(&ctx, &mut sess, "GetAllianceWarFactionID", &[]).await.is_ok());
    assert!(call_facwar(&ctx, &mut sess, "GetCorporationWarFactionID", &[]).await.is_ok());
    for m in [
        "WithdrawJoinFactionAsAlliance",
        "WithdrawLeaveFactionAsAlliance",
        "WithdrawLeaveFactionAsCorporation",
    ] {
        // Without a pending request, withdrawing returns false (or AccessDenied if not an officer).
        let res = call_facwar(&ctx, &mut sess, m, &[]).await;
        assert!(matches!(res, Ok(PyRep::Bool(false)) | Err(_)), "unexpected {m}: {res:?}");
    }
}
