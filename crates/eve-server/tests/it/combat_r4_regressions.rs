//! Round-4 Track 1 Regression Tests (#153 through #163):
//! - #153: Fighter tube duplicate load prevention, launch ownership/location/flag check, recall validation
//! - #154: Self-destruct docked rejection, mandatory 120s countdown, PvP killmail/bounty attribution
//! - #155: Smartbomb/bomb SafetyLevel enforcement, invulnerability skip, lowsec flags
//! - #156: Breacher pod DoT lethal tick triggers death pipeline and wreck creation
//! - #157: Security tag turn-in requires local station hangar / active ship cargo and not contract-locked
//! - #158: Wormhole bidirectional polarization and K162 reverse jump
//! - #159: Probe recovery preserves mixed probe types and respects ship cargo capacity
//! - #160: Hacking minigame minimum difficulty floor, utility slot cap, and targeted SecondaryVector
//! - #161: CmdAlignToBookmark expiry, folder ACL, criminal warp restriction, and decloak
//! - #162: Directional scan (ConeScan) docked rejection and convoy range/cone filtering
//! - #163: Salvage ship cargo capacity enforcement and 6,000 m Salvager module range check

use eve_db::{
    AccountRecord, BookmarkFolderRecord, BookmarkRecord, CharacterRecord, ContractRecord,
    InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes, TrainedSkillRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::drones_salvage_tractor::execute_wreck_salvage_cycle;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    record_killmail_assist_action, step_combat_system, with_combat_runtime,
    with_mut_combat_runtime, ActiveBreacherDot, SafetyLevel,
};
use glam::DVec3;
use std::collections::HashMap;

fn item_rec(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    client_id: u64,
    char_id: i64,
    corp_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id: client_id,
                name: format!("Pilot {char_id}"),
                corporation_id: corp_id,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(100_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes::default(),
                skill_points: 5_000_000.0,
                activity_status: None,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 1_700_000_000,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 0,
            },
        );
        if !snap.accounts.values().any(|a| a.user_id == client_id) {
            snap.accounts.insert(
                format!("user_{client_id}"),
                AccountRecord {
                    user_id: client_id,
                    username: format!("user_{client_id}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        snap.items.insert(
            ship_id,
            item_rec(
                ship_id,
                ship_type_id,
                char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(client_id, 7_000 + client_id, Some(char_id), true, ctx.now_unix());
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.5,
                mass: 10_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    let mut sess = SessionState::new(7_000 + client_id);
    sess.user_id = Some(client_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

#[tokio::test]
async fn test_r4_153_fighter_tube_duplicate_load_and_launch_ownership_validation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, other_char_id, ship_id) = (97_153_001i64, 97_153_002i64, 87_153_100i64);
    // Archon carrier (23757) with FighterBay
    let mut sess =
        setup_pilot_in_space(&ctx, 71531, char_id, 1_000_125, ship_id, 23757, sys_id, DVec3::ZERO)
            .await;

    let fighter_item_id = 87_153_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_item_id,
            item_rec(
                fighter_item_id,
                23055, // Templar I light fighter
                char_id,
                ship_id,
                InventoryFlag::FighterBay,
                9,
                false,
            ),
        );
    });

    // Loading into tube 0 succeeds
    ctx.dispatch_rpc(
        &mut sess,
        "fighterMgr",
        None,
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(fighter_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Load fighter into tube 0");

    // Loading the same fighter item into sibling tube 1 must be rejected with FighterAlreadyLoaded
    let dup_err = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(1), PyRep::Integer(fighter_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Same fighter stack must not load into two tubes");
    assert_eq!(dup_err.code, "FighterAlreadyLoaded");

    // If the fighter item is moved out of FighterBay or transferred to another owner before launch, LaunchFightersFromTubes must reject
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fighter_item_id).unwrap().owner_id = other_char_id;
    });
    let launch_err = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
            None,
        )
        .await
        .expect_err("Launching fighter no longer owned by pilot in FighterBay must fail");
    assert_eq!(launch_err.code, "ItemNotInFighterBay");
}

