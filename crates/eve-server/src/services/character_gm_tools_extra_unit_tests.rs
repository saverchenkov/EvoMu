//! Deep branch-coverage and mutation-killing unit tests for `services::character_gm_tools`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::character_skills::character_gm_tools::{
    bloodline_name_pools, handle_char_unbound, handle_gm_qa_rpcs, handle_gm_slash_ext,
    is_valid_character_name_syntax,
};
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::service_roles::{ROLE_GML, ROLE_QA};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    effects: &[u16],
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("GmExtraType_{new_tid}");
    rec.radius = 50.0;
    rec.mass = 1_000.0;
    rec.effect_ids = effects.to_vec();
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn put_item(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
}

fn call_unbound(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_char_unbound(ctx, session, "charUnboundMgr", method, args, kwargs)
}

async fn call_slash_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_gm_qa_rpcs(ctx, session, service, method, args).await
}

#[test]
fn name_syntax_boundaries_and_bloodline_pools_kill_all_mutants() {
    assert!(!is_valid_character_name_syntax("Ab"));
    assert!(is_valid_character_name_syntax("Abc"));
    assert!(is_valid_character_name_syntax(&"A".repeat(37)));
    assert!(!is_valid_character_name_syntax(&"A".repeat(38)));

    for bad in [
        " Abc", "Abc ", "-Abc", "Abc-", "'Abc", "Abc'", "Ab--cd", "Ab''cd", "Ab -cd", "Ab- cd",
        "Ab' cd", "One Two Three Four", "ccp alpha", "CCP-Alpha", "ccpAlpha", "gm alpha",
        "isd alpha", "dev alpha", "The AdminOne", "ModeratorX", "Concordia", "BadFuckWord",
        "BadShitWord", "BadHitlerWord", "BadNaziWord",
    ] {
        assert!(!is_valid_character_name_syntax(bad), "expected {bad} to be rejected");
    }
    for ok in ["A-B'C D", "One Two Three", "Devonshire", "Gmallory"] {
        assert!(is_valid_character_name_syntax(ok));
    }

    let expected = [
        (1, "Tibus", "Heth"),
        (2, "Kael", "Mercer"),
        (11, "Kenji", "Takahashi"),
        (3, "Toran", "Kouris"),
        (4, "Karishal", "Shakor"),
        (14, "Temujin", "Khasar"),
        (5, "Jamyl", "Sarum"),
        (6, "Rashid", "Al-Maktoum"),
        (13, "Garkeh", "Khanid"),
        (7, "Jacus", "Roden"),
        (8, "Vindhya", "Isu"),
        (12, "Liang", "Zhang"),
        (999, "Alexander", "Vander"),
    ];
    for (bid, f0, l0) in expected {
        let (f, l) = bloodline_name_pools(bid);
        assert_eq!((f[0], l[0], f.len(), l.len()), (f0, l0, 8, 8));
    }
}

