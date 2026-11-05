#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use crate::services::insurgency_services::{add_system_corruption, init_insurgency_campaign};

#[tokio::test]
async fn ice_heist_lifecycle_and_escort_mutations() {
    assert_eq!(militia_corp_for_faction(Some(500_001)), Some(1_000_180));
    assert_eq!(militia_corp_for_faction(Some(500_002)), Some(1_000_182));
    assert_eq!(militia_corp_for_faction(Some(500_003)), Some(1_000_179));
    assert_eq!(militia_corp_for_faction(Some(500_004)), Some(1_000_181));
    assert_eq!(militia_corp_for_faction(Some(FACTION_ANGEL_CARTEL)), Some(1_000_436));
    assert_eq!(militia_corp_for_faction(Some(FACTION_GURISTAS)), Some(1_000_437));
    assert_eq!(militia_corp_for_faction(Some(999)), None);

    assert!(is_winning_side_faction(Some(FACTION_ANGEL_CARTEL), true));
    assert!(is_winning_side_faction(Some(FACTION_GURISTAS), true));
    assert!(!is_winning_side_faction(Some(500_001), true));
    assert!(is_winning_side_faction(Some(500_001), false));
    assert!(!is_winning_side_faction(Some(FACTION_GURISTAS), false));

    let ctx = test_ctx();
    let dps = thunderchild_guard_dps_vector(&ctx);
    assert!(dps.total() > 0.0);

    let sys_id = 30045329_u32;
    // Before corruption stage 3..=4, spawn_ice_heist_checked fails
    let err = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err();
    assert_eq!(err.code, "CorruptionStageOutOfRange");

    // Start campaign and advance corruption to stage 3 (450 pts)
    let camp_id = 77_001_i64;
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, sys_id, vec![sys_id]);
    let (_, stg) = add_system_corruption(&ctx, camp_id, sys_id, 450);
    assert_eq!(stg, 3);

    let inst_id = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap();
    assert!(inst_id >= 800_001);

    // Calling spawn_ice_heist_checked again returns the existing active instance
    assert_eq!(spawn_ice_heist_checked(&ctx, sys_id).await.unwrap(), inst_id);

    // Unknown instance fails escort
    assert!(escort_ice_heist_tanker(&ctx, 90_000_001, 999_999, true).await.is_err());

    // Step system tick
    step_ice_heist_system(&ctx, sys_id).await;

    // Escort 9 tankers on pirate side
    for i in 1..10 {
        let rep = escort_ice_heist_tanker(&ctx, 90_000_001, inst_id, true)
            .await
            .unwrap();
        assert_eq!(rep.dict_get("pirateTankersSaved"), Some(&PyRep::Integer(i)));
        assert_eq!(rep.dict_get("completed"), Some(&PyRep::Bool(false)));
    }
    // 10th tanker completes the Ice Heist and sets 4h cooldown
    let win_rep = escort_ice_heist_tanker(&ctx, 90_000_001, inst_id, true)
        .await
        .unwrap();
    assert_eq!(win_rep.dict_get("pirateTankersSaved"), Some(&PyRep::Integer(10)));
    assert_eq!(win_rep.dict_get("completed"), Some(&PyRep::Bool(true)));

    // Escorting completed instance fails
    assert!(escort_ice_heist_tanker(&ctx, 90_000_001, inst_id, true).await.is_err());

    // Respawning during cooldown fails
    let cd_err = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err();
    assert_eq!(cd_err.code, "IceHeistRespawnCooldown");
}
