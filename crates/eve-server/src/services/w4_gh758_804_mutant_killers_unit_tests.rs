//! Targeted boundary, boolean, and arithmetic mutant-killing unit tests for Wave 4
//! GH issues (`#758`, `#761`, `#789`–`#795`, `#799`, `#801`–`#804`).

#![forbid(unsafe_code)]

use crate::router::{
    binding::special_fx_guid_for_module_sde,
    cache_notifications::NotificationTarget,
    client_compat::{normalize_macho_notification_with_sde, normalize_macho_rpc_reply},
    ServerContext,
};
use eve_db::{CorporationRecord, KillmailRecord, PersistedPeaceTreaty};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DogmaEffectType, EffectId, GroupId, InvType, SdeRegistry, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

fn insert_test_type(
    sde: &mut SdeRegistry,
    type_id: TypeId,
    group_id: GroupId,
    name: &str,
    default_effect_id: Option<EffectId>,
    effect_ids: Vec<EffectId>,
) {
    sde.types.insert(
        type_id,
        InvType {
            type_id,
            group_id,
            category_id: 7,
            name: name.to_string(),
            mass: 1.0,
            volume: 1.0,
            capacity: 0.0,
            radius: 1.0,
            base_price: 100.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id,
            description: String::new(),
            effect_ids,
            default_attributes: HashMap::new(),
        },
    );
}

