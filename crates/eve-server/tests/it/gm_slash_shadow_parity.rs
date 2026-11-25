//! Wire-path `ClientHarness` (`:26000`) & `ShadowClient` parity tests for GM slash commands
//! (`#1193`, `#949`, `#1137`, `#1138`).
//!
//! Track 1 tests live wire/dispatcher behavior against unchanged `main` handlers. Any family
//! defect uncovered (`gm_fit_updates_shadow_client_over_wire`, `gm_tr_updates_shadow_client_over_wire`)
//! belongs to that family's Track 2 (`#1193`) rather than Track 1 routing.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::service_roles;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT: i64 = 90_000_001;
const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const SKILL_MINMATAR_CRUISER: u32 = 3333;
const MODULE_DCU_II: u32 = 2048;

async fn prepare_undocked_gm_wire_harness<'c>(
    ctx: &'c ServerContext,
    session_id: u64,
) -> (
    eve_testkit::client_harness::ClientHarness<common::ServerWireTransport<'c>>,
    i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acct) = snap.accounts.get_mut("demo") {
            acct.role = u64::MAX;
        }
    });
    let kw = HashMap::new();
    let mut harness = common::new_wire_harness(ctx, session_id).with_shadow_enforcement(true);

    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT)],
            &kw,
        )
        .await;
    let cash_rep = harness
        .call_ok("account", None, "GetCashBalance", &[], &kw)
        .await;
    assert!(
        cash_rep.as_f64().is_some(),
        "GetCashBalance should return numeric balance: {cash_rep:?}"
    );
    let dogma_info = harness
        .call_ok("dogmaIM", None, "GetAllInfo", &[], &kw)
        .await;
    assert!(
        !matches!(dogma_info, PyRep::None),
        "dogmaIM.GetAllInfo should return payload"
    );
    let skills_rep = harness
        .call_ok("skillMgr2", None, "GetSkills", &[], &kw)
        .await;
    assert!(
        !matches!(skills_rep, PyRep::None),
        "skillMgr2.GetSkills should return payload"
    );
    let undock_rep = harness
        .call_ok("stationSvc", None, "Undock", &[], &kw)
        .await;
    assert!(
        undock_rep.dict_get("beyonceBoundID").is_some(),
        "Undock should return beyonceBoundID: {undock_rep:?}"
    );
    let formations_rep = harness
        .call_ok("beyonce", None, "GetFormations", &[], &kw)
        .await;
    assert!(
        !matches!(formations_rep, PyRep::None),
        "GetFormations should return payload"
    );
    harness.assert_shadow_converged();

    let ship_id = harness
        .shadow()
        .and_then(|s| s.session.ship_id)
        .expect("undocked active ship_id in ShadowClient");
    (harness, ship_id)
}

