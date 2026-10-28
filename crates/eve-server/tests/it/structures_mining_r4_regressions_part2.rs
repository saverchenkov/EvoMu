//! Round-4 Track 2 Regression Tests Part 2 (#170 through #175):
//! Upwell Structure Control, ESS Reserve Unlock, Incursion Wave Respawn, FW Rearguard LP, Mutaplasmids & POCO Tax.

#![forbid(unsafe_code)]

use crate::common::login_pilot;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_r4_170_structure_release_control_restores_docked_ship() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_170_001;
    let struct_id = 950_170_001i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = struct_id;
            sh.flag = InventoryFlag::Hangar;
        }
        let mut roles = HashMap::new();
        roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Gunner 170 Corp".into(),
                ticker: "G170".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                member_roles: roles,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "170 Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "TakeControl",
        &[PyRep::Integer(struct_id)],
        &kw,
        None,
    )
    .await
    .expect("TakeControl");
    assert_eq!(sess.ship_id, Some(struct_id));

    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "ReleaseControl",
        &[PyRep::Integer(struct_id)],
        &kw,
        None,
    )
    .await
    .expect("ReleaseControl");

    assert_eq!(sess.ship_id, Some(ship_id));
    assert_eq!(ctx.db.get_character(char_id).unwrap().active_ship_id, ship_id);
}