#[test]
fn special_fx_guid_for_module_sde_covers_all_groups_effects_and_fallbacks() {
    let mut sde = SdeRegistry::default();

    // 1. Mining exclusions: group_id, effect_id, and effect_name
    for (idx, &gid) in [54_u32, 464, 483, 737, 2004, 4138].iter().enumerate() {
        let tid = 900_000 + idx as u32;
        insert_test_type(&mut sde, tid, gid, "Miner", None, vec![]);
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, tid, 0, Some("targetAttack")),
            None,
            "mining group_id={gid} must return None"
        );
    }
    insert_test_type(&mut sde, 900_010, 1, "Generic", None, vec![]);
    for eid in [17_i64, 67, 2726] {
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, 900_010, eid, None),
            None,
            "mining effect_id={eid} must return None"
        );
    }
    for ename in ["miningLaser", "miningClouds", "mining"] {
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, 900_010, 0, Some(ename)),
            None,
            "mining effect_name={ename} must return None"
        );
    }

    // 2. Cloak by group_id == 330 or case-insensitive "cloak" in effect_name
    insert_test_type(&mut sde, 900_020, 330, "Prototype Cloak", None, vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_020, 0, None),
        Some(("effects.Cloak".to_string(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_010, 0, Some("CloakingDeviceII")),
        Some(("effects.Cloak".to_string(), false))
    );

    // 3. Hybrid, Projectile, and Laser turrets
    insert_test_type(&mut sde, 900_030, 74, "Hybrid Turret", None, vec![]);
    insert_test_type(&mut sde, 900_031, 53, "Heavy Ion Blaster II", None, vec![]);
    insert_test_type(&mut sde, 900_032, 53, "425mm Railgun II", None, vec![]);
    insert_test_type(&mut sde, 900_033, 53, "Mega Pulse Laser II", None, vec![]);
    insert_test_type(&mut sde, 900_034, 55, "800mm Repeating Cannon II", None, vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_030, 0, None),
        Some(("effects.HybridFired".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_010, 0, Some("hybridFired")),
        Some(("effects.HybridFired".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_031, 0, None),
        Some(("effects.HybridFired".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_032, 0, None),
        Some(("effects.HybridFired".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_033, 0, None),
        Some(("effects.Laser".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_010, 0, Some("targetAttack")),
        Some(("effects.Laser".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_034, 0, None),
        Some(("effects.ProjectileFired".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_010, 0, Some("projectileFired")),
        Some(("effects.ProjectileFired".to_string(), true))
    );

    // 4. SDE effect.guid resolution: explicit effect_id > 0 vs fallback default_effect_id when <= 0
    sde.effects.insert(
        500,
        DogmaEffectType {
            effect_id: 500,
            name: "sphereLaunch".to_string(),
            is_offensive: true,
            guid: Some(" effects.WarpDisruptSphere , trailing ".to_string()),
            ..Default::default()
        },
    );
    sde.effects.insert(
        501,
        DogmaEffectType {
            effect_id: 501,
            name: "triage".to_string(),
            is_offensive: false,
            guid: Some("effects.TriageMode".to_string()),
            ..Default::default()
        },
    );
    sde.effects.insert(
        502,
        DogmaEffectType {
            effect_id: 502,
            name: "nonEffectsPrefix".to_string(),
            is_offensive: true,
            guid: Some("custom.NotAnEffectsGuid".to_string()),
            ..Default::default()
        },
    );
    insert_test_type(&mut sde, 900_040, 9999, "DefaultGuidMod", Some(501), vec![501]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_040, 500, None),
        Some(("effects.WarpDisruptSphere".to_string(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_040, 0, None),
        Some(("effects.TriageMode".to_string(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_040, -1, None),
        Some(("effects.TriageMode".to_string(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_040, 502, None),
        None
    );

    // 5. All group_id arms (including MWD vs AB on group 46 and missile group boundaries)
    let group_cases: &[(u32, &str, bool)] = &[
        (65, "effects.ModifyTargetSpeed", true),
        (52, "effects.WarpScramble", true),
        (201, "effects.ElectronicAttributeModifyTarget", true),
        (208, "effects.ElectronicAttributeModifyTarget", true),
        (291, "effects.ElectronicAttributeModifyTarget", true),
        (842, "effects.ElectronicAttributeModifyTarget", true),
        (379, "effects.TargetPaint", true),
        (71, "effects.EnergyDestabilization", true),
        (68, "effects.EnergyVampire", true),
        (40, "effects.ShieldBoosting", false),
        (1156, "effects.ShieldBoosting", false),
        (62, "effects.ArmorRepair", false),
        (1199, "effects.ArmorRepair", false),
        (506, "effects.MissileDeployment", true),
        (507, "effects.MissileDeployment", true),
        (508, "effects.MissileDeployment", true),
        (509, "effects.MissileDeployment", true),
        (510, "effects.MissileDeployment", true),
        (511, "effects.MissileDeployment", true),
        (524, "effects.MissileDeployment", true),
        (1245, "effects.MissileDeployment", true),
    ];
    for (idx, &(gid, expected_guid, expected_off)) in group_cases.iter().enumerate() {
        let tid = 900_100 + idx as u32;
        insert_test_type(&mut sde, tid, gid, "GroupMod", None, vec![]);
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, tid, 0, None),
            Some((expected_guid.to_string(), expected_off)),
            "group_id={gid}"
        );
    }
    insert_test_type(&mut sde, 900_190, 505, "BelowMissileRange", None, vec![]);
    insert_test_type(&mut sde, 900_191, 512, "AboveMissileRange", None, vec![]);
    assert_eq!(special_fx_guid_for_module_sde(&sde, 900_190, 0, None), None);
    assert_eq!(special_fx_guid_for_module_sde(&sde, 900_191, 0, None), None);

    // Group 46: MWD by eff_name, by effect_ids containing 6730, by name containing "microwarp", else Afterburner
    insert_test_type(&mut sde, 900_200, 46, "10MN Prop Mod", None, vec![]);
    insert_test_type(&mut sde, 900_201, 46, "10MN Prop Mod", None, vec![6730]);
    insert_test_type(&mut sde, 900_202, 46, "50MN Microwarpdrive II", None, vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_200, 0, None),
        Some(("effects.Afterburner".to_string(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_200, 0, Some("moduleBonusMicrowarpdrive")),
        Some(("effects.MicroWarpDrive".to_string(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_201, 0, None),
        Some(("effects.MicroWarpDrive".to_string(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_202, 0, None),
        Some(("effects.MicroWarpDrive".to_string(), false))
    );

    // 6. Fallback eff_name match arms when group_id is unrecognized
    let eff_name_cases: &[(&str, &str, bool)] = &[
        ("remoteWebifierFalloff", "effects.ModifyTargetSpeed", true),
        ("modifyTargetSpeed2", "effects.ModifyTargetSpeed", true),
        ("decreaseTargetSpeed", "effects.ModifyTargetSpeed", true),
        ("warpScramble", "effects.WarpScramble", true),
        ("warpScrambleForEntity", "effects.WarpScramble", true),
        ("warpScrambleBlockMWDWithNPCEffect", "effects.WarpScramble", true),
        ("remoteSensorDampFalloff", "effects.ElectronicAttributeModifyTarget", true),
        ("remoteECMFalloff", "effects.ElectronicAttributeModifyTarget", true),
        (
            "structureModuleEffectRemoteSensorDampener",
            "effects.ElectronicAttributeModifyTarget",
            true,
        ),
        ("remoteWeaponDisruptFalloff", "effects.ElectronicAttributeModifyTarget", true),
        ("ewTargetPaint", "effects.TargetPaint", true),
        ("remoteTargetPaintFalloff", "effects.TargetPaint", true),
        ("energyDestabilizationNew", "effects.EnergyDestabilization", true),
        ("energyNeutralizerFalloff", "effects.EnergyDestabilization", true),
        ("energyVampire", "effects.EnergyVampire", true),
        ("energyNosferatuFalloff", "effects.EnergyVampire", true),
        ("moduleBonusMicrowarpdrive", "effects.MicroWarpDrive", false),
        ("moduleBonusAfterburner", "effects.Afterburner", false),
        ("speedBoost", "effects.Afterburner", false),
        ("shieldBoosting", "effects.ShieldBoosting", false),
        ("fueledShieldBoosting", "effects.ShieldBoosting", false),
        ("armorRepair", "effects.ArmorRepair", false),
        ("fueledArmorRepair", "effects.ArmorRepair", false),
    ];
    for &(ename, expected_guid, expected_off) in eff_name_cases {
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, 900_010, 0, Some(ename)),
            Some((expected_guid.to_string(), expected_off)),
            "eff_name={ename}"
        );
    }
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 900_010, 0, Some("online")),
        None
    );
}

#[test]
fn resolve_missing_dynamic_ship_stub_type_id_boundaries_and_killmail_tiebreakers() {
    use crate::services::fitting_containers_ships::resolve_missing_dynamic_ship_stub_type_id;

    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;

    // 1. Exact item_id range boundaries: 200_000_000..1_000_000_000_000
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 199_999_999),
        None
    );
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 200_000_000),
        Some(670)
    );
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 999_999_999_999),
        Some(670)
    );
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 1_000_000_000_000),
        None
    );

    // 2. Exact char_id <= 0 boundaries
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, 0, 200_000_000),
        None
    );
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, -1, 200_000_000),
        None
    );
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, 1, 200_000_000),
        Some(670)
    );

    // 3. Existing item in snap.items returns None
    let existing_id = 200_555_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            existing_id,
            eve_db::InventoryItemRecord {
                item_id: existing_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Killmail with ship_type_id == 0 must be ignored
        snap.killmails.insert(
            1,
            KillmailRecord {
                kill_id: 1,
                solar_system_id: 30_000_142,
                killed_unix: 9_999_999_999,
                victim_character_id: char_id,
                victim_corp_id: 1_000_009,
                victim_alliance_id: None,
                ship_type_id: 0,
                ..Default::default()
            },
        );
        // Killmail for another character must be ignored
        snap.killmails.insert(
            2,
            KillmailRecord {
                kill_id: 2,
                solar_system_id: 30_000_142,
                killed_unix: 9_999_999_999,
                victim_character_id: char_id + 1,
                victim_corp_id: 1_000_009,
                victim_alliance_id: None,
                ship_type_id: 621,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, existing_id),
        None
    );
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 200_555_002),
        Some(670)
    );

    // 4. Max by (killed_unix, kill_id) tie-breaking
    ctx.db.with_mut_snapshot(|snap| {
        for (kid, ts, stid) in [(10_i64, 1_000_u64, 587_u32), (11, 2_000, 621), (12, 2_000, 645)] {
            snap.killmails.insert(
                kid,
                KillmailRecord {
                    kill_id: kid,
                    solar_system_id: 30_000_142,
                    killed_unix: ts,
                    victim_character_id: char_id,
                    victim_corp_id: 1_000_009,
                    victim_alliance_id: None,
                    ship_type_id: stid,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(
        resolve_missing_dynamic_ship_stub_type_id(&ctx, char_id, 200_555_002),
        Some(645)
    );
}

#[tokio::test]
async fn cycle_weapon_damage_flush_and_execute_pending_actions_boundaries() {
    use crate::services::inventory_fitting::fitting_module_cycles::execute_pending_cycle_actions;
    use crate::system_actor::{flush_pending_cycle_damage_notifications, PendingCycleAction};

    let ctx = ServerContext::new();
    let acid = 90_000_001_i64;
    let vcid = 90_000_002_i64;
    let pending_map = RwLock::new(HashMap::new());
    let (tx, mut rx) = tokio::sync::broadcast::channel(32);

    // 1. pending_opt == None returns early without flipping `notified`
    let mut actions = vec![PendingCycleAction::CycleWeaponDamage {
        attacker_char_id: Some(acid),
        attacker_ship_id: 200_100,
        attacker_ship_type_id: 587,
        target_id: 200_200,
        target_char_id: Some(vcid),
        module_type_id: 3640,
        applied_dmg: 25.0,
        notified: false,
    }];
    flush_pending_cycle_damage_notifications(Some(&ctx.db), None, Some(&tx), &mut actions);
    assert!(matches!(
        actions[0],
        PendingCycleAction::CycleWeaponDamage { notified: false, .. }
    ));

    // 2. applied_dmg <= 0.0 (0.0 and -5.0) is skipped; self-damage (target_char_id == attacker_char_id)
    //    suppresses AttackedBy; attacker_char_id == Some(0) suppresses AttackHit
    let mut edge_actions = vec![
        PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(acid),
            attacker_ship_id: 200_100,
            attacker_ship_type_id: 587,
            target_id: 200_200,
            target_char_id: Some(vcid),
            module_type_id: 3640,
            applied_dmg: 0.0,
            notified: false,
        },
        PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(acid),
            attacker_ship_id: 200_100,
            attacker_ship_type_id: 587,
            target_id: 200_100,
            target_char_id: Some(acid), // self-damage: no AttackedBy
            module_type_id: 3640,
            applied_dmg: 15.0,
            notified: false,
        },
        PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(0), // NPC/non-char attacker: no AttackHit
            attacker_ship_id: 200_300,
            attacker_ship_type_id: 10_001,
            target_id: 200_200,
            target_char_id: Some(vcid),
            module_type_id: 3640,
            applied_dmg: 30.0,
            notified: false,
        },
    ];
    flush_pending_cycle_damage_notifications(
        Some(&ctx.db),
        Some(&pending_map),
        Some(&tx),
        &mut edge_actions,
    );
    assert!(matches!(
        edge_actions[0],
        PendingCycleAction::CycleWeaponDamage { notified: false, .. }
    ));
    assert!(matches!(
        edge_actions[1],
        PendingCycleAction::CycleWeaponDamage { notified: true, .. }
    ));
    assert!(matches!(
        edge_actions[2],
        PendingCycleAction::CycleWeaponDamage { notified: true, .. }
    ));
    {
        let g = pending_map.read().unwrap();
        assert!(!g.contains_key(&0));
        // acid only received AttackHit (OnDamageMessage + OnDamageMessages = 2), no AttackedBy
        let a_list = g.get(&acid).expect("acid notifications");
        assert_eq!(a_list.len(), 2);
        assert_eq!(a_list[0].args.len(), 1);
        assert_eq!(
            a_list[0].args.first().and_then(|d| d.dict_get("attackType")),
            Some(&PyRep::String("me".into()))
        );
        assert_eq!(
            a_list[0].args.first().and_then(|d| d.dict_get("isBanked")),
            Some(&PyRep::Bool(false))
        );
        // vcid only received AttackedBy from action[2] (OnDamageMessage + OnDamageMessages = 2)
        let v_list = g.get(&vcid).expect("vcid notifications");
        assert_eq!(v_list.len(), 2);
        assert_eq!(v_list[0].args.len(), 1);
        assert_eq!(
            v_list[0].args.first().and_then(|d| d.dict_get("attackType")),
            Some(&PyRep::String("other".into()))
        );
        assert_eq!(
            v_list[0].args.first().and_then(|d| d.dict_get("isBanked")),
            Some(&PyRep::Bool(false))
        );
    }
    assert!(rx.try_recv().is_ok());

    // Calling a second time on already-notified slice is a no-op
    flush_pending_cycle_damage_notifications(
        Some(&ctx.db),
        Some(&pending_map),
        Some(&tx),
        &mut edge_actions,
    );
    assert_eq!(pending_map.read().unwrap().get(&acid).map(Vec::len), Some(2));

    // 3. execute_pending_cycle_actions: unnotified CycleWeaponDamage emits AttackHit + AttackedBy,
    //    while notified == true or applied_dmg == 0.0 skips duplicate notifications
    let sys_id = 30_000_142_u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, cid) in [(501_u64, acid), (502_u64, vcid)] {
        let uid = ctx
            .db
            .with_snapshot(|s| s.characters.get(&cid).map(|c| c.user_id).unwrap_or(sid));
        let _ = ctx
            .db
            .acquire_online_lock(uid, sid, Some(cid), false, ctx.now_unix());
    }
    let _ = ctx.drain_notifications_for_char(acid);
    let _ = ctx.drain_notifications_for_char(vcid);
    {
        let mut st = sys_handle.actor_state.write().unwrap();
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(acid),
            attacker_ship_id: 200_100,
            attacker_ship_type_id: 587,
            target_id: 200_200,
            target_char_id: Some(vcid),
            module_type_id: 3640,
            applied_dmg: 40.0,
            notified: false,
        });
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(acid),
            attacker_ship_id: 200_100,
            attacker_ship_type_id: 587,
            target_id: 200_200,
            target_char_id: Some(vcid),
            module_type_id: 3640,
            applied_dmg: 40.0,
            notified: true,
        });
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(acid),
            attacker_ship_id: 200_100,
            attacker_ship_type_id: 587,
            target_id: 200_200,
            target_char_id: Some(vcid),
            module_type_id: 3640,
            applied_dmg: 0.0,
            notified: false,
        });
    }
    execute_pending_cycle_actions(&ctx, sys_id).await;
    let a_notifs = ctx.drain_notifications_for_char(acid);
    let v_notifs = ctx.drain_notifications_for_char(vcid);
    assert_eq!(
        a_notifs.iter().filter(|n| n.event == "OnDamageMessage").count(),
        1
    );
    assert_eq!(
        v_notifs.iter().filter(|n| n.event == "OnDamageMessage").count(),
        1
    );
}

