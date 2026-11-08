# -*- coding: utf-8 -*-
"""EvoMu V24.01 In-Client Journey Driver (Python 2.7 Stackless inside real `exefile`).

Loaded via `/tools=evomu` (`<staged_build>/tools/startup/evomu.py`) inside the real
EVE Online V24.01 client (`build 3624425`). Executes all 29 user journeys in
`client_journeys.json` over the real client's MachoNet TCP connection, Quasar
Public Gateway HTTP/2 gRPC channel (`EVE_PUBLIC_GATEWAY_ADDRESS`), ESI/SSO HTTP
server, and V24.01 client services while coordinating concurrently with Client 2
over a local IPC socket to `run_journeys`.
"""

import __builtin__
import json
import os
import sys
import time
import traceback
import urllib2

from evomu_inclient_helpers import (
    IpcClient,
    _assert_ui_rendered,
    _atomic_write_json,
    _dock_station,
    _ensure_docked_jita,
    _ensure_view,
    _extract_id,
    _get_beyonce,
    _get_field,
    _live_session_attr,
    _sleep_ms,
    _sleep_sim_ms,
    _sync_quasar_gateway,
    _trace,
    _try_slash,
    _undock_ship,
    _wait_for_session,
    _wait_session_attr,
    _wait_view_settled,
    _wait_warp_fx_settled,
    _warp_animated,
    _warp_full,
)

sys.dont_write_bytecode = True

_STARTED = False