#[test]
fn char_unbound_auth_ship_names_create_10arg_and_select_conflicts() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut unauth = SessionState::default();

    for m in [
        "GetCharactersToSelect",
        "ValidateNameEx",
        "GetValidRandomName",
        "CreateCharacterWithDoll",
        "SelectCharacterID",
    ] {
        assert!(call_unbound(&ctx, &mut unauth, m, &[], &kw).unwrap().is_err());
    }
    assert!(call_unbound(&ctx, &mut unauth, "unknown_method", &[], &kw).is_none());

    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let mut sess = SessionState {
        session_id: 88,
        user_id: Some(1),
        character_id: Some(char_id),
        role: ROLE_QA,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(ship_id, "CustomSnapShip".into());
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.alliance_id = Some(99_000_001);
            c.station_id = None;
            c.structure_id = Some(1_040_000_001);
        }
    });
    let crow = call_unbound(&ctx, &mut sess, "GetCharactersToSelect", &[], &kw).unwrap().unwrap();
    let rows = crow.crowset_rows().unwrap();
    assert_eq!(rows[0].dict_get("allianceID"), Some(&PyRep::Integer(99_000_001)));
    assert_eq!(rows[0].dict_get("stationID"), Some(&PyRep::Integer(1_040_000_001)));
    assert_eq!(rows[0].dict_get("shipName"), Some(&PyRep::String("CustomSnapShip".into())));

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&ship_id);
    });
    ctx.esi_state.set_custom_item_name(ship_id, "EsiNamedShip");
    let crow2 = call_unbound(&ctx, &mut sess, "GetCharactersToSelect", &[], &kw).unwrap().unwrap();
    assert_eq!(
        crow2.crowset_rows().unwrap()[0].dict_get("shipName"),
        Some(&PyRep::String("EsiNamedShip".into()))
    );

    let def_rand = call_unbound(&ctx, &mut sess, "GetValidRandomName", &[], &kw).unwrap().unwrap();
    assert!(def_rand.as_str().is_some_and(is_valid_character_name_syntax));

    let bad_bl = call_unbound(&ctx, &mut sess, "GetValidRandomName", &[PyRep::Integer(999_999)], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(bad_bl.code, "InvalidBloodline");

    assert_eq!(
        call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &[], &kw).unwrap().unwrap_err().code,
        "InvalidCharacterName"
    );
    assert_eq!(
        call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &[PyRep::String("CCP Bad".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCharacterName"
    );

    let anc = ctx.sde.ancestries.values().next().unwrap().clone();
    let bl = ctx.sde.bloodlines[&anc.bloodline_id].clone();
    let sch = ctx.sde.schools.values().find(|s| s.race_id == bl.race_id).unwrap().clone();
    let qa_sys = ctx.sde.solar_systems.values().find(|s| !s.stations.is_empty()).map(|s| s.solar_system_id).unwrap();
    let doll = PyRep::keyval([("hair", PyRep::Integer(1))]);
    let port = PyRep::keyval([("pose", PyRep::Integer(2))]);

    let bad_gender_args = [
        PyRep::String("Valid Pilot One".into()),
        PyRep::Integer(bl.bloodline_id as i64),
        PyRep::Integer(2),
        PyRep::Integer(anc.ancestry_id as i64),
        doll.clone(),
        port.clone(),
        PyRep::Integer(sch.school_id as i64),
    ];
    assert_eq!(
        call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &bad_gender_args, &kw).unwrap().unwrap_err().code,
        "InvalidGender"
    );

    let bad_doll_args = [
        PyRep::String("Valid Pilot One".into()),
        PyRep::Integer(bl.bloodline_id as i64),
        PyRep::Integer(1),
        PyRep::Integer(anc.ancestry_id as i64),
        PyRep::None,
        PyRep::None,
        PyRep::Integer(sch.school_id as i64),
    ];
    assert_eq!(
        call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &bad_doll_args, &kw).unwrap().unwrap_err().code,
        "InvalidDollInfo"
    );

    let bad_bl_args = [
        PyRep::String("Valid Pilot One".into()),
        PyRep::Integer(999_999),
        PyRep::Integer(1),
        PyRep::Integer(anc.ancestry_id as i64),
        doll.clone(),
        port.clone(),
        PyRep::Integer(sch.school_id as i64),
    ];
    assert_eq!(
        call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &bad_bl_args, &kw).unwrap().unwrap_err().code,
        "CharacterCreationFailed"
    );

    let create_10args = [
        PyRep::String("Qa Starter Pilot".into()),
        PyRep::Integer(bl.race_id as i64),
        PyRep::Integer(bl.bloodline_id as i64),
        PyRep::Integer(1),
        PyRep::Integer(anc.ancestry_id as i64),
        doll,
        port,
        PyRep::Integer(sch.school_id as i64),
        PyRep::None,
        PyRep::Integer(qa_sys as i64),
    ];
    let created_id = call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &create_10args, &kw)
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&created_id];
        assert_eq!((ch.gender, ch.solar_system_id), (1, qa_sys));
        assert!(snap.character_doll_info.contains_key(&created_id));
        assert!(snap.skill_history.get(&created_id).is_some_and(|h| h.iter().all(|e| e.event_type_id == 33)));
    });

    assert_eq!(
        call_unbound(&ctx, &mut sess, "SelectCharacterID", &[], &kw).unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    let _ = ctx.db.acquire_online_lock(1, 999, Some(created_id), true, ctx.now_unix());
    let mut kw_sel = HashMap::new();
    kw_sel.insert("charID".to_string(), PyRep::Integer(created_id));
    assert_eq!(
        call_unbound(&ctx, &mut sess, "SelectCharacterID", &[], &kw_sel).unwrap().unwrap_err().code,
        "CharacterAlreadyOnline"
    );
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(created_id), true, ctx.now_unix());
    let sel_ok = call_unbound(&ctx, &mut sess, "SelectCharacterID", &[], &kw_sel).unwrap().unwrap();
    assert_eq!(sel_ok.dict_get("characterID"), Some(&PyRep::Integer(created_id)));
    assert_eq!(sess.character_id, Some(created_id));
}

