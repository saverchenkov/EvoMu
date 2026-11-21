# -*- coding: utf-8 -*-
"""Canonical registry and classification table for client behaviour guards (#1539).

Must remain compatible with BOTH Python 2.7 (EVE client embedded interpreter)
and Python 3 (host playtester, scenario runner, journey runner, unit tests).
"""

import os


DISABLE_SERVER_GUARDS_ENV = "EVOMU_DISABLE_SERVER_GUARDS"
TRUTHY_ENV_VALUES = ("1", "true", "yes", "on")

# Ratchet ceilings from #1539: both counts may only fall.
MAX_BEHAVIOUR_GUARDS = 15
MAX_BENIGN_USER_ERROR_MARKERS = 75

CLASSIFICATION_SERVER_CAUSED = "server-caused"
CLASSIFICATION_CLIENT_ONLY = "client-only"
VALID_CLASSIFICATIONS = (
    CLASSIFICATION_SERVER_CAUSED,
    CLASSIFICATION_CLIENT_ONLY,
)

# Out-of-scope environment adapters and telemetry hooks in evomu_site.py that stay untouched.
OUT_OF_SCOPE_ADAPTERS = (
    "_install_logmodule_hook",
    "_install_xmpp_plaintext_guard",
    "_install_xmpp_port_guard",
    "_install_flatbuffer_cdn_http_guard",
    "_install_fsd_typelist_preload_hook",
)