#[tokio::test]
async fn peace_treaties_expiry_alliance_and_client_compat_boundaries() {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let my_corp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
        .unwrap_or(1_000_009);
    let my_ally = 99_000_777_i64;
    let peer_corp_1 = 98_000_801_i64;
    let peer_corp_2 = 98_000_802_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.peace_treaties = vec![
            // Exactly at `now`: must be skipped (`expiry_unix <= now`)
            PersistedPeaceTreaty {
                war_id: 1,
                corp_a: my_corp,
                corp_b: peer_corp_1,
                expiry_unix: now,
            },
            // `now + 1` via alliance as aggressor -> outgoing
            PersistedPeaceTreaty {
                war_id: 2,
                corp_a: my_ally,
                corp_b: peer_corp_1,
                expiry_unix: now + 1,
            },
            // `now + 1` via alliance as defender -> incoming
            PersistedPeaceTreaty {
                war_id: 3,
                corp_a: peer_corp_2,
                corp_b: my_ally,
                expiry_unix: now + 1,
            },
            // Treaty with `corp_a == 0` must NOT match `session.alliance_id == Some(0)`
            PersistedPeaceTreaty {
                war_id: 4,
                corp_a: 0,
                corp_b: peer_corp_2,
                expiry_unix: now + 100,
            },
        ];
    });

    // 1. session.alliance_id == Some(0) is filtered out (`filter(|&a| a > 0)`)
    let mut sess_zero_ally = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(my_corp),
        alliance_id: Some(0),
        ..Default::default()
    };
    let res_zero = crate::services::social_corp_fleet::handle(
        &ctx,
        &mut sess_zero_ally,
        "peaceTreatyMgr",
        "GetPeaceTreatiesForSession",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        res_zero,
        PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::List(vec![])])
    );

    // 2. session.alliance_id == Some(my_ally) matches war 2 (outgoing) and war 3 (incoming)
    let mut sess_ally = SessionState {
        alliance_id: Some(my_ally),
        ..sess_zero_ally
    };
    let res_ally = crate::services::social_corp_fleet::handle(
        &ctx,
        &mut sess_ally,
        "peaceTreatyMgr",
        "GetPeaceTreatiesForSession",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts) = res_ally else {
        panic!("expected 2-tuple");
    };
    let PyRep::List(ref out_list) = parts[0] else {
        panic!("expected outgoing list");
    };
    let PyRep::List(ref in_list) = parts[1] else {
        panic!("expected incoming list");
    };
    assert_eq!(out_list.len(), 1);
    assert_eq!(in_list.len(), 1);
    assert_eq!(
        out_list[0].dict_get("otherOwnerID"),
        Some(&PyRep::Integer(peer_corp_1))
    );
    assert_eq!(
        in_list[0].dict_get("otherOwnerID"),
        Some(&PyRep::Integer(peer_corp_2))
    );

    // 3. client_compat: 2-tuple preserved, 1-tuple / non-tuple normalized to ([], [])
    let valid_two = PyRep::Tuple(vec![
        PyRep::List(vec![PyRep::Integer(1)]),
        PyRep::List(vec![]),
    ]);
    assert_eq!(
        normalize_macho_rpc_reply(
            "peaceTreatyMgr",
            "GetPeaceTreatiesForSession",
            Ok(valid_two.clone())
        )
        .unwrap(),
        valid_two
    );
    for bad in [
        PyRep::Tuple(vec![PyRep::List(vec![])]),
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]),
        PyRep::None,
    ] {
        assert_eq!(
            normalize_macho_rpc_reply("peaceTreatyMgr", "GetPeaceTreatiesForSession", Ok(bad))
                .unwrap(),
            PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::List(vec![])])
        );
    }
}

#[test]
fn on_module_attribute_changes_notify_and_client_compat_boundaries() {
    let ctx = ServerContext::new();
    let cid = 90_000_001_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&cid].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 501, Some(cid), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(cid);

    // 1. ServerContext::notify fills t[1] when None and t[4] when Integer(0),
    //    but preserves existing non-None t[1] and non-zero t[4]
    let tup_fill = PyRep::Tuple(vec![
        PyRep::String("OnModuleAttributeChanges".into()),
        PyRep::None,
        PyRep::Integer(1001),
        PyRep::Integer(6),
        PyRep::Integer(0),
        PyRep::Real(10.0),
        PyRep::Real(5.0),
        PyRep::None,
    ]);
    let tup_keep = PyRep::Tuple(vec![
        PyRep::String("OnModuleAttributeChanges".into()),
        PyRep::Integer(777_888),
        PyRep::Integer(1002),
        PyRep::Integer(6),
        PyRep::Integer(123_456_789),
        PyRep::Real(10.0),
        PyRep::Real(5.0),
        PyRep::None,
    ]);
    let sent = ctx.notify(
        NotificationTarget::Character(cid),
        "OnModuleAttributeChanges",
        "charid",
        vec![PyRep::List(vec![tup_fill, tup_keep])],
    );
    assert_eq!(sent.len(), 1);
    let PyRep::List(ref items) = sent[0].args[0] else {
        panic!("expected List");
    };
    let PyRep::Tuple(ref t0) = items[0] else {
        panic!("expected Tuple 0");
    };
    let PyRep::Tuple(ref t1) = items[1] else {
        panic!("expected Tuple 1");
    };
    assert_eq!(t0[1], PyRep::Integer(cid));
    assert!(t0[4].as_i64().unwrap_or(0) > 0);
    assert_eq!(t1[1], PyRep::Integer(777_888));
    assert_eq!(t1[4], PyRep::Integer(123_456_789));

    // 2. normalize_macho_notification_with_sde for 8-tuple, 7-tuple, and scalar fallback
    let norm = normalize_macho_notification_with_sde(
        "OnModuleAttributeChanges",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(cid),
                PyRep::Integer(1001),
                PyRep::Integer(0), // -> 2
                PyRep::Integer(10),
                PyRep::Real(1.0),
                PyRep::Real(0.0),
                PyRep::Bool(false), // -> None
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(cid),
                PyRep::Integer(1002),
                PyRep::Integer(16), // preserved
                PyRep::Integer(10),
                PyRep::Real(1.0),
                PyRep::Real(0.0),
                PyRep::Integer(999), // preserved
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(cid),
                PyRep::Integer(1003),
                PyRep::Integer(0), // -> 2, and None appended for 8th slot
                PyRep::Integer(10),
                PyRep::Real(1.0),
                PyRep::Real(0.0),
            ]),
            PyRep::Integer(1004), // scalar fallback -> 8-tuple
        ])],
        None,
    );
    let PyRep::List(ref n_items) = norm[0] else {
        panic!("expected normalized List");
    };
    assert_eq!(n_items.len(), 4);
    let PyRep::Tuple(ref nt0) = n_items[0] else { unreachable!() };
    assert_eq!(nt0[3], PyRep::Integer(2));
    assert_eq!(nt0[7], PyRep::None);
    let PyRep::Tuple(ref nt1) = n_items[1] else { unreachable!() };
    assert_eq!(nt1[3], PyRep::Integer(16));
    assert_eq!(nt1[7], PyRep::Integer(999));
    let PyRep::Tuple(ref nt2) = n_items[2] else { unreachable!() };
    assert_eq!(nt2.len(), 8);
    assert_eq!(nt2[3], PyRep::Integer(2));
    assert_eq!(nt2[7], PyRep::None);
    let PyRep::Tuple(ref nt3) = n_items[3] else { unreachable!() };
    assert_eq!(nt3.len(), 8);
    assert_eq!(nt3[2], PyRep::Integer(1004));
    assert_eq!(nt3[3], PyRep::Integer(2));
    assert_eq!(nt3[7], PyRep::None);
}