#[tokio::test]
async fn gm_giveisk_and_giveskill_update_shadow_client_over_wire() {
    let ctx = ServerContext::new();
    let _guard = common::teardown_guard(&ctx);
    let kw = HashMap::new();
    let (mut harness, _ship_id) = prepare_undocked_gm_wire_harness(&ctx, 119_301).await;

    // 1. `/giveisk` updates ShadowWallet immediately without re-querying `GetCashBalance`.
    let pre_isk = harness
        .shadow()
        .and_then(|s| s.wallet.personal_balance_isk)
        .expect("wallet balance synced");
    let isk_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 2500000".into())],
            &kw,
        )
        .await;
    assert_eq!(isk_rep.dict_get("command"), Some(&PyRep::String("/giveisk 2500000".into())));
    harness.assert_shadow_converged();
    let post_isk = harness
        .shadow()
        .and_then(|s| s.wallet.personal_balance_isk)
        .expect("wallet balance after /giveisk");
    assert!(
        (post_isk - (pre_isk + 2_500_000.0)).abs() < 0.01,
        "ShadowWallet balance must increment by 2.5M ISK without GetCashBalance refresh: pre={pre_isk}, post={post_isk}"
    );

    let isk_me_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk me 500000".into())],
            &kw,
        )
        .await;
    assert_eq!(
        isk_me_rep.dict_get("command"),
        Some(&PyRep::String("/giveisk me 500000".into()))
    );
    harness.assert_shadow_converged();
    let post_me_isk = harness
        .shadow()
        .and_then(|s| s.wallet.personal_balance_isk)
        .expect("wallet balance after /giveisk me");
    assert!(
        (post_me_isk - (pre_isk + 3_000_000.0)).abs() < 0.01,
        "ShadowWallet balance must increment cumulatively on /giveisk me: pre={pre_isk}, post={post_me_isk}"
    );
    ctx.db.with_snapshot(|snap| {
        let j = snap
            .wallet_journal
            .last()
            .expect("GMGiveIsk wallet journal row persisted");
        assert_eq!(j.character_id, PILOT);
        assert_eq!(j.entry_type, "GMGiveIsk");
    });

    // 2. `/giveskill <tid> 5`, `/setskill me <tid> 4`, and `/giveskill <tid> 0` update
    //    ShadowClient skills via notifications (#1137, #1138, #1193).
    let grant_skill_cmd = format!("/giveskill {SKILL_MINMATAR_CRUISER} 5");
    let grant_skill_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(grant_skill_cmd.clone())],
            &kw,
        )
        .await;
    assert_eq!(
        grant_skill_rep.dict_get("command"),
        Some(&PyRep::String(grant_skill_cmd))
    );
    harness.assert_shadow_converged();
    assert_eq!(
        harness
            .shadow()
            .and_then(|s| s.skills.get(&SKILL_MINMATAR_CRUISER).map(|(lvl, _)| *lvl)),
        Some(5),
        "/giveskill level 5 must update ShadowClient skills without calling GetSkills again"
    );

    let set_skill_me_cmd = format!("/setskill me {SKILL_MINMATAR_CRUISER} 4");
    let set_skill_me_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(set_skill_me_cmd.clone())],
            &kw,
        )
        .await;
    assert_eq!(
        set_skill_me_rep.dict_get("command"),
        Some(&PyRep::String(set_skill_me_cmd))
    );
    harness.assert_shadow_converged();
    assert_eq!(
        harness
            .shadow()
            .and_then(|s| s.skills.get(&SKILL_MINMATAR_CRUISER).map(|(lvl, _)| *lvl)),
        Some(4),
        "/setskill me level 4 must downgrade ShadowClient skill level live"
    );

    let clear_skill_cmd = format!("/giveskill {SKILL_MINMATAR_CRUISER} 0");
    let clear_skill_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(clear_skill_cmd.clone())],
            &kw,
        )
        .await;
    assert_eq!(
        clear_skill_rep.dict_get("command"),
        Some(&PyRep::String(clear_skill_cmd))
    );
    harness.assert_shadow_converged();
    assert!(
        !harness
            .shadow()
            .unwrap()
            .skills
            .contains_key(&SKILL_MINMATAR_CRUISER),
        "/giveskill level 0 must remove skill from ShadowClient without calling GetSkills again"
    );

    // 3. Invalid typed parser inputs reject cleanly over wire without mutating shadow state.
    for (bad_cmd, expected_code) in [
        ("/giveisk -5", "InvalidSlashArgument"),
        ("/giveisk badtarget 100", "InvalidSlashArgument"),
        ("/giveskill 3300 6", "InvalidSlashArgument"),
        ("/giveskill 34 3", "InvalidSkillType"),
    ] {
        let err = harness
            .call(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(bad_cmd.into())],
                &kw,
            )
            .await
            .expect_err(&format!("expected {bad_cmd} to fail"));
        assert_eq!(err.code, expected_code, "for {bad_cmd}: {err:?}");
    }
    harness.assert_shadow_converged();
    let final_isk = harness
        .shadow()
        .and_then(|s| s.wallet.personal_balance_isk)
        .expect("wallet balance unchanged after rejected commands");
    assert!((final_isk - post_me_isk).abs() < 0.01);
}