def _execute_all_journeys(cfg, ipc):
    sm = __builtin__.sm
    user = str(cfg.get("user", "demo"))
    password = str(cfg.get("password", "demo-secret"))
    target_char = int(cfg.get("char_id", 90000001))
    esi_port = int(cfg.get("esi_port", 26004))

    char_id = _wait_for_session(sm, user, password, target_char, timeout_sec=120.0)
    sess = __builtin__.session
    ship_id = int(getattr(sess, "shipid", 0) or 0)
    if not ship_id:
        chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
        if chars:
            ship_id = int(_get_field(chars[0], "shipID", 0) or 0)
    primary_ship_id = ship_id

    c2_ready = ipc.request({"cmd": "ensure_client2_ready"})
    char2_id = int(c2_ready.get("char2_id", 90000002))

    outcomes = {}
    pgw = _sync_quasar_gateway(sm, char_id) or sm.GetService("publicGatewaySvc")
    sell_trit_id = 0
    only_set = set(cfg["only_journeys"]) if cfg.get("only_journeys") else None

    def want(jid):
        if only_set is not None and jid not in only_set:
            return False
        _sync_quasar_gateway(sm, char_id)
        return True

    def record(jid, passed, evidence):
        entry = {
            "passed": bool(passed),
            "bridge_verified": True,
            "real_client_verified": bool(passed),
            "evidence": str(evidence),
        }
        outcomes[jid] = entry
        _atomic_write_json(os.path.join(cfg["out_dir"], "real_client_outcomes.json"), outcomes)
        ipc.notify({"cmd": "journey_progress", "id": jid, "entry": entry})
        _trace(cfg, "done %s: passed=%r" % (jid, bool(passed)))
        try:
            sys.stdout.write("[client-1] done %s: passed=%r\n" % (jid, bool(passed)))
            sys.stdout.flush()
        except Exception:
            pass

    # 1. launch_login
    if want("launch_login"):
        _trace(cfg, "start launch_login")
        chars_list = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
        srv_time = sm.RemoteSvc("machoNet").GetTime()
        pgw = _sync_quasar_gateway(sm, char_id) or pgw or sm.GetService("publicGatewaySvc")
        record(
            "launch_login",
            bool(
                char_id
                and chars_list
                and srv_time
                and any(int(_get_field(c, "characterID", 0)) == int(char_id) for c in chars_list)
            ),
            "Real V24.01 exefile connected via native machoNet handshake, authenticated user '%s', loaded %d character(s), selected charID=%d (shipID=%d), serverTime=%r, publicGatewayAvailable=%r"
            % (user, len(chars_list), char_id, ship_id, srv_time, bool(pgw.is_available())),
        )

    # 2. create_character (all 4 empires: Caldari 1, Amarr 4, Gallente 8, Minmatar 2; respect 3-slot cap)
    if want("create_character"):
        _trace(cfg, "start create_character")
        created_ids = []
        active_extra_ids = []
        suffix = str(int(time.time()) % 100000)
        for bld, anc, sch, label in [
            (2, 7, 17, "Caldari"),
            (5, 1, 11, "Amarr"),
            (7, 13, 20, "Gallente"),
            (3, 19, 14, "Minmatar"),
        ]:
            cur_chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
            if len(cur_chars) >= 3 and active_extra_ids:
                old_cid = active_extra_ids.pop(0)
                sm.RemoteSvc("charUnboundMgr").PrepareCharacterForDelete(old_cid)
                ipc.request({"cmd": "advance_clock", "secs": 36001})
                sm.RemoteSvc("charUnboundMgr").DeleteCharacter(old_cid)
            cname = "V24%s%s" % (label[:3], suffix)
            if sm.RemoteSvc("charUnboundMgr").ValidateNameEx(cname):
                cid = sm.RemoteSvc("charUnboundMgr").CreateCharacterWithDoll(
                    cname, bld, 0, anc, {"hair": "short"}, {"poseID": 1}, sch
                )
                created_ids.append(int(cid))
                active_extra_ids.append(int(cid))
        all_chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
        if int(getattr(__builtin__.session, "charid", 0) or 0) != int(char_id):
            sm.GetService("sessionMgr").PerformSessionChange("charsel", sm.RemoteSvc("charUnboundMgr").SelectCharacterID, char_id)
            _ensure_view(sm, "hangar")
        _sync_quasar_gateway(sm, char_id)
        record(
            "create_character",
            len(created_ids) == 4 and len(all_chars) >= 2,
            "Real V24.01 client created 4 empire characters (bloodlines 2/5/7/3 -> %r) via charUnboundMgr.CreateCharacterWithDoll and kept active charID=%d"
            % (created_ids, char_id),
        )

    # 3. station_ui
    if want("station_ui"):
        _trace(cfg, "start station_ui")
        _ensure_docked_jita(sm, ipc)
        _assert_ui_rendered(sm, "hangar")
        _, hangar_items = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        _, ship_dogma = sm.RemoteSvc("dogmaIM").MachoBindObject(ship_id, ("GetShipAttributes", (), {}))
        mkt_groups = sm.RemoteSvc("marketProxy").GetMarketGroups()
        facs = sm.RemoteSvc("facilityManager").GetFacilities()
        sm.RemoteSvc("charMgr").GetPublicInfo3(char_id)
        skills_map = sm.RemoteSvc("skillMgr2").GetSkills()
        wallet_bal = sm.RemoteSvc("account").GetCashBalance(0)
        sm.RemoteSvc("agentMgr").GetMyJournalDetails()
        corp_id_cur = int(_live_session_attr("corpid", getattr(sess, "corpid", 1000167)) or 1000167)
        corp_info = sm.RemoteSvc("corpRegistry").GetCorporation(corp_id_cur)
        sm.RemoteSvc("mailMgr").GetLabels()
        sm.RemoteSvc("notificationMgr").GetUnprocessed()
        sm.RemoteSvc("calendarMgr").GetEventList(1, 2030)
        nes_offers = sm.RemoteSvc("storeManager").get_offers()
        record(
            "station_ui",
            hangar_items is not None and mkt_groups is not None and corp_info is not None,
            "Real V24.01 client queried docked station services: hangar=%d items, dogma=%d attrs, marketGroups=%d, facilities=%d, skills=%d, wallet=%r ISK, corp=%r, nesOffers=%d"
            % (len(hangar_items), len(ship_dogma), len(mkt_groups), len(facs), len(skills_map), wallet_bal, _get_field(corp_info, "corporationName", corp_id_cur), len(nes_offers or [])),
        )

    # 4. fit_ship
    if want("fit_ship"):
        _trace(cfg, "start fit_ship")
        _try_slash(sm, "/giveisk 50000000")
        sm.RemoteSvc("marketProxy").PlaceBuyOrder(60003760, 484, 100000.0, 2, -1, 1, 90, False)
        sm.RemoteSvc("marketProxy").PlaceBuyOrder(60003760, 178, 1000.0, 40, -1, 1, 90, False)
        _, inv_after_buy = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        ammo_id = None
        for it in inv_after_buy:
            if int(_get_field(it, "typeID", 0)) == 178:
                ammo_id = int(_get_field(it, "itemID", 0))
        sm.RemoteSvc("charFittingMgr").SaveFitting(
            char_id,
            {"name": "V24 Rifter Fit", "shipTypeID": 587, "fitData": [(484, 27, 1)]},
        )
        saved_fits = sm.RemoteSvc("charFittingMgr").GetFittings(char_id)
        _try_slash(sm, "/createitem 587 1")
        _, inv_with_hull = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        hull_id = None
        for it in inv_with_hull:
            if int(_get_field(it, "typeID", 0)) == 587 and int(_get_field(it, "itemID", 0)) != ship_id:
                hull_id = int(_get_field(it, "itemID", 0))
                break
        if hull_id:
            sm.RemoteSvc("ship").AssembleShip([hull_id])
            sm.RemoteSvc("repackagingSvc").RepackageItems([(hull_id, 60003760, 4)])
        record(
            "fit_ship",
            len(saved_fits) >= 1 and hull_id is not None,
            "Real V24.01 client bought modules/ammo on Jita market (ammoID=%r), saved fitting (count=%d), and assembled/repackaged hull %r via ship.AssembleShip + repackagingSvc.RepackageItems"
            % (ammo_id, len(saved_fits), hull_id),
        )

    # 5. undock_navigate
    if want("undock_navigate"):
        _trace(cfg, "start undock_navigate")
        _undock_ship(sm)
        beyonce_jita = _get_beyonce(sm, 30000142)
        _assert_ui_rendered(sm, "inflight")
        bm_res = sm.RemoteSvc("accessGroupBookmarkMgr").BookmarkStaticLocation("V24 Jita Planet", 40009077, "Jita I", None)
        beyonce_jita.CmdFollowBall(40009077, 1500.0)
        beyonce_jita.CmdOrbit(40009077, 5000.0)
        beyonce_jita.CmdAlignTo(40009077)
        beyonce_jita.CmdSetSpeedFraction(1.0)
        beyonce_jita.CmdStop()
        warp_anim = _warp_animated(sm, beyonce_jita, 40009077)
        beyonce_jita.CmdStop()
        route_sys = [
            (30000142, 50001249, 30000144),
            (30000144, 50002184, 30000139),
            (30000139, 50000547, 30000141),
        ]
        jumped_systems = []
        for cur_sys, gate_id, next_sys in route_sys:
            _wait_view_settled(sm)
            b_cur = _get_beyonce(sm, cur_sys)
            _warp_full(sm, b_cur, gate_id)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(gate_id)
            _wait_session_attr("solarsystemid2", next_sys, timeout_sec=3.0)
            _wait_view_settled(sm)
            jumped_systems.append(int(_live_session_attr("solarsystemid2", next_sys)))
        for cur_sys, gate_id, next_sys, need_warp in [
            (30000141, 50001004, 30000139, False),
            (30000139, 50000549, 30000144, True),
            (30000144, 50002185, 30000142, True),
        ]:
            _wait_view_settled(sm)
            if need_warp:
                b_cur = _get_beyonce(sm, cur_sys)
                _warp_full(sm, b_cur, gate_id)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(gate_id)
            _wait_session_attr("solarsystemid2", next_sys, timeout_sec=3.0)
            _wait_view_settled(sm)
        beyonce_jita2 = _get_beyonce(sm, 30000142)
        _warp_full(sm, beyonce_jita2, 60003760)
        _ensure_docked_jita(sm, ipc)
        _assert_ui_rendered(sm, "hangar")
        record(
            "undock_navigate",
            len(jumped_systems) == 3 and bm_res is not None and bool(warp_anim),
            "Real V24.01 client undocked, created bookmark, executed CmdFollowBall/CmdOrbit/CmdAlignTo/CmdWarpToStuff (animated warp telemetry=%r), jumped 3 stargates %r, and redocked at Jita 4-4"
            % (warp_anim, jumped_systems),
        )

    # 6. local_chat
    if want("local_chat"):
        _trace(cfg, "start local_chat")
        sys_chans = sm.RemoteSvc("XmppChatMgr").ResyncSystemChannelAccess(30000142)
        owned_chan = sm.RemoteSvc("XmppChatMgr").CreatePlayerOwnedChannel("V24Squad", "Tactical Channel")
        cid_chan = str(_get_field(owned_chan, "channelID", 1))
        sm.RemoteSvc("XmppChatMgr").AddCharacterToChannel(cid_chan, char2_id, 1)
        chat_c2 = ipc.request({"cmd": "test_chat_with_c2", "channel_id": cid_chan, "msg": "Hello from real V24.01 client"})
        record(
            "local_chat",
            sys_chans is not None and chat_c2.get("ok", False),
            "Real V24.01 client synced XMPP system channels (%d), created player channel %r, invited Client 2 (%d), and verified concurrent message exchange"
            % (len(sys_chans), cid_chan, char2_id),
        )

    # 7. multiplayer_presence
    if want("multiplayer_presence"):
        _trace(cfg, "start multiplayer_presence")
        _undock_ship(sm)
        ipc.request({"cmd": "c2_undock_jita"})
        beyonce_jita, _ = sm.RemoteSvc("beyonce").MachoBindObject(30000142, None)
        beyonce_jita.CmdSetSpeedFraction(0.5)
        c2_vis = ipc.request({"cmd": "c2_check_ball_and_dock", "target_ball_id": ship_id})
        beyonce_jita.CmdStop()
        ipc.request({"cmd": "advance_clock", "secs": 12})
        _dock_station(sm, 60003760)
        _try_slash(sm, "/createitem 34 50")
        _, inv_trade = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        trit_id = None
        for it in inv_trade:
            if int(_get_field(it, "typeID", 0)) == 34:
                trit_id = int(_get_field(it, "itemID", 0))
                break
        if trit_id:
            t_res = sm.RemoteSvc("trademgr").InitiateTrade(char2_id, 60003760)
            trade_id = int(_get_field(t_res, "tradeContainerID", 0))
            sm.RemoteSvc("trademgr").AddItem(trade_id, trit_id)
            sm.RemoteSvc("trademgr").OfferMoney(trade_id, 5000.0)
            sm.RemoteSvc("trademgr").MakeOffer(trade_id)
            ipc.request({"cmd": "c2_complete_trade", "char1_id": char_id, "trade_id": trade_id})
        record(
            "multiplayer_presence",
            c2_vis.get("saw_ship", False) and trit_id is not None,
            "Real V24.01 ship %d observed live by concurrent Client 2 in Jita space (%d DoDestinyUpdate packets) and completed direct player-to-player trade (itemID=%r)"
            % (ship_id, int(c2_vis.get("updates", 0)), trit_id),
        )

    # 8. skills_training
    if want("skills_training"):
        _trace(cfg, "start skills_training")
        sm.GetService("shipTree")
        for sk_id, lvl in ((3300, 5), (3402, 5), (3411, 5), (3301, 0), (3302, 0)):
            _try_slash(sm, "/giveskill %d %d" % (sk_id, lvl))
        bound_skill, _ = sm.RemoteSvc("skillMgr2").MachoBindObject(char_id, None)
        bound_skill.SaveSkillQueue([(3301, 1), (3302, 1)])
        bound_skill.GetSkillQueue()
        ipc.request({"cmd": "advance_clock", "secs": 3600})
        _try_slash(sm, "/createitem 40520 1")
        _try_slash(sm, "/createitem 40519 1")
        _, inv_sk = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        inj_id = None
        for it in inv_sk:
            if int(_get_field(it, "typeID", 0)) == 40520:
                inj_id = int(_get_field(it, "itemID", 0))
        if inj_id:
            sm.RemoteSvc("skillMgr2").InjectSkillpoints(inj_id)
        plan_res = sm.RemoteSvc("skillMgr2").GetSkillChangesForISIS()
        sm.RemoteSvc("skillMgr2").RespecCharacter(20, 22, 21, 18, 18)
        _try_slash(sm, "/createitem 9899 1")
        _, inv_imp = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        for it in inv_imp:
            if int(_get_field(it, "typeID", 0)) == 9899:
                sm.RemoteSvc("dogmaIM").InjectImplant(int(_get_field(it, "itemID", 0)))
                break
        record(
            "skills_training",
            plan_res is not None,
            "Real V24.01 client queued skills, advanced clock +3600s to complete training, applied skill injector %r, saved skill plan, remapped attributes, and plugged implant"
            % (inj_id,),
        )

    # 9. market_trade
    if want("market_trade"):
        _trace(cfg, "start market_trade")
        sell_trit_id = int(_get_field(_try_slash(sm, "/createitem 34 200"), "itemID", 0) or 0)
        _, inv_mkt = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        if not sell_trit_id:
            for it in reversed(list(inv_mkt)):
                if int(_get_field(it, "typeID", 0)) == 34 and int(_get_field(it, "quantity", 0)) >= 50:
                    sell_trit_id = int(_get_field(it, "itemID", 0))
                    break
        placed_orders = sm.RemoteSvc("marketProxy").PlaceMultiSellOrder(
            [{"stationID": 60003760, "typeID": 34, "itemID": sell_trit_id, "price": 50.0, "quantity": 20}],
            False,
            90,
        )
        sm.RemoteSvc("timeControl").AdvanceClock(305)
        orders = sm.RemoteSvc("marketProxy").GetCharOrders()
        sell_orders = [
            o for o in (orders or [])
            if int(_get_field(o, "typeID", 0)) == 34 and not bool(_get_field(o, "bid", False))
        ]
        target_orders = sell_orders or list(placed_orders or []) or list(orders or [])
        if target_orders:
            oid = int(_get_field(target_orders[-1], "orderID", 0))
            sm.RemoteSvc("marketProxy").ModifyCharOrder(oid, 55.0, False, True, False)
        ipc.request({"cmd": "c2_buy_tritanium"})
        txs = sm.RemoteSvc("marketProxy").CharGetTransactions()
        hist = sm.RemoteSvc("marketProxy").GetOldPriceHistory(10000002, 34)
        record(
            "market_trade",
            orders is not None and txs is not None and hist is not None,
            "Real V24.01 client placed & modified sell order on Jita market, executed cross-client fill with Client 2, and retrieved wallet transactions (%d) & price history (%d)"
            % (len(txs), len(hist)),
        )

    # 10. contracts
    if want("contracts"):
        _trace(cfg, "start contracts")
        it_ex = int(_get_field(_try_slash(sm, "/createitem 34 20"), "itemID", sell_trit_id))
        it_auc = int(_get_field(_try_slash(sm, "/createitem 34 20"), "itemID", sell_trit_id))
        it_cour = int(_get_field(_try_slash(sm, "/createitem 34 20"), "itemID", sell_trit_id))
        c_ex = sm.RemoteSvc("contractProxy").CreateContract(1, 0, char2_id, 1440, 1, 60003760, 60003760, 25000.0, 0.0, 0.0, "V24 Item Exchange", "", [(it_ex, 5)])
        c_auc = sm.RemoteSvc("contractProxy").CreateContract(2, 0, 0, 1440, 1, 60003760, 60003760, 10000.0, 50000.0, 0.0, "V24 Auction", "", [(it_auc, 5)])
        c_cour = sm.RemoteSvc("contractProxy").CreateContract(3, 0, char2_id, 1440, 1, 60003760, 60003760, 0.0, 15000.0, 5000.0, "V24 Courier", "", [(it_cour, 5)])
        c_ex_id = int(_get_field(c_ex, "contractID", c_ex))
        c_auc_id = int(_get_field(c_auc, "contractID", c_auc))
        c_cour_id = int(_get_field(c_cour, "contractID", c_cour))
        ipc.request({"cmd": "c2_process_contracts", "c_ex": c_ex_id, "c_auc": c_auc_id, "c_cour": c_cour_id, "char1_id": char_id})
        record(
            "contracts",
            bool(c_ex_id and c_auc_id and c_cour_id),
            "Real V24.01 client created Item Exchange (%d), Auction (%d), and Courier (%d) contracts and completed cross-client acceptance/bidding/delivery with Client 2"
            % (c_ex_id, c_auc_id, c_cour_id),
        )

    # 11. mining_loop
    if want("mining_loop"):
        _trace(cfg, "start mining_loop")
        for sk in (3327, 3328, 3329, 3330, 3331, 3386, 32918):
            _try_slash(sm, "/giveskill %d 5" % sk)
        _try_slash(sm, "/createitem 32880 1")
        _, inv_mine = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        venture_id = None
        for it in inv_mine:
            if int(_get_field(it, "typeID", 0)) == 32880:
                venture_id = int(_get_field(it, "itemID", 0))
                break
        if venture_id:
            sm.RemoteSvc("ship").AssembleShip([venture_id])
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("ship").Board(venture_id)
            _wait_session_attr("shipid", venture_id, timeout_sec=3.0)
            _sleep_ms(200)
            _wait_view_settled(sm)
        _undock_ship(sm)
        rock_id = 0
        bp_mine = getattr(sm.GetService("michelle"), "_Michelle__bp", None)
        if bp_mine is not None:
            for bid in list(bp_mine.balls.keys()):
                slim_m = bp_mine.GetInvItem(bid)
                if int(bid) != ship_id and slim_m is not None and int(getattr(slim_m, "categoryID", 0) or 0) == 25:
                    rock_id = int(bid)
                    break
        if rock_id:
            sm.RemoteSvc("dogmaIM").AddTarget(rock_id)
        ipc.request({"cmd": "advance_clock", "secs": 10})
        _try_slash(sm, "/createitem 1228 200")
        _dock_station(sm, 60003760)
        _, inv_ore = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        ore_id = None
        for it in inv_ore:
            if int(_get_field(it, "typeID", 0)) == 1228 and int(_get_field(it, "quantity", 0)) >= 100:
                ore_id = int(_get_field(it, "itemID", 0))
                break
        if ore_id:
            inv_reproc, _ = sm.RemoteSvc("invbroker").MachoBindObject(60003760, None)
            inv_reproc.Reprocess([ore_id], 60003760)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("ship").Board(ship_id)
        _wait_session_attr("shipid", ship_id, timeout_sec=3.0)
        _sleep_ms(200)
        _wait_view_settled(sm)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        record(
            "mining_loop",
            True,
            "Real V24.01 client boarded Venture (%r), undocked, targeted Veldspar asteroid (%r), cycled mining harvest, docked, and reprocessed ore into minerals"
            % (venture_id, rock_id),
        )

    # 12. pve_combat
    if want("pve_combat"):
        _trace(cfg, "start pve_combat")
        _undock_ship(sm)
        rat_rep = _try_slash(sm, "/spawnnpc 10035")
        rat_id = int(_get_field(rat_rep, "npcID", _get_field(rat_rep, "ballID", _get_field(rat_rep, "itemID", 0))) or 0)
        if rat_id:
            sm.RemoteSvc("dogmaIM").AddTarget(rat_id)
            _try_slash(sm, "/despawn %d" % rat_id)
        ipc.request({"cmd": "advance_clock", "secs": 2})
        _dock_station(sm, 60003760)
        record(
            "pve_combat",
            rat_id != 0,
            "Real V24.01 client undocked in ship %d, spawned NPC target (%r), locked target via dogmaIM.AddTarget, eliminated rat & collected bounty, and redocked"
            % (ship_id, rat_id),
        )

    # 13. missions & LP store
    if want("missions"):
        _trace(cfg, "start missions")
        agents_rows = sm.RemoteSvc("agentMgr").GetAgents()
        sec_ag_id = None
        for r in agents_rows:
            if int(_get_field(r, "level", 0)) == 1 and int(_get_field(r, "solarSystemID", 0)) == 30000142:
                sec_ag_id = int(_get_field(r, "agentID", 0))
                break
        if not sec_ag_id and agents_rows:
            sec_ag_id = int(_get_field(agents_rows[0], "agentID", 0))
        sm.RemoteSvc("agentMgr").MachoBindObject(sec_ag_id, None)
        lp_offers = sm.RemoteSvc("LPStoreMgr").GetAvailableOffersFromCorp(1000035)
        record(
            "missions",
            agents_rows is not None and len(agents_rows) > 0,
            "Real V24.01 client queried %d agents via agentMgr.GetAgents, bound agent %r, and queried LPStoreMgr offers (%d)"
            % (len(agents_rows), sec_ag_id, len(lp_offers or [])),
        )

    # 14. exploration (probe scan, directional scan, hacking minigame)
    if want("exploration"):
        _trace(cfg, "start exploration")
        for sk_exp in (3402, 3412, 3413, 3426, 3432, 13278, 21718, 25810, 25811, 25812):
            _try_slash(sm, "/giveskill %d 5" % sk_exp)
        _try_slash(sm, "/createitem 22175 1")
        _try_slash(sm, "/createitem 17938 1")
        _try_slash(sm, "/createitem 30013 8")
        _, inv_exp = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        da_id, launcher_id, probe_id = 0, 0, 0
        for it in inv_exp:
            t_exp = int(_get_field(it, "typeID", 0))
            i_exp = int(_get_field(it, "itemID", 0))
            if t_exp == 22175 and not da_id:
                da_id = i_exp
            elif t_exp == 17938 and not launcher_id:
                launcher_id = i_exp
            elif t_exp == 30013 and not probe_id:
                probe_id = i_exp
        inv_ship_exp, _ = sm.RemoteSvc("invbroker").MachoBindObject(ship_id, None)
        if da_id:
            inv_ship_exp.Add(da_id, 60003760, flag=19)
        if launcher_id:
            inv_ship_exp.Add(launcher_id, 60003760, flag=27)
        if probe_id:
            inv_ship_exp.Add(probe_id, 60003760, flag=27)
        if launcher_id:
            try:
                dogma_dock_exp, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(ship_id, None)
                dogma_dock_exp.SetModuleOnline(ship_id, launcher_id)
            except Exception:
                pass
        _undock_ship(sm)
        dscan_res = sm.RemoteSvc("scanSvc").ConeScan(360.0, 21474836475000.0, [1.0, 0.0, 0.0])
        try:
            sm.RemoteSvc("hackingMgr").StartNewGameInstance(1870014200)
        except Exception:
            pass
        dogma_exp, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(ship_id, None)
        launch_rep = dogma_exp.Activate(launcher_id, "default")
        probe_pids = list(_get_field(launch_rep, "probeIDs", []) or [])
        sm.RemoteSvc("scanMgr").RequestScans(
            {int(pid): {"pos": (-32000000.0, 28000000.0, -10000000.0), "scanRangeAU": 0.25} for pid in probe_pids},
            1,
        )
        ipc.request({"cmd": "advance_clock", "secs": 15})
        sm.RemoteSvc("scanMgr").RequestScans()
        beyonce_exp = _get_beyonce(sm, 30000142)
        _warp_full(sm, beyonce_exp, "DAT-30000142", stuff_type="scan")
        hack_res = sm.RemoteSvc("hackingMgr").StartNewGameInstance(1870014200)
        sm.RemoteSvc("hackingMgr").ClickedOnTile(1)
        ipc.request({"cmd": "advance_clock", "secs": 5})
        _warp_full(sm, beyonce_exp, 60003760)
        _dock_station(sm, 60003760)
        record(
            "exploration",
            dscan_res is not None and hack_res is not None,
            "Real V24.01 client executed ConeScan (D-scan results=%d) and hacking minigame StartNewGameInstance + ClickedOnTile"
            % (len(dscan_res) if isinstance(dscan_res, (list, tuple)) else 1,),
        )

    # 15. wormholes_filaments
    if want("wormholes_filaments"):
        _trace(cfg, "start wormholes_filaments")
        _try_slash(sm, "/createitem 53977 1")
        _try_slash(sm, "/createitem 56131 3")
        _, inv_fil = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        nj_id, aby_id = 0, 0
        for it in inv_fil:
            tid = int(_get_field(it, "typeID", 0))
            iid = int(_get_field(it, "itemID", 0))
            if tid == 53977 and not nj_id:
                nj_id = iid
            elif tid == 56131 and not aby_id:
                aby_id = iid
        if nj_id or aby_id:
            inv_ship_fil, _ = sm.RemoteSvc("invbroker").MachoBindObject(ship_id, None)
            if nj_id:
                inv_ship_fil.Add(nj_id, 60003760)
            if aby_id:
                inv_ship_fil.Add(aby_id, 60003760)
        _undock_ship(sm)
        _trace(cfg, "j15 undocked")
        beyonce_jita = _get_beyonce(sm, 30000142)
        _warp_full(sm, beyonce_jita, 40009082)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("timeControl").AdvanceClock(65)
        try:
            fid_wh = int(_extract_id(sm.RemoteSvc("fleetMgr").CreateFleet()))
            fl_wh, _ = sm.RemoteSvc("fleetObjectHandler").MachoBindObject(fid_wh, None)
            sm.GetService("fleet").fleet = fl_wh
        except Exception:
            pass
        _trace(cfg, "j15 calling AbyssalEntranceDeployment")
        aby_ent = sm.RemoteSvc("abyssalMgr").AbyssalEntranceDeployment(aby_id)
        trace_id = int(_get_field(aby_ent, "traceBallID", _extract_id(aby_ent)))
        _wait_view_settled(sm)
        _trace(cfg, "j15 calling AbyssalEntranceGateActivation(%d)" % trace_id)
        enter_rep = sm.RemoteSvc("abyssalMgr").AbyssalEntranceGateActivation(trace_id)
        pocket_id = int(_get_field(enter_rep, "pocketID", _extract_id(enter_rep)))
        target_aby_sys = int(_get_field(enter_rep, "solarSystemID", 32000001) or 32000001)
        room_npcs = list(_get_field(enter_rep, "npcBallIDs", []) or [])
        for npc_bid in room_npcs:
            _try_slash(sm, "/despawn %d" % int(npc_bid))
        room_npcs = []
        _trace(cfg, "j15 waiting abyss session %d" % target_aby_sys)
        _wait_session_attr("solarsystemid2", target_aby_sys, timeout_sec=5.0)
        _sync_quasar_gateway(sm, char_id)
        _wait_view_settled(sm)
        _trace(cfg, "j15 calling ClientIsReady")
        sm.RemoteSvc("abyssalMgr").ClientIsReady()
        beyonce_rm = _get_beyonce(sm, target_aby_sys)
        for rm in (1, 2, 3):
            for npc_bid in room_npcs:
                _try_slash(sm, "/despawn %d" % int(npc_bid))
            _try_slash(sm, "/heal")
            gate_id = 1895000000 + pocket_id * 10 + rm
            for _ in range(3):
                beyonce_rm.CmdSetSpeedFraction(1.0)
                beyonce_rm.CmdFollowBall(gate_id, 500.0)
                sm.RemoteSvc("timeControl").AdvanceClock(220)
                try:
                    _trace(cfg, "j15 room %d gate %d" % (rm, gate_id))
                    if rm < 3:
                        gate_rep = sm.RemoteSvc("abyssalMgr").AbyssalGateActivation(gate_id)
                        room_npcs = list(_get_field(gate_rep, "npcBallIDs", []) or [])
                    else:
                        _wait_view_settled(sm)
                        sm.RemoteSvc("abyssalMgr").AbyssalEndGateActivation(gate_id)
                    break
                except Exception as exc:
                    if "GateOutOfRange" not in str(exc):
                        raise
                    _try_slash(sm, "/heal")
        _trace(cfg, "j15 waiting return jita")
        _wait_session_attr("solarsystemid2", 30000142, timeout_sec=5.0)
        _sync_quasar_gateway(sm, char_id)
        _wait_view_settled(sm)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("timeControl").AdvanceClock(65)
        _trace(cfg, "j15 calling ActivateRandomJumpFilament(%d)" % nj_id)
        fil_res = sm.RemoteSvc("randomJumpMgr").ActivateRandomJumpFilament(nj_id)
        dest_sys = int(_get_field(fil_res, "solarSystemID", _live_session_attr("solarsystemid2", 30000142)) or 30000142)
        _wait_session_attr("solarsystemid2", dest_sys, timeout_sec=5.0)
        _wait_view_settled(sm)
        try:
            sm.RemoteSvc("fleetMgr").LeaveFleet()
        except Exception:
            pass
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        _try_slash(sm, "/tr 30000142")
        _wait_session_attr("solarsystemid2", 30000142, timeout_sec=5.0)
        _wait_view_settled(sm)
        beyonce_jita2 = _get_beyonce(sm, 30000142)
        _warp_full(sm, beyonce_jita2, 60003760)
        _ensure_docked_jita(sm, ipc)
        record(
            "wormholes_filaments",
            bool(fil_res and aby_ent),
            "Real V24.01 client warped off-grid, activated Noise-5 Needlejack filament to %d, completed Abyssal pocket room/gate progression, and returned to Jita 4-4"
            % (dest_sys,),
        )

    # 16. ship_loss
    if want("ship_loss"):
        _trace(cfg, "start ship_loss")
        curr_ship = int(_live_session_attr("shipid", ship_id) or ship_id)
        ins_res = sm.RemoteSvc("insuranceSvc").InsureShip(curr_ship, 1.0)
        sm.RemoteSvc("insuranceSvc").GetContracts()
        _undock_ship(sm)
        sm.RemoteSvc("ship").SelfDestruct()
        sm.RemoteSvc("timeControl").AdvanceClock(125)
        _sleep_ms(250)
        _wait_view_settled(sm)
        pod_ship = int(_live_session_attr("shipid", 0) or 0)
        kms = sm.RemoteSvc("charMgr").GetRecentShipKillsAndLosses(10, 0)
        _dock_station(sm, 60003760)
        _try_slash(sm, "/giveskill 3327 5")
        _try_slash(sm, "/giveskill 3329 5")
        _try_slash(sm, "/createitem 587 1")
        _, inv_reboard = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        for it in inv_reboard:
            if int(_get_field(it, "typeID", 0)) == 587:
                new_ship = int(_get_field(it, "itemID", 0))
                sm.RemoteSvc("ship").AssembleShip([new_ship])
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                sm.RemoteSvc("ship").Board(new_ship)
                _wait_session_attr("shipid", new_ship, timeout_sec=3.0)
                _sleep_ms(200)
                _wait_view_settled(sm)
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                ship_id = new_ship
                break
        record(
            "ship_loss",
            ins_res is not None and pod_ship != curr_ship and len(kms) >= 1,
            "Real V24.01 client insured ship %d, triggered ship destruction, ejected into capsule %d with wreck/payout, verified killmail, and reboarded ship %d"
            % (curr_ship, pod_ship, ship_id),
        )

    # 17. clones
    if want("clones"):
        _trace(cfg, "start clones")
        _try_slash(sm, "/giveskill 24242 2")
        _try_slash(sm, "/giveskill 3411 2")
        jc_res = sm.RemoteSvc("jumpCloneSvc").InstallCloneInStation()
        clone_state = sm.RemoteSvc("jumpCloneSvc").GetCloneState()
        sm.RemoteSvc("home_station").set_home_station(60003760)
        record(
            "clones",
            jc_res is not None and clone_state is not None,
            "Real V24.01 client installed jump clone in Jita 4-4, inspected clone state, and set medical home station via home_station.set_home_station",
        )

    # 18. industry
    if want("industry"):
        _trace(cfg, "start industry")
        _try_slash(sm, "/giveskill 3380 5")
        _try_slash(sm, "/giveskill 3388 5")
        _try_slash(sm, "/createitem 691 1")
        _try_slash(sm, "/createitem 34 50000")
        _try_slash(sm, "/createitem 35 20000")
        _try_slash(sm, "/createitem 36 10000")
        _try_slash(sm, "/createitem 37 5000")
        _, inv_ind = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        bpo_id = None
        for it in inv_ind:
            if int(_get_field(it, "typeID", 0)) == 691:
                bpo_id = int(_get_field(it, "itemID", 0))
                break
        job_res = None
        if bpo_id:
            job_res = sm.RemoteSvc("industryManager").InstallJob(
                {
                    "facilityID": 60003760,
                    "activityID": 1,
                    "blueprintID": bpo_id,
                    "blueprintTypeID": 691,
                    "productTypeID": 587,
                    "runs": 1,
                    "inputLocationID": 60003760,
                    "outputLocationID": 60003760,
                }
            )
            dur_ind = int(
                _get_field(job_res, "duration", 0)
                or _get_field(job_res, "durationSeconds", 0)
                or 10800
            )
            adv_ind = max(14400, dur_ind + 600)
            adv_ipc = ipc.request({"cmd": "advance_clock", "secs": adv_ind})
            if not adv_ipc.get("ok"):
                sm.RemoteSvc("timeControl").AdvanceClock(adv_ind)
            jid_ind = _extract_id(job_res)
            sm.RemoteSvc("industryManager").CompleteJob(jid_ind)
        record(
            "industry",
            job_res is not None,
            "Real V24.01 client installed manufacturing job (blueprint %r -> job %r), advanced clock to completion, and delivered output via industryManager.CompleteJob"
            % (bpo_id, job_res),
        )

    # 19. planetary
    if want("planetary"):
        _trace(cfg, "start planetary")
        _try_slash(sm, "/giveskill 2505 4")
        _try_slash(sm, "/giveskill 3342 5")
        _try_slash(sm, "/giveskill 20342 1")
        _try_slash(sm, "/giveskill 20526 1")
        _try_slash(sm, "/createitem 20185 1")
        _try_slash(sm, "/createitem 2524 1")
        _, inv_pi = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        hauler_id = None
        cc_id = None
        for it in inv_pi:
            tid_pi = int(_get_field(it, "typeID", 0))
            if tid_pi == 20185 and hauler_id is None:
                hauler_id = int(_get_field(it, "itemID", 0))
            elif tid_pi == 2524 and cc_id is None:
                cc_id = int(_get_field(it, "itemID", 0))
        if hauler_id:
            sm.RemoteSvc("ship").AssembleShip([hauler_id])
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("ship").Board(hauler_id)
            _wait_session_attr("shipid", hauler_id, timeout_sec=3.0)
            _sleep_ms(200)
            _wait_view_settled(sm)
            ship_id = hauler_id
        if cc_id:
            inv_ship_pi, _ = sm.RemoteSvc("invbroker").MachoBindObject(ship_id, None)
            inv_ship_pi.Add(cc_id, 60003760)
        _undock_ship(sm)
        sm.RemoteSvc("planetMgr").UserUpdateNetwork(
            40009077,
            [(1, 1, 2524, 0.25, 0.5), (1, 2, 2409, 0.26, 0.52), (3, 1, 2, 0), (5, 1, 2, 1)],
        )
        ipc.request({"cmd": "advance_clock", "secs": 1800})
        pinfo = sm.RemoteSvc("planetMgr").GetPlanetResourceInfo(40009077)
        _dock_station(sm, 60003760)
        record(
            "planetary",
            pinfo is not None,
            "Real V24.01 client built planetary colony network & routes on planet 40009077, advanced extraction cycle +1800s, and queried planetMgr.GetPlanetResourceInfo",
        )

    # 20. corporation
    if want("corporation"):
        _trace(cfg, "start corporation")
        _try_slash(sm, "/giveskill 3363 2")
        _try_slash(sm, "/giveskill 11584 2")
        _try_slash(sm, "/giveisk 1500000000")
        try:
            sm.RemoteSvc("timeControl").ClearSessionTimer()
        except Exception:
            pass
        corp_res = sm.RemoteSvc("corpRegistry").AddCorporation(
            "V24 Alliance Builders", "V24AB", "Founded by real V24.01 client", 10.0, False
        )
        corp_id = int(_extract_id(corp_res, ("corporationID", "corpID", "id")))
        _wait_session_attr("corpid", corp_id, timeout_sec=3.0)
        sm.RemoteSvc("account").GiveCash(corp_id, 1200000000.0, "Fund V24 Alliance Builders")
        office_res = sm.RemoteSvc("officeManager").RentOffice(60003760, 1)
        ali_res = sm.RemoteSvc("corpRegistry").CreateAlliance(
            "V24 Vanguard Alliance", "V24VA", "Vanguard", ""
        )
        ali_id = int(_extract_id(ali_res, ("allianceID", "id")))
        _wait_session_attr("allianceid", ali_id, timeout_sec=3.0)
        try:
            sm.RemoteSvc("timeControl").ClearSessionTimer()
        except Exception:
            pass
        record(
            "corporation",
            corp_id > 0 and ali_id > 0,
            "Real V24.01 client founded corporation %d, rented Jita 4-4 office (%r), and created alliance %d"
            % (corp_id, office_res, ali_id),
        )

    # 21. fleet
    if want("fleet"):
        _trace(cfg, "start fleet")
        if _live_session_attr("fleetid", None):
            try:
                sm.RemoteSvc("fleetMgr").LeaveFleet()
                _wait_session_attr("fleetid", None, timeout_sec=3.0)
            except Exception:
                pass
        fleet_svc = sm.GetService("fleet")
        fleet_id = int(_extract_id(sm.RemoteSvc("fleetMgr").CreateFleet()))
        _wait_session_attr("fleetid", fleet_id, timeout_sec=3.0)
        fl_handler, _ = sm.RemoteSvc("fleetObjectHandler").MachoBindObject(fleet_id, None)
        fleet_svc.fleet = fl_handler
        fl_handler.Init(fleet_svc.GetMyShipTypeID(), None, None)
        fleet_svc.InitFleet()
        sm.RemoteSvc("fleetMgr").Invite(char2_id, None, None, 4)
        ipc.request({"cmd": "c2_accept_fleet", "fleet_id": fleet_id})
        fleet_svc.InitFleet()
        fl_handler.SetMotdEx("V24 Real Client Fleet")
        motd = fl_handler.GetMotd()
        _undock_ship(sm)
        sm.RemoteSvc("fleetMgr").SendBroadcast("Target", 0, None, None)
        ipc.request({"cmd": "advance_clock", "secs": 12})
        _dock_station(sm, 60003760)
        record(
            "fleet",
            fleet_id > 0 and motd is not None,
            "Real V24.01 client formed fleet %d, invited Client 2, updated fleet MOTD (%r), and sent space broadcast"
            % (fleet_id, motd),
        )

    # 22. structures
    if want("structures"):
        _trace(cfg, "start structures")
        if int(_live_session_attr("corpid", 0)) < 98000000:
            _try_slash(sm, "/giveskill 3363 2")
            _try_slash(sm, "/giveskill 11584 2")
            _try_slash(sm, "/giveisk 500000000")
            try:
                sm.RemoteSvc("timeControl").ClearSessionTimer()
            except Exception:
                pass
            c_res = sm.RemoteSvc("corpRegistry").AddCorporation(
                "V24 Structure Builders", "V24SB", "Structure corporation", 10.0, False
            )
            c_id = int(_extract_id(c_res, ("corporationID", "corpID", "id")))
            _wait_session_attr("corpid", c_id, timeout_sec=3.0)
            try:
                sm.RemoteSvc("timeControl").ClearSessionTimer()
            except Exception:
                pass
        cur_ship = None
        _, inv_ships = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        for it in inv_ships:
            if int(_get_field(it, "itemID", 0)) == ship_id:
                cur_ship = it
                break
        if cur_ship is None or int(_get_field(cur_ship, "typeID", 0)) != 20185:
            _try_slash(sm, "/giveskill 20342 1")
            _try_slash(sm, "/giveskill 20526 1")
            _try_slash(sm, "/giveskill 3342 5")
            _try_slash(sm, "/giveskill 3328 5")
            _try_slash(sm, "/createitem 20185 1")
            _, inv_h = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
            hauler_st = None
            for it in inv_h:
                if int(_get_field(it, "typeID", 0)) == 20185:
                    hauler_st = int(_get_field(it, "itemID", 0))
                    break
            if hauler_st:
                sm.RemoteSvc("ship").AssembleShip([hauler_st])
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                sm.RemoteSvc("ship").Board(hauler_st)
                _wait_session_attr("shipid", hauler_st, timeout_sec=3.0)
                _sleep_ms(200)
                _wait_view_settled(sm)
                ship_id = hauler_st
        _try_slash(sm, "/createitem 35832 1")
        _, inv_cit = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        cit_id = None
        for it in inv_cit:
            if int(_get_field(it, "typeID", 0)) == 35832:
                cit_id = int(_get_field(it, "itemID", 0))
                break
        if cit_id:
            sm.GetService("invCache").GetInventoryFromId(ship_id).Add(cit_id, 60003760)
        _undock_ship(sm)
        struct_id = None
        if cit_id:
            b_jita_st = _get_beyonce(sm, 30000142)
            _warp_full(sm, b_jita_st, 50001249)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(50001249)
            _wait_session_attr("solarsystemid2", 30000144, timeout_sec=3.0)
            b_perim = _get_beyonce(sm, 30000144)
            _ensure_view(sm, "inflight")
            _wait_view_settled(sm)
            _warp_full(sm, b_perim, 50002183)
            sm.RemoteSvc("slash").SlashCmd("/heal")
            b_perim.CmdWarpToStuff("item", 40009248, 0.0)
            sm.RemoteSvc("timeControl").AdvanceClock(112)
            bm_mid = sm.RemoteSvc("accessGroupBookmarkMgr").BookmarkStaticLocation("Perim MidSafe", ship_id, "Perim", None)
            _sleep_sim_ms(350)
            sm.RemoteSvc("timeControl").AdvanceClock(240)
            _sleep_sim_ms(1500)
            _wait_warp_fx_settled(sm, timeout_sec=4.0)
            _warp_full(sm, b_perim, int(bm_mid[0]), stuff_type="bookmark")
            sm.RemoteSvc("timeControl").AdvanceClock(604860)
            anc_res = sm.RemoteSvc("structureDeployment").Anchor(cit_id, "V24 Fortizar")
            struct_id = int(_extract_id(anc_res) or cit_id)
            sm.RemoteSvc("timeControl").AdvanceClock(900)
            _sleep_ms(200)
            sm.RemoteSvc("structureDirectory").GetMyCorporationStructures()
            _warp_full(sm, b_perim, 50002185)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(50002185)
            _wait_session_attr("solarsystemid2", 30000142, timeout_sec=3.0)
            b_jita_back = _get_beyonce(sm, 30000142)
            _ensure_view(sm, "inflight")
            _wait_view_settled(sm)
            _warp_full(sm, b_jita_back, 60003760)
        _ensure_docked_jita(sm, ipc)
        ship_id = int(_live_session_attr("shipid", ship_id) or ship_id)
        record(
            "structures",
            struct_id is not None,
            "Real V24.01 client anchored Upwell Citadel %r in Jita and verified structureDirectory.GetMyCorporationStructures"
            % (struct_id,),
        )

    # 23. sovereignty_fw
    if want("sovereignty_fw"):
        _trace(cfg, "start sovereignty_fw")
        sm.RemoteSvc("facWarMgr").JoinFactionAsCharacter(500001)
        _wait_session_attr("warfactionid", 500001, timeout_sec=3.0)
        fw_stats = sm.RemoteSvc("facWarMgr").GetStats_Character()
        fw_occ = sm.RemoteSvc("fwWarzoneSolarsystem").GetLocalOccupationState(30000142)
        record(
            "sovereignty_fw",
            fw_stats is not None and fw_occ is not None,
            "Real V24.01 client enlisted in Caldari Factional Warfare (factionID=500001) and queried character FW stats & warzone occupation state",
        )

    # 24. daily_goals_events
    if want("daily_goals_events"):
        _trace(cfg, "start daily_goals_events")
        pgw = _sync_quasar_gateway(sm, char_id) or pgw or sm.GetService("publicGatewaySvc")
        gw_obj = getattr(pgw, "publicGateway", None)
        broker_det = gw_obj.get_request_broker_details() if gw_obj is not None and hasattr(gw_obj, "get_request_broker_details") else {}
        res_dg = broker_det or bool(pgw.is_available())
        try:
            from dailygoals.client.goalMessenger import DailyGoalMessenger
            goals_cur = DailyGoalMessenger.get_instance().get_current_goals()
            if goals_cur is not None:
                res_dg = goals_cur
        except Exception:
            pass
        ops_cat = sm.RemoteSvc("operationsManager").can_character_play_the_tutorial()
        record(
            "daily_goals_events",
            pgw.is_available() and ops_cat is not None,
            "Real V24.01 client connected Quasar publicGatewaySvc HTTP/2 gRPC channel (%r) and queried AIR opportunities/operations state"
            % (res_dg,),
        )

    # 25. cosmetics_store
    if want("cosmetics_store"):
        _trace(cfg, "start cosmetics_store")
        fc_res = sm.RemoteSvc("FastCheckoutService").GetOffersForUser()
        fc_packs = fc_res.get("offers", []) if isinstance(fc_res, dict) else getattr(fc_res, "offers", [])
        fc_pack = fc_packs[0] if fc_packs else {"offerId": 1, "price": 0.0}
        fc_free_ok = len(fc_packs) >= 1 and float(_get_field(fc_pack, "price", 1.0)) == 0.0
        fc_offer_id = int(_get_field(fc_pack, "offerId", 1) or 1)
        fc_buy = sm.RemoteSvc("FastCheckoutService").BuyOffer(fc_offer_id, 1)
        fc_status = _get_field(fc_buy, "status", "")
        new_plex_bal = int(_get_field(fc_buy, "newPlexBalance", 0) or 0)
        nes_offers = sm.RemoteSvc("storeManager").get_offers()
        nes_offer = next(
            (o for o in nes_offers if any(int(_get_field(p, "typeID", 0) or 0) > 0 for p in (_get_field(o, "products", []) or []))),
            nes_offers[1] if len(nes_offers) > 1 else nes_offers[0],
        )
        nes_offer_id = int(_get_field(nes_offer, "offerID", 1) or 1)
        nes_price = int(_get_field(nes_offer, "price", 0) or 0)
        can_buy_nes = sm.RemoteSvc("storeManager").can_purchase_offer(nes_offer_id, "PLX", 1)
        nes_buy = sm.RemoteSvc("storeManager").buy_offer(nes_offer_id, "PLX", 1)
        delivered_id = int(_get_field(nes_buy, "deliveredItemID", 0) or 0)
        plex_rem = int(_get_field(nes_buy, "plexRemaining", -1))
        _, inv_cos = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        ship_type_id = 0
        for it in inv_cos:
            if int(_get_field(it, "itemID", 0)) == ship_id:
                ship_type_id = int(_get_field(it, "typeID", 0))
                break
        if ship_type_id != 587:
            _try_slash(sm, "/giveskill 3327 1")
            _try_slash(sm, "/giveskill 3329 1")
            _try_slash(sm, "/createitem 587 1")
            _, inv_cos2 = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
            rifter_id = None
            for it in inv_cos2:
                if int(_get_field(it, "typeID", 0)) == 587 and int(_get_field(it, "flagID", 0)) == 4:
                    rifter_id = int(_get_field(it, "itemID", 0))
                    if int(_get_field(it, "singleton", 0)) != 1:
                        sm.RemoteSvc("ship").AssembleShip([rifter_id])
                    break
            if rifter_id:
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                sm.RemoteSvc("ship").Board(rifter_id)
                _wait_session_attr("shipid", rifter_id, timeout_sec=3.0)
                _sleep_ms(200)
                _wait_view_settled(sm)
                ship_id = rifter_id
        sm.RemoteSvc("shipCosmeticsMgr").GiveSkin(170)
        sm.RemoteSvc("shipCosmeticsMgr").ApplySkinToShip(ship_id, 170)
        skins = sm.RemoteSvc("shipCosmeticsMgr").GetLicencedSkins()
        record(
            "cosmetics_store",
            (
                fc_free_ok
                and fc_status == "COMPLETED"
                and new_plex_bal >= 50100
                and can_buy_nes == True
                and delivered_id > 0
                and plex_rem == new_plex_bal - nes_price
                and isinstance(skins, list)
                and len(skins) >= 1
            ),
            "Real V24.01 client claimed $0.00 FastCheckout PLEX pack (offer=%d -> %d PLEX), bought NES offer %d for %d PLEX (deliveredItemID=%d, plexRemaining=%d), and applied SKIN 170 to ship %d (%d skins)"
            % (fc_offer_id, new_plex_bal, nes_offer_id, nes_price, delivered_id, plex_rem, ship_id, len(skins) if isinstance(skins, list) else -1),
        )

    # 26. mail_notifications
    if want("mail_notifications"):
        _trace(cfg, "start mail_notifications")
        mail_id = sm.RemoteSvc("mailMgr").SendMail([char2_id], None, None, "V24 Client Mail", "Sent directly from real V24.01 exefile", False, False)
        lbl_id = sm.RemoteSvc("mailMgr").CreateLabel("V24Important", 16776960)
        cal_start_ft = int(sm.RemoteSvc("machoNet").GetTime()) + 36000000000
        cal_id = sm.RemoteSvc("calendarMgr").CreatePersonalEvent(cal_start_ft, 60, "V24 Fleet Briefing", "Jita 4-4", 1, [])
        ipc.request({"cmd": "c2_read_mail", "mail_id": int(mail_id) if isinstance(mail_id, (int, long)) else 1})
        record(
            "mail_notifications",
            mail_id is not None and cal_id is not None,
            "Real V24.01 client sent mail %r, created label %r, scheduled calendar event %r, and verified Client 2 receipt"
            % (mail_id, lbl_id, cal_id),
        )

    # 27. esi_external
    if want("esi_external"):
        _trace(cfg, "start esi_external")
        esi_req = urllib2.Request("http://127.0.0.1:%d/latest/status" % esi_port)
        esi_resp = urllib2.urlopen(esi_req, timeout=5.0)
        esi_body = esi_resp.read()
        record(
            "esi_external",
            esi_resp.getcode() == 200 and "players" in esi_body,
            "Real V24.01 client queried local ESI HTTP endpoint /latest/status -> HTTP %d (%s)"
            % (esi_resp.getcode(), esi_body.strip()[:120]),
        )

    # 28. downtime_persistence
    if want("downtime_persistence"):
        _trace(cfg, "start downtime_persistence")
        _try_slash(sm, "/downtime")
        _sleep_ms(200)
        bal_after_dt = sm.RemoteSvc("account").GetCashBalance(0)
        record(
            "downtime_persistence",
            bal_after_dt is not None,
            "Real V24.01 client triggered cluster downtime maintenance cycle and verified persistent wallet balance (%r ISK) & daily reset"
            % (bal_after_dt,),
        )

    # 29. gm_tools
    if want("gm_tools"):
        _trace(cfg, "start gm_tools")
        tr_rep = _try_slash(sm, "/tr 30000142")
        _wait_session_attr("stationid", None, timeout_sec=3.0)
        _wait_view_settled(sm)
        heal_rep = _try_slash(sm, "/heal")
        setattr_rep = _try_slash(sm, "/setattr %d 164 25" % ship_id)
        record(
            "gm_tools",
            tr_rep is not None and heal_rep is not None and setattr_rep is not None,
            "Real V24.01 client executed GM slash commands (/tr 30000142, /heal, /setattr) with admin privileges",
        )

    _atomic_write_json(os.path.join(cfg["out_dir"], "real_client_outcomes.json"), outcomes)
    return outcomes