#[test]
fn agent_mission_objective_compat_and_aggression_settings_all_combinations() {
    // 1. normalize_macho_rpc_reply("agentMgr", "GetMissionObjectiveInfo", ...):
    //    originStationID <= 0 fallback (60_003_760), destinationStationID <= 0 vs > 0,
    //    contentID <= 0 clamped to 1, and transport hasCargo across (completion_status, mission_state)
    let combat_raw = PyRep::keyval([
        ("contentID", PyRep::Integer(0)),
        ("missionState", PyRep::Integer(1)),
        ("completionStatus", PyRep::Integer(0)),
        ("originStationID", PyRep::Integer(0)),
        ("destinationStationID", PyRep::Integer(0)),
        ("iskReward", PyRep::Integer(50_000)),
        ("bonusReward", PyRep::Integer(25_000)),
        ("loyaltyPoints", PyRep::Integer(100)),
    ]);
    let combat_norm = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionObjectiveInfo",
        Ok(combat_raw),
    )
    .unwrap();
    let Some(PyRep::List(dungeons)) = combat_norm.dict_get("dungeons") else {
        panic!("expected dungeons list");
    };
    assert_eq!(dungeons.len(), 1);
    assert_eq!(dungeons[0].dict_get("dungeonID"), Some(&PyRep::Integer(1)));
    assert_eq!(
        dungeons[0].dict_get("locationID"),
        Some(&PyRep::Integer(60_003_760))
    );
    assert_eq!(dungeons[0].dict_get("completed"), Some(&PyRep::Bool(false)));

    for (comp_status, m_state, expected_has_cargo) in [
        (0_i64, 1_i64, false),
        (1, 1, true),
        (0, 2, true),
    ] {
        let courier_raw = PyRep::keyval([
            ("contentID", PyRep::Integer(42)),
            ("missionState", PyRep::Integer(m_state)),
            ("completionStatus", PyRep::Integer(comp_status)),
            ("originStationID", PyRep::Integer(60_003_760)),
            ("destinationStationID", PyRep::Integer(60_003_761)),
        ]);
        let courier_norm = normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionObjectiveInfo",
            Ok(courier_raw),
        )
        .unwrap();
        assert_eq!(
            courier_norm.dict_get("dungeons"),
            Some(&PyRep::List(Vec::new()))
        );
        let Some(PyRep::List(objs)) = courier_norm.dict_get("objectives") else {
            panic!("expected objectives");
        };
        assert_eq!(objs.len(), 2);
        let PyRep::Tuple(ref transport_tup) = objs[1] else {
            panic!("expected transport tuple");
        };
        let PyRep::Tuple(ref transport_data) = transport_tup[1] else {
            panic!("expected transport data");
        };
        assert_eq!(
            transport_data[4].dict_get("hasCargo"),
            Some(&PyRep::Bool(expected_has_cargo))
        );
    }

    // 2. AggressionSettings: test all 4 (ff, current_ff) combinations in RegisterNewAggressionSettings
    //    and all (ff, pending_val) combinations in GetAggressionSettings / GetAggressionSettingsForCorps
    let ctx = ServerContext::new();
    let corp_id = 98_700_001_i64;
    let ceo_id = 90_000_001_i64;
    let now_unix = 1_800_050_000_u64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut c = CorporationRecord::new(corp_id, "Agg Test Corp", "AGGT", ceo_id);
        c.hq_station_id = Some(60_003_760);
        c.members = [ceo_id].into_iter().collect();
        c.member_roles = [(ceo_id, u64::MAX)].into_iter().collect();
        snap.corporations.insert(corp_id, c);
    });
    let mut sess = SessionState {
        session_id: 1,
        user_id: Some(ceo_id as u64),
        character_id: Some(ceo_id),
        corporation_id: Some(corp_id),
        corp_role: u64::MAX,
        ..Default::default()
    };
    let eff_ft = crate::sp6_lifecycle::unix_to_filetime(now_unix + 86_400);

    for (current_ff, new_ff, exp_en, exp_dis) in [
        (false, true, PyRep::Integer(eff_ft), PyRep::Integer(0)),
        (true, true, PyRep::Integer(eff_ft), PyRep::None),
        (true, false, PyRep::Integer(0), PyRep::Integer(eff_ft)),
        (false, false, PyRep::None, PyRep::Integer(eff_ft)),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            let gov = snap.corp_governance.entry(corp_id).or_default();
            gov.friendly_fire_legal = current_ff;
            gov.pending_friendly_fire = None;
        });
        let reg = crate::services::fleet_social::fleet_social_corp_tail_dispatch::handle_corp_gov_tail(
            &ctx,
            &mut sess,
            "corpRegistry",
            "RegisterNewAggressionSettings",
            &[PyRep::Bool(new_ff)],
            corp_id,
            ceo_id,
            now_unix,
        )
        .unwrap()
        .unwrap();
        assert_eq!(reg.dict_get("_enableAfter"), Some(&exp_en));
        assert_eq!(reg.dict_get("_disableAfter"), Some(&exp_dis));

        let get_one = crate::services::fleet_social::fleet_social_corp_tail_dispatch::handle_corp_gov_tail(
            &ctx,
            &mut sess,
            "corpmgr",
            "GetAggressionSettingsForCorps",
            &[PyRep::List(vec![PyRep::Integer(corp_id)])],
            corp_id,
            ceo_id,
            now_unix,
        )
        .unwrap()
        .unwrap();
        let PyRep::Dict(entries) = get_one else {
            panic!("expected Dict from GetAggressionSettingsForCorps");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.dict_get("_enableAfter"), Some(&exp_en));
        assert_eq!(entries[0].1.dict_get("_disableAfter"), Some(&exp_dis));
    }
}

#[tokio::test]
async fn fw_hq_system_ids_and_fleet_member_opt_outs_boundaries() {
    // 1. fleet_social_notify::member_opt_outs booleans
    for (warp, regroup, conduit) in [(true, false, true), (false, true, false)] {
        let m = eve_db::FleetMemberRecord {
            character_id: 90_000_001,
            role: eve_db::FleetRole::SquadMember,
            wing_id: Some(1),
            squad_id: Some(1),
            job: 0,
            role_name: String::new(),
            takes_fleet_warp: warp,
            accepts_conduit_jumps: conduit,
            accepts_regroup: regroup,
            join_time_unix: 1_700_000_000,
        };
        let opt = crate::services::fleet_social::fleet_social_notify::member_opt_outs(&m);
        assert_eq!(opt.dict_get("acceptsFleetWarp"), Some(&PyRep::Bool(warp)));
        assert_eq!(
            opt.dict_get("acceptsFleetRegroups"),
            Some(&PyRep::Bool(regroup))
        );
        assert_eq!(opt.dict_get("acceptsRegroup"), Some(&PyRep::Bool(regroup)));
        assert_eq!(
            opt.dict_get("acceptsConduitJumps"),
            Some(&PyRep::Bool(conduit))
        );
    }

    // 2. fw_services::handle("fwWarzoneSolarsystem", "GetHQSystemIDs") with filtered vs empty solar_systems
    let mut sde_empty = SdeRegistry::default();
    sde_empty.solar_systems.clear();
    let ctx_empty = ServerContext::with_sde_and_repository(
        Arc::new(sde_empty),
        Arc::new(eve_db::WorldRepository::new_empty()),
    );
    let mut sess = SessionState::default();
    let all_four = crate::services::fw_services::handle(
        &ctx_empty,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetHQSystemIDs",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Dict(entries_four) = all_four else {
        panic!("expected Dict");
    };
    assert_eq!(entries_four.len(), 4);

    // When solar_systems is non-empty and only contains Onnamon (30_045_324), only Caldari (500_001) is retained
    let mut sde_one = SdeRegistry::default();
    let sample_sys = ServerContext::new()
        .sde
        .solar_systems
        .values()
        .min_by_key(|s| s.solar_system_id)
        .cloned()
        .unwrap();
    sde_one.solar_systems.clear();
    sde_one.solar_systems.insert(30_045_324, sample_sys);
    let ctx_one = ServerContext::with_sde_and_repository(
        Arc::new(sde_one),
        Arc::new(eve_db::WorldRepository::new_empty()),
    );
    let filtered = crate::services::fw_services::handle(
        &ctx_one,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetHQSystemIDs",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        filtered,
        PyRep::Dict(vec![(
            PyRep::Integer(500_001),
            PyRep::Integer(30_045_324)
        )])
    );
}

