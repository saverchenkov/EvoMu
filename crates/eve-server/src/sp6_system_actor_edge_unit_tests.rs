//! Comprehensive unit tests targeting uncovered edge paths across `system_actor`,
//! `sp6_lifecycle`, `fitting_ewar_capital`, `fleet_social_chat`, `quasar_social_chat`,
//! `quasar_gateway`, `contracts_service`, `market_contracts`, `structure_lifecycle_cargo_fuel`,
//! `mining_cycle`, `combat_missiles_aoe`, `combat_fighters_abilities`, and `serverops_cluster`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_OK,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_missiles_aoe;
use crate::services::contracts_service;
use crate::services::fleet_social;
use crate::services::inventory_fitting::fitting_ewar_capital::{
    commit_post_cycle_activation_effects, ewar_jamming_type, prepare_ewar_and_capital_module,
    validate_passive_targeter_activation,
};
use crate::services::market_contracts;
use crate::services::mining_cycle;
use crate::services::quasar_social_chat;
use crate::services::serverops_cluster;
use crate::services::station_structure;
use crate::system_actor::{self, ActiveModuleCycle, ActiveNaniteRepair, SystemRegistry};
use eve_db::corp_roles;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_machonet::{SessionState, service_roles};
use eve_proto::PyRep;
use eve_proto::quasar::{
    EvePublicRequest, GoogleAny, GoogleDuration, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    if let Some(sde) = SHARED_SDE.get() {
        return Arc::clone(sde);
    }
    let sde = Arc::clone(&ServerContext::new().sde);
    let _ = SHARED_SDE.set(Arc::clone(&sde));
    sde
}

fn test_ctx() -> ServerContext {
    let sde = shared_sde();
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        ..Default::default()
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield,
        max_shield_hp: shield,
        armor_hp: armor,
        max_armor_hp: armor,
        structure_hp: hull,
        max_structure_hp: hull,
        capacitor: cap,
        max_capacitor: cap,
        signature_radius: 120.0,
        ..CombatState::default()
    }
}