# Every guard requires an open server issue when server-caused, OR a written case +
# repository owner approval when client-only. Zero guards have client-only approval today.
BEHAVIOUR_GUARDS = (
    {
        "id": "camera_dock_guard",
        "installer": "_install_camera_dock_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1468, 1539),
        "named_issues": (996, 1051, 1154),
        "server_fix_status": "before_period",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "ViewStateSvc._OpenView",
            "ViewStateSvc.ActivateView",
            "Camera.GetLookAtDirectionWithOffset",
            "Camera.GetLookAtDirection",
            "TacticalCamera.OnActivated",
            "TacticalCamera.OnDeactivated",
            "TacticalCamera._GetNewDirection",
            "TacticalCamera._ResetEyeAndAtPosition",
            "TacticalCamera.SetAtPosToFixedDistance",
            "TacticalCamera.SetAppropriateAtPositionForRotation",
            "Michelle.GetBallsAndSlimItems",
            "LocationArrivalIndicationService.on_warp_finished",
            "LocationArrivalIndicationService.check_indicate_arrival",
            "LocationArrivalIndicationService._indicate_arrival",
            "LocationArrivalIndicationService.get_closest_location_slim_item",
            "LocationArrivalIndicationService.get_nearest_celestial_to_player",
            "LocationArrivalIndicationSvc.on_warp_finished",
            "LocationArrivalIndicationSvc.check_indicate_arrival",
            "LocationArrivalIndicationSvc._indicate_arrival",
            "LocationArrivalIndicationSvc.get_closest_location_slim_item",
            "LocationArrivalIndicationSvc.get_nearest_celestial_to_player",
            "LocationArrival.get_nearest_celestial_to_player",
        ),
    },
    {
        "id": "base_space_camera_guard",
        "installer": "_install_base_space_camera_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1340,),
        "server_fix_status": "no",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "BaseSpaceCamera.ResetAnchorPos",
            "BaseSpaceCamera.UpdateAnchorOffset",
            "BaseSpaceCamera.GetTrackPosition",
        ),
    },
    {
        "id": "hangar_transition_guard",
        "installer": "_install_hangar_transition_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1163,),
        "server_fix_status": "yes",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "HangarToHangarTransition.StartTransition",
            "HangarToHangarTransition.EndTransition",
        ),
    },
    {
        "id": "hangar_view_teardown_guard",
        "installer": "_install_hangar_view_teardown_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1154,),
        "server_fix_status": "before_period",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "HangarView.ClearScene",
            "HangarView.ReplaceExistingShipModel",
            "HangarView._ReplaceExistingShipModelModular",
            "HangarView.RemoveAllShipsFromScene",
            "HangarView.AddModelToScene",
            "HangarView.PlayDockSequence",
            "HangarView.PlaySwitchShipSequence",
            "HangarView.PlayUpdateShipSequence",
            "HangarView._SetUpLightingRig",
            "HangarView.ConfigureVFXMultiEffect",
            "HangarView.GetShipItemFromHangar",
            "HangarView.SetUpModels",
            "HangarView.LoadAndSetupScene",
            "HangarView.LoadView",
            "HangarView.LoadCamera",
            "HangarView.ProcessActiveShipChanged",
            "HangarView.OnActiveShipSkinChange",
            "HangarView.ReloadView",
        ),
    },
    {
        "id": "inventory_transition_guard",
        "installer": "_install_inventory_transition_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1267, 1290, 1301, 1333),
        "server_fix_status": "yes",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "Inventory._IsCurrentInventoryValid",
            "Inventory.OnSessionChanged",
            "Inventory._UpdateSelectedItems",
            "Inventory._MoveItems",
            "InvContCapacityGauge.SetSecondaryVolume",
            "InvContCapacityGauge.UpdateLabel",
            "invCacheContainer.List",
        ),
    },
    {
        "id": "fitting_active_ship_guard",
        "installer": "_install_fitting_active_ship_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1512, 1539),
        "named_issues": (1254, 1410, 1443),
        "server_fix_status": "partial (#1254 yes; #1410 no)",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "FittingSvc.GetFittingDictForActiveShip",
            "FittingWarnings.EvaluateFit",
            "WarningIconCont._WarningIconCont__UpdateIcon_thread",
            "StatsPanel.UpdateStats",
            "StatsPanel.UpdateDroneStats",
            "DronePanel.UpdateDroneStats",
        ),
    },
    {
        "id": "base_hangar_behaviour_guard",
        "installer": "_install_base_hangar_behaviour_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1286,),
        "server_fix_status": "no",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "BaseHangarShipBehaviour.SetShipDamage",
        ),
    },
    {
        "id": "modular_capital_hangar_guard",
        "installer": "_install_modular_capital_hangar_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1286,),
        "server_fix_status": "no",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "ModularCapitalHangarShipBehaviour.ProcessSkinChange",
        ),
    },
    {
        "id": "modular_hangar_teardown_guard",
        "installer": "_install_modular_hangar_teardown_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1218, 1286),
        "server_fix_status": "partial (#1218 yes; #1286 no)",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "ModularHangarShipBehaviour.CleanScene",
            "ModularHangarShipBehaviour.ProcessSkinChange",
            "ModularHangarDroneBehaviour.CleanScene",
        ),
    },
    {
        "id": "wreck_rotation_guard",
        "installer": "_install_wreck_rotation_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1468, 1539),
        "named_issues": (1217,),
        "server_fix_status": "yes",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "Wreck.Prepare",
            "Wreck.SetRandomRotation",
            "Wreck.SetDungeonRotation",
            "Wreck.SetBallRotation",
            "SpaceMgr.DoBallsAdded_",
            "SpaceMgr._LoadObjects",
            "SpaceMgr.LoadObject",
        ),
    },
    {
        "id": "slots_container_sync_guard",
        "installer": "_install_slots_container_sync_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1512,
        "related_open_issues": (1190, 1539),
        "named_issues": (1252,),
        "server_fix_status": "yes",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "SlotsContainer.OnModuleFitted",
            "SlotsContainer.ProcessShipEffect",
            "SlotsContainer.OnInvChange",
        ),
    },
    {
        "id": "sensor_suite_gfx_guard",
        "installer": "_install_sensor_suite_gfx_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1292, 1293),
        "server_fix_status": "no",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "GfxHandler.StartGfxSwipe",
            "GfxHandler._GfxSwipeThread",
        ),
    },
    {
        "id": "modular_hangar_camera_guard",
        "installer": "_install_modular_hangar_camera_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1318,),
        "server_fix_status": "no",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "ModularHangarCamera.AnimEnterHangar",
            "ModularHangarCamera.CalculateTargetPoint",
            "ModularHangarCamera.GetSizeDependantZoomConstants",
            "ModularHangarCamera._updateShipChangeLocations",
            "ModularHangarCapitalCamera.AnimEnterHangar",
            "ModularHangarCapitalCamera.CalculateOrbitTarget",
            "ModularHangarCapitalCamera.Orbit",
            "ModularHangarCapitalCamera._configureCameraStartLocations",
            "ModularHangarCapitalCamera.CapitalOrbitUpdateThread",
        ),
    },
    {
        "id": "skill_plan_panel_signal_guard",
        "installer": "_install_skill_plan_panel_signal_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1332,),
        "server_fix_status": "no",
        "server_fix_landed": False,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "SkillPlanPanel.OnSkillPlanCreated",
            "SkillPlanPanel.OnSkillPlanSaved",
            "SkillPlanPanel.OnSkillPlanDeleted",
            "SkillPlanPanel.OnCreateNewPersonalSkillPlanButton",
            "SkillPlanPanel.OnCreateNewCorpSkillPlanButton",
            "SkillPlanPanel.OnEditSkillPlanButton",
            "SkillPlanPanel.OnDeleteSkillPlanButton",
            "SkillPlanPanel.OnCloseNewSkillPlanButton",
            "SkillPlanPanel.OnSkillPlanSelected",
            "SkillPlanPanel.OnSkillPlanCloseButton",
            "SkillPlanPanel.OnShowMultiTrainingMessage",
            "SkillPlanPanel.OnShowAlphaTrainingMessage",
            "SkillPlanPanel.Close",
        ),
    },
    {
        "id": "probe_tracker_guard",
        "installer": "_install_probe_tracker_guard",
        "classification": CLASSIFICATION_SERVER_CAUSED,
        "open_issue": 1190,
        "related_open_issues": (1539,),
        "named_issues": (1334,),
        "server_fix_status": "yes",
        "server_fix_landed": True,
        "client_only_case": "",
        "owner_approved": False,
        "targets": (
            "ProbeTracker.OnProbesIdle",
            "ProbeTracker.SetProbeDestination",
            "ProbeTracker.UpdateProbeState",
        ),
    },
)