#[tokio::test]
async fn deactivate_special_fx_and_cycle_deactivation_hook_and_target_char_fallbacks() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let victim_char = 90_000_002_i64;
    let ship_id = 200_310_001_i64;
    let target_ship_id = 200_310_002_i64;
    let turret_mod_id = 200_310_010_i64;
    let ab_mod_id = 200_310_011_i64;
    let sys_id = 30_000_142_u32;

    // 1. `handle_mining_and_deactivate` ("Deactivate") emits `OnSpecialFX` with `start=0`
    //    for active non-mining modules (`mod_type_opt = Some(tid)`).
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: eve_db::InventoryFlag::None,
                quantity: -1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (mid, tid, flag) in [
            (turret_mod_id, 3640_u32, eve_db::InventoryFlag::HiSlot0),
            (ab_mod_id, 434_u32, eve_db::InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                mid,
                eve_db::InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: -1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_active_states.insert(mid);
        }
    });
    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, tid, flag) in [
        (turret_mod_id, 3640_u32, eve_db::InventoryFlag::HiSlot0 as u16),
        (ab_mod_id, 434_u32, eve_db::InventoryFlag::MedSlot0 as u16),
    ] {
        fit.modules.insert(
            mid,
            eve_dogma::FittedModule {
                item_id: mid,
                type_id: tid,
                flag_id: flag,
                state: eve_dogma::ModuleState::Active,
                charge_type_id: None,
                charge_quantity: 0,
                damage_hp: 0.0,
                max_hp: 40.0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }
    ctx.store_ship_fitting(ship_id, fit);

    let mut sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    for (sid, cid) in [(501_u64, char_id), (502_u64, victim_char)] {
        let uid = ctx
            .db
            .with_snapshot(|s| s.characters.get(&cid).map(|c| c.user_id).unwrap_or(sid));
        let _ = ctx
            .db
            .acquire_online_lock(uid, sid, Some(cid), false, ctx.now_unix());
    }
    let kw = HashMap::new();
    for (mid, tid, eff_name, exp_guid, exp_off) in [
        (turret_mod_id, 3640_u32, "targetAttack", "effects.HybridFired", true),
        (ab_mod_id, 434_u32, "moduleBonusAfterburner", "effects.Afterburner", false),
    ] {
        let _ = ctx.drain_notifications_for_char(char_id);
        let deact = crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Deactivate",
            &[PyRep::Integer(mid), PyRep::String(eff_name.into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(deact.dict_get("overloaded"), Some(&PyRep::Bool(false)));
        let notifs = ctx.drain_notifications_for_char(char_id);
        let sfx = notifs
            .iter()
            .find(|n| n.event == "OnSpecialFX")
            .expect("Deactivate must emit OnSpecialFX with start=0");
        assert_eq!(sfx.args.len(), 14);
        assert_eq!(sfx.args[0], PyRep::Integer(ship_id));
        assert_eq!(sfx.args[1], PyRep::Integer(mid));
        assert_eq!(sfx.args[2], PyRep::Integer(i64::from(tid)));
        assert_eq!(sfx.args[3], PyRep::None);
        assert_eq!(sfx.args[4], PyRep::None);
        assert_eq!(sfx.args[5], PyRep::String(exp_guid.into()));
        assert_eq!(sfx.args[6], PyRep::Bool(exp_off));
        assert_eq!(sfx.args[7], PyRep::Integer(0));
        assert_eq!(sfx.args[8], PyRep::Integer(0));
        assert_eq!(sfx.args[9], PyRep::Real(0.0));
        assert_eq!(sfx.args[10], PyRep::Integer(0));
        assert!(sfx.args[11].as_i64().unwrap_or(0) > 0);
        assert_eq!(sfx.args[12], PyRep::Real(0.0));
        assert_eq!(sfx.args[13], PyRep::None);
    }

    // 2. `binding.rs` `set_module_deactivation_hook` (`OnSpecialFX` with `cycle.duration_s * 1000.0`
    //    and `cycle.target_id`) + `cycles.rs` `target_char_id` resolution from `db.characters` / `db.items`.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_char) {
            ch.active_ship_id = target_ship_id;
        }
    });
    let handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, cid_opt) in [(ship_id, Some(char_id)), (target_ship_id, None)] {
        handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: bid,
                    type_id: 587,
                    position: glam::DVec3::ZERO,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    speed_fraction: 0.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::new(),
                eve_destiny::SlimItemMeta {
                    group_id: 25,
                    category_id: 6,
                    owner_id: cid_opt.unwrap_or(victim_char),
                    char_id: cid_opt,
                    corp_id: 1_000_009,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(victim_char);
    handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            owner_char_id: Some(char_id),
            ship_id,
            module_id: turret_mod_id,
            module_type_id: 3640,
            effect_name: "targetAttack".into(),
            duration_s: 2.5,
            elapsed_s: 2.0,
            cap_need_per_cycle: 0.0,
            repeat: 0,
            target_id: Some(target_ship_id),
            damage_vector: eve_destiny::DamageVector::new(12.0, 8.0, 0.0, 0.0),
            max_effective_range_m: Some(10_000.0),
            ..Default::default()
        })
        .await
        .unwrap();

    handle.step_manual(1.0).await.unwrap();

    let att_notifs = ctx.drain_notifications_for_char(char_id);
    let hook_sfx = att_notifs
        .iter()
        .find(|n| n.event == "OnSpecialFX")
        .expect("deact_hook must emit OnSpecialFX when repeat=0 cycle ends");
    assert_eq!(hook_sfx.args[0], PyRep::Integer(ship_id));
    assert_eq!(hook_sfx.args[1], PyRep::Integer(turret_mod_id));
    assert_eq!(hook_sfx.args[2], PyRep::Integer(3640));
    assert_eq!(hook_sfx.args[3], PyRep::Integer(target_ship_id));
    assert_eq!(hook_sfx.args[4], PyRep::None);
    assert_eq!(hook_sfx.args[5], PyRep::String("effects.HybridFired".into()));
    assert_eq!(hook_sfx.args[6], PyRep::Bool(true));
    assert_eq!(hook_sfx.args[7], PyRep::Integer(0));
    assert_eq!(hook_sfx.args[8], PyRep::Integer(0));
    assert_eq!(hook_sfx.args[9], PyRep::Real(2500.0));
    assert_eq!(hook_sfx.args[10], PyRep::Integer(0));
    assert!(hook_sfx.args[11].as_i64().unwrap_or(0) > 0);
    assert_eq!(hook_sfx.args[12], PyRep::Real(0.0));
    assert_eq!(hook_sfx.args[13], PyRep::None);

    // `target_char_id` was resolved via `db.characters.find(|c| c.active_ship_id == target_id)`
    // even though `ball_slim_meta.char_id` on `target_ship_id` was `None`.
    let vic_notifs = ctx.drain_notifications_for_char(victim_char);
    assert!(
        vic_notifs.iter().any(|n| n.event == "OnDamageMessage"),
        "victim must receive OnDamageMessage via db.characters active_ship_id fallback"
    );

    // Also verify `db.items.get(&target_id)` fallback when `active_ship_id != target_id`
    // and NPC attacker (`owner_char_id: None`, `target_char_id: Some(victim_char)`).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_char) {
            ch.active_ship_id = 0;
        }
        snap.items.insert(
            target_ship_id,
            eve_db::InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: victim_char,
                location_id: i64::from(sys_id),
                flag: eve_db::InventoryFlag::None,
                quantity: -1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            owner_char_id: None,
            ship_id,
            module_id: turret_mod_id,
            module_type_id: 3640,
            effect_name: "targetAttack".into(),
            duration_s: 2.0,
            elapsed_s: 1.5,
            cap_need_per_cycle: 0.0,
            repeat: 0,
            target_id: Some(target_ship_id),
            damage_vector: eve_destiny::DamageVector::new(10.0, 0.0, 0.0, 0.0),
            max_effective_range_m: Some(10_000.0),
            ..Default::default()
        })
        .await
        .unwrap();
    handle.step_manual(1.0).await.unwrap();
    let vic_notifs_items = ctx.drain_notifications_for_char(victim_char);
    assert!(
        vic_notifs_items.iter().any(|n| n.event == "OnDamageMessage"),
        "victim must receive OnDamageMessage from NPC cycle via db.items owner_id fallback"
    );
}