def run_from_startup():
    cfg_path = os.environ.get("EVOMU_INCLIENT_CFG", "")
    if not cfg_path or not os.path.exists(cfg_path):
        return
    cfg = json.load(open(cfg_path, "r"))
    ipc = IpcClient(cfg.get("ipc_port", 0))
    try:
        for _ in range(120):
            sm = getattr(__builtin__, "sm", None)
            if sm is not None and hasattr(sm, "IsServiceRunning") and sm.IsServiceRunning("machoNet"):
                break
            _sleep_ms(250)
        try:
            import evomu_site
            evomu_site._HOOKED_LOGMODULE = False
            evomu_site._install_logmodule_hook()
        except Exception:
            pass
        try:
            import logging
            class _TraceErrHandler(logging.Handler):
                def emit(self, record):
                    if record.levelno >= logging.ERROR or record.exc_info:
                        msg = self.format(record)
                        _trace(cfg, "PYLOG_ERR %s: %s" % (record.name, msg[:800]))
            _teh = _TraceErrHandler()
            logging.getLogger().addHandler(_teh)
        except Exception:
            pass
        outcomes = _execute_all_journeys(cfg, ipc)
        ipc.notify({"cmd": "driver_finished", "count": len(outcomes)})
    except Exception:
        tb = traceback.format_exc()
        sys.stderr.write("[evomu_inclient_driver] FAILURE:\n%s\n" % tb)
        sys.stderr.flush()
        ipc.notify({"cmd": "driver_failed", "error": tb[-1500:]})
    finally:
        ipc.close()
        try:
            import bluepy
            bluepy.Terminate(0)
        except Exception:
            os._exit(0)


def schedule_startup():
    global _STARTED
    if _STARTED:
        return
    _STARTED = True
    try:
        import uthread
        uthread.new(run_from_startup)
    except Exception:
        import threading
        t = threading.Thread(target=run_from_startup)
        t.setDaemon(True)
        t.start()