/// Parity test staged for the `#1193` `/fit` family track (`character_gm_tools.rs:1544-1554` on
/// unchanged `main` emits `OnGodmaShipEffect(effect_id=16)` without priming `effectsByItemEffect`,
/// causing `ClientHarness` `UnprimedGodmaShipEffect` on `/fit me 2048`).
#[ignore = "live main /fit defect (UnprimedGodmaShipEffect); belongs to #1193 Track 2 /fit family"]
#[tokio::test]
async fn gm_fit_updates_shadow_client_over_wire() {
    let ctx = ServerContext::new();
    let _guard = common::teardown_guard(&ctx);
    let kw = HashMap::new();
    let (mut harness, ship_id) = prepare_undocked_gm_wire_harness(&ctx, 119_308).await;

    // `/fit me <type_id>` updates ShadowInventory + online module state without dock/undock or GetAllInfo refresh.
    let hull_skill_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3394 5".into())],
            &kw,
        )
        .await;
    assert_eq!(
        hull_skill_rep.dict_get("command"),
        Some(&PyRep::String("/giveskill 3394 5".into()))
    );
    harness.assert_shadow_converged();
    let fit_cmd = format!("/fit me {MODULE_DCU_II}");
    let fit_rep = harness
        .call_ok("slash", None, "SlashCmd", &[PyRep::String(fit_cmd)], &kw)
        .await;
    let fitted_item_id = fit_rep
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("fitted itemID");
    assert_eq!(
        fit_rep.dict_get("online"),
        Some(&PyRep::Bool(true)),
        "/fit me {MODULE_DCU_II} must bring module online after Hull Upgrades V"
    );
    harness.assert_shadow_converged();
    let shadow = harness.shadow().unwrap();
    let fitted_sh = shadow
        .inventory
        .items
        .get(&fitted_item_id)
        .expect("fitted module replicated to ShadowInventory immediately");
    assert_eq!(fitted_sh.location_id, ship_id);
    assert_eq!(fitted_sh.type_id, MODULE_DCU_II);
    assert_eq!(
        shadow.module_states.online_states.get(&fitted_item_id),
        Some(&true),
        "online /fit module must converge ShadowClient online_states via OnGodmaPrimeItem / OnGodmaShipEffect"
    );
}

#[tokio::test]
async fn gm_heal_updates_shadow_client_over_wire() {
    let ctx = ServerContext::new();
    let _guard = common::teardown_guard(&ctx);
    let kw = HashMap::new();
    let (mut harness, ship_id) = prepare_undocked_gm_wire_harness(&ctx, 119_309).await;

    // `/heal` repairs ship damage and refreshes ShadowClient vital attributes + damage state (#949).
    let setattr_hull_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/setattr me 3 25.0".into())],
            &kw,
        )
        .await;
    assert_eq!(
        setattr_hull_rep.dict_get("command"),
        Some(&PyRep::String("/setattr me 3 25.0".into()))
    );
    let setattr_armor_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/setattr me 266 40.0".into())],
            &kw,
        )
        .await;
    assert_eq!(
        setattr_armor_rep.dict_get("command"),
        Some(&PyRep::String("/setattr me 266 40.0".into()))
    );
    harness.assert_shadow_converged();
    assert_eq!(
        harness
            .shadow()
            .unwrap()
            .item_attributes
            .get(&(ship_id, attr_ids::DAMAGE)),
        Some(&25.0)
    );
    assert_eq!(
        harness
            .shadow()
            .unwrap()
            .item_attributes
            .get(&(ship_id, attr_ids::ARMOR_DAMAGE)),
        Some(&40.0)
    );

    let heal_rep = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/heal".into())],
            &kw,
        )
        .await;
    assert_eq!(heal_rep.dict_get("command"), Some(&PyRep::String("/heal".into())));
    assert_eq!(heal_rep.dict_get("healed"), Some(&PyRep::Bool(true)));
    harness.assert_shadow_converged();
    let sh_healed = harness.shadow().unwrap();
    assert_eq!(
        sh_healed.item_attributes.get(&(ship_id, attr_ids::DAMAGE)),
        Some(&0.0),
        "/heal must clear hull damage in ShadowClient without dock/undock"
    );
    assert_eq!(
        sh_healed.item_attributes.get(&(ship_id, attr_ids::ARMOR_DAMAGE)),
        Some(&0.0),
        "/heal must clear armor damage in ShadowClient without dock/undock"
    );
    assert!(
        sh_healed
            .item_attributes
            .get(&(ship_id, attr_ids::SHIELD_CHARGE))
            .is_some_and(|&v| v > 0.0),
        "/heal must synchronize positive shield charge attribute on ShadowClient"
    );
    let dmg_state = sh_healed
        .ballpark
        .damage_states
        .get(&ship_id)
        .expect("/heal must push OnDamageStateChange");
    assert!(
        (dmg_state.shield - 1.0).abs() < 1e-3
            && (dmg_state.armor - 1.0).abs() < 1e-3
            && (dmg_state.hull - 1.0).abs() < 1e-3,
        "full damage state expected after /heal: {dmg_state:?}"
    );
}