#[tokio::test]
async fn gm_slash_ext_heal_spawn_unspawn_setattr_getattr_and_fit_slots() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let sys_id = 30_000_142_u32;

    let (hi_tid, med_tid, lo_tid, rig_tid, sub_tid, drone_tid, ast_tid) =
        (940_101, 940_102, 940_103, 940_104, 940_105, 940_106, 940_107);
    clone_type_with(&mut ctx.sde, 2048, hi_tid, 53, 7, &[12], &[(37, 100.0)]);
    clone_type_with(&mut ctx.sde, 2048, med_tid, 46, 7, &[13], &[]);
    clone_type_with(&mut ctx.sde, 2048, lo_tid, 60, 7, &[11], &[]);
    clone_type_with(
        &mut ctx.sde,
        2048,
        rig_tid,
        773,
        7,
        &[2663],
        &[(eve_sde::attr_ids::RIG_SIZE, 1.0)],
    );
    clone_type_with(&mut ctx.sde, 2048, sub_tid, 954, 32, &[3772], &[]);
    clone_type_with(&mut ctx.sde, 2454, drone_tid, 100, 18, &[], &[]);
    clone_type_with(&mut ctx.sde, 1228, ast_tid, 462, 25, &[], &[]);

    let mut sess = SessionState {
        session_id: 91,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        role: ROLE_GML | ROLE_QA,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 50.0);
            ship.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 75.0);
            ship.dynamic_attributes.insert(1211, 20.0);
        }
    });
    let heal_ship = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/heal {ship_id}")).await.unwrap().unwrap();
    assert_eq!(heal_ship.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
    ctx.db.with_snapshot(|snap| {
        let ship = &snap.items[&ship_id];
        assert!(!ship.dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
        assert!(!ship.dynamic_attributes.contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
        assert!(!ship.dynamic_attributes.contains_key(&1211));
    });

    for (cmd, expected_code) in [
        ("/spawn bad", "InvalidSlashArgument"),
        ("/spawn 606 0", "InvalidSlashArgument"),
        ("/spawn 606 101", "InvalidSlashArgument"),
        ("/spawn 999999999 1", "TypeNotFound"),
    ] {
        assert_eq!(handle_gm_slash_ext(&ctx, &mut sess, char_id, cmd).await.unwrap().unwrap_err().code, expected_code);
    }

    let sp_ast = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/spawn {ast_tid} 2")).await.unwrap().unwrap();
    let ast_id = sp_ast.dict_get("spawnedID").unwrap().as_i64().unwrap();
    assert_eq!(sp_ast.dict_get("quantity"), Some(&PyRep::Integer(2)));
    assert!(ctx.db.with_snapshot(|s| s.asteroids.contains_key(&ast_id)));

    let child_id = 940_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, child_id, 34, char_id, ast_id, InventoryFlag::Cargo);
    });
    let desp = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/despawn {ast_id}")).await.unwrap().unwrap();
    assert_eq!(desp.dict_get("removedFromDb"), Some(&PyRep::Bool(true)));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&child_id)));
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/unspawn 999999999").await.unwrap().unwrap_err().code,
        "EntityNotFound"
    );

    for bad_cmd in [
        "/setattr ",
        "/setattr me",
        "/setattr me 37",
        "/setattr me 37 NaN",
        "/setattr notanid 37 1.0",
        "/getattr ",
        "/getattr me",
    ] {
        assert_eq!(
            handle_gm_slash_ext(&ctx, &mut sess, char_id, bad_cmd).await.unwrap().unwrap_err().code,
            "InvalidSlashArgument"
        );
    }
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/setattr 999999999 37 1.0").await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/getattr 999999999 37").await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );

    // Subsystem module on non-T3C ship is rejected cleanly without mutating inventory.
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/fit {sub_tid}"))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FittingValidationFailed"
    );

    // Subsystem module fits onto a T3C hull (Tengu = 29984, group_id = 963).
    let t3c_id = 940_222_i64;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, t3c_id, 29984, char_id, 60_003_760, InventoryFlag::Hangar);
    });

    for (target_arg, tid, expected_flag) in [
        ("me".to_string(), hi_tid, InventoryFlag::HiSlot1 as i64),
        ("me".to_string(), hi_tid, InventoryFlag::HiSlot2 as i64),
        ("me".to_string(), med_tid, InventoryFlag::MedSlot0 as i64),
        ("me".to_string(), lo_tid, InventoryFlag::LoSlot0 as i64),
        ("me".to_string(), rig_tid, InventoryFlag::RigSlot0 as i64),
        (t3c_id.to_string(), sub_tid, InventoryFlag::SubSystemSlot0 as i64),
        ("me".to_string(), drone_tid, InventoryFlag::DroneBay as i64),
    ] {
        let fit_rep = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/fit {target_arg} {tid}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fit_rep.dict_get("flagID"), Some(&PyRep::Integer(expected_flag)));
    }
    let fit_exp = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/fit {ship_id} {hi_tid} 27"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fit_exp.dict_get("flagID"), Some(&PyRep::Integer(27)));
}