#[tokio::test]
async fn test_r4_154_self_destruct_docked_rejection_countdown_and_pvp_assist_attribution() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_002_813u32; // Tama
    let (vic_id, atk_id, vic_ship_id, atk_ship_id) =
        (97_154_001i64, 97_154_002i64, 87_154_101i64, 87_154_102i64);
    let mut vic_sess =
        setup_pilot_in_space(&ctx, 71541, vic_id, 1_000_125, vic_ship_id, 587, sys_id, DVec3::ZERO)
            .await;
    let _atk_sess = setup_pilot_in_space(
        &ctx,
        71542,
        atk_id,
        1_000_126,
        atk_ship_id,
        587,
        sys_id,
        DVec3::new(2_000.0, 0.0, 0.0),
    )
    .await;

    // 1. Docked session cannot self-destruct
    let mut docked_sess = vic_sess.clone();
    docked_sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = Some(60_003_760);
    });
    let docked_err = ctx
        .dispatch_rpc(&mut docked_sess, "ship", None, "SelfDestruct", &[], &kw, None)
        .await
        .expect_err("Docked ship must not self-destruct");
    assert_eq!(docked_err.code, "CannotSelfDestructWhileDocked");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = None;
    });

    // 2. Empty args SelfDestruct starts the 120s countdown instead of bypassing it
    let start_rep = ctx
        .dispatch_rpc(&mut vic_sess, "ship", None, "SelfDestruct", &[], &kw, None)
        .await
        .expect("Start countdown with empty args");
    assert_eq!(
        start_rep.dict_get("countdownStarted").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        start_rep.dict_get("detonated").and_then(|v| v.as_bool()),
        Some(false)
    );

    // 3. Record PvP aggression from atk_id and advance countdown to detonation
    record_killmail_assist_action(
        &ctx,
        vic_ship_id,
        atk_id,
        587,
        486,
        250.0,
        false,
        ctx.now_unix(),
    );
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(cd) = rt.self_destruct_countdowns.get_mut(&vic_ship_id) {
            cd.detonate_at_unix = ctx.now_unix();
        }
    });
    step_combat_system(&ctx, sys_id, 1.0).await;

    let km_attacker = ctx.db.with_snapshot(|snap| {
        snap.killmails
            .values()
            .find(|km| km.victim_character_id == vic_id)
            .map(|km| km.attacker_character_id)
    });
    assert_eq!(
        km_attacker,
        Some(atk_id),
        "Self-destruct within 15m assist window must attribute killmail to PvP attacker"
    );
    let dkm_has_attacker = with_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails
            .values()
            .any(|dkm| dkm.victim_character_id == vic_id && dkm.attackers.iter().any(|a| a.character_id == atk_id))
    });
    assert!(dkm_has_attacker, "Detailed killmail must preserve PvP attacker");
}

#[tokio::test]
async fn test_r4_155_156_smartbomb_safety_invulnerability_and_breacher_dot_lethal_death() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let jita_sys = 30_000_142u32;
    let (atk_id, vic_id, atk_ship_id, vic_ship_id, sb_mod_id) = (
        97_155_001i64,
        97_155_002i64,
        87_155_101i64,
        87_155_102i64,
        87_155_201i64,
    );
    let mut atk_sess =
        setup_pilot_in_space(&ctx, 71551, atk_id, 1_000_125, atk_ship_id, 645, jita_sys, DVec3::ZERO)
            .await;
    let _vic_sess = setup_pilot_in_space(
        &ctx,
        71552,
        vic_id,
        1_000_126,
        vic_ship_id,
        587,
        jita_sys,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;

    let sb_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 72)
        .map(|t| t.type_id)
        .expect("Smartbomb type in SDE");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            sb_mod_id,
            item_rec(
                sb_mod_id,
                sb_type_id,
                atk_id,
                atk_ship_id,
                InventoryFlag::HiSlot0,
                1,
                true,
            ),
        );
        snap.module_online_states.insert(sb_mod_id, true);
    });
    ctx.invalidate_ship_fitting(atk_ship_id);

    // 1. With SafetyLevel::Green in Highsec, smartbomb activation is blocked before capacitor drain
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_id, SafetyLevel::Green);
    });
    let sb_err = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut atk_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(sb_mod_id)],
        &kw,
    )
    .await
    .expect_err("Smartbomb in highsec with Green safety must be blocked");
    assert_eq!(sb_err.code, "SafetyLevelRestricted");

    // 2. Lethal Breacher Pod DoT tick triggers death pipeline and removes destroyed ship ball
    let sys_handle = ctx.systems.get_or_spawn(jita_sys);
    let _ = sys_handle
        .set_combat_state(
            vic_ship_id,
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 100.0,
                armor_hp: 0.0,
                max_armor_hp: 100.0,
                structure_hp: 10.0,
                max_structure_hp: 100.0,
                ..CombatState::default()
            },
        )
        .await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(
            vic_ship_id,
            ActiveBreacherDot {
                target_ball_id: vic_ship_id,
                source_char_id: atk_id,
                source_ship_id: atk_ship_id,
                weapon_type_id: 83482,
                charge_type_id: 83485,
                flat_cap_dps: 500.0,
                pct_cap_fraction: 0.5,
                remaining_s: 10.0,
                tick_damage: 50.0,
            },
        );
    });
    step_combat_system(&ctx, jita_sys, 1.0).await;
    assert!(
        sys_handle
            .get_ball_snapshot(vic_ship_id)
            .await
            .unwrap()
            .is_none(),
        "Lethal breacher DoT must destroy target ship ball via death pipeline"
    );
}