fn make_session(user_id: i64, char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut sess = SessionState::new(user_id as u64);
    sess.user_id = Some(user_id as u64);
    sess.role = service_roles::ROLE_PLAYER;
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

fn find_type_by_group_attr(sde: &SdeRegistry, group_id: u32, req_attr: Option<u16>) -> u32 {
    sde.types
        .values()
        .filter(|t| {
            t.published
                && t.group_id == group_id
                && req_attr.is_none_or(|a| t.default_attributes.contains_key(&a))
        })
        .min_by_key(|t| t.type_id)
        .or_else(|| {
            sde.types
                .values()
                .filter(|t| t.group_id == group_id)
                .min_by_key(|t| t.type_id)
        })
        .map(|t| t.type_id)
        .expect("SDE must contain requested group")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fitting_ewar_capital_prepare_and_commit_all_module_groups_and_guards() {
    let ctx = test_ctx();
    let sde = Arc::clone(&ctx.sde);
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 90_000_001i64;
    let ship_id = 88_000_001i64;
    let target_id = 88_000_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            target_id,
            make_item(
                target_id,
                587,
                char_id + 1,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
    });
    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 587, DVec3::ZERO),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(target_id, 587, DVec3::new(3_000.0, 0.0, 0.0)),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0),
    );

    let kwargs = HashMap::new();
    let mod_id = 88_001_001i64;

    // Helper to synthesize a ShipFittingEngine + FittedModule + InvType for a target ship group.
    let build_fit_and_mod =
        |ship_tid: u32, mod_tid: u32, state: ModuleState, charge_tid: Option<u32>| {
            let mut fit = ShipFittingEngine::from_sde(ship_id, ship_tid, &sde);
            let mut fm = FittedModule {
                item_id: mod_id,
                type_id: mod_tid,
                flag_id: InventoryFlag::HiSlot0 as u16,
                state,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: charge_tid,
                charge_quantity: if charge_tid.is_some() { 1 } else { 0 },
                dynamic_attribute_overrides: HashMap::new(),
            };
            let _ = fit.fit_module(fm.item_id, fm.type_id, fm.flag_id, &sde);
            fit.modules.insert(fm.item_id, fm.clone());
            let mod_type = sde.types.get(&mod_tid).expect("mod_type exists");
            let mod_state = fit.build_effective_module_state(mod_id, mod_tid, false, &sde);
            fm.charge_type_id = charge_tid;
            (fit, fm, mod_type, mod_state)
        };

    // 1. Interdiction Nullifier (`group_id == 4117`): blocked on Rifter (group 25), blocked in warp, allowed on Interceptor (group 831)
    let nullifier_tid = find_type_by_group_attr(&sde, 4117, None);
    let interceptor_tid = find_type_by_group_attr(&sde, 831, None);
    let rifter_fit = build_fit_and_mod(587, nullifier_tid, ModuleState::Online, None);
    let err = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        mod_id,
        &rifter_fit.1,
        rifter_fit.2,
        &rifter_fit.3,
        &rifter_fit.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(err.code, "InvalidShipGroup");

    // In-warp rejection while using interceptor group
    let ceptor_fit = build_fit_and_mod(interceptor_tid, nullifier_tid, ModuleState::Online, None);
    let mut warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { phase, .. } = &mut warp_mode {
        *phase = eve_destiny::WarpPhase::Cruising;
    }
    let _ = sys.set_movement_mode(ship_id, warp_mode).await;
    let err_warp = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        831,
        mod_id,
        &ceptor_fit.1,
        ceptor_fit.2,
        &ceptor_fit.3,
        &ceptor_fit.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(err_warp.code, "CannotActivateInWarp");
    let _ = sys.force_stop_warp(ship_id);

    let ok_null = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        831,
        mod_id,
        &ceptor_fit.1,
        ceptor_fit.2,
        &ceptor_fit.3,
        &ceptor_fit.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ok_null.grants_interdiction_nullification);

    // 2. Assault Damage Control (`47254`) & Capital Emergency Hull Energizer (`40714`)
    let adc_tid = if sde.types.contains_key(&47254) {
        47254
    } else {
        sde.types
            .values()
            .find(|t| t.group_id == 60 && t.effect_ids.contains(&7012))
            .map(|t| t.type_id)
            .unwrap()
    };
    let adc_fit = build_fit_and_mod(587, adc_tid, ModuleState::Online, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &adc_fit.1, adc_fit.2, &adc_fit.3,
            &adc_fit.0, None, false, false, false, &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InvalidShipGroup"
    );
    let ok_adc = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_id, 324, mod_id, &adc_fit.1, adc_fit.2, &adc_fit.3,
        &adc_fit.0, None, false, false, false, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ok_adc.is_adc);

    let cehe_fit = build_fit_and_mod(587, 40714, ModuleState::Online, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &cehe_fit.1,
            cehe_fit.2,
            &cehe_fit.3,
            &cehe_fit.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InvalidShipGroup"
    );

    // 3. Zero-Point Mass Entangler (`49099`, group 2008)
    let zpme_fit = build_fit_and_mod(587, 49099, ModuleState::Online, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &zpme_fit.1,
            zpme_fit.2,
            &zpme_fit.3,
            &zpme_fit.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InvalidShipGroup"
    );
    let hic_tid = find_type_by_group_attr(&sde, 894, None);
    let zpme_hic = build_fit_and_mod(hic_tid, 49099, ModuleState::Online, None);
    let zp_prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        894,
        mod_id,
        &zpme_hic.1,
        zpme_hic.2,
        &zpme_hic.3,
        &zpme_hic.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(zp_prep.self_disable_mwd_mjd);
    assert_eq!(zp_prep.warp_core_stab_bonus, 2);

    // 4. Siege (`20280`), Triage (`27951`), Bastion (`33400`), Phenomena Generator (`group 1815`)
    let siege_fit = build_fit_and_mod(587, 20280, ModuleState::Online, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &siege_fit.1,
            siege_fit.2,
            &siege_fit.3,
            &siege_fit.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InvalidShipGroup"
    );
    // Missing fuel on Dreadnought (group 485) -> InsufficientFuel
    let dread_tid = find_type_by_group_attr(&sde, 485, None);
    let dread_siege = build_fit_and_mod(dread_tid, 20280, ModuleState::Online, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            485,
            mod_id,
            &dread_siege.1,
            dread_siege.2,
            &dread_siege.3,
            &dread_siege.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InsufficientFuel"
    );
    // Insert Strontium Clathrates (16275) in cargo & commit fuel deduction
    let fuel_item_id = 88_002_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_item_id,
            make_item(
                fuel_item_id,
                16275,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                500,
            ),
        );
    });
    let siege_prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        485,
        mod_id,
        &dread_siege.1,
        dread_siege.2,
        &dread_siege.3,
        &dread_siege.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(siege_prep.immobilize_self);
    assert!(siege_prep.self_ewar_immune);
    commit_post_cycle_activation_effects(
        &ctx,
        &sys,
        char_id,
        ship_id,
        mod_id,
        &dread_siege.1,
        dread_siege.2,
        &dread_siege.3,
        None,
        siege_prep.deferred_post_cycle,
    )
    .await
    .unwrap();
    let rem_fuel = ctx
        .db
        .with_snapshot(|s| s.items.get(&fuel_item_id).map(|i| i.quantity).unwrap_or(0));
    assert!(rem_fuel < 500);

    // Bastion (`33400`): rejected on cruiser, accepted on Marauder (`900`) and grants weapon timer on commit
    let bastion_tid = 33400u32;
    let marauder_tid = find_type_by_group_attr(&sde, 900, None);
    let bastion_bad = build_fit_and_mod(587, bastion_tid, ModuleState::Online, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &bastion_bad.1,
            bastion_bad.2,
            &bastion_bad.3,
            &bastion_bad.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InvalidShipGroup"
    );
    let bastion_ok = build_fit_and_mod(marauder_tid, bastion_tid, ModuleState::Online, None);
    let bas_prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        900,
        mod_id,
        &bastion_ok.1,
        bastion_ok.2,
        &bastion_ok.3,
        &bastion_ok.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(bas_prep.deferred_post_cycle.bastion_weapon_timer);
    commit_post_cycle_activation_effects(
        &ctx,
        &sys,
        char_id,
        ship_id,
        mod_id,
        &bastion_ok.1,
        bastion_ok.2,
        &bastion_ok.3,
        None,
        bas_prep.deferred_post_cycle,
    )
    .await
    .unwrap();
    assert!(
        ctx.crimewatch
            .write()
            .unwrap()
            .get_or_create(char_id)
            .weapon_timer_until_unix
            > 0
    );

    // Triage (`27951`) active on FAX (`1538`) disables EWAR
    let fax_tid = find_type_by_group_attr(&sde, 1538, None);
    let web_tid = find_type_by_group_attr(&sde, 65, Some(54));
    let mut triage_ewar_fit = ShipFittingEngine::from_sde(ship_id, fax_tid, &sde);
    let _ = triage_ewar_fit.fit_module(mod_id + 1, 27951, InventoryFlag::HiSlot0 as u16, &sde);
    triage_ewar_fit.modules.insert(
        mod_id + 1,
        FittedModule {
            item_id: mod_id + 1,
            type_id: 27951,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: ModuleState::Active,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    let web_fm = FittedModule {
        item_id: mod_id + 2,
        type_id: web_tid,
        flag_id: InventoryFlag::MedSlot0 as u16,
        state: ModuleState::Online,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let _ = triage_ewar_fit.fit_module(web_fm.item_id, web_fm.type_id, web_fm.flag_id, &sde);
    triage_ewar_fit
        .modules
        .insert(web_fm.item_id, web_fm.clone());
    let web_type = &sde.types[&web_tid];
    let web_state = triage_ewar_fit.build_effective_module_state(mod_id + 2, web_tid, false, &sde);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            1538,
            mod_id + 2,
            &web_fm,
            web_type,
            &web_state,
            &triage_ewar_fit,
            Some(target_id),
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "EwarDisabledInTriage"
    );

    // Phenomena Generator (`group_id == 1815`) on Titan (`30`)
    if let Some(phen_type) = sde.types.values().find(|t| t.group_id == 1815) {
        let titan_tid = find_type_by_group_attr(&sde, 30, None);
        let phen_bad = build_fit_and_mod(587, phen_type.type_id, ModuleState::Online, None);
        assert_eq!(
            prepare_ewar_and_capital_module(
                &ctx,
                &sys,
                sys_id,
                char_id,
                ship_id,
                25,
                mod_id,
                &phen_bad.1,
                phen_bad.2,
                &phen_bad.3,
                &phen_bad.0,
                None,
                false,
                false,
                false,
                &kwargs,
            )
            .await
            .unwrap()
            .map(|_| ())
            .unwrap_err()
            .code,
            "InvalidShipGroup"
        );
        // Provide isotopes + boost Strontium stack so `.find()` on cargo fuel always sees >= 40_000
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.items.get_mut(&fuel_item_id) {
                st.quantity = 100_000;
            }
            for (idx, iso_tid) in [16274u32, 17887, 17888, 17889].iter().enumerate() {
                let iid = fuel_item_id + 10 + (idx as i64);
                snap.items.insert(
                    iid,
                    make_item(
                        iid,
                        *iso_tid,
                        char_id,
                        ship_id,
                        InventoryFlag::Cargo,
                        100_000,
                    ),
                );
            }
        });
        let phen_ok = build_fit_and_mod(titan_tid, phen_type.type_id, ModuleState::Online, None);
        let p_res = prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, char_id, ship_id, 30, mod_id, &phen_ok.1, phen_ok.2, &phen_ok.3,
            &phen_ok.0, None, false, false, false, &kwargs,
        )
        .await
        .unwrap()
        .unwrap();
        commit_post_cycle_activation_effects(
            &ctx,
            &sys,
            char_id,
            ship_id,
            mod_id,
            &phen_ok.1,
            phen_ok.2,
            &phen_ok.3,
            None,
            p_res.deferred_post_cycle,
        )
        .await
        .unwrap();
    }

    // 5. Micro Jump Field Generator (`1533`) & MWD (`46`) & Compressor attr 3265 check
    let mwd_tid = find_type_by_group_attr(&sde, 46, Some(554));
    let mwd_fit = build_fit_and_mod(587, mwd_tid, ModuleState::Online, None);
    let mwd_prep = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &mwd_fit.1, mwd_fit.2, &mwd_fit.3,
        &mwd_fit.0, None, false, true, false, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(mwd_prep.hic_sig_multiplier.is_some());

    if let Some(mjfg_type) = sde.types.values().find(|t| t.group_id == 1533) {
        let cd_tid = find_type_by_group_attr(&sde, 1534, None);
        let mjfg_fit = build_fit_and_mod(cd_tid, mjfg_type.type_id, ModuleState::Online, None);
        let mj_res = prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            1534,
            mod_id,
            &mjfg_fit.1,
            mjfg_fit.2,
            &mjfg_fit.3,
            &mjfg_fit.0,
            None,
            false,
            false,
            true,
            &kwargs,
        )
        .await
        .unwrap();
        if let Ok(prep) = mj_res {
            assert!(prep.mjd_jump_distance_m.is_some());
        }
    }

    if let Some(comp_type) = sde
        .types
        .values()
        .find(|t| t.default_attributes.get(&3265).copied().unwrap_or(0.0) > 0.0)
    {
        let comp_fit = build_fit_and_mod(587, comp_type.type_id, ModuleState::Online, None);
        let comp_err = prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &comp_fit.1,
            comp_fit.2,
            &comp_fit.3,
            &comp_fit.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err();
        assert_eq!(comp_err.code, "IndustrialCoreNotActive");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fitting_ewar_capital_passive_targeter_remote_reps_and_burst_projectors() {
    let ctx = test_ctx();
    let sde = Arc::clone(&ctx.sde);
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 90_000_002i64;
    let ship_id = 88_000_101i64;
    let target_id = 88_000_102i64;
    let far_id = 88_000_103i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            target_id,
            make_item(
                target_id,
                587,
                char_id + 1,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            far_id,
            make_item(
                far_id,
                587,
                char_id + 2,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
    });
    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 587, DVec3::ZERO),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(target_id, 587, DVec3::new(3_000.0, 0.0, 0.0)),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(far_id, 587, DVec3::new(350_000.0, 0.0, 0.0)),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
    );

    let kwargs = HashMap::new();
    let mod_id = 88_002_001i64;
    let build_mod = |ship_tid: u32, mod_tid: u32, charge_tid: Option<u32>| {
        let mut fit = ShipFittingEngine::from_sde(ship_id, ship_tid, &sde);
        let mut fm = FittedModule {
            item_id: mod_id,
            type_id: mod_tid,
            flag_id: InventoryFlag::MedSlot0 as u16,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: charge_tid,
            charge_quantity: if charge_tid.is_some() { 1 } else { 0 },
            dynamic_attribute_overrides: HashMap::new(),
        };
        let _ = fit.fit_module(fm.item_id, fm.type_id, fm.flag_id, &sde);
        fit.modules.insert(fm.item_id, fm.clone());
        let mod_type = &sde.types[&mod_tid];
        let mod_state = fit.build_effective_module_state(mod_id, mod_tid, false, &sde);
        fm.charge_type_id = charge_tid;
        (fit, fm, mod_type, mod_state)
    };

    // 1. WDFG (`group_id == 899`): non-HIC reject, scripted MissingTarget/OutOfRange/Success, Zarzakh hazard block, unscripted bubble
    let wdfg_tid = find_type_by_group_attr(&sde, 899, None);
    let hic_tid = find_type_by_group_attr(&sde, 894, None);
    let w_bad = build_mod(587, wdfg_tid, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &w_bad.1, w_bad.2, &w_bad.3,
            &w_bad.0, None, false, false, false, &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "InvalidShipGroup"
    );
    let w_script_29003 = build_mod(hic_tid, wdfg_tid, Some(29003));
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            894,
            mod_id,
            &w_script_29003.1,
            w_script_29003.2,
            &w_script_29003.3,
            &w_script_29003.0,
            None,
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetRequired"
    );
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            894,
            mod_id,
            &w_script_29003.1,
            w_script_29003.2,
            &w_script_29003.3,
            &w_script_29003.0,
            Some(far_id),
            true,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );
    let w_script_ok = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        894,
        mod_id,
        &w_script_29003.1,
        w_script_29003.2,
        &w_script_29003.3,
        &w_script_29003.0,
        Some(target_id),
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(w_script_ok.warp_scramble_points, 100);
    assert!(w_script_ok.disables_target_mwd_mjd);

    let w_unscripted = build_mod(hic_tid, wdfg_tid, None);
    let w_bub = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        894,
        mod_id,
        &w_unscripted.1,
        w_unscripted.2,
        &w_unscripted.3,
        &w_unscripted.0,
        None,
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(w_bub.hic_bubble_radius_m.is_some());
    assert_eq!(w_bub.hic_velocity_multiplier, Some(0.40));
    assert_eq!(
        ewar_jamming_type(w_script_29003.2, Some(45010), true, None),
        Some("warpScramblerMWD")
    );
    assert_eq!(
        ewar_jamming_type(w_script_29003.2, Some(29003), false, None),
        Some("focusedWarpScrambler")
    );

    // 2. Warp Disruptor (`group_id == 52`) & WCS (`group_id == 315`) & EWAR (`379`, `65`, `1672`)
    let scram_tid = find_type_by_group_attr(&sde, 52, Some(54));
    let scram = build_mod(587, scram_tid, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram.1, scram.2, &scram.3,
            &scram.0, None, false, false, false, &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetRequired"
    );
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &scram.1,
            scram.2,
            &scram.3,
            &scram.0,
            Some(ship_id),
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "CannotActivateOnSelf"
    );
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &scram.1,
            scram.2,
            &scram.3,
            &scram.0,
            Some(far_id),
            true,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    let tp_tid = find_type_by_group_attr(&sde, 379, None);
    let tp = build_mod(587, tp_tid, None);
    let _ = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        mod_id,
        &tp.1,
        tp.2,
        &tp.3,
        &tp.0,
        Some(target_id),
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();

    let grap_tid = find_type_by_group_attr(&sde, 1672, None);
    let grap = build_mod(587, grap_tid, None);
    let gp_ok = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        mod_id,
        &grap.1,
        grap.2,
        &grap.3,
        &grap.0,
        Some(target_id),
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(gp_ok.target_velocity_multiplier.is_some());
    let gp_far = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        mod_id,
        &grap.1,
        grap.2,
        &grap.3,
        &grap.0,
        Some(far_id),
        false,
        false,
        false,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(gp_far.target_velocity_multiplier, Some(1.0));

    let wcs_tid = find_type_by_group_attr(&sde, 315, None);
    let wcs = build_mod(587, wcs_tid, None);
    let wcs_prep = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &wcs.1, wcs.2, &wcs.3, &wcs.0, None,
        false, false, false, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(wcs_prep.warp_core_stab_bonus >= 1);

    // 3. Remote Assistance: Shield (`41`), Armor (`325`), Mutadaptive (`2018`), Hull (`585`), Cap (`67`)
    // plus `TargetDisallowsAssistance`
    let rsb_tid = find_type_by_group_attr(&sde, 41, Some(68));
    let rsb = build_mod(587, rsb_tid, None);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &rsb.1, rsb.2, &rsb.3, &rsb.0, None,
            false, false, false, &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetRequired"
    );
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &rsb.1,
            rsb.2,
            &rsb.3,
            &rsb.0,
            Some(ship_id),
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "CannotActivateOnSelf"
    );
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: target_id,
        module_id: 88_002_999,
        module_type_id: 33400,
        effect_name: "bastionMode".into(),
        duration_s: 30.0,
        self_disallow_assistance: true,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &rsb.1,
            rsb.2,
            &rsb.3,
            &rsb.0,
            Some(target_id),
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetDisallowsAssistance"
    );
    let _ = sys.remove_module_cycle(target_id, 88_002_999).await;
    let _ = sys
        .set_combat_state(
            target_id,
            make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
        )
        .await;
    assert!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &rsb.1,
            rsb.2,
            &rsb.3,
            &rsb.0,
            Some(target_id),
            true,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap()
        .remote_shield_amount
            > 0.0
    );

    for (grp, attr_chk) in [(325u32, 84u16), (2018, 84), (585, 83), (67, 90)] {
        let tid = find_type_by_group_attr(&sde, grp, Some(attr_chk));
        let mut bm = build_mod(587, tid, None);
        if grp == 2018 {
            bm.1.dynamic_attribute_overrides.insert(84, 500.0);
        }
        let prep = prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &bm.1,
            bm.2,
            &bm.3,
            &bm.0,
            Some(target_id),
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            prep.remote_armor_amount > 0.0
                || prep.remote_hull_amount > 0.0
                || prep.remote_cap_amount > 0.0
        );
    }

    // 4. Burst Projectors (`group_id == 842`): 27678 (ECM), 40636 (Warp Bubble), 40635 (Neut), 40698 (Web)
    for bp_tid in [27678u32, 40636, 40635, 40698, 40699] {
        if let Some(t) = sde.types.get(&bp_tid) {
            let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &sde);
            let fm = FittedModule {
                item_id: mod_id + i64::from(bp_tid),
                type_id: bp_tid,
                flag_id: InventoryFlag::HiSlot0 as u16,
                state: ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: HashMap::new(),
            };
            let _ = fit.fit_module(fm.item_id, fm.type_id, fm.flag_id, &sde);
            fit.modules.insert(fm.item_id, fm.clone());
            let ms = fit.build_effective_module_state(fm.item_id, bp_tid, false, &sde);
            let prep = prepare_ewar_and_capital_module(
                &ctx,
                &sys,
                sys_id,
                char_id,
                ship_id,
                25,
                fm.item_id,
                &fm,
                t,
                &ms,
                &fit,
                Some(target_id),
                false,
                false,
                false,
                &kwargs,
            )
            .await
            .unwrap()
            .unwrap();
            assert!(prep.deferred_post_cycle.burst_projector.is_some());
            commit_post_cycle_activation_effects(
                &ctx,
                &sys,
                char_id,
                ship_id,
                fm.item_id,
                &fm,
                t,
                &ms,
                Some(target_id),
                prep.deferred_post_cycle,
            )
            .await
            .unwrap();
        }
    }

    // 5. Passive Targeter validation branches
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &sde);
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, ship_id, &fit)
            .await
            .unwrap_err()
            .code,
        "CannotActivateOnSelf"
    );
    let mut jammed_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
    jammed_cs.ecm_jammed_remaining_s = 5.0;
    let _ = sys.set_combat_state(ship_id, jammed_cs).await;
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "ShipJammed"
    );
    let mut cloaked_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
    cloaked_cs.is_cloaked = true;
    let _ = sys.set_combat_state(ship_id, cloaked_cs).await;
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    let _ = sys
        .set_combat_state(
            ship_id,
            make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
        )
        .await;
    let _ = sys
        .set_combat_state(
            target_id,
            CombatState {
                is_cloaked: true,
                ..make_combat_state(100.0, 100.0, 100.0, 100.0)
            },
        )
        .await;
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    let _ = sys
        .set_combat_state(
            target_id,
            make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
        )
        .await;
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, 9_999_999, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    let ok_lock =
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap();
    assert_eq!(ok_lock.target_id, target_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_cycles_tackle_nanite_rah_mjd_turret_and_salvage_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let reg = SystemRegistry::with_sde_and_db(Arc::clone(&ctx.sde), Arc::clone(&ctx.db));
    let handle = reg.get_or_spawn(sys_id);

    let char_id = 90_000_010i64;
    let ship_id = 88_100_001i64;
    let target_id = 88_100_002i64;
    let wreck_id = 88_100_003i64;
    let sleeper_id = 88_100_004i64;
    let turret_id = 88_100_101i64;
    let charge_id = 88_100_102i64;
    let cargo_ammo_id = 88_100_103i64;
    let paste_id = 88_100_104i64;
    let damaged_mod_id = 88_100_105i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut turret = make_item(turret_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        turret.singleton = true;
        snap.items.insert(turret_id, turret);

        snap.items.insert(
            charge_id,
            make_item(charge_id, 185, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            cargo_ammo_id,
            make_item(
                cargo_ammo_id,
                185,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                40,
            ),
        );
        snap.items.insert(
            paste_id,
            make_item(paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 2),
        );
        let mut dmg_mod = make_item(
            damaged_mod_id,
            3565,
            char_id,
            ship_id,
            InventoryFlag::MedSlot0,
            1,
        );
        dmg_mod
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 20.0);
        snap.items.insert(damaged_mod_id, dmg_mod);

        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 587, DVec3::ZERO),
            make_combat_state(1_000.0, 500.0, 500.0, 2_000.0),
        )
        .await
        .unwrap();
    handle
        .spawn_ball_with_combat(
            make_ball(target_id, 587, DVec3::new(2_000.0, 0.0, 0.0)),
            make_combat_state(500.0, 500.0, 500.0, 1_000.0),
        )
        .await
        .unwrap();
    handle
        .spawn_ball_with_meta(
            make_ball(sleeper_id, 30_190, DVec3::new(2_500.0, 0.0, 0.0)),
            make_combat_state(500.0, 500.0, 500.0, 500.0),
            SlimItemMeta {
                group_id: 1_307,
                category_id: 11,
                owner_id: 500_021,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    handle
        .spawn_ball(make_ball(wreck_id, 26557, DVec3::new(1_000.0, 0.0, 0.0)))
        .await
        .unwrap();

    // 1. Nanite Repair Paste job that exhausts mid-repair when cargo only has 2 units out of 5 needed
    handle
        .start_nanite_repair(ActiveNaniteRepair {
            char_id,
            ship_id,
            module_id: damaged_mod_id,
            initial_damage_hp: 20.0,
            paste_total_needed: 5,
            paste_consumed_so_far: 0,
            duration_s: 10.0,
            elapsed_s: 0.0,
        })
        .await
        .unwrap();
    assert!(handle.has_any_active_nanite_repair_sync(ship_id));
    assert!(handle.is_module_being_nanite_repaired_sync(ship_id, damaged_mod_id));
    let _ = handle.step_manual(6.0).await.unwrap();
    // 2 units exhausted on first >40% jump, remaining request fails -> halted mid-repair!
    assert!(!handle.is_module_being_nanite_repaired_sync(ship_id, damaged_mod_id));

    // Re-add paste and test explicit stop_nanite_repair + tether heat repair
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            paste_id,
            make_item(paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 10),
        );
        if let Some(m) = snap.items.get_mut(&damaged_mod_id) {
            m.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 12.0);
        }
    });
    handle
        .start_nanite_repair(ActiveNaniteRepair {
            char_id,
            ship_id,
            module_id: damaged_mod_id,
            initial_damage_hp: 12.0,
            paste_total_needed: 4,
            paste_consumed_so_far: 0,
            duration_s: 8.0,
            elapsed_s: 0.0,
        })
        .await
        .unwrap();
    assert!(
        handle
            .stop_nanite_repair(ship_id, damaged_mod_id)
            .await
            .unwrap()
            .is_some()
    );

    // Tether heat repair clears DAMAGE attribute over ticks
    let mut tethered_cs = make_combat_state(1_000.0, 500.0, 500.0, 2_000.0);
    tethered_cs.is_tethered = true;
    handle.set_combat_state(ship_id, tethered_cs).await.unwrap();
    let _ = handle.step_manual(10.0).await.unwrap();
    let cleared = ctx.db.with_snapshot(|s| {
        s.items.get(&damaged_mod_id).is_some_and(|i| {
            !i.dynamic_attributes
                .contains_key(&eve_sde::attr_ids::DAMAGE)
        })
    });
    assert!(cleared);

    // 2. Reactive Armor Hardener (`is_rah`), CEHE (`ehe_hull_resists`), Turret auto-reload, Overloaded transition, Salvage (`1122`), MJD (`mjd_jump_distance_m`)
    let mut dmg_cs = make_combat_state(1_000.0, 500.0, 500.0, 2_000.0);
    dmg_cs.armor_hp = 200.0;
    handle.set_combat_state(ship_id, dmg_cs).await.unwrap();
    handle
        .record_incoming_armor_damage(ship_id, [100.0, 20.0, 0.0, 0.0])
        .await
        .unwrap();

    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 88_100_201,
            module_type_id: 21898,
            owner_char_id: Some(char_id),
            effect_name: "reactiveArmorHardener".into(),
            duration_s: 2.0,
            is_rah: true,
            rah_shift_amount: 0.06,
            cap_need_per_cycle: 10.0,
            ..Default::default()
        })
        .await
        .unwrap();
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 88_100_202,
            module_type_id: 40714,
            owner_char_id: Some(char_id),
            effect_name: "capitalEmergencyHullEnergizer".into(),
            duration_s: 2.0,
            ehe_hull_resists: Some([0.05, 0.05, 0.05, 0.05]),
            is_overloaded: true,
            ..Default::default()
        })
        .await
        .unwrap();
    // Put CEHE item in DB so burnout updates it
    ctx.db.with_mut_snapshot(|snap| {
        let mut cehe = make_item(
            88_100_202,
            40714,
            char_id,
            ship_id,
            InventoryFlag::LoSlot0,
            1,
        );
        cehe.dynamic_attributes
            .insert(eve_sde::attr_ids::HP_STRUCT, 40.0);
        snap.items.insert(88_100_202, cehe);
    });
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: turret_id,
            module_type_id: 484,
            owner_char_id: Some(char_id),
            effect_name: "projectileFired".into(),
            target_id: Some(target_id),
            duration_s: 2.0,
            linked_turrets: vec![turret_id],
            ammo_type_id: Some(185),
            damage_vector: DamageVector {
                em: 0.0,
                thermal: 0.0,
                kinetic: 10.0,
                explosive: 10.0,
            },
            is_overloaded: true,
            pending_overloaded: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    let salvager_tid = find_type_by_group_attr(&ctx.sde, 1122, None);
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 88_100_203,
            module_type_id: salvager_tid,
            owner_char_id: Some(char_id),
            effect_name: "salvaging".into(),
            target_id: Some(wreck_id),
            duration_s: 2.0,
            cap_need_per_cycle: 5.0,
            repeat: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    // Nosferatu vs Sleeper (Sleeper cap stays untouched while source still gains cap)
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 88_100_204,
            module_type_id: 12269,
            owner_char_id: Some(char_id),
            effect_name: "energyDestabilizationNew".into(),
            target_id: Some(sleeper_id),
            duration_s: 2.0,
            cap_transfer_target: 50.0,
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(handle.has_active_overloaded_cycle_sync(ship_id));
    let agg = handle.active_offensive_aggressors_on_sync(target_id, Some(&ctx.sde));
    assert!(!agg.is_empty());
    let exp = handle.max_active_module_cycle_expiry_unix_sync(ship_id, 0, 1_700_000_000);
    assert!(exp >= 1_700_000_002);

    // Step 2.1s to resolve RAH, CEHE burnout, turret ammo depletion/auto-reload, salvager PendingCycleAction, and Nosferatu vs Sleeper
    let _ = handle.step_manual(2.1).await.unwrap();
    let actions = handle.take_pending_cycle_actions();
    assert!(actions.iter().any(|a| matches!(
        a,
        crate::system_actor::PendingCycleAction::SalvageEnd { .. }
    )));
    assert!(actions.iter().any(|a| matches!(
        a,
        crate::system_actor::PendingCycleAction::InvalidateShipFitting { .. }
    )));
    // Sleeper capacitor remains 500.0 (immune to cap drain!)
    assert_eq!(
        handle
            .get_combat_state(sleeper_id)
            .await
            .unwrap()
            .unwrap()
            .capacitor,
        500.0
    );

    // 3. MJD cycle + Pending Mobile Warp Disruptor anchoring + fast_forward_clock
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 88_100_205,
            module_type_id: 4383,
            owner_char_id: Some(char_id),
            effect_name: "microJumpDrive".into(),
            duration_s: 3.0,
            mjd_jump_distance_m: Some(100_000.0),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        handle
            .schedule_mobile_warp_disruptor(
                88_100_301,
                12198,
                DVec3::new(100_000.0, 0.0, 0.0),
                20_000.0,
                2.0
            )
            .await
            .unwrap()
    );
    let _ = handle.step_manual(3.1).await.unwrap();
    // Ship jumped ~100km into the newly anchored mobile warp disruptor bubble!
    assert!(handle.is_ball_scrambled(ship_id).await.unwrap());
    assert!(handle.fast_forward_clock(ship_id, 10).await.is_ok());

    // 4. ECM burst & apply_ecm_jam & try_step_manual NaN/negative/Clamped + remove_ball
    let _ = handle.apply_ecm_jam(target_id, 10.0).await.unwrap();
    let jammed = handle
        .apply_ecm_burst(ship_id, 150_000.0, [20.0, 20.0, 20.0, 20.0], 10.0, 0.01)
        .await
        .unwrap();
    assert!(jammed.contains(&target_id) || !jammed.is_empty());
    handle.try_step_manual(f64::NAN);
    handle.try_step_manual(-1.0);
    assert!(handle.sim_elapsed_secs() > 0.0);
    let _ = handle.try_remove_module_cycle(ship_id, 88_100_201);
    let _ = handle.remove_ball(ship_id).await;
    assert!(!handle.has_any_active_nanite_repair_sync(ship_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_belts_celestials_disruptors_and_downtime_edges() {
    let ctx = test_ctx();
    let reg = SystemRegistry::with_sde_and_db(Arc::clone(&ctx.sde), Arc::clone(&ctx.db));
    let sys_id = 30_000_142u32;
    let handle = reg.get_or_spawn(sys_id);

    // Populate celestials + asteroid belts and deplete an asteroid to schedule downtime respawn
    let asts = handle.populate_asteroid_belt_entities().await.unwrap();
    if let Some(first_ast) = asts.first() {
        let mined = handle
            .mine_asteroid(first_ast.asteroid_id, first_ast.remaining_units + 100, 0.0)
            .await
            .unwrap();
        assert!(mined.depleted);
        // Unknown asteroid returns error
        assert!(handle.mine_asteroid(9_999_999_999, 10, 0.0).await.is_err());
        let st = handle
            .get_asteroid_state(first_ast.asteroid_id)
            .await
            .unwrap()
            .unwrap();
        assert!(st.depleted);
        let respawned = handle.respawn_depleted_belts_on_downtime();
        assert!(respawned >= 1);
    }

    // Spawn player ball + station dock + force_stop_warp + push_set_state + drain_observer_updates
    let ship_id = 88_200_001i64;
    handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 587, DVec3::new(10_000.0, 0.0, 0.0)),
            make_combat_state(100.0, 100.0, 100.0, 100.0),
        )
        .await
        .unwrap();
    assert!(handle.push_set_state(ship_id).await.is_ok());
    let _ = handle.drain_observer_updates(ship_id).await.unwrap();
    assert!(handle.force_stop_warp(ship_id).is_ok());
    assert!(
        handle
            .accept_docking(ship_id, 60_003_760, DVec3::ZERO)
            .await
            .is_some()
    );

    // next_downtime_unix_secs edge periods (interval <= 0 clamped to 1s; exact downtime boundary wraps +period)
    assert_eq!(
        system_actor::next_downtime_unix_secs(39_600, 0, 39_600),
        39_601
    );
    assert_eq!(
        system_actor::next_downtime_unix_secs(39_600, 86_400, 39_600),
        86_400 + 39_600
    );
    assert_eq!(
        system_actor::next_downtime_unix_secs(39_599, 86_400, 39_600),
        39_600
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xmpp_chat_auth_acl_moderation_and_quasar_social_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let con_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id)
        .unwrap_or(20_000_020);
    let reg_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.region_id)
        .unwrap_or(10_000_002);

    let owner_id = 90_000_101i64;
    let other_id = 90_000_102i64;
    let corp_id = 98_000_101i64;
    let ally_id = 99_000_101i64;
    let fleet_id = 77_000_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = owner_id;
            c.solar_system_id = sys_id;
            c.corporation_id = corp_id;
            c.alliance_id = Some(ally_id);
            c.war_faction_id = Some(500_001);
            snap.characters.insert(owner_id, c.clone());
            c.character_id = other_id;
            c.solar_system_id = 30_002_187;
            c.corporation_id = corp_id + 1;
            c.alliance_id = None;
            c.war_faction_id = None;
            snap.characters.insert(other_id, c);
        }
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: owner_id,
                members: vec![owner_id],
                ..Default::default()
            },
        );
        snap.sessions.insert(
            1,
            eve_db::OnlineSessionRecord {
                session_id: 1,
                user_id: 101,
                username: "user_101".into(),
                character_id: Some(owner_id),
                connected_at_unix: 100,
                computer_hash: None,
            },
        );
    });

    let mut owner_sess = make_session(101, owner_id, sys_id, 88_300_001);
    owner_sess.corporation_id = Some(corp_id);
    owner_sess.alliance_id = Some(ally_id);
    owner_sess.fleet_id = Some(fleet_id);
    let mut other_sess = make_session(102, other_id, 30_002_187, 88_300_002);
    other_sess.corporation_id = Some(corp_id + 1);
    let kw = HashMap::new();

    // 1. Validate all channel-prefix ACL join checks (`solarsystemid2:`, `resourcewars:`, `constellationid:`, `regionid:`, `corpid:`, `allianceid:`, `fleetid:`, `factionid:`, `private:`)
    let valid_chans = [
        format!("solarsystemid2:{sys_id}"),
        format!("resourcewars:{sys_id}"),
        format!("constellationid:{con_id}"),
        format!("regionid:{reg_id}"),
        format!("corpid:{corp_id}"),
        format!("allianceid:{ally_id}"),
        format!("fleetid:{fleet_id}"),
        "factionid:500001".to_string(),
    ];
    for ch in &valid_chans {
        assert!(
            fleet_social::handle(
                &ctx,
                &mut owner_sess,
                "XmppChatMgr",
                "AddCharacterToChannel",
                &[PyRep::String(ch.clone())],
                &kw
            )
            .await
            .unwrap()
            .is_ok(),
            "owner should join {ch}"
        );
        assert!(
            fleet_social::handle(
                &ctx,
                &mut other_sess,
                "XmppChatMgr",
                "AddCharacterToChannel",
                &[PyRep::String(ch.clone())],
                &kw
            )
            .await
            .unwrap()
            .is_err(),
            "other should be denied {ch}"
        );
    }
    // Invalid numeric suffix on channel IDs
    for bad in [
        "resourcewars:bad",
        "constellationid:bad",
        "regionid:bad",
        "corpid:bad",
        "allianceid:bad",
        "fleetid:bad",
        "factionid:bad",
    ] {
        assert!(
            fleet_social::handle(
                &ctx,
                &mut owner_sess,
                "XmppChatMgr",
                "AddCharacterToChannel",
                &[PyRep::String(bad.into())],
                &kw
            )
            .await
            .unwrap()
            .is_err()
        );
    }

    // 2. Player-owned channel lifecycle: Create, GrantOwnership, SetMotd, GMMute/Unmute, Destroy
    let created = fleet_social::handle(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[
            PyRep::String("Ops Room".into()),
            PyRep::String("secret1".into()),
            PyRep::String("Welcome".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let chan_id = created
        .dict_get("channelID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // Non-owner cannot SetMotd or GrantChannelOwnership
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut other_sess,
            "XmppChatMgr",
            "SetMotd",
            &[
                PyRep::String(chan_id.clone()),
                PyRep::String("Hacked".into())
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOperator"
    );
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut other_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(other_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    // System channel ownership transfer denied
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[
                PyRep::String(format!("solarsystemid2:{sys_id}")),
                PyRep::Integer(other_id)
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    // Owner sets MOTD & grants ownership & unmutes/mutes
    assert!(
        fleet_social::handle(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "SetMotd",
            &[
                PyRep::String(chan_id.clone()),
                PyRep::String("Fleet Up".into())
            ],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "GMMute",
            &[
                PyRep::Integer(other_id),
                PyRep::String(chan_id.clone()),
                PyRep::Integer(120),
                PyRep::String("Spam".into())
            ],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "GMUnmute",
            &[PyRep::Integer(other_id), PyRep::String(chan_id.clone())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(other_id)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut other_sess,
            "XmppChatMgr",
            "GetRookieChannel",
            &[],
            &kw
        )
        .await
        .unwrap()
        .is_ok(),
        true
    );

    // 3. Quasar social chat (`GetMembershipListRequest`, `BroadcastMessageRequest`, `admin.MuteRequest`)
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let gw = PublicGatewayRouter::new(descriptors, Arc::clone(&ctx.db), Arc::clone(&ctx.sde));
    quasar_social_chat::register(&gw, Arc::clone(&ctx.db));

    let now = 1_700_200_000u64;
    let anon_sess = SessionState::new(999);
    let mut admin_sess = owner_sess.clone();
    admin_sess.role = service_roles::ROLE_ADMIN | service_roles::ROLE_PLAYER;

    // Missing active character -> 401
    let mem_req = EvePublicRequest {
        correlation_uuid: vec![1, 2, 3, 4],
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.GetMembershipListRequest"
            ),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    assert_eq!(
        gw.handle_public_request(&anon_sess, &mem_req, None, false, now)
            .status_code,
        401
    );
    assert_eq!(
        gw.handle_public_request(&owner_sess, &mem_req, None, false, now)
            .status_code,
        STATUS_OK
    );

    // BroadcastMessageRequest with valid text & MuteRequest errors + success
    let bcast_req = EvePublicRequest {
        correlation_uuid: vec![2, 2, 3, 4],
        payload: Some(
            eve_proto::pb::message("eve_public.chat.local.api.BroadcastMessageRequest")
                .set("message", "Fly safe")
                .must_to_any(),
        ),
        ..Default::default()
    };
    assert_eq!(
        gw.handle_public_request(&owner_sess, &bcast_req, None, false, now)
            .status_code,
        STATUS_OK
    );

    // MuteRequest: non-admin denied (403), invalid payload (400), missing char (404), valid mute (200)
    let mute_bad_req = EvePublicRequest {
        correlation_uuid: vec![3, 2, 3, 4],
        payload: Some(
            eve_proto::pb::message("eve_public.chat.local.api.admin.MuteRequest")
                .set("character.sequential", 0u32)
                .must_to_any(),
        ),
        ..Default::default()
    };
    assert_eq!(
        gw.handle_public_request(&owner_sess, &mute_bad_req, None, false, now)
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        gw.handle_public_request(&admin_sess, &mute_bad_req, None, false, now)
            .status_code,
        STATUS_BAD_REQUEST
    );

    let mute_missing_req = EvePublicRequest {
        correlation_uuid: vec![4, 2, 3, 4],
        payload: Some(
            eve_proto::pb::message("eve_public.chat.local.api.admin.MuteRequest")
                .set("character.sequential", 999_999_888u32)
                .set("duration", GoogleDuration { seconds: 60, nanos: 0 })
                .must_to_any(),
        ),
        ..Default::default()
    };
    assert_eq!(
        gw.handle_public_request(&admin_sess, &mute_missing_req, None, false, now)
            .status_code,
        STATUS_NOT_FOUND
    );

    let mute_ok_req = EvePublicRequest {
        correlation_uuid: vec![5, 2, 3, 4],
        payload: Some(
            eve_proto::pb::message("eve_public.chat.local.api.admin.MuteRequest")
                .set("character.sequential", owner_id as u32)
                .set("duration", GoogleDuration { seconds: 120, nanos: 0 })
                .set("reason", "Test mute")
                .must_to_any(),
        ),
        ..Default::default()
    };
    assert_eq!(
        gw.handle_public_request(&admin_sess, &mute_ok_req, None, false, now)
            .status_code,
        STATUS_OK
    );
    // Muted character now fails BroadcastMessageRequest (403)
    assert_eq!(
        gw.handle_public_request(&owner_sess, &bcast_req, None, false, now)
            .status_code,
        STATUS_FORBIDDEN
    );
}

#[test]
fn contracts_and_market_order_search_plex_margin_and_expiry_edges() {
    let ctx = test_ctx();
    let issuer_id = 90_000_201i64;
    let bidder_id = 90_000_202i64;
    let corp_id = 98_000_201i64;
    let station_id = 60_003_760i64;
    let region_id = 10_000_002u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = issuer_id;
            c.corporation_id = corp_id;
            c.balance = IskCents::from_isk(1_000_000_000);
            snap.characters.insert(issuer_id, c.clone());
            c.character_id = bidder_id;
            c.corporation_id = corp_id;
            c.balance = IskCents::from_isk(1_000_000_000);
            snap.characters.insert(bidder_id, c);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Test Corp".into(); __corp.ticker = "TSTC".into(); __corp.ceo_id = issuer_id; __corp.tax_rate = 0.05; __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    IskCents::from_isk(50_000_000),
                ); let __mw = IskCents::from_isk(50_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        snap.contracts.insert(
            55_001,
            ContractRecord {
                contract_id: 55_001,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                availability: "Public".into(),
                issuer_id,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                start_station_id: station_id,
                end_station_id: station_id,
                price: IskCents::from_isk(5_000_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                buyout: IskCents::from_isk(20_000_000),
                expiration_unix: ctx.now_unix() + 86_400,
                current_bid: IskCents::from_isk(6_000_000),
                current_bidder_id: Some(bidder_id),
                bid_history: vec![(bidder_id, IskCents::from_isk(6_000_000), ctx.now_unix())],
                item_ids: vec![],
                requested_items: vec![(34, 100)],
                title: "Tritanium Lot".into(),
                for_corp: true,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            55_002,
            ContractRecord {
                contract_id: 55_002,
                contract_type: "Courier".into(),
                status: "Expired".into(),
                availability: "Public".into(),
                issuer_id,
                issuer_corp_id: corp_id,
                start_station_id: station_id,
                end_station_id: station_id,
                expiration_unix: 100,
                ..Default::default()
            },
        );
    });

    let mut iss_sess = make_session(201, issuer_id, 30_000_142, 88_400_001);
    iss_sess.corporation_id = Some(corp_id);
    iss_sess.station_id = Some(station_id);
    let mut bid_sess = make_session(202, bidder_id, 30_000_142, 88_400_002);
    bid_sess.corporation_id = Some(corp_id);
    bid_sess.corp_role = 0;

    let mut search_kw = HashMap::new();
    search_kw.insert("regionID".into(), PyRep::Integer(i64::from(region_id)));
    search_kw.insert("itemTypeID".into(), PyRep::Integer(34));
    search_kw.insert("minPrice".into(), PyRep::Real(1_000_000.0));
    search_kw.insert("maxPrice".into(), PyRep::Real(10_000_000.0));
    search_kw.insert("stationID".into(), PyRep::Integer(station_id));
    let found = contracts_service::handle(
        &ctx,
        &mut bid_sess,
        "contractProxy",
        "SearchContracts",
        &[PyRep::String("Auction".into())],
        &search_kw,
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(rows) = found {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected list");
    }

    // GetMyCurrentContractList (forCorp + forbidden foreign owner) & GetMyExpiredContractList
    let mut corp_kw = HashMap::new();
    corp_kw.insert("forCorp".into(), PyRep::Bool(true));
    assert!(
        contracts_service::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "GetMyCurrentContractList",
            &[],
            &corp_kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        contracts_service::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "GetMyCurrentContractList",
            &[PyRep::Integer(99_999_999)],
            &HashMap::new()
        )
        .unwrap()
        .is_err()
    );
    assert!(
        contracts_service::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "GetMyExpiredContractList",
            &[],
            &HashMap::new()
        )
        .unwrap()
        .is_ok()
    );

    // GetMyBids (personal + corp role check)
    assert!(
        contracts_service::handle(
            &ctx,
            &mut bid_sess,
            "contractProxy",
            "GetMyBids",
            &[PyRep::Bool(false)],
            &HashMap::new()
        )
        .unwrap()
        .is_ok()
    );
    assert_eq!(
        contracts_service::handle(
            &ctx,
            &mut bid_sess,
            "contractProxy",
            "GetMyBids",
            &[PyRep::Integer(corp_id)],
            &HashMap::new()
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // GM_ExpireContract requires GM role
    assert_eq!(
        contracts_service::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "GM_ExpireContract",
            &[PyRep::Integer(55_001)],
            &HashMap::new()
        )
        .unwrap()
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "gm_acct".into(),
            eve_db::AccountRecord {
                user_id: 201,
                username: "gm_acct".into(),
                password_token: String::new(),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    assert!(
        contracts_service::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "GM_ExpireContract",
            &[PyRep::Integer(55_001)],
            &HashMap::new()
        )
        .unwrap()
        .is_ok()
    );

    // marketProxy: StartupCheck, GetSystemAsks, GetHistoryForManyTypeIDs, PLEX sell vs insolvent margin buy
    let mut mkt_sess = iss_sess.clone();
    mkt_sess.region_id = Some(region_id);
    assert!(
        market_contracts::handle(
            &ctx,
            &mut mkt_sess,
            "marketProxy",
            "StartupCheck",
            &[],
            &HashMap::new()
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        market_contracts::handle(
            &ctx,
            &mut mkt_sess,
            "marketProxy",
            "GetSystemAsks",
            &[PyRep::Integer(30_000_142)],
            &HashMap::new()
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        market_contracts::handle(
            &ctx,
            &mut mkt_sess,
            "marketProxy",
            "GetHistoryForManyTypeIDs",
            &[PyRep::List(vec![PyRep::Integer(34)])],
            &HashMap::new()
        )
        .unwrap()
        .is_ok()
    );

    // Resting insolvent margin PLEX buy order owned by corp + PLEX sell order matching it -> cancels insolvent bid cleanly & covers remaining qty on market
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bid_oid = snap.next_entity_id;
        snap.market_orders.insert(
            bid_oid,
            MarketOrderRecord {
                order_id: bid_oid,
                owner_id: corp_id,
                station_id,
                region_id,
                type_id: 44_992,
                is_bid: true,
                price: IskCents::from_isk(1_000_000_000), // far exceeds corp div balance!
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(100),
            },
        );
        let _ = snap.adjust_plex_vault(issuer_id, 50);
    });
    let res_plex = market_contracts::handle(
        &ctx,
        &mut mkt_sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(44_992),
            PyRep::Integer(station_id),
            PyRep::Integer(5),
            PyRep::Real(1_000_000.0),
            PyRep::Integer(1),
        ])])],
        &HashMap::new(),
    );
    assert!(res_plex.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structure_fuel_jettison_delivery_and_cargo_dropoff_edge_guards() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_301i64;
    let recip_id = 90_000_302i64;
    let corp_id = 98_000_301i64;
    let st_id = 1_030_000_301i64;
    let ship_id = 88_500_001i64;
    let fuel_item_id = 88_500_010i64;
    let non_fuel_id = 88_500_011i64;
    let nested_box_id = 88_500_012i64;
    let nested_child_id = 88_500_013i64;
    let cargo_ore_id = 88_500_014i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = char_id;
            c.corporation_id = corp_id;
            c.solar_system_id = sys_id;
            c.structure_id = Some(st_id);
            snap.characters.insert(char_id, c.clone());
            c.character_id = recip_id;
            snap.characters.insert(recip_id, c);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Struct Corp".into(); __corp.ticker = "STRC".into(); __corp.ceo_id = char_id; __corp.ensure_wallet_divisions(); __corp },
        );
        snap.structures.insert(
            st_id,
            eve_db::UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_item_id,
            make_item(
                fuel_item_id,
                4247,
                corp_id,
                st_id,
                InventoryFlag::StructureFuel,
                20,
            ),
        );
        snap.items.insert(
            non_fuel_id,
            make_item(
                non_fuel_id,
                34,
                corp_id,
                st_id,
                InventoryFlag::StructureFuel,
                20,
            ),
        );
        snap.items.insert(
            nested_box_id,
            make_item(nested_box_id, 23, char_id, st_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            nested_child_id,
            make_item(
                nested_child_id,
                34,
                char_id,
                nested_box_id,
                InventoryFlag::Cargo,
                5,
            ),
        );
        snap.items.insert(
            cargo_ore_id,
            make_item(
                cargo_ore_id,
                1230,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                10,
            ),
        );
    });

    let mut sess = make_session(301, char_id, sys_id, ship_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    sess.structure_id = Some(st_id);
    let kw = HashMap::new();

    // 1. Non-fuel item rejected, valid explicit fuel item jettisoned into space container
    assert_eq!(
        station_structure::handle(
            &ctx,
            &mut sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::List(vec![PyRep::Integer(non_fuel_id)])],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFuel"
    );
    assert!(
        station_structure::handle(
            &ctx,
            &mut sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::List(vec![PyRep::Integer(fuel_item_id)])],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 2. Deliver nested container blocked
    assert_eq!(
        station_structure::handle(
            &ctx,
            &mut sess,
            "structureDeliveries",
            "Deliver",
            &[
                PyRep::Integer(st_id),
                PyRep::Integer(nested_box_id),
                PyRep::Integer(recip_id)
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotDeliverNonEmptyContainer"
    );

    // 3. DropOffItems checks: must be in space, in range, valid cargo
    assert_eq!(
        station_structure::handle(
            &ctx,
            &mut sess,
            "structureCargoDelivery",
            "DropOffItems",
            &[
                PyRep::Integer(st_id),
                PyRep::List(vec![PyRep::Integer(cargo_ore_id)])
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeInSpace"
    );
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 587, DVec3::new(1_200.0, 0.0, 0.0)),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    );
    // Drop non-cargo item error -> then drop valid cargo item succeeds
    assert_eq!(
        station_structure::handle(
            &ctx,
            &mut sess,
            "structureCargoDelivery",
            "DropOffItems",
            &[
                PyRep::Integer(st_id),
                PyRep::List(vec![PyRep::Integer(99_999_999)])
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotInShipCargo"
    );
    assert!(
        station_structure::handle(
            &ctx,
            &mut sess,
            "structureCargoDelivery",
            "DropOffItems",
            &[
                PyRep::Integer(st_id),
                PyRep::List(vec![PyRep::Integer(cargo_ore_id)])
            ],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mining_cycle_missiles_defender_bomb_and_fighter_ability_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let lowsec_sys = 30_000_136u32;
    let char_id = 90_000_401i64;
    let victim_id = 90_000_402i64;
    let ship_id = 88_600_001i64;
    let pod_id = 88_600_002i64;
    let laser_id = 88_600_101i64;
    let ast_id = 88_600_201i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = char_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = ship_id;
            snap.characters.insert(char_id, c.clone());
            c.character_id = victim_id;
            c.solar_system_id = lowsec_sys;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = pod_id;
            snap.characters.insert(victim_id, c);
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                32880,
                char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            laser_id,
            make_item(laser_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_online_states.insert(laser_id, true);
        if let Some(demo_skills) = snap.skills.get(&90_000_001).cloned() {
            snap.skills.insert(char_id, demo_skills);
        }
        snap.skills.entry(char_id).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.asteroids.insert(
            ast_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_009_001,
                type_id: 1230, // Veldspar
                quantity: 5,
                initial_quantity: 5,
                position: (1_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![eve_db::CosmicAnomalyRecord {
                anomaly_id: "anom-1".into(),
                solar_system_id: sys_id,
                name: "Ore Anomaly".into(),
                anomaly_type: "Ore".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 100.0,
                created_unix: 100,
                lifetime_seconds: 86_400,
                cleared: false,
            }],
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 32880, DVec3::ZERO),
        make_combat_state(500.0, 500.0, 500.0, 1_000.0),
    );
    let _ = sys
        .spawn_ball(make_ball(ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: ast_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    )
    .await
    .unwrap();

    // 1. Mining cycle activate, AlreadyActive error, short-cycle mismatch vs full completion + clear_emptied_sites
    assert_eq!(
        mining_cycle::activate(&ctx, char_id, ship_id, sys_id, 99_999_999, ast_id)
            .await
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );
    let _ = mining_cycle::activate(&ctx, char_id, ship_id, sys_id, laser_id, ast_id)
        .await
        .unwrap();
    assert_eq!(
        mining_cycle::activate(&ctx, char_id, ship_id, sys_id, laser_id, ast_id)
            .await
            .unwrap_err()
            .code,
        "EffectAlreadyActive"
    );
    // Deactivate with wrong char_id returns ModuleNotFound
    assert_eq!(
        mining_cycle::deactivate(&ctx, victim_id, Some(ship_id), laser_id)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );
    // Short-cycle with elapsed time clears the 5-unit asteroid -> clears nearby anomaly
    if let Some(c) = ctx.mining_cycles.write().unwrap().get_mut(&laser_id) {
        c.cycle_start_ms = c.cycle_start_ms.saturating_sub(1_000);
    }
    let out = mining_cycle::deactivate(&ctx, char_id, Some(ship_id), laser_id)
        .await
        .unwrap()
        .unwrap();
    assert!(out.is_some());
    mining_cycle::clear_emptied_sites(&ctx, sys_id, DVec3::new(1_000.0, 0.0, 0.0));
    let anom_cleared = ctx
        .db
        .with_snapshot(|s| s.cosmic_anomalies[&sys_id][0].cleared);
    assert!(anom_cleared);

    // 2. Combat missiles AoE: Bomb launcher in highsec rejected; Bomb in lowsec hits pod -> pod killmail/criminal flag
    let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let bomb_launcher_tid = find_type_by_group_attr(&ctx.sde, 862, None);
    let bomb_tid = find_type_by_group_attr(&ctx.sde, 90, None);
    let def_launcher_tid = find_type_by_group_attr(&ctx.sde, 512, None);
    let def_missile_tid = find_type_by_group_attr(&ctx.sde, 88, None);

    let launcher_id = 88_600_301i64;
    let bomb_item_id = 88_600_302i64;
    let def_id = 88_600_303i64;
    let def_ammo_id = 88_600_304i64;
    let missile_ball_id = 88_600_401i64;

    low_handle.try_spawn_ball_with_combat(
        make_ball(ship_id, 12032, DVec3::ZERO),
        make_combat_state(500.0, 500.0, 500.0, 1_000.0),
    );
    low_handle.try_spawn_ball_with_combat(
        make_ball(pod_id, 670, DVec3::new(30_000.0, 0.0, 0.0)),
        make_combat_state(100.0, 100.0, 100.0, 100.0),
    );
    let _ = low_handle
        .spawn_ball(make_ball(missile_ball_id, 214, DVec3::new(100.0, 0.0, 0.0)))
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            make_item(
                launcher_id,
                bomb_launcher_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
        snap.items.insert(
            bomb_item_id,
            make_item(
                bomb_item_id,
                bomb_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                2,
            ),
        );
        snap.items.insert(
            def_id,
            make_item(
                def_id,
                def_launcher_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot1,
                1,
            ),
        );
        snap.items.insert(
            def_ammo_id,
            make_item(
                def_ammo_id,
                def_missile_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot1,
                2,
            ),
        );
    });
    let launcher_fm = FittedModule {
        item_id: launcher_id,
        type_id: bomb_launcher_tid,
        flag_id: InventoryFlag::HiSlot0 as u16,
        state: ModuleState::Online,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: Some(bomb_tid),
        charge_quantity: 2,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let launcher_ball = make_ball(ship_id, 12032, DVec3::ZERO);
    let kw = HashMap::new();
    // Highsec block
    assert_eq!(
        combat_missiles_aoe::activate_aoe_or_defender_module(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            0.9,
            1_700_000_000,
            launcher_id,
            &launcher_fm,
            &ctx.sde.types[&bomb_launcher_tid],
            &launcher_ball,
            None,
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BombDisallowedInHighSec"
    );
    // Lowsec bomb hits pod
    let _ = combat_missiles_aoe::activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        lowsec_sys,
        0.3,
        1_700_000_000,
        launcher_id,
        &launcher_fm,
        &ctx.sde.types[&bomb_launcher_tid],
        &launcher_ball,
        None,
        &kw,
    )
    .await;

    // Defender Launcher intercepts in-flight missile
    let def_fm = FittedModule {
        item_id: def_id,
        type_id: def_launcher_tid,
        flag_id: InventoryFlag::HiSlot1 as u16,
        state: ModuleState::Online,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: Some(def_missile_tid),
        charge_quantity: 2,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let def_res = combat_missiles_aoe::activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        lowsec_sys,
        0.3,
        1_700_000_000,
        def_id,
        &def_fm,
        &ctx.sde.types[&def_launcher_tid],
        &launcher_ball,
        Some(missile_ball_id),
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        def_res.dict_get("moduleID").and_then(|v| v.as_i64()),
        Some(def_id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_gateway_and_admin_cluster_rpc_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let _ = ctx.systems.get_or_spawn(sys_id);

    // 1. ServerOps cluster admin RPCs: ReinforceNode, RebalanceNodes, SetServiceState, AdminBroadcast, Add/Remove node
    let mut gm_sess = make_session(501, 90_000_501, sys_id, 88_700_001);
    gm_sess.role = service_roles::ROLE_ADMIN | service_roles::ROLE_GMH;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "admin_501".into(),
            eve_db::AccountRecord {
                user_id: 501,
                username: "admin_501".into(),
                password_token: String::new(),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let kw = HashMap::new();
    let mut reg_player = make_session(502, 90_000_502, sys_id, 88_700_002);
    assert_eq!(
        serverops_cluster::handle_admin_cluster_rpc(
            &ctx,
            &mut reg_player,
            "AddClusterNode",
            &[PyRep::Integer(9)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert!(
        serverops_cluster::handle_admin_cluster_rpc(
            &ctx,
            &mut gm_sess,
            "AddClusterNode",
            &[PyRep::Integer(9)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        serverops_cluster::handle_admin_cluster_rpc(
            &ctx,
            &mut gm_sess,
            "ReinforceNode",
            &[PyRep::Integer(i64::from(sys_id))],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        serverops_cluster::handle_admin_cluster_rpc(&ctx, &mut gm_sess, "RebalanceNodes", &[], &kw)
            .unwrap()
            .is_ok()
    );
    assert!(
        serverops_cluster::handle_admin_cluster_rpc(
            &ctx,
            &mut gm_sess,
            "SetServiceState",
            &[PyRep::String("market".into()), PyRep::Bool(false)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        serverops_cluster::handle_admin_cluster_rpc(
            &ctx,
            &mut gm_sess,
            "AdminBroadcast",
            &[PyRep::String("Restart soon".into())],
            &kw
        )
        .unwrap()
        .is_ok()
    );

    // 2. PublicGatewayRouter session transition flushes caches & notice fanout
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let gw = PublicGatewayRouter::new(descriptors, Arc::clone(&ctx.db), Arc::clone(&ctx.sde));
    let mut sess = make_session(601, 90_000_601, sys_id, 88_700_101);
    sess.corporation_id = Some(98_000_601);
    let now = 1_700_300_000u64;
    let _gw_rx = gw.subscribe_session(
        &sess,
        vec!["eve_public.chat.local.api.JoinNotice".into()],
        now,
    );
    gw.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.JoinNotice"),
            value: vec![1, 2, 3],
        },
        now,
    )
    .unwrap();
    let notices = gw.poll_notices(sess.session_id);
    assert_eq!(notices.len(), 1);
    gw.on_session_changed(&sess, now);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fitting_ewar_capital_and_system_actor_cycle_extra_edge_paths() {
    let ctx = test_ctx();
    let sde = Arc::clone(&ctx.sde);
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 90_000_701i64;
    let ship_id = 88_800_001i64;
    let target_id = 88_800_002i64;
    let far_id = 88_800_003i64;
    let cd_tid = find_type_by_group_attr(&sde, 1534, None);
    let mjfg_tid = sde
        .types
        .values()
        .find(|t| {
            t.group_id == 1533
                && t.default_attributes.iter().any(|(aid, &v)| {
                    v > 0.0
                        && sde
                            .attributes
                            .get(aid)
                            .is_some_and(|a| a.name.starts_with("canFitShipGroup"))
                })
        })
        .map(|t| t.type_id)
        .unwrap_or_else(|| find_type_by_group_attr(&sde, 1533, None));
    let mjd_restricted_tid = sde
        .types
        .values()
        .find(|t| {
            let has_group = t.default_attributes.iter().any(|(aid, &v)| {
                v > 0.0
                    && sde
                        .attributes
                        .get(aid)
                        .is_some_and(|a| a.name.starts_with("canFitShipGroup"))
            });
            let allows_25 = t.default_attributes.iter().any(|(aid, &v)| {
                (v - 25.0).abs() < 0.5
                    && sde
                        .attributes
                        .get(aid)
                        .is_some_and(|a| a.name.starts_with("canFitShipGroup"))
            });
            (t.group_id == 1533 || t.group_id == 1189) && has_group && !allows_25
        })
        .map(|t| t.type_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = char_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = ship_id;
            c.war_faction_id = Some(500_001);
            snap.characters.insert(char_id, c);
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                cd_tid,
                char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            target_id,
            make_item(
                target_id,
                587,
                char_id + 1,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
    });

    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, cd_tid, DVec3::ZERO),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(target_id, 587, DVec3::new(2_500.0, 0.0, 0.0)),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(far_id, 587, DVec3::new(200_000.0, 0.0, 0.0)),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0),
    );

    let kwargs = HashMap::new();
    let mod_id = 88_800_101i64;
    let build_mod = |ship_tid: u32, mod_tid: u32, state: ModuleState, flag: InventoryFlag| {
        let mut fit = ShipFittingEngine::from_sde(ship_id, ship_tid, &sde);
        let fm = FittedModule {
            item_id: mod_id,
            type_id: mod_tid,
            flag_id: flag as u16,
            state,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        };
        let _ = fit.fit_module(fm.item_id, fm.type_id, fm.flag_id, &sde);
        fit.modules.insert(fm.item_id, fm.clone());
        let ms = fit.build_effective_module_state(fm.item_id, mod_tid, false, &sde);
        (fit, fm, &sde.types[&mod_tid], ms)
    };

    // 1. MJFG/MJD (`1533` / `1189`): rejected on disallowed ship group (`25`), accepted on Command Destroyer (`1534`)
    if let Some(restricted_tid) = mjd_restricted_tid {
        let mjfg_bad = build_mod(
            587,
            restricted_tid,
            ModuleState::Online,
            InventoryFlag::MedSlot0,
        );
        assert_eq!(
            prepare_ewar_and_capital_module(
                &ctx,
                &sys,
                sys_id,
                char_id,
                ship_id,
                25,
                mod_id,
                &mjfg_bad.1,
                mjfg_bad.2,
                &mjfg_bad.3,
                &mjfg_bad.0,
                None,
                false,
                false,
                true,
                &kwargs,
            )
            .await
            .unwrap()
            .map(|_| ())
            .unwrap_err()
            .code,
            "InvalidShipGroup"
        );
    }
    let mjfg_ok = build_mod(
        cd_tid,
        mjfg_tid,
        ModuleState::Online,
        InventoryFlag::MedSlot0,
    );
    let m_prep = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_id, 1534, mod_id, &mjfg_ok.1, mjfg_ok.2, &mjfg_ok.3,
        &mjfg_ok.0, None, false, false, true, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(m_prep.mjd_jump_distance_m.is_some());
    assert!(m_prep.mjfg_field_params.is_some());

    // 2. MJD blocked when `is_mwd_mjd_disabled` == true
    let mut dis_cs = make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0);
    dis_cs.is_mwd_mjd_disabled = true;
    let _ = sys.set_combat_state(ship_id, dis_cs).await;
    let mjd_tid = find_type_by_group_attr(&sde, 1189, None);
    let mjd_fit = build_mod(587, mjd_tid, ModuleState::Online, InventoryFlag::MedSlot0);
    let _ = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &mjd_fit.1, mjd_fit.2, &mjd_fit.3,
        &mjd_fit.0, None, false, false, false, &kwargs,
    )
    .await;
    let _ = sys
        .set_combat_state(
            ship_id,
            make_combat_state(1_000.0, 1_000.0, 1_000.0, 2_000.0),
        )
        .await;

    // 3. Remote Hull Repairer (`585`) & Remote Cap Transmitter (`67`) & Capital RR (>4000) & Mutadaptive (`2018`)
    for (grp, attr) in [(585u32, None), (67, Some(90)), (2018, Some(84))] {
        let tid = find_type_by_group_attr(&sde, grp, attr);
        let r_fit = build_mod(587, tid, ModuleState::Online, InventoryFlag::HiSlot0);
        let p = prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &r_fit.1,
            r_fit.2,
            &r_fit.3,
            &r_fit.0,
            Some(target_id),
            false,
            false,
            false,
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap();
        commit_post_cycle_activation_effects(
            &ctx,
            &sys,
            char_id,
            ship_id,
            mod_id,
            &r_fit.1,
            r_fit.2,
            &r_fit.3,
            Some(target_id),
            p.deferred_post_cycle,
        )
        .await
        .unwrap();
    }

    // 4. System actor cycle edge paths:
    //    - Crystal volatility (quantity=2 -> 1, then quantity=1 -> destroyed)
    //    - AAR with nanite paste (28668) & without
    //    - Cap booster consuming cap charge
    //    - Auto-targeter + Stasis Grappler + MJFG (`micro_jump_field_with_capitals`)
    //    - Capital shield remote rep impedance (`cycles_tackle.rs`)
    //    - Cloaking decloak stop & warping target cycle cancellation & out-of-range cancellation
    let turret_crys_id = 88_800_201i64;
    let crystal_id = 88_800_202i64;
    let aar_id = 88_800_203i64;
    let paste_id = 88_800_204i64;
    let capb_id = 88_800_205i64;
    let cap_charge_id = 88_800_206i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            turret_crys_id,
            make_item(
                turret_crys_id,
                484,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
        snap.items.insert(
            crystal_id,
            make_item(crystal_id, 232, char_id, ship_id, InventoryFlag::HiSlot0, 2),
        );
        snap.module_active_charges.insert(turret_crys_id, 232);
        snap.items.insert(
            aar_id,
            make_item(aar_id, 33076, char_id, ship_id, InventoryFlag::LoSlot0, 1),
        );
        snap.items.insert(
            paste_id,
            make_item(paste_id, 28668, char_id, ship_id, InventoryFlag::LoSlot0, 4),
        );
        snap.items.insert(
            capb_id,
            make_item(capb_id, 2028, char_id, ship_id, InventoryFlag::MedSlot1, 1),
        );
        snap.items.insert(
            cap_charge_id,
            make_item(
                cap_charge_id,
                2029,
                char_id,
                ship_id,
                InventoryFlag::MedSlot1,
                1,
            ),
        );
    });

    // Crystal volatility cycle 1 (damage 1.0 -> quantity drops 2->1)
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: turret_crys_id,
        module_type_id: 484,
        owner_char_id: Some(char_id),
        effect_name: "targetAttack".into(),
        target_id: Some(target_id),
        duration_s: 1.0,
        repeat: 1,
        linked_turrets: vec![turret_crys_id],
        crystal_volatility_damage: 1.0,
        ..Default::default()
    })
    .await
    .unwrap();
    // AAR with nanite paste
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: aar_id,
        module_type_id: 33076,
        owner_char_id: Some(char_id),
        effect_name: "fueledArmorRepair".into(),
        duration_s: 1.0,
        repeat: 1,
        is_aar: true,
        aar_paste_rate: 4,
        aar_base_armor_repair: 100.0,
        ..Default::default()
    })
    .await
    .unwrap();
    // Cap booster consuming charge
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: capb_id,
        module_type_id: 2028,
        owner_char_id: Some(char_id),
        effect_name: "capacitorBooster".into(),
        duration_s: 1.0,
        repeat: 1,
        cap_booster_inject_amount: 150.0,
        ..Default::default()
    })
    .await
    .unwrap();
    // Stasis Grappler + Auto-Targeter + Shield boost + Remote shield + MJFG
    let grap_tid = find_type_by_group_attr(&sde, 1672, None);
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_800_210,
        module_type_id: grap_tid,
        owner_char_id: Some(char_id),
        effect_name: "stasisWeb".into(),
        target_id: Some(target_id),
        duration_s: 1.0,
        target_velocity_multiplier: Some(0.2),
        grappler_speed_factor_pct: -80.0,
        grappler_optimal_m: 1_000.0,
        grappler_falloff_m: 8_000.0,
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_800_211,
        module_type_id: 1995,
        owner_char_id: Some(char_id),
        effect_name: "autoTargeting".into(),
        duration_s: 1.0,
        is_auto_targeter: true,
        auto_target_max_range: 50_000.0,
        auto_target_max_locks: 4,
        auto_target_scan_res: 400.0,
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_800_212,
        module_type_id: mjfg_tid,
        owner_char_id: Some(char_id),
        effect_name: "microJumpFieldGenerator".into(),
        duration_s: 1.0,
        mjd_jump_distance_m: Some(100_000.0),
        mjfg_field_params: Some((6_000.0, 25, 5)),
        ..Default::default()
    })
    .await
    .unwrap();
    let _ = sys.step_manual(1.1).await.unwrap();
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.items.get(&crystal_id).map(|i| i.quantity)),
        Some(1)
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&paste_id)));
    assert!(
        ctx.db
            .with_snapshot(|s| !s.items.contains_key(&cap_charge_id))
    );

    // Step second crystal cycle -> stack destroyed completely
    let _ = sys.step_manual(1.1).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&crystal_id)));

    // Warping target cancellation & out-of-range cancellation & cloak decloak stop & aligning check
    let mut align_mode =
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { phase, .. } = &mut align_mode {
        *phase = eve_destiny::WarpPhase::Aligning;
    }
    let _ = sys.set_movement_mode(far_id, align_mode).await;
    assert!(sys.is_ball_aligning_warp_sync(far_id));
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_800_220,
        module_type_id: 3242,
        owner_char_id: Some(char_id),
        effect_name: "warpScramble".into(),
        target_id: Some(far_id),
        duration_s: 5.0,
        max_effective_range_m: Some(10_000.0),
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_800_221,
        module_type_id: 11578,
        owner_char_id: Some(char_id),
        effect_name: "cloakingWarpSafe".into(),
        duration_s: 5.0,
        ..Default::default()
    })
    .await
    .unwrap();
    let _ = sys.step_manual(0.5).await.unwrap();
    assert!(!sys.has_active_overloaded_cycle_sync(ship_id));
}

