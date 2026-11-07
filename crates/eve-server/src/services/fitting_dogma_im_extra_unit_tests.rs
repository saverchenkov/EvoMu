//! Comprehensive branch-coverage and mutation-killing tests for `fitting_dogma_im.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState};
use crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im;
use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let mut pt = sde.types.values().next().cloned().unwrap();
        pt.type_id = 581;
        pt.group_id = 82;
        pt.category_id = 7;
        pt.default_attributes.clear();
        pt.effect_ids = vec![13];
        sde.types.insert(581, pt);
        Arc::new(sde)
    }))
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    station_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.station_id = station_id;
    s.ship_id = ship_id;
    s
}

pub(super) async fn call_dogma_im(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["dogmaIM"][0];
    handle_dogma_im(ctx, session, bound, svc, method, args, &HashMap::new()).await.unwrap()
}

pub(super) async fn call_ship_stance_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["shipStanceMgr"][0];
    handle_dogma_im(ctx, session, bound, svc, method, args, &HashMap::new()).await.unwrap()
}

pub(super) async fn call_char_fitting_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["charFittingMgr"][0];
    handle_dogma_im(ctx, session, None, svc, method, args, &HashMap::new()).await.unwrap()
}

pub(super) async fn call_invbroker(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["invbroker"][0];
    handle_dogma_im(ctx, session, None, svc, method, args, &HashMap::new()).await.unwrap()
}

#[tokio::test]
async fn item_get_info_space_drone_charges_ship_attributes_and_ship_stances() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_400_100i64; // Confessor (34317)
    let charge_id = 96_400_101i64;
    let offline_mod_id = 96_400_102i64;
    let drone_ball_id = 96_400_500i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "u_96401".into(),
            AccountRecord {
                user_id: char_id as u64,
                username: "u_96401".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(60_003_760);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        for (iid, tid, loc, flg, sgl) in [
            (ship_id, 34317, 60_003_760, InventoryFlag::Hangar, true),
            (charge_id, 242, ship_id, InventoryFlag::HiSlot0, false),
            (offline_mod_id, 484, ship_id, InventoryFlag::HiSlot1, true),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: flg,
                    quantity: 1,
                    singleton: sgl,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.item_custom_names.insert(ship_id, "NamedConfessor".into());
        snap.module_online_states.insert(offline_mod_id, false);
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball_id,
            DroneSpaceState {
                ball_id: drone_ball_id,
                drone_item_id: drone_ball_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 300.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let mut session =
        make_session(char_id, 1_000_009, Some(sys_id), Some(60_003_760), Some(ship_id));

    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "TypeNotFound"
    );

    let drone_info =
        call_dogma_im(&ctx, &mut session, None, "ItemGetInfo", &[PyRep::Integer(drone_ball_id)])
            .await
            .unwrap();
    assert_eq!(drone_info.dict_get("typeID"), Some(&PyRep::Integer(2454)));
    assert_eq!(drone_info.dict_get("ownerID"), Some(&PyRep::Integer(char_id)));

    let charge_info =
        call_dogma_im(&ctx, &mut session, None, "GetDynamicItemInfo", &[PyRep::Integer(charge_id)])
            .await
            .unwrap();
    assert_eq!(charge_info.dict_get("categoryID"), Some(&PyRep::Integer(8)));

    let ship_info = call_invbroker(&ctx, &mut session, "GetItemInfo", &[PyRep::Integer(ship_id)])
        .await
        .unwrap();
    assert_eq!(ship_info.dict_get("customName"), Some(&PyRep::String("NamedConfessor".into())));

    // GetAllInfo with bound_entry and offline module
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: String::new(),
        object_id: ship_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    let ship_attrs =
        call_dogma_im(&ctx, &mut session, Some(&bound), "GetAllInfo", &[]).await.unwrap();
    assert!(ship_attrs.dict_get("shipInfo").is_some());

    // shipStanceMgr.SetShipStance branches
    assert_eq!(
        call_ship_stance_mgr(&ctx, &mut session, None, "SetShipStance", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidShipStance"
    );
    assert_eq!(
        call_ship_stance_mgr(
            &ctx,
            &mut session,
            None,
            "SetShipStance",
            &[PyRep::Integer(ship_id), PyRep::Integer(587)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipStance"
    );
    let st_ok = call_ship_stance_mgr(
        &ctx,
        &mut session,
        None,
        "SetShipStance",
        &[PyRep::Integer(ship_id), PyRep::Integer(34321)],
    )
    .await
    .unwrap();
    assert_eq!(st_ok, PyRep::Integer(34321));

    assert_eq!(
        call_ship_stance_mgr(&ctx, &mut session, None, "SetShipStance", &[PyRep::Integer(34323)])
            .await
            .unwrap_err()
            .code,
        "StanceSwitchCooldown"
    );
}