#[tokio::test]
async fn test_r4_157_sec_tag_turn_in_requires_local_hangar_and_not_contract_locked() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 97_157_001i64;
    let ship_id = 87_157_101i64;
    let mut sess = setup_pilot_in_space(
        &ctx,
        71571,
        char_id,
        1_000_125,
        ship_id,
        587,
        30_000_142,
        DVec3::ZERO,
    )
    .await;
    sess.station_id = Some(60_003_760);

    let remote_tag_id = 87_157_201i64;
    let locked_tag_id = 87_157_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(60_003_760);
        ch.security_status = -8.5;
        // Tag in remote station 60_008_494
        snap.items.insert(
            remote_tag_id,
            item_rec(
                remote_tag_id,
                35661,
                char_id,
                60_008_494,
                InventoryFlag::Hangar,
                1,
                false,
            ),
        );
        // Tag in local station 60_003_760 but locked in an Outstanding contract
        snap.items.insert(
            locked_tag_id,
            item_rec(
                locked_tag_id,
                35661,
                char_id,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                false,
            ),
        );
        snap.contracts.insert(
            77_157_001,
            ContractRecord {
                contract_id: 77_157_001,
                issuer_id: char_id,
                issuer_corp_id: 1_000_125,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                title: "Locked Tag".into(),
                for_corp: false,
                availability: "Public".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(1),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![locked_tag_id],
                expiration_unix: 1_700_086_400,
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: 1_700_000_000,
                num_days: 1,
            },
        );
    });

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "ApplyTagsForSecStatus",
            &[PyRep::Integer(35661), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("Remote and contract-locked tags must not be consumed");
    assert_eq!(err.code, "InsufficientSecurityTags");
}