#[tokio::test]
async fn test_r4_171_ess_concurrent_link_rejection_and_reserve_unlock_window() {
    let ctx = ServerContext::new();
    let mut sess1 = SessionState::new(17101);
    let mut sess2 = SessionState::new(17102);
    login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let s1_id = 100_000_001;
    let s2_id = 100_000_002;
    let sys_id = 30_004_759u32; // Nullsec
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();

    for (sess, cid, sid) in [(&mut sess1, c1, s1_id), (&mut sess2, c2, s2_id)] {
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(sid);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = sid;
            }
        });
    }

    let ess_ball_id = 950_171_000i64;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball(eve_destiny::Ball {
            id: ess_ball_id,
            type_id: 32882, // ESS
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    for sid in [s1_id, s2_id] {
        let _ = handle
            .spawn_ball(eve_destiny::Ball {
                id: sid,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.ess_main_bank = IskCents::from_isk(10_000_000);
        st.ess_reserve_bank = IskCents::from_isk(30_000_000);
        // Give c1 a 15-min SW ESS Reserve Security Key (57720) in cargo for Delve (30_004_759)
        snap.items.insert(
            770_171_001,
            InventoryItemRecord {
                item_id: 770_171_001,
                type_id: 57720,
                owner_id: c1,
                location_id: s1_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // c1 links to Main Bank
    ctx.dispatch_rpc(
        &mut sess1,
        "essMgr",
        None,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("c1 links to Main Bank");

    // c2 attempting to link to Main Bank while c1 is linked is rejected with EssAlreadyLinked
    let c2_main_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(c2_main_err.code, "EssAlreadyLinked");

    // c1 links to Reserve Bank (consumes key 57714 and sets ess_reserve_unlocked_until)
    ctx.dispatch_rpc(
        &mut sess1,
        "essMgr",
        None,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
        &kw,
        None,
    )
    .await
    .expect("c1 links to Reserve Bank");
    let unlocked = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.get(&sys_id).copied().unwrap_or(0)
    });
    assert!(unlocked > ctx.now_unix(), "Reserve Bank unlock window must be populated");
}

#[tokio::test]
async fn test_r4_172_incursion_wave_respawn_attaches_combat_state_and_site_claim() {
    let ctx = Arc::new(ServerContext::new());
    let sys_id = 30_000_142u32;
    let site_id = "inc-r4-172-vg";
    let initial_boss_bid = 910_172_000i64;
    let boss_tid = 19720u32; // Sansha NPC type

    let now_u = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg_db(&ctx.db, now_u, |reg| {
        reg.incursions.insert(
            20_000_020,
            eve_server::services::pve_incursion_sites::IncursionTaleRecord {
                tale_id: 20_000_020,
                constellation_id: 20_000_020,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![sys_id],
                hq_solar_system_id: sys_id,
                influence: 0.5,
                state: "Established".into(),
                lockout_until_filetime: None,
                mothership_spawned: false,
                mothership_defeated: false,
                tale_stop_published: false,
                sites: vec![eve_server::services::pve_incursion_sites::IncursionSiteEntry {
                    site_id: site_id.into(),
                    dungeon_id: 172,
                    dungeon_name: "Sansha Vanguard".into(),
                    tier: "Vanguard".into(),
                    solar_system_id: sys_id,
                    position: glam::DVec3::new(100_000.0, 0.0, 0.0),
                    min_pilots: 1,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_000,
                    influence_delta: 0.1,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: initial_boss_bid,
                    boss_type_id: boss_tid,
                    boss_destroyed: true,
                    killer_char_id: None,
                    killer_fleet_id: None,
                    escort_spawns: vec![],
                }],
            },
        );
    });

    let res = eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        None,
        None,
        sys_id,
        site_id,
        None,
    )
    .await
    .expect("complete incursion wave");
    let next_bid = initial_boss_bid + 1_000;
    assert_eq!(res.tier, "Vanguard");
    assert!(
        !ctx.systems.claim_special_site_ball(sys_id, next_bid),
        "next_bid must already be claimed as a special-site ball"
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let cs = handle
        .get_combat_state(next_bid)
        .await
        .unwrap()
        .expect("next wave boss must have CombatState");
    assert!(cs.max_structure_hp > 0.0, "Respawned wave boss must have non-zero HP");
}

#[tokio::test]
async fn test_r4_173_join_faction_as_character_enforces_cooldown_standing_corp_and_rearguard_lp() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(17301);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let caldari = 500_001i64;
    let minmatar = 500_002i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    // 1. Negative standing blocks JoinFactionAsCharacter
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(minmatar, -0.5);
    });
    let err_st = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(minmatar)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_st.code, "FwStandingTooLow");

    // 2. Enlist via fwCharacterEnlistmentMgr, retire (starts 24h cooldown), and verify JoinFactionAsCharacter is blocked by cooldown
    ctx.dispatch_rpc(&mut sess, "fwCharacterEnlistmentMgr", None, "CreateMyDirectEnlistment", &[PyRep::Integer(caldari)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "fwCharacterEnlistmentMgr", None, "RemoveMyDirectEnlistment", &[], &kw, None)
        .await
        .unwrap();
    let err_cd = ctx
        .dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(caldari)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_cd.code, "FwEnlistmentCooldown");

    // Advance past 24h cooldown and enlist in Minmatar (after fixing standing)
    ctx.advance_virtual_clock(86_405);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(minmatar, 1.0);
    });
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(minmatar)], &kw, None)
        .await
        .expect("JoinFactionAsCharacter after cooldown");

    // 3. In a low-sec Rearguard FW system, completing CaptureComplex pays 0.75x * 15,000 = 11,250 LP (not 1.5x = 22,500 LP)
    let rearguard_sys = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .keys()
            .copied()
            .find(|&sid| {
                let sec = ctx.sde.solar_systems.get(&sid).map(|s| s.security_status).unwrap_or(1.0);
                sec > 0.0
                    && sec < 0.45
                    && matches!(snap.fw_system_occupations[&sid].occupier_faction_id, 500_001 | 500_003)
                    && eve_server::services::fw_services::compute_system_frontline_status(&ctx, snap, sid)
                        == eve_server::services::fw_services::STATUS_REARGUARD
            })
            .expect("must have at least one low-sec Rearguard FW system")
    });
    let ship_id = 100_000_001i64;
    sess.station_id = None;
    sess.solar_system_id = Some(rearguard_sys);
    sess.ship_id = Some(ship_id);
    let handle = ctx.systems.get_or_spawn(rearguard_sys);
    let _ = handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = handle.step_manual(1.0).await;
    let militia_corp = ctx
        .sde
        .factions
        .get(&(minmatar as u32))
        .and_then(|f| f.militia_corporation_id)
        .map(|c| c as i64)
        .unwrap_or(minmatar);
    let (lp_before, vp_before) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.solar_system_id = rearguard_sys;
        }
        let lp = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&militia_corp).copied())
            .unwrap_or(0);
        let vp = snap
            .fw_system_occupations
            .get(&rearguard_sys)
            .map(|o| o.victory_points)
            .unwrap_or(0);
        let st = snap.system_states.entry(rearguard_sys).or_default();
        st.solar_system_id = rearguard_sys;
        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.fw_capture_duration_seconds = 600;
        st.fw_capture_seconds_elapsed = 599;
        st.fw_contested_pct = 10.0;
        st.last_fw_capture_tick = 0;
        (lp, vp)
    });
    assert!(
        eve_server::services::insurgency_services::step_system_insurgency_capture_tick(
            &ctx,
            rearguard_sys,
            ship_id,
            minmatar,
        )
        .await,
        "1 Hz server capture tick must advance Rearguard FW complex"
    );
    ctx.db.with_snapshot(|snap| {
        let lp_after = snap.characters[&char_id].loyalty_points.get(&militia_corp).copied().unwrap_or(0);
        assert_eq!(lp_after - lp_before, 11_250);
        assert!(snap.system_states[&rearguard_sys].fw_complex_position.is_none());
        let occ = &snap.fw_system_occupations[&rearguard_sys];
        assert_eq!(occ.victory_points - vp_before, 20);
        let expected_pct = ((occ.victory_points as f64 / (occ.victory_points_threshold.max(1) as f64)) * 100.0).clamp(0.0, 100.0);
        assert!((snap.system_states[&rearguard_sys].fw_contested_pct - expected_pct).abs() < 1e-6);
    });
}