BEHAVIOUR_GUARDS_BY_ID = dict((entry["id"], entry) for entry in BEHAVIOUR_GUARDS)


def get_guard_spec(guard_id, registry=None):
    """Return the registry entry for ``guard_id`` or ``None``."""
    table = BEHAVIOUR_GUARDS_BY_ID if registry is None else registry
    return table.get(guard_id)


def get_guard_classification(guard_id, registry=None):
    """Return the classification string for ``guard_id``."""
    spec = get_guard_spec(guard_id, registry=registry)
    if spec is None:
        return CLASSIFICATION_SERVER_CAUSED
    return spec.get("classification", CLASSIFICATION_SERVER_CAUSED)


def is_approved_client_only(guard_id, registry=None):
    """Return True only if ``guard_id`` has an owner-approved written client-only case."""
    spec = get_guard_spec(guard_id, registry=registry)
    if spec is None:
        return False
    if spec.get("classification") != CLASSIFICATION_CLIENT_ONLY:
        return False
    case_text = str(spec.get("client_only_case") or "").strip()
    return bool(spec.get("owner_approved")) and bool(case_text)


def is_server_guards_disabled(environ=None):
    """Return True when ``EVOMU_DISABLE_SERVER_GUARDS`` disables server-caused guards."""
    env = os.environ if environ is None else environ
    raw = str(env.get(DISABLE_SERVER_GUARDS_ENV, "")).strip().lower()
    return raw in TRUTHY_ENV_VALUES


def is_guard_enabled(guard_id, environ=None, registry=None):
    """Return True if ``guard_id`` should be installed under the current env."""
    if not is_server_guards_disabled(environ=environ):
        return True
    return is_approved_client_only(guard_id, registry=registry)


def summarize_guard_hits(hits, client_guards_off=None, registry=None):
    """Aggregate hit counts and client-only breakdown from recorded guard hit dicts."""
    by_guard = {}
    client_only_by_guard = {}
    total = 0
    client_only_total = 0
    for item in hits or ():
        if not isinstance(item, dict):
            continue
        gid = str(item.get("guard") or "unknown")
        total += 1
        by_guard[gid] = by_guard.get(gid, 0) + 1
        if is_approved_client_only(gid, registry=registry):
            client_only_total += 1
            client_only_by_guard[gid] = client_only_by_guard.get(gid, 0) + 1

    ordered_by_guard = {}
    for gid in sorted(by_guard.keys()):
        ordered_by_guard[gid] = by_guard[gid]

    ordered_client_only_by_guard = {}
    for gid in sorted(client_only_by_guard.keys()):
        ordered_client_only_by_guard[gid] = client_only_by_guard[gid]

    guards_off = is_server_guards_disabled() if client_guards_off is None else bool(client_guards_off)
    return {
        "client_guards_off": guards_off,
        "guard_hits_count": total,
        "guard_hits_by_guard": ordered_by_guard,
        "client_only_guard_hits": client_only_total,
        "client_only_guard_hits_by_guard": ordered_client_only_by_guard,
        "server_caused_guard_hits": total - client_only_total,
    }
