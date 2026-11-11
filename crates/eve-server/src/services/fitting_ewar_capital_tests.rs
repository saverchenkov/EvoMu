//! Unit tests for `fitting_ewar_capital` (`ewar_jamming_type`, `emit_committed_ewar_and_system_wide_notifications`, `prepare_ewar_and_capital_module`).

use super::*;
use eve_db::{AccountRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn synth_mod_type(type_id: u32, group_id: u32, name: &str, effects: Vec<u16>) -> InvType {
    let mut t = shared_sde().types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = 7;
    t.name = name.into();
    t.default_attributes.clear();
    t.effect_ids = effects;
    t
}

#[test]
fn ewar_jamming_type_and_notifications_cover_all_ewar_groups() {
    let scram = synth_mod_type(448, 52, "Warp Scrambler II", vec![]);
    assert_eq!(ewar_jamming_type(&scram, None, true, None), Some("warpScramblerMWD"));
    assert_eq!(ewar_jamming_type(&scram, None, false, None), Some("warpScrambler"));

    let hic = synth_mod_type(16242, 899, "Warp Disruption Field Generator I", vec![]);
    assert_eq!(ewar_jamming_type(&hic, Some(45010), false, None), Some("warpScramblerMWD"));
    assert_eq!(ewar_jamming_type(&hic, Some(28661), false, None), Some("focusedWarpScrambler"));
    assert_eq!(ewar_jamming_type(&hic, None, false, Some(20_000.0)), None);

    let web = synth_mod_type(526, 65, "Stasis Webifier I", vec![]);
    assert_eq!(ewar_jamming_type(&web, None, false, None), Some("webify"));

    let ecm = synth_mod_type(1952, 201, "Multispectral ECM I", vec![]);
    assert_eq!(ewar_jamming_type(&ecm, None, false, None), Some("electronic"));

    let damp = synth_mod_type(1968, 208, "Remote Sensor Dampener I", vec![]);
    assert_eq!(ewar_jamming_type(&damp, None, false, None), Some("ewRemoteSensorDamp"));

    let td = synth_mod_type(2108, 291, "Tracking Disruptor I", vec![]);
    assert_eq!(ewar_jamming_type(&td, None, false, None), Some("ewTrackingDisrupt"));

    let gd = synth_mod_type(37540, 291, "Guidance Disruptor I", vec![6425]);
    assert_eq!(ewar_jamming_type(&gd, None, false, None), Some("ewGuidanceDisrupt"));

    let tp = synth_mod_type(12709, 379, "Target Painter I", vec![]);
    assert_eq!(ewar_jamming_type(&tp, None, false, None), Some("ewTargetPaint"));

    let neut = synth_mod_type(533, 71, "Heavy Energy Neutralizer I", vec![]);
    assert_eq!(ewar_jamming_type(&neut, None, false, None), Some("ewEnergyNeut"));

    let nos = synth_mod_type(530, 68, "Heavy Energy Nosferatu I", vec![]);
    assert_eq!(ewar_jamming_type(&nos, None, false, None), Some("ewEnergyVampire"));

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_100_001i64;
    let target_owner = 96_100_002i64;
    let ship_id = 96_100_100i64;
    let target_ship_id = 96_100_200i64;
    let module_id = 96_100_300i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for (uid, cid) in [(96_101u64, char_id), (96_102, target_owner)] {
            snap.accounts.insert(
                format!("user_{uid}"),
                AccountRecord {
                    user_id: uid,
                    username: format!("user_{uid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = format!("Pilot{uid}");
            ch.corporation_id = 1_000_009;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = if cid == char_id { ship_id } else { target_ship_id };
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: target_owner,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.db.acquire_online_lock(96_101, 1, Some(char_id), true, 100);
    let _ = ctx.db.acquire_online_lock(96_102, 2, Some(target_owner), true, 100);

    let phenomena = synth_mod_type(43554, 1815, "Amarr Phenomena Generator", vec![6753]);
    emit_committed_ewar_and_system_wide_notifications(
        &ctx,
        30_000_142,
        char_id,
        ship_id,
        module_id,
        43554,
        &phenomena,
        None,
        false,
        None,
        None,
    );

    emit_committed_ewar_and_system_wide_notifications(
        &ctx,
        30_000_142,
        char_id,
        ship_id,
        module_id,
        448,
        &scram,
        None,
        true,
        None,
        Some(target_ship_id),
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnSystemWideEffectStart"));
    assert!(notifs.iter().any(|n| n.event == "OnEwarStart"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_ewar_and_capital_module_validates_hull_groups_and_prepares_adc_and_nullifier() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 96_100_010i64;
    let ship_id = 96_100_110i64;
    let module_id = 96_100_310i64;

    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let _ = fit.fit_module_with_state(module_id, 47254, 11, true, &ctx.sde);
    let fm = fit.modules.get(&module_id).cloned().unwrap_or(eve_dogma::FittedModule {
        item_id: module_id,
        type_id: 47254,
        flag_id: 11,
        state: eve_dogma::ModuleState::Online,
        charge_type_id: None,
        charge_quantity: 0,
        damage_hp: 0.0,
        max_hp: 40.0,
        dynamic_attribute_overrides: HashMap::new(),
    });
    let adc_type = synth_mod_type(47254, 60, "Assault Damage Control I", vec![7012]);
    let mod_state = fit.build_effective_module_state(module_id, 47254, false, &ctx.sde);

    // Non-assault frigate (group 25) is rejected for ADC
    let err = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        module_id,
        &fm,
        &adc_type,
        &mod_state,
        &fit,
        None,
        false,
        false,
        false,
        &HashMap::new(),
    )
    .await
    .expect("Some")
    .err()
    .expect("ADC rejected on group 25");
    assert_eq!(err.code, "InvalidShipGroup");

    // Assault Frigate (group 324) succeeds with is_adc = true
    let prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        324,
        module_id,
        &fm,
        &adc_type,
        &mod_state,
        &fit,
        None,
        false,
        false,
        false,
        &HashMap::new(),
    )
    .await
    .expect("Some")
    .expect("ADC accepted on group 324");
    assert!(prep.is_adc);

    // Interdiction Nullifier (group 4117) on Interceptor (group 831) succeeds
    let nullifier_type = synth_mod_type(59600, 4117, "Compact Interdiction Nullifier", vec![]);
    let null_prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        831,
        module_id,
        &fm,
        &nullifier_type,
        &mod_state,
        &fit,
        None,
        false,
        false,
        false,
        &HashMap::new(),
    )
    .await
    .expect("Some")
    .expect("Nullifier accepted on group 831");
    assert!(null_prep.grants_interdiction_nullification);
}

#[cfg(test)]
#[path = "fitting_ewar_capital_extra_unit_tests.rs"]
mod extra_unit_tests;