#[tokio::test]
async fn test_r4_174_cannot_mutate_fitted_module() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17401);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    let fitted_ab_id = 770_174_001i64;
    let muta_id = 770_174_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fitted_ab_id,
            InventoryItemRecord {
                item_id: fitted_ab_id,
                type_id: 438, // 1MN Afterburner II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            muta_id,
            InventoryItemRecord {
                item_id: muta_id,
                type_id: 47746, // Decayed 1MN Afterburner Mutaplasmid
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47746), PyRep::Integer(fitted_ab_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotMutateFittedItem");
}

#[tokio::test]
async fn test_r4_175_poco_tax_syncs_corp_wallet_division_zero() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(17501);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_175_001i64;
    let planet_id = 40_009_082i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    sess.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "POCO 175 Corp".into(),
                ticker: "P175".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                master_wallet: IskCents(0),
                wallet_divisions: vec![],
                ..Default::default()
            },
        );
        // Seed command center on planet 40009082
        snap.items.insert(
            770_175_001,
            InventoryItemRecord {
                item_id: 770_175_001,
                type_id: 2534,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Configure player POCO owned by corp_id with 10% tax rate
        let settings = eve_server::services::industry_pi::pi_helpers::OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2233,
            owner_corp_id: corp_id,
            corporation_tax_rate: 0.10,
            alliance_tax_rate: 0.10,
            standing_high_tax_rate: 0.10,
            standing_good_tax_rate: 0.10,
            standing_neutral_tax_rate: 0.10,
            standing_bad_tax_rate: 0.10,
            standing_horrible_tax_rate: 0.10,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        };
        eve_server::services::industry_pi::pi_helpers::save_orbital_settings(snap, planet_id, &settings);
    });

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2534),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Place 100x Water (3645) in POCO and import to colony
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_175_002,
            InventoryItemRecord {
                item_id: 770_175_002,
                type_id: 3645,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(3645),
            PyRep::Integer(100),
            PyRep::String("Import".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("UserLaunchCommodities Import");

    let (master_bal, div0_bal) = ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        (
            corp.master_wallet.0,
            corp.wallet_divisions.first().map(|d| d.balance.0).unwrap_or(-1),
        )
    });
    assert!(master_bal > 0, "Corp master_wallet must be credited with POCO tax");
    assert_eq!(div0_bal, master_bal, "Corp wallet_divisions[0].balance must match master_wallet");
}