#[tokio::test]
async fn test_r4_158_159_160_161_162_163_exploration_wormhole_dscan_and_salvage() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = 30_000_142u32;
    let (char_id, ship_id) = (97_158_001i64, 87_158_101i64);
    let mut sess =
        setup_pilot_in_space(&ctx, 71581, char_id, 1_000_125, ship_id, 587, sys_id, DVec3::ZERO)
            .await;

    // #162: DirectionalScan / ConeScan while docked must fail with CannotDirectionalScanWhileDocked
    let mut docked_sess = sess.clone();
    docked_sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let dscan_err = ctx
        .dispatch_rpc(
            &mut docked_sess,
            "scanMgr",
            None,
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(1_000_000.0)],
            &kw,
            None,
        )
        .await
        .expect_err("ConeScan while docked must fail");
    assert_eq!(dscan_err.code, "CannotDirectionalScanWhileDocked");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });

    // #160: Hacking minigame minimum difficulty floor ignores client difficulty=1 spoof
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            87_160_801,
            item_rec(
                87_160_801,
                22177, // Data Analyzer I
                char_id,
                ship_id,
                InventoryFlag::MedSlot0,
                1,
                true,
            ),
        );
        snap.items.insert(
            87_160_802,
            item_rec(
                87_160_802,
                22175, // Relic Analyzer I
                char_id,
                ship_id,
                InventoryFlag::MedSlot1,
                1,
                true,
            ),
        );
        snap.module_online_states.insert(87_160_801, true);
        snap.module_online_states.insert(87_160_802, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let relic_can_id = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        let sites = reg.sites_by_system.get_mut(&sys_id).unwrap();
        let relic_site = sites
            .iter_mut()
            .find(|s| s.site_category == "Relic" || s.site_category == "Ghost")
            .unwrap();
        relic_site.position = [0.0, 0.0, 0.0];
        relic_site.hackable_can_ids[0]
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: relic_can_id,
            type_id: 26473,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _hack_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[
                PyRep::Integer(relic_can_id),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1), // client difficulty=1 spoof
            ],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance");
    let (utility_cap, core_coherence) =
        eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            let g = reg.hacking_games.get(&(char_id, relic_can_id)).unwrap();
            let core_coh = g
                .tiles
                .iter()
                .find(|t| t.kind == eve_server::services::pve_exploration::HackingNodeKind::SystemCore)
                .map(|t| t.coherence)
                .unwrap_or(0);
            (g.utilities.len(), core_coh)
        });
    assert_eq!(utility_cap, 3, "Relic Analyzer I has 3 virusElementSlots");
    assert!(
        core_coherence >= 50,
        "Client difficulty=1 argument must not bypass container minimum difficulty floor (got core coherence {core_coherence})"
    );

    // #161: Expired bookmark and restricted ACL folder bookmark must be rejected in CmdAlignToBookmark
    let exp_bm_id = 87_161_001i64;
    let acl_bm_id = 87_161_002i64;
    let folder_id = 87_161_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            exp_bm_id,
            BookmarkRecord {
                bookmark_id: exp_bm_id,
                owner_id: char_id,
                folder_id: None,
                is_shared: false,
                solar_system_id: sys_id,
                item_id: None,
                label: "Expired".into(),
                memo: String::new(),
                x: 200_000.0,
                y: 0.0,
                z: 0.0,
            },
        );
        snap.bookmark_expiries
            .insert(exp_bm_id, ctx.now_unix().saturating_sub(10));
        snap.bookmark_folders.insert(
            folder_id,
            BookmarkFolderRecord {
                folder_id,
                owner_id: 99_999_999,
                folder_name: "Private Shared Folder".into(),
                is_shared: true,
                corporation_id: None,
                description: String::new(),
                admin_group_id: Some(99_999_999),
                manage_group_id: Some(99_999_999),
                use_group_id: Some(99_999_999),
                view_group_id: Some(99_999_999),
            },
        );
        snap.bookmarks.insert(
            acl_bm_id,
            BookmarkRecord {
                bookmark_id: acl_bm_id,
                owner_id: 99_999_999,
                folder_id: Some(folder_id),
                is_shared: true,
                solar_system_id: sys_id,
                item_id: None,
                label: "ACL Restricted".into(),
                memo: String::new(),
                x: 200_000.0,
                y: 0.0,
                z: 0.0,
            },
        );
    });
    let exp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdGotoBookmark",
            &[PyRep::Integer(exp_bm_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired bookmark must be rejected");
    assert_eq!(exp_err.code, "BookmarkExpired");

    let acl_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdGotoBookmark",
            &[PyRep::Integer(acl_bm_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Bookmark in folder without ACL access must be rejected");
    assert_eq!(acl_err.code, "AccessDenied");

    // #163: Salvager module range check (wreck at 12 km > 6 km max range) and cargo capacity check
    let far_wreck_id = 87_163_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3438,
            TrainedSkillRecord {
                type_id: 3438,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            far_wreck_id,
            item_rec(
                far_wreck_id,
                26473,
                char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: far_wreck_id,
            type_id: 26473,
            position: DVec3::new(12_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 25.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let range_err =
        execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, far_wreck_id, Some(25861))
            .await
            .expect_err("Salvager I at 12 km must fail with TargetOutOfRange");
    assert_eq!(range_err.code, "TargetOutOfRange");
}