#[tokio::test]
async fn issue_818_target_lost_notifications_in_step_manual_and_try_step_full_tick() {
    let ctx = ServerContext::new();
    let (att_char, vic_char, att_ship, vic_ship, sys_id) = (90_000_001_i64, 90_000_002_i64, 200_410_001_i64, 200_410_002_i64, 30_000_142_u32);
    let handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, x, cid_opt) in [(att_ship, 0.0_f64, Some(att_char)), (vic_ship, 5_000.0_f64, Some(vic_char))] {
        handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: bid, type_id: 587, position: glam::DVec3::new(x, 0.0, 0.0), velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0, speed_fraction: 0.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::new(),
                eve_destiny::SlimItemMeta {
                    group_id: 25, category_id: 6, owner_id: cid_opt.unwrap_or(1), char_id: cid_opt, corp_id: 1_000_009,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // 1. `step_manual` (`system_actor.rs`): out-of-range lock drops (`mode == "lost"`),
    //    delivering `OnTarget(vic_ship, "lost")` + `OnMultiEvent(("OnTarget", "lost", vic_ship, ""))`
    //    to `att_char` and `OnMultiEvent(("OnTarget", "otherlost", att_ship, ""))` to `vic_char`.
    {
        let mut bp = handle.ballpark.write().unwrap();
        bp.target_locks.insert(
            att_ship,
            vec![eve_destiny::TargetLock {
                target_id: vic_ship, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 1_000.0, max_range_m: Some(1_000.0),
            }],
        );
    }
    let _ = ctx.drain_notifications_for_char(att_char);
    let _ = ctx.drain_notifications_for_char(vic_char);
    handle.step_manual(1.0).await.unwrap();

    let att_notifs = ctx.drain_notifications_for_char(att_char);
    let on_tgt = att_notifs.iter().find(|n| n.event == "OnTarget").expect("attacker must receive OnTarget(target_id, 'lost')");
    assert_eq!(on_tgt.args, vec![PyRep::Integer(vic_ship), PyRep::String("lost".into())]);
    let att_multi = att_notifs.iter().find(|n| n.event == "OnMultiEvent").expect("attacker must receive OnMultiEvent for lost target");
    assert_eq!(
        att_multi.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()), PyRep::String("lost".into()),
                PyRep::Integer(vic_ship), PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );

    let vic_notifs = ctx.drain_notifications_for_char(vic_char);
    let vic_multi = vic_notifs.iter().find(|n| n.event == "OnMultiEvent").expect("victim must receive OnMultiEvent('otherlost', source_id, '')");
    assert_eq!(
        vic_multi.args,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()), PyRep::String("otherlost".into()),
                PyRep::Integer(att_ship), PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );

    // 2. `try_step_full_tick` (`cycles.rs`) + `db.characters` / `db.items` pilot resolution fallbacks
    //    + `mode != "lost"` ignored + self-lock `target_char_id == source_char_id` filtered out.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&att_char) { ch.active_ship_id = att_ship; }
        if let Some(ch) = snap.characters.get_mut(&vic_char) { ch.active_ship_id = 0; }
        snap.items.insert(
            vic_ship,
            eve_db::InventoryItemRecord {
                item_id: vic_ship, type_id: 587, owner_id: vic_char, location_id: i64::from(sys_id),
                flag: eve_db::InventoryFlag::None, quantity: -1, singleton: true, version: 1, ..Default::default()
            },
        );
    });
    {
        let mut bp = handle.ballpark.write().unwrap();
        if let Some(m) = bp.ball_slim_meta.get_mut(&att_ship) { m.char_id = None; }
        if let Some(m) = bp.ball_slim_meta.get_mut(&vic_ship) { m.char_id = None; }
        bp.target_locks.insert(
            att_ship,
            vec![eve_destiny::TargetLock {
                target_id: vic_ship, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 1_000.0, max_range_m: Some(1_000.0),
            }],
        );
    }
    handle.try_step_full_tick(1.0);
    assert!(!ctx.drain_notifications_for_char(att_char).is_empty());
    assert!(!ctx.drain_notifications_for_char(vic_char).is_empty());

    // Direct helper boundary checks: `pending_opt = None`, `db_opt = None`, `mode = "locked"`,
    // `char_id <= 0`, and `target_char_id == source_char_id`.
    let bp_snap = handle.ballpark.read().unwrap().clone();
    crate::system_actor::flush_target_lost_notifications(
        &bp_snap,
        &[eve_destiny::DestinyEvent::OnTarget { mode: "lost".into(), source_id: att_ship, target_id: vic_ship }],
        None, None, None,
    );

    let mut bp_custom = eve_destiny::Ballpark::new(sys_id);
    bp_custom.ball_slim_meta.insert(att_ship, eve_destiny::SlimItemMeta { char_id: Some(att_char), ..Default::default() });
    bp_custom.ball_slim_meta.insert(vic_ship, eve_destiny::SlimItemMeta { char_id: Some(0), ..Default::default() });
    let pending_map = std::sync::RwLock::new(
        std::collections::HashMap::<i64, Vec<crate::router::PendingMachoNotification>>::new(),
    );
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    crate::system_actor::flush_target_lost_notifications(
        &bp_custom,
        &[
            eve_destiny::DestinyEvent::OnTarget { mode: "locked".into(), source_id: att_ship, target_id: vic_ship },
            eve_destiny::DestinyEvent::OnTarget { mode: "lost".into(), source_id: att_ship, target_id: att_ship },
            eve_destiny::DestinyEvent::OnTarget { mode: "lost".into(), source_id: vic_ship, target_id: att_ship },
        ],
        None, Some(&pending_map), Some(&tx),
    );
    let map_guard = pending_map.read().unwrap();
    let att_q = map_guard.get(&att_char).expect("att_char notifications");
    assert_eq!(att_q.len(), 3);
    assert_eq!(att_q[0].created_filetime, 133_500_000_000_000_000);
    assert!(!map_guard.contains_key(&0));
    assert!(rx.try_recv().is_ok());
}

#[test]
fn compute_required_hypercores_and_title_bitmask_role_member_boundaries() {
    use crate::services::{
        corp_registry_ops::handle_get_member,
        fleet_social::fleet_social_corp_tail::title_mask_grants_roles,
        hypernet_raffle_ops::compute_required_hypercores,
    };
    use eve_db::snapshot_ext::{title_id_to_bit, CorpTitleEntry};

    // 1. `compute_required_hypercores` (`#787`): exact `.floor()` & `.clamp(1.0, ...)` boundaries
    for (val, tickets, exp) in [
        (1.0, 8, 1), (6_999_999.0, 16, 1), (7_000_000.0, 48, 1), (13_999_999.0, 512, 1),
        (14_000_000.0, 8, 2), (20_999_999.0, 16, 2), (21_000_000.0, 48, 3),
    ] {
        assert_eq!(compute_required_hypercores(val, tickets).unwrap(), exp);
    }
    for bad_tickets in [0_u32, 1, 7, 9, 32, 513] {
        assert_eq!(compute_required_hypercores(14_000_000.0, bad_tickets).unwrap_err().code, "InvalidRaffleTicketCount");
    }
    for bad_val in [0.0_f64, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(compute_required_hypercores(bad_val, 8).unwrap_err().code, "InvalidRafflePrice");
    }

    // 2. `title_id_to_bit` (`#787`): <= 0, powers of two, 1..=64 non-power-of-two, and > 64 non-power-of-two
    for (tid, exp_bit) in [
        (0_i64, 0_u64), (-1, 0), (i64::MIN, 0), (1_i64 << 40, 1_u64 << 40),
        (3, 4), (5, 16), (63, 1_u64 << 62), (64, 64), (65, 0), (100, 0),
    ] {
        assert_eq!(title_id_to_bit(tid), exp_bit);
    }
    for shift in 0..16 {
        let bit = 1_i64 << shift;
        assert_eq!(title_id_to_bit(bit), bit as u64);
    }

    // 3. `title_mask_grants_roles`: tmask == 0, g.titles (0 vs non-zero), and each of the 5 CorpTitleEntry vectors
    let ctx = ServerContext::new();
    let (corp_id, ceo_id, member_id) = (98_880_001_i64, 90_000_001_i64, 90_000_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        assert!(!title_mask_grants_roles(snap, corp_id, 0));
        {
            let g = snap.corp_governance.entry(corp_id).or_default();
            g.titles.insert(1, ("NoRoles".into(), 0));
            g.titles.insert(2, ("HasRoles".into(), 128));
        }
        assert!(!title_mask_grants_roles(snap, corp_id, 1));
        assert!(title_mask_grants_roles(snap, corp_id, 2));
        snap.corp_governance.entry(corp_id).or_default().titles.clear();

        for vec_idx in 0..5 {
            let mut entry = CorpTitleEntry { title_id: 4, name: format!("TitleVec{vec_idx}"), ..Default::default() };
            snap.corp_titles.insert(corp_id, vec![entry.clone()]);
            assert!(!title_mask_grants_roles(snap, corp_id, 4));
            match vec_idx {
                0 => entry.roles.push("roleA".into()),
                1 => entry.grantable_roles.push("roleB".into()),
                2 => entry.roles_at_hq.push("roleC".into()),
                3 => entry.roles_at_base.push("roleD".into()),
                _ => entry.roles_at_other.push("roleE".into()),
            }
            snap.corp_titles.insert(corp_id, vec![entry]);
            assert!(title_mask_grants_roles(snap, corp_id, 4));
            assert!(!title_mask_grants_roles(snap, corp_id, 8));
        }

        // 4. Seed player corp + member titles for `handle_get_member` (`#781`, `#782`, `#787`)
        let mut c = CorporationRecord::new(corp_id, "Title Bitmask Corp", "TBMK", ceo_id);
        c.members = [ceo_id, member_id].into_iter().collect();
        snap.corporations.insert(corp_id, c);
        if let Some(ch) = snap.characters.get_mut(&member_id) { ch.corporation_id = corp_id; }
        let g = snap.corp_governance.entry(corp_id).or_default();
        g.titles.insert(1, ("Beta".into(), 0));
        g.titles.insert(2, ("Alpha".into(), 0));
        g.titles.insert(4, (String::new(), 0)); // empty title name must be skipped
        g.member_titles.insert(member_id, 1 | 2 | 4);
    });

    let mut sess = SessionState::new(7870);
    sess.user_id = Some(1);
    sess.character_id = Some(member_id);
    sess.corporation_id = Some(corp_id);
    let member_rep = handle_get_member(&ctx, &mut sess, &[PyRep::Integer(member_id)]).expect("handled").expect("player corp member ok");
    assert_eq!(member_rep.dict_get("title"), Some(&PyRep::String("Beta, Alpha".into())));
    assert_eq!(member_rep.dict_get("titleMask"), Some(&PyRep::Integer(7)));
    assert_eq!(member_rep.dict_get("blockRoles"), Some(&PyRep::Integer(0)));
    assert_eq!(member_rep.dict_get("blocksRoles"), Some(&PyRep::Integer(0)));

    let npc_corp_id = ctx
        .sde
        .npc_corporations
        .keys()
        .copied()
        .min()
        .expect("SDE NPC corp");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) { ch.corporation_id = npc_corp_id; }
    });
    let mut npc_sess = SessionState::new(7871);
    npc_sess.user_id = Some(1);
    npc_sess.character_id = Some(ceo_id);
    npc_sess.corporation_id = Some(npc_corp_id);
    let npc_rep = handle_get_member(&ctx, &mut npc_sess, &[PyRep::Integer(ceo_id)]).expect("handled").expect("npc corp member ok");
    assert_eq!(npc_rep.dict_get("corporationID"), Some(&PyRep::Integer(npc_corp_id)));
    assert_eq!(npc_rep.dict_get("titleMask"), Some(&PyRep::Integer(0)));
    assert_eq!(npc_rep.dict_get("blockRoles"), Some(&PyRep::Integer(0)));
    assert_eq!(npc_rep.dict_get("blocksRoles"), Some(&PyRep::Integer(0)));
}

