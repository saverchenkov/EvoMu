//! Unit tests for `quasar_progression`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    EvePublicRequest, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn default_and_sde_progression_managers_register_all_tracks() {
    let def_mgrs = ProgressionManagers::default();
    assert!(def_mgrs.daily_goals.get_templates().is_empty());

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgrs = new_progression_managers(Arc::clone(&db), Arc::clone(&sde));
    assert_eq!(mgrs.daily_goals.get_templates().len(), 6);
    assert!(!mgrs.achievements.definitions.is_empty());

    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_all_progression_handlers(&router, Arc::clone(&db), Arc::clone(&mgrs));

    let mut s = SessionState::new(1);
    s.user_id = Some(1);
    s.character_id = Some(90_000_001);
    s.corporation_id = Some(98_000_001);
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    let now = 1_760_000_000u64;

    for ty in [
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        "eve_public.career.goal.api.GetAllRequest",
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        "eve_public.skill.plan.GetAllRequest",
        "eve.tale.api.GetActiveTalesCountRequest",
    ] {
        let rsp = router.handle_public_request(
            &s,
            &EvePublicRequest {
                correlation_uuid: vec![1; 16],
                payload: Some(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"),
                    value: Vec::new(),
                }),
                ..Default::default()
            },
            None,
            false,
            now,
        );
        assert_eq!(rsp.status_code, STATUS_OK, "{ty}");
    }
}