/// Parity test staged for the `#1193` `/tr` move family track (`auth_cache.rs:1922,2122-2133` on
/// unchanged `main` only emits ship `OnItemChange` when `old_dock_id.is_some()`, leaving
/// `ShadowInventory` ship `location_id` at Jita `30000142` on space-to-space `/tr me 30000144`).
#[ignore = "live main /tr defect (omits ship OnItemChange when old_dock_id is None); belongs to #1193 Track 2 /tr family"]
#[tokio::test]
async fn gm_tr_updates_shadow_client_over_wire() {
    let ctx = ServerContext::new();
    let _guard = common::teardown_guard(&ctx);
    let kw = HashMap::new();
    let (mut harness, ship_id) = prepare_undocked_gm_wire_harness(&ctx, 119_310).await;

    // `/tr me <dest_system>` moves session + ship item location seamlessly on wire without dock/undock refresh.
    let tr_cmd = format!("/tr me {PERIMETER}");
    let tr_rep = harness
        .call_ok("slash", None, "SlashCmd", &[PyRep::String(tr_cmd)], &kw)
        .await;
    assert_eq!(tr_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(PERIMETER))));
    harness.assert_shadow_converged();
    let sh_tr = harness.shadow().unwrap();
    assert_eq!(sh_tr.session.solar_system_id, Some(PERIMETER));
    assert_eq!(sh_tr.session.station_id, None);
    assert_eq!(
        sh_tr.inventory.items.get(&ship_id).map(|i| i.location_id),
        Some(i64::from(PERIMETER))
    );
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&PILOT).expect("pilot record");
        assert_eq!(ch.solar_system_id, PERIMETER);
        assert_ne!(ch.solar_system_id, JITA);
        assert_eq!(snap.items.get(&ship_id).map(|i| i.location_id), Some(i64::from(PERIMETER)));
    });
}

#[tokio::test]
async fn slash_role_gating_rejects_unprivileged_accounts_over_wire() {
    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acct) = snap.accounts.get_mut("demo") {
            acct.role = service_roles::ROLE_PLAYER;
        }
    });
    let _guard = common::teardown_guard(&ctx);
    let kw = HashMap::new();
    let mut harness = common::new_wire_harness(&ctx, 119_302);

    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT)],
            &kw,
        )
        .await;

    for restricted in ["/giveisk 100", "/heal", "/tr me 30000144", "/fit me 2048"] {
        let err = harness
            .call_exchange(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(restricted.into())],
                &kw,
            )
            .await
            .result
            .expect_err("ROLE_PLAYER must be rejected for GM slash commands");
        assert_eq!(err.code, "InsufficientRole", "unexpected error for {restricted}: {err:?}");
    }
}

#[tokio::test]
async fn slash_structure_case_bypass_rejected_and_createccpevent_role_parity() {
    let kw = HashMap::new();

    for (conn_id, role, cmd, expect_insufficient_role) in [
        (119_303, service_roles::ROLE_GMS, "/STRUCTURE state 1001 110", true),
        (119_304, service_roles::ROLE_GMS, "/structure state 1001 110", true),
        (119_305, service_roles::ROLE_WORLDMOD, "/STRUCTURE state 1001 110", true),
        (119_306, service_roles::ROLE_GML, "/createccpevent 1 test", true),
        (119_307, service_roles::ROLE_GMS, "/createccpevent", false),
    ] {
        let ctx = ServerContext::new();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(acct) = snap.accounts.get_mut("demo") {
                acct.role = role;
            }
        });
        let _guard = common::teardown_guard(&ctx);
        let mut harness = common::new_wire_harness(&ctx, conn_id);
        harness
            .call_ok(
                "authentication",
                None,
                "Login",
                &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
                &kw,
            )
            .await;
        harness
            .call_ok(
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(PILOT)],
                &kw,
            )
            .await;
        let res = harness
            .call_exchange("slash", None, "SlashCmd", &[PyRep::String(cmd.into())], &kw)
            .await
            .result;
        if expect_insufficient_role {
            let err = res.expect_err("expected InsufficientRole rejection");
            assert_eq!(err.code, "InsufficientRole", "cmd={cmd}, role={role}: {err:?}");
        } else if let Err(err) = res {
            assert_ne!(
                err.code, "InsufficientRole",
                "cmd={cmd}, role={role} must satisfy ROLE_HIGH_GM: {err:?}"
            );
        }
    }
}