#[tokio::test]
async fn reprocess_ship_ore_hold_flag_resolution_and_place_multi_sell_tombstones() {
    use crate::services::{
        market_contracts::{JITA_STATION_ID, THE_FORGE_REGION_ID},
        market_trading::handle_orders_escrow,
        space_entity_mining::handle as handle_mining,
    };
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};

    // 1. `normalize_macho_rpc_reply("reprocessingSvc", "Reprocess", ...)` (`#784`, `#788`):
    let multi_reproc = PyRep::List(vec![
        PyRep::keyval([
            ("itemID", PyRep::Integer(990_001)),
            ("typeID", PyRep::Integer(1230)),
            ("reprocessedQuantity", PyRep::Integer(100)),
            ("outputs", PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(200)), ("unrecoverable", PyRep::Integer(15))])])),
        ]),
        PyRep::keyval([
            ("itemID", PyRep::Integer(990_002)),
            ("typeID", PyRep::Integer(1230)),
            ("reprocessedQuantity", PyRep::Integer(200)),
            ("outputs", PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(400)), ("unrecoverable", PyRep::Integer(30))])])),
        ]),
    ]);
    let PyRep::Tuple(reproc_tup) = normalize_macho_rpc_reply("reprocessingSvc", "Reprocess", Ok(multi_reproc)).unwrap() else {
        panic!("expected 2-tuple (input_ids, outputs_by_type) from Reprocess");
    };
    assert_eq!(reproc_tup.len(), 2);
    assert_eq!(reproc_tup[0], PyRep::List(vec![PyRep::Integer(990_001), PyRep::Integer(990_002)]));
    assert_eq!(reproc_tup[1], PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(600))]));

    // 2. `space_entity_mining::handle("reprocessingSvc", "Reprocess", ...)` (`#784`, `#788`):
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 7840, Some(char_id), false, ctx.now_unix());
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let station_id = JITA_STATION_ID;
    let (ship_id, ore_partial_id, ore_full_id) = (990_784_001_i64, 990_784_002_i64, 990_784_003_i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.items.retain(|_, it| it.owner_id != char_id || it.location_id != station_id);
        for (iid, tid, loc, flg, qty, sgl) in [
            (ship_id, 32_880_u32, station_id, InventoryFlag::Hangar, 1_i32, true),
            (ore_partial_id, 1230, ship_id, InventoryFlag::SpecializedOreHold, 150, false),
            (ore_full_id, 1230, ship_id, InventoryFlag::SpecializedOreHold, 100, false),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: char_id, location_id: loc, flag: flg, quantity: qty, singleton: sgl, version: 1, ..Default::default()
            });
        }
    });

    let mut sess = SessionState::new(7840);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(THE_FORGE_REGION_ID);
    sess.ship_id = Some(ship_id);

    let bad_from = handle_mining(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![PyRep::Integer(ore_partial_id)]), PyRep::Integer(ship_id + 999)], None).await.unwrap().unwrap_err();
    assert_eq!(bad_from.code, "OwnershipOrLocationViolation");

    let _ = ctx.drain_notifications_for_char(char_id);
    let PyRep::List(rep_part_list) = handle_mining(
        &ctx, &mut sess, "reprocessingSvc", "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(ore_partial_id)]), PyRep::Integer(ship_id), PyRep::None, PyRep::Integer(InventoryFlag::SpecializedOreHold as i64)],
        None,
    ).await.unwrap().expect("partial ore hold reprocess ok") else {
        panic!("expected List from multi-arg Reprocess");
    };
    assert_eq!(rep_part_list[0].dict_get("remainderQuantity"), Some(&PyRep::Integer(50)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ore_partial_id].quantity, 50);
        assert!(snap.items.values().any(|it| it.owner_id == char_id && it.type_id == 34 && it.location_id == station_id && it.flag == InventoryFlag::Hangar));
    });
    let part_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(part_notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(part_notifs.iter().any(|n| n.event == "OnItemChange" && n.args[0].dict_get("itemID") == Some(&PyRep::Integer(ore_partial_id)) && n.args[0].dict_get("quantity") == Some(&PyRep::Integer(50))));

    let _ = ctx.drain_notifications_for_char(char_id);
    let PyRep::List(rep_full_list) = handle_mining(
        &ctx, &mut sess, "reprocessingSvc", "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(ore_full_id)]), PyRep::Integer(ship_id), PyRep::None, PyRep::Integer(InventoryFlag::SpecializedOreHold as i64)],
        None,
    ).await.unwrap().expect("full ore hold reprocess ok") else {
        panic!("expected List from multi-arg Reprocess");
    };
    assert_eq!(rep_full_list[0].dict_get("remainderQuantity"), Some(&PyRep::Integer(0)));
    let full_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(full_notifs.iter().any(|n| n.event == "OnItemChange" && n.args[0].dict_get("itemID") == Some(&PyRep::Integer(ore_full_id)) && n.args[0].dict_get("locationID") == Some(&PyRep::Integer(6))));

    // 3. `PlaceMultiSellOrder` full-stack vs partial-stack `OnItemChange` tombstones (`#781`, `#785`, `#786`):
    let (sell_stack_id, sell_single_id, sell_part_id) = ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, qty, sgl) in [(990_785_001_i64, 34_u32, 50_i32, false), (990_785_002, 587, 1, true), (990_785_003, 34, 20, false)] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: char_id, location_id: station_id, flag: InventoryFlag::Hangar, quantity: qty, singleton: sgl, version: 1, ..Default::default()
            });
        }
        (990_785_001_i64, 990_785_002_i64, 990_785_003_i64)
    });

    let _ = ctx.drain_notifications_for_char(char_id);
    let mk_sell = |iid: i64, tid: i64, price: f64, qty: i64| PyRep::keyval([
        ("itemID", PyRep::Integer(iid)), ("typeID", PyRep::Integer(tid)), ("price", PyRep::Real(price)),
        ("quantity", PyRep::Integer(qty)), ("stationID", PyRep::Integer(station_id)),
    ]);
    let assembled_err = handle_orders_escrow(
        &ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder",
        &[PyRep::List(vec![mk_sell(sell_single_id, 587, 250_000.0, 1)]), PyRep::Bool(false), PyRep::Integer(14)],
        &HashMap::new(),
    ).unwrap().unwrap_err();
    assert_eq!(assembled_err.code, "CannotSellAssembledItem");

    let sell_entries = PyRep::List(vec![mk_sell(sell_stack_id, 34, 10.0, 50), mk_sell(sell_part_id, 34, 10.0, 8)]);
    handle_orders_escrow(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[sell_entries, PyRep::Bool(false), PyRep::Integer(14)], &HashMap::new())
        .unwrap()
        .expect("PlaceMultiSellOrder ok");

    let sell_notifs = ctx.drain_notifications_for_char(char_id);
    let change_get = |rep: &PyRep, k: i64| -> Option<i64> {
        let PyRep::Dict(entries) = rep else { return None };
        entries.iter().find(|(dk, _)| dk.as_i64() == Some(k)).and_then(|(_, v)| v.as_i64())
    };
    let stack_del = sell_notifs.iter().find(|n| n.event == "OnItemChange" && n.args[0].dict_get("itemID") == Some(&PyRep::Integer(sell_stack_id))).unwrap();
    assert_eq!(stack_del.args[0].dict_get("locationID"), Some(&PyRep::Integer(6)));
    assert_eq!(stack_del.args[0].dict_get("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(stack_del.args[0].dict_get("quantity"), Some(&PyRep::Integer(0)));
    assert_eq!((change_get(&stack_del.args[1], 3), change_get(&stack_del.args[1], 4), change_get(&stack_del.args[1], 9), change_get(&stack_del.args[1], 10)), (Some(station_id), Some(4), Some(50), None));

    let part_upd = sell_notifs.iter().find(|n| n.event == "OnItemChange" && n.args[0].dict_get("itemID") == Some(&PyRep::Integer(sell_part_id))).unwrap();
    assert_eq!(part_upd.args[0].dict_get("locationID"), Some(&PyRep::Integer(station_id)));
    assert_eq!(part_upd.args[0].dict_get("quantity"), Some(&PyRep::Integer(12)));
    assert_eq!((change_get(&part_upd.args[1], 9), change_get(&part_upd.args[1], 3)), (Some(20), None));
}

#[tokio::test]
async fn agent_mgr_implicit_greeting_vs_explicit_offer_and_client_compat_states() {
    use crate::router::BoundObjectEntry;
    use crate::services::agents_missions::handle_with_bound;
    use eve_db::ActiveMissionRecord;

    // 1. `client_compat::normalize_macho_rpc_reply("agentMgr", ...)` state & fallback branches (`#796`, `#797`)
    for (state_str, m_state, exp_comp, exp_dec, exp_quit) in [
        ("Accepted", 2_i64, false, false, false),
        ("Completed", 4_i64, true, false, false),
        ("Declined", 5_i64, false, true, false),
        ("Quit", 6_i64, false, false, true),
        ("Refused", 0_i64, false, false, false),
    ] {
        let rep = normalize_macho_rpc_reply(
            "agentMgr", "DoAction",
            Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(901)),
                ("state", PyRep::String(state_str.into())),
                ("missionState", PyRep::Integer(m_state)),
                ("dialogue", PyRep::List(vec![PyRep::Integer(2), PyRep::Tuple(vec![PyRep::Integer(6), PyRep::String("Complete".into())])])),
            ])),
        ).unwrap();
        let PyRep::Tuple(outer) = rep else { panic!("expected 2-tuple for state={state_str}") };
        let PyRep::Tuple(inner) = &outer[0] else { panic!("expected inner tuple") };
        assert_eq!(inner[1], PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(2)]), PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(6)])]));
        assert_eq!(outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(exp_comp)));
        assert_eq!(outer[1].dict_get("missionDeclined"), Some(&PyRep::Bool(exp_dec)));
        assert_eq!(outer[1].dict_get("missionQuit"), Some(&PyRep::Bool(exp_quit)));
    }

    let loc_space = normalize_macho_rpc_reply("agentMgr", "GetAgentLocationWrap", Ok(PyRep::keyval([("agentID", PyRep::Integer(3_018_910)), ("solarSystemID", PyRep::Integer(30_000_142))]))).unwrap();
    assert_eq!(loc_space.dict_get("locationID"), Some(&PyRep::Integer(30_000_142)));

    // 2. `handle_with_bound("agentMgr", "DoAction", &[])` (`is_implicit_greeting == true`) vs explicit `DoAction([2])` (`#796`, `#797`):
    let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();

    let l2_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 2 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .unwrap();
    let (l2_sys, l2_st) = (l2_ag.solar_system_id.unwrap(), l2_ag.station_id.unwrap());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = l2_sys;
            ch.station_id = Some(l2_st);
            ch.standings.clear();
            ch.standings.insert(l2_ag.agent_id, 0.5);
        }
    });
    let mut sess = SessionState::new(7960);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(l2_sys);
    sess.station_id = Some(l2_st);
    let sess_id = sess.session_id;
    let make_bound = |aid: i64| BoundObjectEntry {
        bound_id: 1, session_id: sess_id, user_id: 1, character_id: char_id,
        service: "agentMgr".into(), object_id: aid, oid_str: format!("agentMgr:{aid}"), ref_id: 1,
    };

    let bound_l2 = make_bound(l2_ag.agent_id);
    let greet_l2 = handle_with_bound(&ctx, &mut sess, Some(&bound_l2), "agentMgr", "DoAction", &[], &kw).await.unwrap().unwrap();
    assert_eq!(greet_l2.dict_get("state"), Some(&PyRep::String("Refused".into())));
    assert_eq!(greet_l2.dict_get("missionState"), Some(&PyRep::Integer(0)));
    assert_eq!(greet_l2.dict_get("dialogue"), Some(&PyRep::List(Vec::new())));

    let explicit_l2_err = handle_with_bound(&ctx, &mut sess, Some(&bound_l2), "agentMgr", "DoAction", &[PyRep::Integer(2)], &kw).await.unwrap().unwrap_err();
    assert_eq!(explicit_l2_err.code, "InsufficientAgentStanding");

    let l1_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .unwrap();
    let (l1_sys, l1_st) = (l1_ag.solar_system_id.unwrap(), l1_ag.station_id.unwrap());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = l1_sys;
            ch.station_id = Some(l1_st);
            ch.standings.clear();
            ch.standings.insert(l1_ag.agent_id, -2.0);
        }
    });
    sess.solar_system_id = Some(l1_sys);
    sess.station_id = Some(l1_st);
    let bound_l1 = make_bound(l1_ag.agent_id);
    let greet_l1 = handle_with_bound(&ctx, &mut sess, Some(&bound_l1), "agentMgr", "DoAction", &[], &kw).await.unwrap().unwrap();
    assert_eq!(greet_l1.dict_get("state"), Some(&PyRep::String("Refused".into())));

    let mid = ctx.sde.missions.keys().copied().min().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.clear(); }
        snap.active_missions.insert(
            (char_id, mid),
            ActiveMissionRecord {
                mission_id: mid, character_id: char_id, agent_id: l1_ag.agent_id, solar_system_id: l1_sys,
                state: "Accepted".into(), current_room_id: 1, npcs_remaining: 1, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
            },
        );
    });
    let greet_acc = handle_with_bound(&ctx, &mut sess, Some(&bound_l1), "agentMgr", "DoAction", &[], &kw).await.unwrap().unwrap();
    assert_eq!(greet_acc.dict_get("state"), Some(&PyRep::String("Accepted".into())));
    assert_eq!(greet_acc.dict_get("missionState"), Some(&PyRep::Integer(2)));
    assert_eq!(greet_acc.dict_get("missionID"), Some(&PyRep::Integer(mid as i64)));

    if let Some(rnd_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let (r_sys, r_st) = (rnd_ag.solar_system_id.unwrap(), rnd_ag.station_id.unwrap());
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.clear();
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = r_sys;
                ch.station_id = Some(r_st);
                ch.standings.insert(rnd_ag.agent_id, 10.0);
                ch.standings.insert(rnd_ag.corporation_id, 10.0);
            }
        });
        sess.solar_system_id = Some(r_sys);
        sess.station_id = Some(r_st);
        let bound_rnd = make_bound(rnd_ag.agent_id);
        let greet_rnd = handle_with_bound(&ctx, &mut sess, Some(&bound_rnd), "agentMgr", "DoAction", &[], &kw).await.unwrap().unwrap();
        assert_eq!(greet_rnd.dict_get("state"), Some(&PyRep::String("Greeting".into())));
        assert_eq!(greet_rnd.dict_get("missionState"), Some(&PyRep::Integer(0)));
        let Some(PyRep::List(dlg)) = greet_rnd.dict_get("dialogue") else { panic!("expected dialogue list") };
        assert!(dlg.contains(&PyRep::Tuple(vec![PyRep::Integer(12), PyRep::String("Start Research".into())])));
    }
}
