use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

#[test]
fn campaign_definition_wire_encoders_and_gateway_handlers() {
    let mut out = Vec::new();
    encode_contribution_method_definition(
        "MineAsteroid",
        665_390,
        696_245,
        "mining",
        &[("ore_type", "harvestable_type", "mining")],
        &mut out,
    );
    assert!(!out.is_empty());

    let mut out_empty = Vec::new();
    encode_contribution_method_definition("Custom", 0, 0, "", &[("k", "t", "")], &mut out_empty);
    assert!(!out_empty.is_empty());

    let mut trig_out = Vec::new();
    encode_trigger_eventmatch_definition(
        "ShipDestroyed",
        &[("allow_on_guard_object", "true"), ("allow_on_guards", "false")],
        &mut trig_out,
    );
    assert!(!trig_out.is_empty());

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let auth = AuthoritativeContext::for_session(
        Some(1),
        Some(100_000_001),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("req");

    let handlers = {
        let map = ctx.quasar_gateway.handlers.read().unwrap();
        map.clone()
    };

    let cm_res = handlers["eve_public.campaign.contributionmethod.definition.api.GetAllRequest"](
        &auth, &any, &empty,
    )
    .expect("contribution methods GetAllRequest");
    assert!(cm_res
        .type_url
        .ends_with("eve_public.campaign.contributionmethod.definition.api.GetAllResponse"));
    assert!(!cm_res.value.is_empty());

    let em_res = handlers["eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest"](
        &auth, &any, &empty,
    )
    .expect("trigger eventmatch GetAllRequest");
    assert!(em_res
        .type_url
        .ends_with("eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse"));
}
