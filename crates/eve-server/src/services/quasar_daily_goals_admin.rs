//! Admin RPC registration for Quasar Daily Goals (`eve.dailygoal.api.admin.*`).

use super::quasar_daily_goals::DailyGoalManager;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::WorldRepository;
use eve_proto::quasar::{GoogleAny, TYPE_GOOGLEAPIS_PREFIX};
use std::sync::Arc;

pub(super) fn register(
    router: &PublicGatewayRouter,
    manager: &Arc<DailyGoalManager>,
    db: &Arc<WorldRepository>,
) {
    let mgr = Arc::clone(manager);
    let db_auto = Arc::clone(db);
    router.register_handler(
        "eve.dailygoal.api.admin.AutoRedeemRequest",
        Arc::new(move |ctx, _any, _dyn| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            if let Some(cid) = ctx.effective_character_id() {
                let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
                for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                    let _ = mgr.redeem_goal(cid, &g.uuid, ctx.now_unix(), &db_auto);
                }
                mgr.save_to_db();
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.AutoRedeemResponse"),
                value: Vec::new(),
            })
        }),
    );
}