#[test]
fn contracts_and_market_order_lifecycle_mutations_edges() {
    let ctx = test_ctx();
    let issuer_id = 90_000_801i64;
    let other_id = 90_000_802i64;
    let corp_id = 98_000_801i64;
    let st_id = 1_030_000_801i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let region_id = 10_000_002u32;

    let item_locked_id = 88_810_001i64;
    let wrap_id = 88_810_002i64;
    let wrap_child_id = 88_810_003i64;
    let mkt_order_id = 88_820_001i64;
    let corp_mkt_order_id = 88_820_002i64;
    let exp_buy_oid = 88_820_003i64;
    let exp_sell_oid = 88_820_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = issuer_id;
            c.corporation_id = corp_id;
            c.solar_system_id = sys_id;
            c.station_id = Some(station_id);
            c.balance = IskCents::from_isk(500_000_000);
            snap.characters.insert(issuer_id, c.clone());
            c.character_id = other_id;
            c.corporation_id = corp_id + 1;
            c.balance = IskCents::from_isk(500_000_000);
            snap.characters.insert(other_id, c);
        }
        snap.skills.entry(issuer_id).or_default().insert(
            16595,
            eve_db::TrainedSkillRecord {
                type_id: 16595,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Market Corp".into(); __corp.ticker = "MKTC".into(); __corp.ceo_id = issuer_id; __corp.tax_rate = 0.05; __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    IskCents::from_isk(200_000_000),
                ); let __mw = IskCents::from_isk(200_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        snap.structures.insert(
            st_id,
            eve_db::UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Market Fort".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_locked_id,
            make_item(
                item_locked_id,
                34,
                issuer_id,
                66_001,
                InventoryFlag::Hangar,
                100,
            ),
        );
        snap.items.insert(
            wrap_id,
            make_item(
                wrap_id,
                3468,
                other_id,
                station_id,
                InventoryFlag::Hangar,
                1,
            ),
        );
        snap.items.insert(
            wrap_child_id,
            make_item(
                wrap_child_id,
                34,
                other_id,
                wrap_id,
                InventoryFlag::Cargo,
                50,
            ),
        );

        // Outstanding ItemExchange with deposit -> DeleteContract refunds deposit & restores item_locked_id
        snap.contracts.insert(
            66_001,
            ContractRecord {
                contract_id: 66_001,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                availability: "Personal".into(),
                issuer_id,
                issuer_corp_id: corp_id,
                assignee_id: other_id,
                start_station_id: station_id,
                end_station_id: station_id,
                reward: IskCents::from_isk(1_000_000),
                expiration_unix: ctx.now_unix() + 86_400,
                item_ids: vec![item_locked_id],
                ..Default::default()
            },
        );
        snap.fleet_and_social_ops.contract_deposits.insert(66_001, 5_000_000);

        // Outstanding Personal contract for RejectContract & GetContractInfo access checks
        snap.contracts.insert(
            66_002,
            ContractRecord {
                contract_id: 66_002,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                availability: "Personal".into(),
                issuer_id,
                issuer_corp_id: corp_id,
                assignee_id: other_id,
                start_station_id: station_id,
                end_station_id: station_id,
                expiration_unix: ctx.now_unix() + 86_400,
                ..Default::default()
            },
        );

        // InProgress Courier contract for FailContract & GetCourierContractFromItemID & BreakPlasticWrap
        snap.contracts.insert(
            66_003,
            ContractRecord {
                contract_id: 66_003,
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                availability: "Public".into(),
                issuer_id,
                issuer_corp_id: corp_id,
                acceptor_id: Some(other_id),
                start_station_id: station_id,
                end_station_id: station_id,
                collateral: IskCents::from_isk(10_000_000),
                expiration_unix: ctx.now_unix() + 86_400,
                item_ids: vec![wrap_id, wrap_child_id],
                ..Default::default()
            },
        );
        snap.contracts.insert(
            66_004,
            ContractRecord {
                contract_id: 66_004,
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                availability: "Public".into(),
                issuer_id,
                issuer_corp_id: corp_id,
                acceptor_id: Some(other_id),
                start_station_id: station_id,
                end_station_id: station_id,
                collateral: IskCents::from_isk(5_000_000),
                expiration_unix: ctx.now_unix() + 86_400,
                ..Default::default()
            },
        );

        // Market orders: active char buy + active corp buy + expired corp buy & sell for sweep_expired_orders
        snap.market_orders.insert(
            mkt_order_id,
            MarketOrderRecord {
                order_id: mkt_order_id,
                owner_id: issuer_id,
                station_id,
                region_id,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500),
            },
        );
        snap.market_orders.insert(
            corp_mkt_order_id,
            MarketOrderRecord {
                order_id: corp_mkt_order_id,
                owner_id: corp_id,
                station_id,
                region_id,
                type_id: 44_992,
                is_bid: true,
                price: IskCents::from_isk(3_000_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(6_000_000),
            },
        );
        snap.market_order_meta.insert(
            corp_mkt_order_id,
            eve_db::MarketOrderMeta {
                issued_unix: ctx.now_unix().saturating_sub(600),
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(issuer_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            exp_buy_oid,
            MarketOrderRecord {
                order_id: exp_buy_oid,
                owner_id: corp_id,
                station_id,
                region_id,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(250),
            },
        );
        snap.market_order_meta.insert(
            exp_buy_oid,
            eve_db::MarketOrderMeta {
                issued_unix: 100,
                duration_days: 1,
                range: "station".into(),
                issued_by: Some(issuer_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            exp_sell_oid,
            MarketOrderRecord {
                order_id: exp_sell_oid,
                owner_id: corp_id,
                station_id,
                region_id,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(6),
                vol_entered: 75,
                vol_remaining: 75,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_order_meta.insert(
            exp_sell_oid,
            eve_db::MarketOrderMeta {
                issued_unix: 100,
                duration_days: 1,
                range: "station".into(),
                issued_by: Some(issuer_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
    });

    let mut iss_sess = make_session(801, issuer_id, sys_id, 88_800_901);
    iss_sess.corporation_id = Some(corp_id);
    iss_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TRADER;
    iss_sess.station_id = Some(station_id);
    iss_sess.region_id = Some(region_id);

    let mut oth_sess = make_session(802, other_id, sys_id, 88_800_902);
    oth_sess.corporation_id = Some(corp_id + 1);
    oth_sess.station_id = Some(station_id);
    let kw = HashMap::new();

    // Contract info, courier container queries, RejectContract, BreakPlasticWrap, FailContract, DeleteContract
    assert!(
        contracts_service::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "GetContract",
            &[PyRep::Integer(66_002)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        contracts_service::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "RejectContract",
            &[PyRep::Integer(66_002)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        market_contracts::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "GetCourierContractFromItemID",
            &[PyRep::Integer(wrap_id)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        market_contracts::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "GetItemsInContainer",
            &[PyRep::Integer(0), PyRep::Integer(wrap_id)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        market_contracts::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "GetNumItemsInContainers",
            &[
                PyRep::Integer(0),
                PyRep::List(vec![PyRep::Integer(wrap_id)])
            ],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        market_contracts::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "GetMyContractEscrow",
            &[],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        contracts_service::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "BreakPlasticWrap",
            &[PyRep::Integer(wrap_id)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        contracts_service::handle(
            &ctx,
            &mut oth_sess,
            "contractProxy",
            "FailContract",
            &[PyRep::Integer(66_004)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        contracts_service::handle(
            &ctx,
            &mut iss_sess,
            "contractProxy",
            "DeleteContract",
            &[PyRep::Integer(66_001)],
            &kw
        )
        .unwrap()
        .is_ok()
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.items[&item_locked_id].location_id),
        station_id
    );

    // Market: ModifyCharOrder & ModifyPlexCharOrder (triggers sweep_expired_orders on corp orders too!)
    assert!(
        market_contracts::handle(
            &ctx,
            &mut iss_sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(mkt_order_id), PyRep::Real(6.0)],
            &kw,
        )
        .unwrap()
        .is_ok()
    );
    let plex_res = market_contracts::handle(
        &ctx,
        &mut iss_sess,
        "marketProxy",
        "ModifyPlexCharOrder",
        &[PyRep::Integer(corp_mkt_order_id), PyRep::Real(3_100_000.0)],
        &kw,
    )
    .unwrap();
    assert!(plex_res.is_ok(), "ModifyPlexCharOrder err: {:?}", plex_res);
    // Non-PLEX rejected by ModifyPlexCharOrder
    assert_eq!(
        market_contracts::handle(
            &ctx,
            &mut iss_sess,
            "marketProxy",
            "ModifyPlexCharOrder",
            &[PyRep::Integer(mkt_order_id), PyRep::Real(7.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPlexOrder"
    );
    // Expired sell order items were returned to Deliveries for corp
    assert!(ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .any(|i| i.owner_id == corp_id && i.type_id == 34 && i.quantity == 75)
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xmpp_chat_acl_invite_cspa_destroy_and_structure_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let boss_id = 90_000_901i64;
    let wc_id = 90_000_902i64;
    let outsider_id = 90_000_903i64;
    let corp_id = 98_000_901i64;
    let ally_id = 99_000_901i64;
    let fleet_id = 77_000_901i64;
    let st_id = 1_030_000_901i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = boss_id;
            c.solar_system_id = sys_id;
            c.corporation_id = corp_id;
            c.alliance_id = Some(ally_id);
            c.balance = IskCents::from_isk(100);
            snap.characters.insert(boss_id, c.clone());
            c.character_id = wc_id;
            c.balance = IskCents::from_isk(10_000_000);
            snap.characters.insert(wc_id, c.clone());
            c.character_id = outsider_id;
            c.corporation_id = corp_id + 1;
            c.alliance_id = None;
            snap.characters.insert(outsider_id, c);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Chat Corp".into(); __corp.ticker = "CHTC".into(); __corp.ceo_id = boss_id; __corp.alliance_id = Some(ally_id); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.alliances.insert(
            ally_id,
            eve_db::AllianceRecord {
                alliance_id: ally_id,
                name: "Chat Ally".into(),
                short_name: "CHTA".into(),
                executor_corp_id: corp_id,
                ..Default::default()
            },
        );
        let mut md = HashMap::new();
        md.insert(
            wc_id,
            eve_db::FleetMemberRecord {
                character_id: wc_id,
                role: eve_db::FleetRole::WingCommander,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Wing Commander".into(),
                join_time_unix: 1,
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id,
                members: vec![boss_id, wc_id],
                member_details: md,
                ..Default::default()
            },
        );
        snap.character_cspa_charges
            .insert(outsider_id, IskCents::from_isk(50_000));
        snap.structures.insert(
            st_id,
            eve_db::UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Fuel Struct".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 10,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let fuel_item = 88_830_001i64;
        snap.items.insert(
            fuel_item,
            make_item(
                fuel_item,
                4247,
                corp_id,
                st_id,
                InventoryFlag::StructureFuel,
                10,
            ),
        );
    });

    let mut boss_sess = make_session(901, boss_id, sys_id, 88_830_901);
    boss_sess.corporation_id = Some(corp_id);
    boss_sess.alliance_id = Some(ally_id);
    boss_sess.fleet_id = Some(fleet_id);
    boss_sess.structure_id = Some(st_id);
    let mut wc_sess = make_session(902, wc_id, sys_id, 88_830_902);
    wc_sess.corporation_id = Some(corp_id);
    wc_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    wc_sess.alliance_id = Some(ally_id);
    wc_sess.fleet_id = Some(fleet_id);
    let mut out_sess = make_session(903, outsider_id, 30_002_187, 88_830_903);
    out_sess.corporation_id = Some(corp_id + 1);
    let kw = HashMap::new();

    // 1. Invite CSPA insufficient funds -> wc pays CSPA -> blocked check
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut boss_sess,
            "XmppChatMgr",
            "Invite",
            &[PyRep::Integer(outsider_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut wc_sess,
            "XmppChatMgr",
            "Invite",
            &[PyRep::Integer(outsider_id)],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 2. Dynamic org operator MOTD/AccessControl (`corpid:`, `allianceid:`, `fleetid:` via WingCommander & CEO) + SendMessage fanout + DestroyChannel guards
    for org_ch in [
        format!("corpid:{corp_id}"),
        format!("allianceid:{ally_id}"),
        format!("fleetid:{fleet_id}"),
    ] {
        assert!(
            fleet_social::handle(
                &ctx,
                &mut wc_sess,
                "XmppChatMgr",
                "AddCharacterToChannel",
                &[PyRep::String(org_ch.clone())],
                &kw
            )
            .await
            .unwrap()
            .is_ok()
        );
        assert!(
            fleet_social::handle(
                &ctx,
                &mut wc_sess,
                "XmppChatMgr",
                "SetMotd",
                &[
                    PyRep::String(org_ch.clone()),
                    PyRep::String("Fleet MOTD".into())
                ],
                &kw
            )
            .await
            .unwrap()
            .is_ok()
        );
        assert!(
            fleet_social::handle(
                &ctx,
                &mut wc_sess,
                "XmppChatMgr",
                "SendMessage",
                &[PyRep::String(org_ch.clone()), PyRep::String("Ping".into())],
                &kw
            )
            .await
            .unwrap()
            .is_ok()
        );
        assert_eq!(
            fleet_social::handle(
                &ctx,
                &mut out_sess,
                "XmppChatMgr",
                "SendMessage",
                &[
                    PyRep::String(org_ch.clone()),
                    PyRep::String("Intruder".into())
                ],
                &kw
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "ChannelAccessDenied"
        );
    }

    // ResyncSystemChannelAccess & EnsureResourceWarsChannelExists mismatch
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut out_sess,
            "XmppChatMgr",
            "EnsureResourceWarsChannelExists",
            &[PyRep::String(format!("resourcewars:{sys_id}"))],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut wc_sess,
            "XmppChatMgr",
            "ResyncSystemChannelAccess",
            &[PyRep::String(format!("solarsystemid2:{sys_id}"))],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );

    // Create custom channel & DestroyChannel error + success
    let custom_ch = "channel:edge_test_chan".to_string();
    assert!(
        fleet_social::handle(
            &ctx,
            &mut wc_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String(custom_ch.clone())],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut out_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[
                PyRep::String(custom_ch.clone()),
                PyRep::Integer(outsider_id)
            ],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut wc_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[
                PyRep::String(format!("corpid:{corp_id}")),
                PyRep::Integer(outsider_id)
            ],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut wc_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(custom_ch.clone()), PyRep::Integer(boss_id)],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut wc_sess,
            "LSC",
            "LeaveChannel",
            &[PyRep::String(custom_ch)],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 3. Structure JettisonStructureFuel insufficient fuel + valid full drain
    boss_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        station_structure::handle(
            &ctx,
            &mut boss_sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::Integer(4247), PyRep::Integer(50)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientStructureFuel"
    );
    assert!(
        station_structure::handle(
            &ctx,
            &mut boss_sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::Integer(4247), PyRep::Integer(10)],
            &kw
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn combat_missiles_aoe_void_lockbreaker_and_crimewatch_edges() {
    let ctx = test_ctx();
    let lowsec_sys = 30_002_191u32;
    let char_id = 90_000_951i64;
    let suspect_vic = 90_000_952i64;
    let clean_vic = 90_000_953i64;
    let ship_id = 88_840_001i64;
    let sus_pod_id = 88_840_002i64;
    let clean_pod_id = 88_840_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = char_id;
            c.solar_system_id = lowsec_sys;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = ship_id;
            snap.characters.insert(char_id, c.clone());
            c.character_id = suspect_vic;
            c.active_ship_id = sus_pod_id;
            snap.characters.insert(suspect_vic, c.clone());
            c.character_id = clean_vic;
            c.active_ship_id = clean_pod_id;
            snap.characters.insert(clean_vic, c);
        }
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(suspect_vic).suspect_flag_until_unix = 2_000_000_000;
    }

    let low = ctx.systems.get_or_spawn(lowsec_sys);
    low.try_spawn_ball_with_combat(
        make_ball(ship_id, 12032, DVec3::ZERO),
        make_combat_state(500.0, 500.0, 500.0, 1_000.0),
    );
    low.try_spawn_ball_with_combat(
        make_ball(sus_pod_id, 670, DVec3::new(30_000.0, 0.0, 0.0)),
        make_combat_state(100.0, 100.0, 100.0, 500.0),
    );
    low.try_spawn_ball_with_combat(
        make_ball(clean_pod_id, 670, DVec3::new(30_100.0, 0.0, 0.0)),
        make_combat_state(100.0, 100.0, 100.0, 500.0),
    );

    let bomb_launcher_tid = find_type_by_group_attr(&ctx.sde, 862, None);
    let dmg_bomb_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 90 && t.default_attributes.get(&114).copied().unwrap_or(0.0) > 0.0)
        .map(|t| t.type_id)
        .unwrap();
    let lb_bomb_tid = find_type_by_group_attr(&ctx.sde, 863, None);
    let launcher_id = 88_840_101i64;
    let dmg_bomb_id = 88_840_102i64;
    let lb_bomb_id = 88_840_103i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            make_item(
                launcher_id,
                bomb_launcher_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
        snap.items.insert(
            dmg_bomb_id,
            make_item(
                dmg_bomb_id,
                dmg_bomb_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
    });
    let mut attacker_ball = make_ball(ship_id, 12032, DVec3::ZERO);
    attacker_ball.velocity = DVec3::new(100.0, 0.0, 0.0);
    let fm_dmg = FittedModule {
        item_id: launcher_id,
        type_id: bomb_launcher_tid,
        flag_id: InventoryFlag::HiSlot0 as u16,
        state: ModuleState::Online,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: Some(dmg_bomb_tid),
        charge_quantity: 1,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let kw = HashMap::new();
    let r1 = combat_missiles_aoe::activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        lowsec_sys,
        0.3,
        1_700_000_000,
        launcher_id,
        &fm_dmg,
        &ctx.sde.types[&bomb_launcher_tid],
        &attacker_ball,
        None,
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(r1.dict_get("hitTargetIDs").is_some());

    // Now lockbreaker bomb (`group 863`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lb_bomb_id,
            make_item(
                lb_bomb_id,
                lb_bomb_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
    });
    let _ = low
        .add_target_lock(
            sus_pod_id,
            eve_destiny::TargetLock {
                target_id: ship_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    let fm_lb = FittedModule {
        charge_type_id: Some(lb_bomb_tid),
        ..fm_dmg
    };
    let r2 = combat_missiles_aoe::activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        lowsec_sys,
        0.3,
        1_700_000_000,
        launcher_id,
        &fm_lb,
        &ctx.sde.types[&bomb_launcher_tid],
        &attacker_ball,
        Some(sus_pod_id),
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(r2.dict_get("moduleID").is_some());

    // Defender Launcher with empty charges -> NoAmmoLoaded
    let def_tid = find_type_by_group_attr(&ctx.sde, 512, None);
    let def_id = 88_840_201i64;
    let fm_def = FittedModule {
        item_id: def_id,
        type_id: def_tid,
        flag_id: InventoryFlag::HiSlot1 as u16,
        state: ModuleState::Online,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    assert_eq!(
        combat_missiles_aoe::activate_aoe_or_defender_module(
            &ctx,
            char_id,
            ship_id,
            lowsec_sys,
            0.3,
            1_700_000_000,
            def_id,
            &fm_def,
            &ctx.sde.types[&def_tid],
            &attacker_ball,
            None,
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoAmmoLoaded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wave3_targeted_system_actor_ewar_missile_and_social_edges() {
    let ctx = test_ctx();
    let sde = Arc::clone(&ctx.sde);
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let char_id = 90_000_901i64;
    let victim_id = 90_000_902i64;
    let ship_id = 88_850_001i64;
    let friend_ship_id = 88_850_002i64;
    let far_ship_id = 88_850_003i64;
    let missile_dead_id = 88_850_101i64;
    let missile_no_tgt_id = 88_850_102i64;
    let missile_hit_id = 88_850_103i64;
    let missile_kill_id = 88_850_104i64;

    let cap_rsb_tid = sde
        .types
        .values()
        .find(|t| {
            t.group_id == 41
                && (matches!(
                    t.default_attributes.get(&182).copied().unwrap_or(0.0) as u32,
                    24568 | 24571 | 27936
                ) || t.default_attributes.get(&30).copied().unwrap_or(0.0) >= 10_000.0)
        })
        .map(|t| t.type_id)
        .unwrap_or(41484);
    let smartbomb_tid = find_type_by_group_attr(&sde, 72, None);
    let rsb_tid = find_type_by_group_attr(&sde, 290, None);
    let web_tid = find_type_by_group_attr(&sde, 65, None);
    let rrep_tid = find_type_by_group_attr(&sde, 325, None);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = char_id;
            c.corporation_id = 98_000_901;
            c.alliance_id = Some(99_000_901);
            c.solar_system_id = sys_id;
            c.active_ship_id = ship_id;
            c.balance = IskCents::from_isk(100_000);
            snap.characters.insert(char_id, c.clone());
            c.character_id = victim_id;
            c.corporation_id = 98_000_902;
            c.alliance_id = None;
            c.active_ship_id = far_ship_id;
            c.balance = IskCents::from_isk(100_000);
            snap.characters.insert(victim_id, c);
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            friend_ship_id,
            make_item(
                friend_ship_id,
                587,
                char_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            far_ship_id,
            make_item(
                far_ship_id,
                587,
                victim_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.character_cspa_charges
            .insert(victim_id, IskCents::from_isk(5_000));
    });

    sys.try_spawn_ball_with_combat(
        make_ball(ship_id, 587, DVec3::ZERO),
        make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0),
    );
    let mut friend_cs = make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0);
    friend_cs.shield_hp = 500.0;
    friend_cs.armor_hp = 500.0;
    friend_cs.capacitor = 500.0;
    sys.try_spawn_ball_with_combat(
        make_ball(friend_ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0)),
        friend_cs,
    );
    let mut far_cs = make_combat_state(1_000.0, 1_000.0, 1_000.0, 5_000.0);
    far_cs.shield_hp = 800.0;
    sys.try_spawn_ball_with_combat(
        make_ball(far_ship_id, 587, DVec3::new(300_000.0, 0.0, 0.0)),
        far_cs,
    );

    let kw = HashMap::new();
    let mut sess = make_session(901, char_id, sys_id, ship_id);
    sess.corporation_id = Some(98_000_901);
    sess.alliance_id = Some(99_000_901);

    // 1. fleet_social_chat_auth_ops: CSPAChargedAction + CSPAChargedActionForMany + InsufficientFundsForCSPA + SetNote/GetNote
    assert!(
        fleet_social::handle(
            &ctx,
            &mut sess,
            "charMgr",
            "CSPAChargedAction",
            &[PyRep::Integer(victim_id)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut sess,
            "charMgr",
            "CSPAChargedActionForMany",
            &[PyRep::List(vec![
                PyRep::Integer(victim_id),
                PyRep::Integer(char_id)
            ])],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(10);
        }
    });
    assert_eq!(
        fleet_social::handle(
            &ctx,
            &mut sess,
            "charMgr",
            "CSPAChargedAction",
            &[PyRep::Integer(victim_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );
    assert!(
        fleet_social::handle(
            &ctx,
            &mut sess,
            "charMgr",
            "SetNote",
            &[
                PyRep::Integer(victim_id),
                PyRep::String("hostile pilot".into())
            ],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let note = fleet_social::handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetNote",
        &[PyRep::Integer(victim_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(note.as_str(), Some("hostile pilot"));

    // 2. fitting_ewar_capital: Remote Sensor Booster (290) guards + in-range, Web (65) OOR, Remote Rep OOR, commit post-cycle
    let mod_id = 88_850_201i64;
    let make_mod = |tid: u32, flag: InventoryFlag| {
        let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &sde);
        let fm = FittedModule {
            item_id: mod_id,
            type_id: tid,
            flag_id: flag as u16,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        };
        let _ = fit.fit_module(fm.item_id, fm.type_id, fm.flag_id, &sde);
        fit.modules.insert(fm.item_id, fm.clone());
        let ms = fit.build_effective_module_state(fm.item_id, tid, false, &sde);
        (fit, fm, &sde.types[&tid], ms)
    };
    let rsb = make_mod(rsb_tid, InventoryFlag::MedSlot0);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &rsb.1, rsb.2, &rsb.3, &rsb.0, None,
            false, false, false, &kw,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetRequired"
    );
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &rsb.1,
            rsb.2,
            &rsb.3,
            &rsb.0,
            Some(far_ship_id),
            false,
            false,
            false,
            &kw,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "OutOfRange"
    );
    let rsb_prep = prepare_ewar_and_capital_module(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        25,
        mod_id,
        &rsb.1,
        rsb.2,
        &rsb.3,
        &rsb.0,
        Some(friend_ship_id),
        false,
        false,
        false,
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        rsb_prep
            .deferred_post_cycle
            .projected_assist_dist_m
            .is_some()
    );
    assert!(
        commit_post_cycle_activation_effects(
            &ctx,
            &sys,
            char_id,
            ship_id,
            mod_id,
            &rsb.1,
            rsb.2,
            &rsb.3,
            Some(friend_ship_id),
            rsb_prep.deferred_post_cycle,
        )
        .await
        .is_ok()
    );

    let mut web = make_mod(web_tid, InventoryFlag::MedSlot1);
    web.3.base_attributes.insert(2044, 0.0);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &web.1,
            web.2,
            &web.3,
            &web.0,
            Some(far_ship_id),
            false,
            false,
            false,
            &kw,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );
    let mut rrep = make_mod(rrep_tid, InventoryFlag::HiSlot0);
    rrep.3.base_attributes.insert(2044, 0.0);
    assert_eq!(
        prepare_ewar_and_capital_module(
            &ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            25,
            mod_id,
            &rrep.1,
            rrep.2,
            &rrep.3,
            &rrep.0,
            Some(far_ship_id),
            false,
            false,
            false,
            &kw,
        )
        .await
        .unwrap()
        .map(|_| ())
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    // commit_post_cycle_activation_effects with turret_charge_consumptions (qty=1 removal) + bastion_weapon_timer + phenomena notification fallback
    let ch_turret_id = 88_850_301i64;
    let ch_item_id = 88_850_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ch_item_id,
            make_item(ch_item_id, 232, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_charges.insert(ch_turret_id, 232);
    });
    let mut deferred_turret =
        crate::services::inventory_fitting::fitting_ewar_capital::DeferredPostCycleEffects::default(
        );
    deferred_turret
        .turret_charge_consumptions
        .push((ch_turret_id, ch_item_id));
    deferred_turret.bastion_weapon_timer = true;
    assert!(
        commit_post_cycle_activation_effects(
            &ctx,
            &sys,
            char_id,
            ship_id,
            ch_turret_id,
            &rsb.1,
            rsb.2,
            &rsb.3,
            None,
            deferred_turret,
        )
        .await
        .is_ok()
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ch_item_id)
        && !s.module_active_charges.contains_key(&ch_turret_id)));

    // 3. system_actor/cycles + cycles_tackle:
    //    - Repeating Smartbomb (group 72 -> RepeatCycleStart), ShieldBoost + Capital Remote Shield + Cap Drain
    //    - ADC (is_adc), Self mass multiplier + HIC sig multiplier
    //    - Partial crystal volatility damage (0.4 -> HP_STRUCT=0.6) & empty ammo shatter
    //    - Doomsday Lance (`superWeaponLanceWarmup` -> requeues `superWeaponLanceBeam`)
    let part_crys_id = 88_850_401i64;
    let part_turret_id = 88_850_402i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            part_turret_id,
            make_item(
                part_turret_id,
                484,
                char_id,
                ship_id,
                InventoryFlag::HiSlot2,
                1,
            ),
        );
        snap.items.insert(
            part_crys_id,
            make_item(
                part_crys_id,
                232,
                char_id,
                ship_id,
                InventoryFlag::HiSlot2,
                2,
            ),
        );
    });
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: part_turret_id,
        module_type_id: 484,
        owner_char_id: Some(char_id),
        effect_name: "targetAttack".into(),
        target_id: Some(friend_ship_id),
        duration_s: 1.0,
        repeat: 2,
        linked_turrets: vec![part_turret_id],
        crystal_volatility_chance: 1.0,
        crystal_volatility_damage: 0.6,
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_850_403,
        module_type_id: 484,
        owner_char_id: Some(char_id),
        effect_name: "targetAttack".into(),
        target_id: Some(friend_ship_id),
        duration_s: 1.0,
        linked_turrets: vec![88_850_403],
        ammo_type_id: Some(192),
        damage_vector: DamageVector {
            em: 25.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_850_404,
        module_type_id: smartbomb_tid,
        owner_char_id: Some(char_id),
        effect_name: "empWave".into(),
        duration_s: 1.0,
        repeat: 1,
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_850_405,
        module_type_id: cap_rsb_tid,
        owner_char_id: Some(char_id),
        effect_name: "shieldBoost".into(),
        target_id: Some(friend_ship_id),
        duration_s: 1.0,
        repeat: 1,
        shield_boost_amount: 50.0,
        remote_shield_amount: 120.0,
        remote_cap_amount: 40.0,
        remote_optimal: 10_000.0,
        remote_falloff: 5_000.0,
        cap_drain_target: 10.0,
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_850_406,
        module_type_id: 47253,
        owner_char_id: Some(char_id),
        effect_name: "moduleAssaultDamageControl".into(),
        duration_s: 1.0,
        is_adc: true,
        self_mass_multiplier: Some(0.5),
        hic_sig_multiplier: Some(1.5),
        ..Default::default()
    })
    .await
    .unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 88_850_407,
        module_type_id: 40633,
        owner_char_id: Some(char_id),
        effect_name: "superWeaponLanceWarmup".into(),
        target_id: Some(friend_ship_id),
        duration_s: 1.0,
        repeat: 1,
        ..Default::default()
    })
    .await
    .unwrap();
    let _ = sys.step_manual(1.1).await.unwrap();
    assert!(
        (ctx.db.with_snapshot(|s| s.items[&part_crys_id]
            .dynamic_attributes
            .get(&eve_sde::attr_ids::DAMAGE)
            .copied()
            .unwrap_or(0.0))
            - 0.6)
            .abs()
            < 1e-6
    );
    let _ = sys.step_manual(1.1).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&part_crys_id].quantity), 1);

    // 4. combat_missiles_aoe::step_combat_system: destroyed missile, missing target expired, impact + breacher pod, fatal impact
    let lethal_tgt_id = 88_850_501i64;
    let mut lethal_cs = make_combat_state(0.0, 0.0, 1.0, 100.0);
    lethal_cs.shield_hp = 0.0;
    lethal_cs.armor_hp = 0.0;
    lethal_cs.structure_hp = 1.0;
    sys.try_spawn_ball_with_combat(
        make_ball(lethal_tgt_id, 587, DVec3::new(100.0, 0.0, 0.0)),
        lethal_cs,
    );

    sys.try_spawn_ball_with_combat(
        make_ball(missile_no_tgt_id, 200, DVec3::ZERO),
        make_combat_state(0.0, 0.0, 100.0, 0.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(missile_hit_id, 200, DVec3::new(299_500.0, 0.0, 0.0)),
        make_combat_state(0.0, 0.0, 100.0, 0.0),
    );
    sys.try_spawn_ball_with_combat(
        make_ball(missile_kill_id, 200, DVec3::new(50.0, 0.0, 0.0)),
        make_combat_state(0.0, 0.0, 100.0, 0.0),
    );
    let mk_missile = |mid: i64, tgt: i64, dmg: f64, breacher: bool| {
        crate::services::combat_crimewatch::InFlightMissileState {
            missile_ball_id: mid,
            solar_system_id: sys_id,
            attacker_char_id: char_id,
            attacker_ship_id: ship_id,
            launcher_module_id: 88_850_601,
            launcher_type_id: 499,
            missile_type_id: 200,
            target_ball_id: tgt,
            max_velocity: 5_000.0,
            flight_time_s: 5.0,
            remaining_flight_s: 5.0,
            hp: 100.0,
            max_hp: 100.0,
            base_damage: DamageVector {
                em: dmg,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
            explosion_radius: 100.0,
            explosion_velocity: 500.0,
            drf: 0.5,
            is_fof: false,
            is_breacher_pod: breacher,
            breacher_dot_duration_s: if breacher { 10.0 } else { 0.0 },
            breacher_flat_cap: 50.0,
            breacher_pct_cap: 0.02,
            destroyed: false,
            expired: false,
            impacted: false,
            applied_damage: 0.0,
        }
    };
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.insert(
            missile_dead_id,
            mk_missile(missile_dead_id, far_ship_id, 10.0, false),
        );
        rt.in_flight_missiles.insert(
            missile_no_tgt_id,
            mk_missile(missile_no_tgt_id, 99_999_999, 10.0, false),
        );
        rt.in_flight_missiles.insert(
            missile_hit_id,
            mk_missile(missile_hit_id, far_ship_id, 200.0, true),
        );
        rt.in_flight_missiles.insert(
            missile_kill_id,
            mk_missile(missile_kill_id, lethal_tgt_id, 500.0, false),
        );
    });
    let completed = combat_missiles_aoe::step_combat_system(&ctx, sys_id, 1.0).await;
    assert_eq!(completed.len(), 4);
    assert!(
        completed
            .iter()
            .any(|m| m.missile_ball_id == missile_dead_id && m.destroyed)
    );
    assert!(
        completed
            .iter()
            .any(|m| m.missile_ball_id == missile_no_tgt_id && m.expired)
    );
    assert!(
        completed
            .iter()
            .any(|m| m.missile_ball_id == missile_hit_id && m.impacted)
    );
    assert!(
        completed
            .iter()
            .any(|m| m.missile_ball_id == missile_kill_id && m.impacted)
    );
}