#[tokio::test]
async fn gm_slash_kill_contact_cluster_energy_secstatus_and_qa_rpcs() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let sys_id = 30_000_142_u32;
    let mut sess = SessionState {
        session_id: 92,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        role: ROLE_GML | ROLE_QA,
        ..Default::default()
    };

    let npc_ball_id = 950_001_i64;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: npc_ball_id,
                type_id: 606,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let kill_ball = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/kill {npc_ball_id}")).await.unwrap().unwrap();
    assert_eq!(kill_ball.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/kill 999999999").await.unwrap().unwrap_err().code,
        "TargetNotFound"
    );

    let ally_id = 99_000_123_i64;
    let corp_id = 98_000_123_i64;
    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test Ally".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp".into(),
                ticker: "TSTC".into(),
                ..Default::default()
            },
        );
    });
    let c_ally = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/contact {ally_id} 25.0")).await.unwrap().unwrap();
    assert_eq!(c_ally.dict_get("standing"), Some(&PyRep::Real(10.0)));
    let c_ally2 = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/standing {ally_id} -15.0")).await.unwrap().unwrap();
    assert_eq!(c_ally2.dict_get("standing"), Some(&PyRep::Real(-10.0)));
    let c_npc = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/contact {npc_corp_id}")).await.unwrap().unwrap();
    assert_eq!(c_npc.dict_get("standing"), Some(&PyRep::Real(5.0)));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].standings.get(&npc_corp_id).copied()), Some(5.0));
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/contact 999888777 5.0").await.unwrap().unwrap_err().code,
        "ContactNotFound"
    );

    let dt_rep = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/downtime").await.unwrap().unwrap();
    assert!(matches!(dt_rep, PyRep::Dict(_)));
    assert_eq!(handle_gm_slash_ext(&ctx, &mut sess, char_id, "/reinforce 42").await.unwrap().unwrap(), PyRep::Integer(42));
    sess.solar_system_id = None;
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/tidi 0.5").await.unwrap().unwrap_err().code,
        "InvalidLocation"
    );
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/reinforce").await.unwrap().unwrap_err().code,
        "InvalidLocation"
    );
    sess.solar_system_id = Some(sys_id);

    let sec_clamped = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/secstatus {char_id} -25.0")).await.unwrap().unwrap();
    assert_eq!(sec_clamped, PyRep::Real(-10.0));
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/secstatus 999999999 1.0").await.unwrap().unwrap_err().code,
        "CharacterNotFound"
    );
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/energy 999999999 500").await.unwrap().unwrap_err().code,
        "CharacterNotFound"
    );

    let unk = call_slash_rpc(&ctx, &mut sess, "publicQaToolsServer", "SlashCmd", &[PyRep::String("/not_a_real_slash_cmd".into())])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(unk.code, "UnknownSlashCommand");

    let ok_qa = call_slash_rpc(&ctx, &mut sess, "publicQaToolsServer", "SlashCmd", &[PyRep::String("/whoami".into())])
        .await
        .unwrap()
        .unwrap();
    assert!(ok_qa.as_str().is_some_and(|s| s.contains("charID=")));
}
