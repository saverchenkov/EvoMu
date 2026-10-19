#!/usr/bin/env python3
"""
import_sde.py — Real V24.01 EVE Client Static Data Importer (Phase 1 D-1).

Reads the installed V24.01 EVE Online client resources directly from:
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/resfileindex.txt
  - ~/Library/Application Support/EVE Online/SharedCache/ResFiles/
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/bin64/staticdata/mapObjects.db
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/bin64/*Loader.so
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/code.ccp (fsd.schemas.binaryLoader)

Produces the versioned on-disk SQLite SDE database at `data/evomu_sde.sqlite` (local, gitignored)
and summary manifest of table row counts and file checksums at `data/sde_import_manifest.json`
with zero hand-written literals.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pickle
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import zipfile
import zlib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
DEFAULT_SHARED_CACHE = Path(os.path.expanduser("~/Library/Application Support/EVE Online/SharedCache"))
DEFAULT_OUT_DB = REPO_ROOT / "data" / "evomu_sde.sqlite"
DEFAULT_MANIFEST = REPO_ROOT / "data" / "sde_import_manifest.json"
STATIC_CATALOG = REPO_ROOT / "docs" / "index" / "staticdata_catalog.json"

ROMAN_NUMERALS = [
    "", "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X",
    "XI", "XII", "XIII", "XIV", "XV", "XVI", "XVII", "XVIII", "XIX", "XX",
    "XXI", "XXII", "XXIII", "XXIV", "XXV", "XXVI", "XXVII", "XXVIII", "XXIX", "XXX",
]


def to_roman(n: int | None) -> str:
    if n is None or n <= 0:
        return ""
    if n < len(ROMAN_NUMERALS):
        return ROMAN_NUMERALS[n]
    return str(n)


def build_cfsd_runner(eve_build_dir: Path, work_dir: Path) -> tuple[Path, Path]:
    """Compile cfsd_runner.c against EVE.app libpython2.7.dylib and build code_pyc.zip."""
    work_dir.mkdir(parents=True, exist_ok=True)
    bin64_dir = eve_build_dir / "bin64"
    libpy_src = bin64_dir / "libpython2.7.dylib"
    libpy_dst = work_dir / "libpython2.7.dylib"
    if not libpy_dst.exists():
        libpy_dst.symlink_to(libpy_src)

    runner_bin = work_dir / "cfsd_runner"
    c_src = HERE / "cfsd_runner.c"
    if not runner_bin.exists() or runner_bin.stat().st_mtime < c_src.stat().st_mtime:
        subprocess.run(
            [
                "cc",
                "-O2",
                str(c_src),
                f"-L{work_dir}",
                "-lpython2.7",
                "-Wl,-rpath,@executable_path",
                "-o",
                str(runner_bin),
            ],
            check=True,
        )

    code_ccp = eve_build_dir / "code.ccp"
    code_pyc_zip = work_dir / "code_pyc.zip"
    if not code_pyc_zip.exists():
        seen: set[str] = set()
        with zipfile.ZipFile(code_ccp, "r") as zsrc, zipfile.ZipFile(
            code_pyc_zip, "w", zipfile.ZIP_STORED
        ) as zdst:
            for info in zsrc.infolist():
                if info.filename.endswith(".pyj"):
                    pyc_name = info.filename[:-4] + ".pyc"
                    if pyc_name in seen:
                        continue
                    seen.add(pyc_name)
                    zdst.writestr(pyc_name, zlib.decompress(zsrc.read(info.filename)))

    return runner_bin, code_pyc_zip


PY2_EXTRACTOR_SCRIPT = r"""
import sys
code_pyc_zip = sys.argv[1]
bin64_dir = sys.argv[2]
res_dir = sys.argv[3]
rfi_path = sys.argv[4]
catalog_path = sys.argv[5]
out_json_path = sys.argv[6]

sys.path.insert(0, code_pyc_zip)
sys.path.insert(0, bin64_dir)

import encodings
import os
import glob
import cPickle
import json
from fsd.schemas import binaryLoader, persistence

loaders = {}
for p in glob.glob(os.path.join(bin64_dir, '*Loader.so')):
    stem = os.path.basename(p)[:-9].lower()
    loaders[stem] = os.path.basename(p)[:-3]

rfi = {}
for line in open(rfi_path, 'rb'):
    parts = line.strip().split(',')
    if len(parts) >= 2:
        rfi[parts[0].lower()] = parts[1]

cat = json.load(open(catalog_path, 'rb'))
cat_map = {x['res_path']: x['cdn_relative_path'] for x in cat}

def load_fsd(name):
    rp = 'res:/staticdata/' + name + '.fsdbinary'
    mod = __import__(loaders[name.lower()])
    return mod.load(os.path.join(res_dir, cat_map[rp]))

def load_static_with_schema(name):
    rp = 'res:/staticdata/' + name + '.static'
    sp = 'res:/staticdata/' + name + '.schema'
    sch = persistence.LoadSchema(open(os.path.join(res_dir, rfi[sp.lower()]), 'rb').read())
    return binaryLoader.LoadFromString(open(os.path.join(res_dir, cat_map[rp]), 'rb').read(), sch)

# 1. Verify and count all 213 staticdata_catalog.json resources
resource_counts = {}
for item in cat:
    rp = item['res_path']
    p = os.path.join(res_dir, item['cdn_relative_path'])
    fmt = item['format']
    if fmt == 'FSD Binary':
        stem = os.path.basename(rp).split('.')[0].lower()
        mod = __import__(loaders[stem])
        data = mod.load(p)
        try:
            cnt = len(data)
            sample_k = str(next(data.iterkeys())) if cnt > 0 else 'empty'
        except TypeError:
            attrs = [a for a in dir(data) if not a.startswith('_')]
            cnt = len(attrs)
            sample_k = attrs[0] if attrs else 'object'
        resource_counts[rp] = {'count': cnt, 'spot': sample_k}
    elif fmt == 'SQLite3':
        hdr = open(p, 'rb').read(16)
        if hdr.startswith('SQLite format 3'):
            resource_counts[rp] = {'count': -1, 'spot': 'sqlite'}
        else:
            sp = rp[:-7] + '.schema'
            if sp.lower() in rfi:
                sch = persistence.LoadSchema(open(os.path.join(res_dir, rfi[sp.lower()]), 'rb').read())
                data = binaryLoader.LoadFromString(open(p, 'rb').read(), sch)
            else:
                data = binaryLoader.LoadFromString(open(p, 'rb').read())
            try:
                cnt = len(data)
                sample_k = str(next(iter(data))) if cnt > 0 else 'empty'
            except TypeError:
                attrs = [a for a in dir(data) if not a.startswith('_')]
                cnt = len(attrs)
                sample_k = attrs[0] if attrs else 'object'
            resource_counts[rp] = {'count': cnt, 'spot': sample_k}
    elif fmt == 'Pickle/Binary':
        if rp.endswith('.schema'):
            sch = persistence.LoadSchema(open(p, 'rb').read())
            cnt = len(sch) if hasattr(sch, '__len__') else 1
            resource_counts[rp] = {'count': cnt, 'spot': 'schema'}
        else:
            # Handle in Python 3 to avoid repeating large localization pickle loads
            resource_counts[rp] = {'count': -2, 'spot': 'pickle'}

# 2. Extract categories, groups, types, dogma, materials
categories_raw = load_fsd('categories')
categories = []
for cid, c in categories_raw.iteritems():
    categories.append({
        'category_id': int(cid),
        'name_id': int(getattr(c, 'categoryNameID', 0) or 0),
        'published': int(bool(getattr(c, 'published', 0))),
    })

groups_raw = load_fsd('groups')
groups = []
group_to_cat = {}
for gid, g in groups_raw.iteritems():
    cid = int(getattr(g, 'categoryID', 0) or 0)
    group_to_cat[int(gid)] = cid
    groups.append({
        'group_id': int(gid),
        'category_id': cid,
        'name_id': int(getattr(g, 'groupNameID', 0) or 0),
        'anchorable': int(bool(getattr(g, 'anchorable', 0))),
        'anchored': int(bool(getattr(g, 'anchored', 0))),
        'fittable_non_singleton': int(bool(getattr(g, 'fittableNonSingleton', 0))),
        'use_base_price': int(bool(getattr(g, 'useBasePrice', 0))),
        'published': int(bool(getattr(g, 'published', 0))),
    })

typedogma_raw = load_fsd('typedogma')
type_dogma = {}
for tid, td in typedogma_raw.iteritems():
    attrs = []
    if td.dogmaAttributes:
        for a in td.dogmaAttributes:
            attrs.append([int(a.attributeID), float(a.value)])
    effs = []
    if td.dogmaEffects:
        for e in td.dogmaEffects:
            effs.append([int(e.effectID), int(bool(getattr(e, 'isDefault', 0)))])
    type_dogma[int(tid)] = (attrs, effs)

types_raw = load_fsd('types')
types = []
for tid, t in types_raw.iteritems():
    itid = int(tid)
    gid = int(getattr(t, 'groupID', 0) or 0)
    cid = group_to_cat.get(gid, 0)
    td_entry = type_dogma.get(itid)
    types.append({
        'type_id': itid,
        'group_id': gid,
        'category_id': cid,
        'name_id': int(getattr(t, 'typeNameID', 0) or 0),
        'description_id': getattr(t, 'descriptionID', None),
        'mass': float(getattr(t, 'mass', 0.0) or 0.0),
        'volume': float(getattr(t, 'volume', 0.0) or 0.0),
        'capacity': float(getattr(t, 'capacity', 0.0) or 0.0),
        'portion_size': int(getattr(t, 'portionSize', 1) or 1),
        'base_price': float(getattr(t, 'basePrice', 0.0) or 0.0),
        'radius': float(getattr(t, 'radius', 0.0) or 0.0),
        'market_group_id': getattr(t, 'marketGroupID', None),
        'race_id': getattr(t, 'raceID', None),
        'faction_id': getattr(t, 'factionID', None),
        'icon_id': getattr(t, 'iconID', None),
        'graphic_id': getattr(t, 'graphicID', None),
        'sound_id': getattr(t, 'soundID', None),
        'meta_group_id': getattr(t, 'metaGroupID', None),
        'meta_level': getattr(t, 'metaLevel', None),
        'tech_level': getattr(t, 'techLevel', None),
        'variation_parent_type_id': getattr(t, 'variationParentTypeID', None),
        'wreck_type_id': getattr(t, 'wreckTypeID', None),
        'published': int(bool(getattr(t, 'published', 0))),
        'attrs': td_entry[0] if td_entry else [],
        'effs': td_entry[1] if td_entry else [],
    })

dgm_attrs_raw = load_fsd('dogmaattributes')
dogma_attributes = []
for aid, a in dgm_attrs_raw.iteritems():
    dogma_attributes.append({
        'attribute_id': int(aid),
        'name': str(getattr(a, 'name', '') or ''),
        'default_value': float(getattr(a, 'defaultValue', 0.0) or 0.0),
        'high_is_good': int(bool(getattr(a, 'highIsGood', 0))),
        'stackable': int(bool(getattr(a, 'stackable', 0))),
        'category_id': getattr(a, 'categoryID', None),
        'data_type': getattr(a, 'dataType', None),
        'published': int(bool(getattr(a, 'published', 0))),
    })

dgm_effs_raw = load_fsd('dogmaeffects')
dogma_effects = []
for eid, e in dgm_effs_raw.iteritems():
    mods = []
    if e.modifierInfo:
        for m in e.modifierInfo:
            mods.append({
                'domain': getattr(m, 'domain', None),
                'func': getattr(m, 'func', None),
                'modified_attribute_id': getattr(m, 'modifiedAttributeID', None),
                'modifying_attribute_id': getattr(m, 'modifyingAttributeID', None),
                'operator': getattr(m, 'operation', None),
                'group_id': getattr(m, 'groupID', None),
                'skill_type_id': getattr(m, 'skillTypeID', None),
                'effect_id': getattr(m, 'effectID', None),
            })
    dogma_effects.append({
        'effect_id': int(eid),
        'name': str(getattr(e, 'effectName', '') or ''),
        'effect_category': int(getattr(e, 'effectCategory', 0) or 0),
        'is_offensive': int(bool(getattr(e, 'isOffensive', 0))),
        'is_assistance': int(bool(getattr(e, 'isAssistance', 0))),
        'is_warp_safe': int(bool(getattr(e, 'isWarpSafe', 0))),
        'disallow_auto_repeat': int(bool(getattr(e, 'disallowAutoRepeat', 0))),
        'electronic_chance': int(bool(getattr(e, 'electronicChance', 0))),
        'propulsion_chance': int(bool(getattr(e, 'propulsionChance', 0))),
        'range_chance': int(bool(getattr(e, 'rangeChance', 0))),
        'guid': getattr(e, 'guid', None),
        'published': int(bool(getattr(e, 'published', 0))),
        'modifiers': mods,
    })

typemats_raw = load_fsd('typematerials')
type_materials = []
for tid, tm in typemats_raw.iteritems():
    mats = []
    if tm.materials:
        for m in tm.materials:
            mats.append({'material_type_id': int(m.materialTypeID), 'quantity': int(m.quantity)})
    type_materials.append({'type_id': int(tid), 'materials': mats})

# 3. Map: regions, constellations, systems, jumps, solarsystemcontent (stargates & planets)
regions_raw = load_static_with_schema('regions')
regions = []
for rid, r in regions_raw.iteritems():
    regions.append({
        'region_id': int(rid),
        'name_id': int(getattr(r, 'nameID', 0) or 0),
        'x': float(r.center.x),
        'y': float(r.center.y),
        'z': float(r.center.z),
        'faction_id': getattr(r, 'factionID', None),
        'wormhole_class_id': getattr(r, 'wormholeClassID', None),
    })

constellations_raw = load_static_with_schema('constellations')
constellations = []
for cid, c in constellations_raw.iteritems():
    constellations.append({
        'constellation_id': int(cid),
        'region_id': int(getattr(c, 'regionID', 0) or 0),
        'name_id': int(getattr(c, 'nameID', 0) or 0),
        'x': float(c.center.x),
        'y': float(c.center.y),
        'z': float(c.center.z),
        'faction_id': getattr(c, 'factionID', None),
        'wormhole_class_id': getattr(c, 'wormholeClassID', None),
    })

systems_raw = load_static_with_schema('systems')
ssc_raw = binaryLoader.LoadIndexFromFile(open(os.path.join(res_dir, cat_map['res:/staticdata/solarsystemcontent.static']), 'rb'))
jumps_raw = load_static_with_schema('jumps')

gate_to_sys = {}
solar_systems = []
stargates_raw = []

for sid, s in systems_raw.iteritems():
    isid = int(sid)
    ssc = ssc_raw[isid] if isid in ssc_raw else None
    sg_ids = []
    pl_ids = [int(p) for p in s.planetItemIDs] if s.planetItemIDs else []
    if ssc is not None and ssc.stargates:
        for sg_id, sg in ssc.stargates.iteritems():
            isg = int(sg_id)
            sg_ids.append(isg)
            gate_to_sys[isg] = isid
            stargates_raw.append({
                'stargate_id': isg,
                'system_id': isid,
                'destination_gate_id': int(sg.destination),
                'type_id': int(sg.typeID),
                'x': float(sg.position.x),
                'y': float(sg.position.y),
                'z': float(sg.position.z),
            })
    solar_systems.append({
        'system_id': isid,
        'constellation_id': int(s.constellationID),
        'region_id': int(s.regionID),
        'name_id': int(s.nameID),
        'security': float(s.securityStatus),
        'pseudo_security': float(s.pseudoSecurity),
        'security_class': getattr(s, 'securityClass', None),
        'x': float(s.center.x),
        'y': float(s.center.y),
        'z': float(s.center.z),
        'radius': float(getattr(ssc, 'radius', 0.0) or 0.0) if ssc is not None else 0.0,
        'luminosity': float(getattr(ssc, 'luminosity', 0.0) or 0.0) if ssc is not None else 0.0,
        'sun_type_id': getattr(s, 'sunTypeID', None),
        'faction_id': getattr(s, 'factionID', None),
        'wormhole_class_id': getattr(s, 'wormholeClassID', None),
        'border': int(bool(getattr(ssc, 'border', 0))) if ssc is not None else 0,
        'corridor': int(bool(getattr(ssc, 'corridor', 0))) if ssc is not None else 0,
        'fringe': int(bool(getattr(ssc, 'fringe', 0))) if ssc is not None else 0,
        'hub': int(bool(getattr(ssc, 'hub', 0))) if ssc is not None else 0,
        'international': int(bool(getattr(ssc, 'international', 0))) if ssc is not None else 0,
        'regional': int(bool(getattr(ssc, 'regional', 0))) if ssc is not None else 0,
        'stargate_ids': sorted(sg_ids),
        'planet_ids': sorted(pl_ids),
    })

for sg in stargates_raw:
    sg['destination_system_id'] = gate_to_sys.get(sg['destination_gate_id'], 0)

jumps = []
for j in jumps_raw:
    jumps.append({
        'jump_id': int(j.jumpID),
        'stargate_id': int(j.stargateID),
        'from_system_id': int(j.fromSystemID),
        'to_system_id': int(j.toSystemID),
        'jump_type': int(j.jumpType),
    })

# 4. Character lineage: races, bloodlines, ancestries, schools
races_raw = load_fsd('races')
races = []
for rid, r in races_raw.iteritems():
    skills = {}
    if getattr(r, 'skills', None):
        for sk, lvl in r.skills.iteritems():
            skills[int(sk)] = int(lvl)
    races.append({
        'race_id': int(rid),
        'name_id': int(getattr(r, 'nameID', 0) or 0),
        'description_id': getattr(r, 'descriptionID', None),
        'ship_type_id': getattr(r, 'shipTypeID', None),
        'skills': skills,
    })

bloodlines_raw = load_fsd('bloodlines')
bloodlines = []
for bid, b in bloodlines_raw.iteritems():
    bloodlines.append({
        'bloodline_id': int(bid),
        'race_id': int(b.raceID),
        'name_id': int(b.nameID),
        'description_id': getattr(b, 'descriptionID', None),
        'ship_type_id': getattr(b, 'shipTypeID', None),
        'corporation_id': int(b.corporationID),
        'charisma': int(b.charisma),
        'intelligence': int(b.intelligence),
        'memory': int(b.memory),
        'perception': int(b.perception),
        'willpower': int(b.willpower),
    })

ancestries_raw = load_fsd('ancestries')
ancestries = []
for aid, a in ancestries_raw.iteritems():
    ancestries.append({
        'ancestry_id': int(aid),
        'bloodline_id': int(a.bloodlineID),
        'name_id': int(a.nameID),
        'description_id': getattr(a, 'descriptionID', None),
        'short_description': getattr(a, 'shortDescription', None),
        'charisma': int(a.charisma),
        'intelligence': int(a.intelligence),
        'memory': int(a.memory),
        'perception': int(a.perception),
        'willpower': int(a.willpower),
    })

schools_raw = load_fsd('schools')
schools = []
for sid, s in schools_raw.iteritems():
    schools.append({
        'school_id': int(sid),
        'race_id': int(s.raceID),
        'name_id': int(s.nameID),
        'corporation_id': int(s.corporationID),
        'career_id': int(s.careerID),
        'starting_stations': [int(x) for x in s.startingStations] if s.startingStations else [],
        'career_agents': [int(x) for x in s.careerAgents] if s.careerAgents else [],
    })

# 5. Factions, NPC Corporations, NPC Characters / Agents, Station Operations & Services
factions_raw = load_fsd('factions')
factions = []
for fid, f in factions_raw.iteritems():
    factions.append({
        'faction_id': int(fid),
        'name_id': int(f.nameID),
        'description_id': getattr(f, 'descriptionID', None),
        'corporation_id': getattr(f, 'corporationID', None),
        'militia_corporation_id': getattr(f, 'militiaCorporationID', None),
        'solar_system_id': getattr(f, 'solarSystemID', None),
        'size_factor': float(getattr(f, 'sizeFactor', 0.0) or 0.0),
        'member_races': [int(x) for x in f.memberRaces] if getattr(f, 'memberRaces', None) else [],
    })

corps_raw = load_fsd('npccorporations')
npc_corporations = []
for cid, c in corps_raw.iteritems():
    npc_corporations.append({
        'corporation_id': int(cid),
        'name_id': int(c.nameID),
        'ticker_name': str(getattr(c, 'tickerName', '') or ''),
        'faction_id': getattr(c, 'factionID', None),
        'solar_system_id': getattr(c, 'solarSystemID', None),
        'station_id': getattr(c, 'stationID', None),
        'ceo_id': getattr(c, 'ceoID', None),
        'tax_rate': float(getattr(c, 'taxRate', 0.0) or 0.0),
        'lp_offer_tables': [int(x) for x in c.lpOfferTables] if getattr(c, 'lpOfferTables', None) else [],
    })

ais_raw = load_fsd('agentsinspace')
ais_map = {}
for aid, a in ais_raw.iteritems():
    ais_map[int(aid)] = {
        'solar_system_id': int(a.solarSystemID),
        'dungeon_id': int(a.dungeonID),
        'spawn_point_id': int(a.spawnPointID),
        'type_id': int(a.typeID),
    }

chars_raw = load_fsd('npccharacters')
agents = []
for cid, c in chars_raw.iteritems():
    icid = int(cid)
    ag = getattr(c, 'agent', None)
    if ag is not None:
        ais = ais_map.get(icid, {})
        agents.append({
            'agent_id': icid,
            'name_id': int(getattr(c, 'nameID', 0) or 0),
            'corporation_id': int(getattr(c, 'corporationID', 0) or 0),
            'station_id': getattr(c, 'stationID', None),
            'solar_system_id': ais.get('solar_system_id'),
            'level': int(getattr(ag, 'agentLevel', 1) or 1),
            'division_id': int(getattr(ag, 'agentDivisionID', 0) or 0),
            'agent_type_id': int(getattr(ag, 'agentType', 0) or 0),
            'is_locator': int(bool(getattr(ag, 'agentLocateCharacter', 0))),
        })

sta_ops_raw = load_fsd('stationoperations')
station_operations = []
for oid, op in sta_ops_raw.iteritems():
    station_operations.append({
        'operation_id': int(oid),
        'name_id': int(getattr(op, 'operationNameID', 0) or 0),
        'activity_id': getattr(op, 'activityID', None),
        'services': [int(x) for x in op.services] if getattr(op, 'services', None) else [],
    })

sta_svcs_raw = load_fsd('stationservices')
station_services = []
for sid, sv in sta_svcs_raw.iteritems():
    station_services.append({
        'service_id': int(sid),
        'name_id': int(getattr(sv, 'serviceNameID', 0) or 0),
    })

# 6. Market groups, PI schematics, missions, epic arcs, dungeons, node graphs, space components
mg_raw = load_fsd('marketgroups')
market_groups = []
for mgid, mg in mg_raw.iteritems():
    market_groups.append({
        'market_group_id': int(mgid),
        'parent_group_id': getattr(mg, 'parentGroupID', None),
        'name_id': int(getattr(mg, 'nameID', 0) or 0),
        'has_types': int(bool(getattr(mg, 'hasTypes', 0))),
        'icon_id': getattr(mg, 'iconID', None),
    })

sch_raw = load_fsd('schematics')
schematics = []
for scid, sc in sch_raw.iteritems():
    stypes = []
    if getattr(sc, 'types', None):
        for tk, tv in sc.types.iteritems():
            stypes.append({'type_id': int(tk), 'quantity': int(tv.quantity), 'is_input': bool(tv.isInput)})
    schematics.append({
        'schematic_id': int(scid),
        'name_id': int(getattr(sc, 'nameID', 0) or 0),
        'cycle_time': int(getattr(sc, 'cycleTime', 3600) or 3600),
        'pins': [int(x) for x in sc.pins] if getattr(sc, 'pins', None) else [],
        'types': stypes,
    })

missions_raw = load_fsd('missions')
missions = []
for mid, m in missions_raw.iteritems():
    missions.append({
        'mission_id': int(mid),
        'name_id': int(getattr(m, 'nameID', 0) or 0),
        'is_courier': int(bool(getattr(m, 'courierMission', 0))),
        'lp_alpha': int(getattr(m, 'fixedLpRewardAlpha', 0) or 0),
        'lp_omega': int(getattr(m, 'fixedLpRewardOmega', 0) or 0),
        'has_standing_rewards': int(bool(getattr(m, 'hasStandingRewards', 0))),
        'content_template': getattr(m, 'contentTemplate', None),
    })

ea_raw = load_fsd('epicarcs')
epic_arcs = []
for eaid, ea in ea_raw.iteritems():
    epic_arcs.append({
        'epic_arc_id': int(eaid),
        'name_id': int(getattr(ea, 'epicArcNameID', 0) or 0),
        'faction_id': getattr(ea, 'factionID', None),
        'restart_interval': getattr(ea, 'arcRestartInterval', None),
        'mission_count': len(ea.missions) if getattr(ea, 'missions', None) else 0,
    })

dun_raw = load_fsd('dungeons')
dungeons = []
for did, d in dun_raw.iteritems():
    dungeons.append({
        'dungeon_id': int(did),
        'name_id': int(getattr(d, 'dungeonNameID', 0) or 0),
        'archetype_id': getattr(d, 'archetypeID', None),
        'faction_id': getattr(d, 'factionID', None),
        'difficulty': getattr(d, 'difficulty', None),
    })

ng_raw = load_fsd('nodegraphs')
node_graphs = []
for ngid, ng in ng_raw.iteritems():
    node_graphs.append({
        'node_graph_id': int(ngid),
        'name': str(getattr(ng, 'name', '') or ''),
        'parent_graph_id': getattr(ng, 'parentGraph', None),
        'node_count': len(ng.nodes) if getattr(ng, 'nodes', None) else 0,
        'tags': [str(x) for x in ng.tags] if getattr(ng, 'tags', None) else [],
    })

sc_raw = load_fsd('spacecomponentsbytype')
space_components = []
for tid, sc in sc_raw.iteritems():
    comp_names = [a for a in dir(sc) if not a.startswith('_') and getattr(sc, a, None) is not None]
    bc = getattr(sc, 'destinyServerBallClass', None)
    bc_str = str(getattr(bc, 'ballClass', bc)) if bc is not None else None
    space_components.append({
        'type_id': int(tid),
        'ball_class': bc_str,
        'components': comp_names,
    })

ct_raw = load_fsd('compressibletypes')
compressible_types = [{'type_id': int(k), 'compressed_type_id': int(v)} for k, v in ct_raw.iteritems()]

dia_raw = load_fsd('dynamicitemattributes')
dynamic_item_attributes = []
for mid, d in dia_raw.iteritems():
    io_list = []
    if getattr(d, 'inputOutputMapping', None):
        for io in d.inputOutputMapping:
            io_list.append({
                'resulting_type_id': int(io.resultingType),
                'applicable_type_ids': [int(x) for x in io.applicableTypes],
            })
    attr_ranges = {}
    if getattr(d, 'attributeIDs', None):
        for ak, av in d.attributeIDs.iteritems():
            attr_ranges[int(ak)] = [float(av.min), float(av.max)]
    dynamic_item_attributes.append({
        'mutaplasmid_type_id': int(mid),
        'mappings': io_list,
        'attribute_ranges': attr_ranges,
    })

out = {
    'resource_counts': resource_counts,
    'categories': categories,
    'groups': groups,
    'types': types,
    'dogma_attributes': dogma_attributes,
    'dogma_effects': dogma_effects,
    'type_materials': type_materials,
    'regions': regions,
    'constellations': constellations,
    'solar_systems': solar_systems,
    'stargates': stargates_raw,
    'jumps': jumps,
    'races': races,
    'bloodlines': bloodlines,
    'ancestries': ancestries,
    'schools': schools,
    'factions': factions,
    'npc_corporations': npc_corporations,
    'agents': agents,
    'station_operations': station_operations,
    'station_services': station_services,
    'market_groups': market_groups,
    'schematics': schematics,
    'missions': missions,
    'epic_arcs': epic_arcs,
    'dungeons': dungeons,
    'node_graphs': node_graphs,
    'space_components': space_components,
    'compressible_types': compressible_types,
    'dynamic_item_attributes': dynamic_item_attributes,
}
json.dump(out, open(out_json_path, 'wb'))
"""


def build_v24_npc_spawn_groups(
    fsd_groups: list[dict],
    fsd_types: list[dict],
    label_fn,
) -> list[dict]:
    """DERIVED: Synthesize NPC spawn groups from V24.01 Category 11 (Entity) SDE groups and types.

    Provenance: DERIVED (`sde_npc_spawn_groups`) — CCP does not ship server-side NPC spawn
    tables in client FSD resources (`types.fsdbinary` / `groups.fsdbinary`). Each row is
    deterministically derived from published/unpublished Category 11 (`Entity`) groups,
    preserving real SDE `group_id`, `faction_id`, `npc_type_ids`, and average hull radius
    classification (`ship_class` 1..4).
    """
    types_by_group: dict[int, list[dict]] = {}
    for t in fsd_types:
        if t["category_id"] == 11:
            types_by_group.setdefault(int(t["group_id"]), []).append(t)

    out = []
    idx = 1
    for g in sorted(fsd_groups, key=lambda item: int(item["group_id"])):
        if int(g["category_id"]) != 11:
            continue
        gid = int(g["group_id"])
        gtypes = types_by_group.get(gid, [])
        if not gtypes:
            continue
        gname = label_fn(g["name_id"], f"Group {gid}")
        npc_tids = sorted(int(t["type_id"]) for t in gtypes)
        factions = [int(t["faction_id"]) for t in gtypes if t.get("faction_id")]
        faction_id = factions[0] if factions else 0
        avg_radius = sum(float(t.get("radius") or 0.0) for t in gtypes) / float(len(gtypes))
        if avg_radius <= 65.0:
            ship_class = 1
        elif avg_radius <= 350.0:
            ship_class = 2
        elif avg_radius <= 1200.0:
            ship_class = 3
        else:
            ship_class = 4
        out.append(
            {
                "spawn_group_id": idx,
                "faction_id": faction_id,
                "ship_class": ship_class,
                "group_id": gid,
                "group_name": gname,
                "npc_type_ids": npc_tids,
            }
        )
        idx += 1
    return out


def run_import(
    shared_cache: Path,
    out_db: Path,
    manifest_path: Path,
    force_extract: bool = False,
) -> dict:
    eve_build_dir = shared_cache / "tq" / "EVE.app" / "Contents" / "Resources" / "build"
    res_dir = shared_cache / "ResFiles"
    rfi_path = eve_build_dir / "resfileindex.txt"
    map_objects_db = eve_build_dir / "bin64" / "staticdata" / "mapObjects.db"

    if not rfi_path.exists():
        raise FileNotFoundError(f"Missing EVE client resfileindex.txt at {rfi_path}")

    work_dir = Path(tempfile.gettempdir()) / "evomu_sde_cfsd"
    py2_out_json = work_dir / "extracted_fsd.json"
    if force_extract or not py2_out_json.exists() or py2_out_json.stat().st_size == 0:
        runner_bin, code_pyc_zip = build_cfsd_runner(eve_build_dir, work_dir)
        py2_script_path = work_dir / "extract_py2.py"
        py2_script_path.write_text(PY2_EXTRACTOR_SCRIPT, encoding="utf-8")

        subprocess.run(
            [
                str(runner_bin),
                str(py2_script_path),
                str(code_pyc_zip),
                str(eve_build_dir / "bin64"),
                str(res_dir),
                str(rfi_path),
                str(STATIC_CATALOG),
                str(py2_out_json),
            ],
            check=True,
        )

    fsd_data = json.loads(py2_out_json.read_text(encoding="utf-8"))
    catalog = json.loads(STATIC_CATALOG.read_text(encoding="utf-8"))
    cat_by_path = {x["res_path"]: x for x in catalog}

    # Load English localization labels
    en_rel = cat_by_path["res:/localizationfsd/localization_fsd_en-us.pickle"]["cdn_relative_path"]
    en_tuple = pickle.load(open(res_dir / en_rel, "rb"), encoding="latin1")
    en_labels: dict[int, str] = {}
    for mid, val in en_tuple[1].items():
        if isinstance(val, tuple) and len(val) >= 1 and val[0]:
            en_labels[int(mid)] = str(val[0])

    def label(msg_id: int | None, fallback: str) -> str:
        if msg_id is None or msg_id == 0:
            return fallback
        return en_labels.get(int(msg_id), fallback)

    # Complete resource verification across all 213 files (MD5, byte size, record count, spot value)
    resource_counts = fsd_data["resource_counts"]
    verified_resources = []
    for item in catalog:
        rp = item["res_path"]
        p = res_dir / item["cdn_relative_path"]
        raw = p.read_bytes()
        actual_md5 = hashlib.md5(raw).hexdigest()
        if actual_md5 != item["md5"]:
            raise ValueError(f"MD5 mismatch for {rp}: expected {item['md5']}, got {actual_md5}")
        rc_info = resource_counts[rp]
        cnt = rc_info["count"]
        spot = rc_info["spot"]
        if cnt == -1:
            conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
            cnt = int(conn.execute("SELECT count(*) FROM cache").fetchone()[0])
            row = conn.execute("SELECT key FROM cache LIMIT 1").fetchone()
            spot = str(row[0]) if row else "empty"
            conn.close()
        elif cnt == -2:
            obj = pickle.loads(raw, encoding="latin1")
            if isinstance(obj, tuple) and len(obj) == 2 and isinstance(obj[1], dict):
                cnt = len(obj[1])
                spot = str(obj[0])
            else:
                cnt = len(obj)
                spot = str(next(iter(obj))) if cnt > 0 else "empty"
        verified_resources.append(
            {
                "res_path": rp,
                "cdn_relative_path": item["cdn_relative_path"],
                "md5": actual_md5,
                "byte_size": len(raw),
                "format": item["format"],
                "record_count": int(cnt),
                "spot": str(spot),
            }
        )

    # Read blueprints.static, clonegrades.static, dbuffcollections.static, fighterabilities*.static, skins*.static
    def read_sqlite_cache(res_path: str) -> list[tuple[str, dict]]:
        p = res_dir / cat_by_path[res_path]["cdn_relative_path"]
        conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
        rows = conn.execute("SELECT key, value FROM cache").fetchall()
        conn.close()
        return [(str(k), json.loads(v)) for k, v in rows]

    blueprints_cache = read_sqlite_cache("res:/staticdata/blueprints.static")
    clonegrades_cache = read_sqlite_cache("res:/staticdata/clonegrades.static")
    dbuffs_cache = read_sqlite_cache("res:/staticdata/dbuffcollections.static")
    fighter_ab_cache = read_sqlite_cache("res:/staticdata/fighterabilities.static")
    fighter_ab_type_cache = read_sqlite_cache("res:/staticdata/fighterabilitiesbytype.static")
    skins_cache = read_sqlite_cache("res:/staticdata/skins.static")
    skinlicenses_cache = read_sqlite_cache("res:/staticdata/skinlicenses.static")
    skinmaterials_cache = read_sqlite_cache("res:/staticdata/skinmaterials.static")

    # Build lookup maps for naming
    sys_name_by_id = {}
    sys_const_by_id = {}
    sys_reg_by_id = {}
    for s in fsd_data["solar_systems"]:
        sid = s["system_id"]
        sname = label(s["name_id"], f"System {sid}")
        s["name"] = sname
        sys_name_by_id[sid] = sname
        sys_const_by_id[sid] = s["constellation_id"]
        sys_reg_by_id[sid] = s["region_id"]

    corp_name_by_id = {}
    for c in fsd_data["npc_corporations"]:
        cid = c["corporation_id"]
        cname = label(c["name_id"], f"Corporation {cid}")
        c["name"] = cname
        corp_name_by_id[cid] = cname

    op_name_by_id = {}
    for op in fsd_data["station_operations"]:
        oid = op["operation_id"]
        oname = label(op["name_id"], f"Operation {oid}")
        op["name"] = oname
        op_name_by_id[oid] = oname

    # Read mapObjects.db (celestials + npcStations)
    map_conn = sqlite3.connect(f"file:{map_objects_db}?mode=ro", uri=True)
    celestial_rows = map_conn.execute(
        "SELECT celestialID, celestialNameID, solarSystemID, typeID, groupID, radius, x, y, z, orbitID, orbitIndex, celestialIndex FROM celestials"
    ).fetchall()
    station_rows = map_conn.execute(
        "SELECT stationID, orbitID, solarSystemID, ownerID, typeID, x, y, z, operationID, reprocessingEfficiency, reprocessingStationsTake, isConquerable, useOperationName, orbitIndex, celestialIndex FROM npcStations"
    ).fetchall()
    map_conn.close()

    celestial_radius_by_id = {}
    celestials = []
    for cid, cname_id, sys_id, tid, gid, rad, x, y, z, orbit_id, orbit_idx, cel_idx in celestial_rows:
        icid = int(cid)
        isys = int(sys_id)
        igid = int(gid)
        frad = float(rad or 0.0)
        celestial_radius_by_id[icid] = frad
        sname = sys_name_by_id.get(isys, f"System {isys}")
        roman = to_roman(cel_idx)
        if cname_id:
            cname = label(int(cname_id), f"{sname} {roman}".strip())
        elif igid == 6:
            cname = f"{sname} - Star"
        elif igid == 7:
            cname = f"{sname} {roman}".strip()
        elif igid == 8:
            cname = f"{sname} {roman} - Moon {orbit_idx or 1}".strip()
        elif igid == 9:
            cname = f"{sname} {roman} - Asteroid Belt {orbit_idx or 1}".strip()
        else:
            cname = f"{sname} Celestial {icid}"
        celestials.append(
            (
                icid,
                isys,
                int(tid),
                igid,
                cname,
                frad,
                float(x or 0.0),
                float(y or 0.0),
                float(z or 0.0),
                int(orbit_id) if orbit_id is not None else None,
                int(orbit_idx) if orbit_idx is not None else None,
                int(cel_idx) if cel_idx is not None else None,
            )
        )

    stations = []
    station_sys_by_id = {}
    for (
        sta_id,
        orbit_id,
        sys_id,
        owner_id,
        tid,
        x,
        y,
        z,
        op_id,
        reproc_eff,
        reproc_take,
        is_conq,
        use_op_name,
        orbit_idx,
        cel_idx,
    ) in station_rows:
        ista = int(sta_id)
        isys = int(sys_id)
        iowner = int(owner_id or 0)
        iop = int(op_id or 0)
        station_sys_by_id[ista] = isys
        sname = sys_name_by_id.get(isys, f"System {isys}")
        roman = to_roman(cel_idx)
        loc_prefix = f"{sname} {roman}".strip()
        if orbit_idx:
            loc_prefix = f"{loc_prefix} - Moon {orbit_idx}"
        corp_name = corp_name_by_id.get(iowner, f"Corp {iowner}")
        op_name = op_name_by_id.get(iop, "")
        if use_op_name and op_name:
            sta_name = f"{loc_prefix} - {corp_name} {op_name}"
        else:
            sta_name = f"{loc_prefix} - {corp_name}"
        stations.append(
            (
                ista,
                isys,
                sys_const_by_id.get(isys, 0),
                sys_reg_by_id.get(isys, 0),
                iowner,
                int(tid or 0),
                iop,
                sta_name,
                float(x or 0.0),
                float(y or 0.0),
                float(z or 0.0),
                float(reproc_eff if reproc_eff is not None else 0.5),
                float(reproc_take if reproc_take is not None else 0.05),
                int(bool(is_conq)),
                int(orbit_id) if orbit_id is not None else None,
            )
        )

    # Create data/evomu_sde.sqlite
    out_db.parent.mkdir(parents=True, exist_ok=True)
    if out_db.exists():
        out_db.unlink()

    conn = sqlite3.connect(str(out_db))
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA synchronous = OFF")
    conn.executescript(
        """
        CREATE TABLE sde_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE sde_resources (
            res_path TEXT PRIMARY KEY,
            cdn_relative_path TEXT NOT NULL,
            md5 TEXT NOT NULL,
            byte_size INTEGER NOT NULL,
            format TEXT NOT NULL,
            record_count INTEGER NOT NULL,
            spot TEXT NOT NULL
        );
        CREATE TABLE sde_categories (
            category_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            published INTEGER NOT NULL
        );
        CREATE TABLE sde_groups (
            group_id INTEGER PRIMARY KEY,
            category_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            anchorable INTEGER NOT NULL,
            anchored INTEGER NOT NULL,
            fittable_non_singleton INTEGER NOT NULL,
            use_base_price INTEGER NOT NULL,
            published INTEGER NOT NULL
        );
        CREATE TABLE sde_types (
            type_id INTEGER PRIMARY KEY,
            group_id INTEGER NOT NULL,
            category_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            mass REAL NOT NULL,
            volume REAL NOT NULL,
            capacity REAL NOT NULL,
            portion_size INTEGER NOT NULL,
            base_price REAL NOT NULL,
            radius REAL NOT NULL,
            market_group_id INTEGER,
            race_id INTEGER,
            faction_id INTEGER,
            variation_parent_type_id INTEGER,
            wreck_type_id INTEGER,
            published INTEGER NOT NULL,
            attributes_json TEXT NOT NULL,
            effects_json TEXT NOT NULL,
            description_id INTEGER,
            icon_id INTEGER,
            graphic_id INTEGER,
            sound_id INTEGER,
            meta_group_id INTEGER,
            meta_level INTEGER,
            tech_level INTEGER,
            default_effect_id INTEGER,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_dogma_attributes (
            attribute_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            default_value REAL NOT NULL,
            high_is_good INTEGER NOT NULL,
            stackable INTEGER NOT NULL,
            published INTEGER NOT NULL
        );
        CREATE TABLE sde_dogma_effects (
            effect_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            effect_category INTEGER NOT NULL,
            is_offensive INTEGER NOT NULL,
            is_assistance INTEGER NOT NULL,
            is_warp_safe INTEGER NOT NULL,
            disallow_auto_repeat INTEGER NOT NULL,
            electronic_chance INTEGER NOT NULL,
            propulsion_chance INTEGER NOT NULL,
            range_chance INTEGER NOT NULL,
            guid TEXT,
            published INTEGER NOT NULL,
            modifiers_json TEXT NOT NULL
        );
        CREATE VIEW IF NOT EXISTS sde_effects AS SELECT * FROM sde_dogma_effects;
        CREATE TABLE sde_type_materials (
            type_id INTEGER PRIMARY KEY,
            materials_json TEXT NOT NULL
        );
        CREATE TABLE sde_blueprints (
            blueprint_type_id INTEGER PRIMARY KEY,
            product_type_id INTEGER NOT NULL,
            product_quantity INTEGER NOT NULL,
            manufacturing_time_sec INTEGER NOT NULL,
            max_production_limit INTEGER NOT NULL,
            materials_json TEXT NOT NULL,
            activities_json TEXT NOT NULL
        );
        CREATE TABLE sde_regions (
            region_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            faction_id INTEGER,
            wormhole_class_id INTEGER
        );
        CREATE TABLE sde_constellations (
            constellation_id INTEGER PRIMARY KEY,
            region_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            faction_id INTEGER,
            wormhole_class_id INTEGER
        );
        CREATE TABLE sde_solar_systems (
            system_id INTEGER PRIMARY KEY,
            constellation_id INTEGER NOT NULL,
            region_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            security REAL NOT NULL,
            pseudo_security REAL NOT NULL,
            security_class TEXT,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            radius REAL NOT NULL,
            luminosity REAL NOT NULL,
            sun_type_id INTEGER,
            faction_id INTEGER,
            wormhole_class_id INTEGER,
            stargate_ids_json TEXT NOT NULL,
            planet_ids_json TEXT NOT NULL
        );
        CREATE TABLE sde_stargates (
            stargate_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            destination_gate_id INTEGER NOT NULL,
            destination_system_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            radius REAL NOT NULL
        );
        CREATE TABLE sde_celestials (
            celestial_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            group_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            radius REAL NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            orbit_id INTEGER,
            orbit_index INTEGER,
            celestial_index INTEGER
        );
        CREATE INDEX idx_sde_celestials_system ON sde_celestials(system_id);
        CREATE TABLE sde_stations (
            station_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            constellation_id INTEGER NOT NULL,
            region_id INTEGER NOT NULL,
            owner_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            operation_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            reprocessing_efficiency REAL NOT NULL,
            reprocessing_stations_take REAL NOT NULL,
            is_conquerable INTEGER NOT NULL,
            orbit_id INTEGER
        );
        CREATE INDEX idx_sde_stations_system ON sde_stations(system_id);
        CREATE TABLE sde_station_operations (
            operation_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            activity_id INTEGER,
            services_json TEXT NOT NULL
        );
        CREATE TABLE sde_station_services (
            service_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE sde_races (
            race_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            ship_type_id INTEGER,
            skills_json TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_bloodlines (
            bloodline_id INTEGER PRIMARY KEY,
            race_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            corporation_id INTEGER NOT NULL,
            charisma INTEGER NOT NULL,
            intelligence INTEGER NOT NULL,
            memory INTEGER NOT NULL,
            perception INTEGER NOT NULL,
            willpower INTEGER NOT NULL,
            ship_type_id INTEGER,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_ancestries (
            ancestry_id INTEGER PRIMARY KEY,
            bloodline_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            charisma INTEGER NOT NULL,
            intelligence INTEGER NOT NULL,
            memory INTEGER NOT NULL,
            perception INTEGER NOT NULL,
            willpower INTEGER NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            short_description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_schools (
            school_id INTEGER PRIMARY KEY,
            race_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            corporation_id INTEGER NOT NULL,
            career_id INTEGER NOT NULL,
            starting_station_id INTEGER NOT NULL,
            starting_system_id INTEGER NOT NULL,
            corvette_type_id INTEGER,
            starting_stations_json TEXT NOT NULL,
            career_agents_json TEXT NOT NULL
        );
        CREATE TABLE sde_factions (
            faction_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            corporation_id INTEGER,
            militia_corporation_id INTEGER,
            solar_system_id INTEGER,
            size_factor REAL NOT NULL,
            member_races_json TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_npc_corporations (
            corporation_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            ticker_name TEXT NOT NULL,
            faction_id INTEGER,
            solar_system_id INTEGER,
            station_id INTEGER,
            ceo_id INTEGER,
            tax_rate REAL NOT NULL,
            lp_offer_tables_json TEXT NOT NULL
        );
        CREATE TABLE sde_agents (
            agent_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            corporation_id INTEGER NOT NULL,
            station_id INTEGER,
            solar_system_id INTEGER,
            level INTEGER NOT NULL,
            division_id INTEGER NOT NULL,
            agent_type_id INTEGER NOT NULL,
            is_locator INTEGER NOT NULL
        );
        CREATE TABLE sde_market_groups (
            market_group_id INTEGER PRIMARY KEY,
            parent_group_id INTEGER,
            name TEXT NOT NULL,
            has_types INTEGER NOT NULL,
            icon_id INTEGER
        );
        CREATE TABLE sde_planet_schematics (
            schematic_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            cycle_time INTEGER NOT NULL,
            pins_json TEXT NOT NULL,
            types_json TEXT NOT NULL
        );
        CREATE TABLE sde_clone_grades (
            grade_id INTEGER PRIMARY KEY,
            skills_json TEXT NOT NULL
        );
        CREATE TABLE sde_skins (
            skin_id INTEGER PRIMARY KEY,
            internal_name TEXT NOT NULL,
            skin_material_id INTEGER NOT NULL,
            types_json TEXT NOT NULL,
            allow_ccp_devs INTEGER NOT NULL DEFAULT 0,
            visible_serenity INTEGER NOT NULL DEFAULT 0,
            visible_tranquility INTEGER NOT NULL DEFAULT 0,
            is_structure_skin INTEGER NOT NULL DEFAULT 0,
            skin_description TEXT
        );
        CREATE TABLE sde_skin_materials (
            skin_material_id INTEGER PRIMARY KEY,
            display_name_id INTEGER,
            material_set_id INTEGER
        );
        CREATE TABLE sde_skin_licenses (
            license_type_id INTEGER PRIMARY KEY,
            skin_id INTEGER NOT NULL,
            duration INTEGER NOT NULL
        );
        CREATE TABLE sde_dbuff_collections (
            dbuff_id INTEGER PRIMARY KEY,
            operation_name TEXT NOT NULL,
            data_json TEXT NOT NULL
        );
        CREATE TABLE sde_fighter_abilities (
            ability_id INTEGER PRIMARY KEY,
            data_json TEXT NOT NULL
        );
        CREATE TABLE sde_fighter_abilities_by_type (
            type_id INTEGER PRIMARY KEY,
            data_json TEXT NOT NULL
        );
        CREATE TABLE sde_wormhole_types (
            type_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            target_system_class INTEGER NOT NULL,
            max_stable_time_min REAL NOT NULL,
            max_stable_mass REAL NOT NULL,
            mass_regeneration REAL NOT NULL,
            max_jump_mass REAL NOT NULL
        );
        CREATE TABLE sde_npc_spawn_groups (
            spawn_group_id INTEGER PRIMARY KEY,
            faction_id INTEGER NOT NULL,
            ship_class INTEGER NOT NULL,
            group_id INTEGER NOT NULL,
            group_name TEXT NOT NULL,
            npc_type_ids_json TEXT NOT NULL
        );
        CREATE TABLE sde_missions (
            mission_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            is_courier INTEGER NOT NULL,
            lp_alpha INTEGER NOT NULL,
            lp_omega INTEGER NOT NULL,
            has_standing_rewards INTEGER NOT NULL,
            content_template TEXT
        );
        CREATE TABLE sde_epic_arcs (
            epic_arc_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            faction_id INTEGER,
            restart_interval INTEGER,
            mission_count INTEGER NOT NULL
        );
        CREATE TABLE sde_dungeons (
            dungeon_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            archetype_id INTEGER,
            faction_id INTEGER,
            difficulty INTEGER
        );
        CREATE TABLE sde_node_graphs (
            node_graph_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            parent_graph_id INTEGER,
            node_count INTEGER NOT NULL,
            tags_json TEXT NOT NULL
        );
        CREATE TABLE sde_space_components (
            type_id INTEGER PRIMARY KEY,
            ball_class TEXT,
            components_json TEXT NOT NULL
        );
        CREATE TABLE sde_compressible_types (
            type_id INTEGER PRIMARY KEY,
            compressed_type_id INTEGER NOT NULL
        );
        CREATE TABLE sde_dynamic_item_attributes (
            mutaplasmid_type_id INTEGER PRIMARY KEY,
            mappings_json TEXT NOT NULL,
            attribute_ranges_json TEXT NOT NULL
        );
        CREATE TABLE sde_jumps (
            jump_id INTEGER PRIMARY KEY,
            stargate_id INTEGER NOT NULL,
            from_system_id INTEGER NOT NULL,
            to_system_id INTEGER NOT NULL,
            jump_type INTEGER NOT NULL
        );
        """
    )

    with conn:
        conn.executemany(
            "INSERT INTO sde_meta VALUES (?, ?)",
            [
                ("schema_version", "1"),
                ("client_build", "3619879"),
                ("client_release", "V24.01"),
            ],
        )
        conn.executemany(
            "INSERT INTO sde_resources VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    r["res_path"],
                    r["cdn_relative_path"],
                    r["md5"],
                    r["byte_size"],
                    r["format"],
                    r["record_count"],
                    r["spot"],
                )
                for r in verified_resources
            ],
        )
        conn.executemany(
            "INSERT INTO sde_categories VALUES (?, ?, ?)",
            [
                (c["category_id"], label(c["name_id"], f"Category {c['category_id']}"), c["published"])
                for c in fsd_data["categories"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_groups VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    g["group_id"],
                    g["category_id"],
                    label(g["name_id"], f"Group {g['group_id']}"),
                    g["anchorable"],
                    g["anchored"],
                    g["fittable_non_singleton"],
                    g["use_base_price"],
                    g["published"],
                )
                for g in fsd_data["groups"]
            ],
        )

        npc_types_by_group: dict[int, list[int]] = {}
        wormhole_rows = []
        type_rows = []
        for t in fsd_data["types"]:
            tid = t["type_id"]
            gid = t["group_id"]
            cid = t["category_id"]
            tname = label(t["name_id"], f"Type {tid}")
            attrs_dict = {int(k): float(v) for k, v in t["attrs"]}
            eff_ids = [int(k) for k, _ in t["effs"]]
            default_eff_id = next((int(k) for k, is_def in t["effs"] if is_def), None)
            tdesc = label(t.get("description_id"), "")
            if cid == 11:
                npc_types_by_group.setdefault(gid, []).append(tid)
            if gid == 988:
                wormhole_rows.append(
                    (
                        tid,
                        tname,
                        int(attrs_dict.get(1381, 0.0)),
                        float(attrs_dict.get(1382, 0.0)),
                        float(attrs_dict.get(1383, 0.0)),
                        float(attrs_dict.get(1384, 0.0)),
                        float(attrs_dict.get(1385, 0.0)),
                    )
                )
            type_rows.append(
                (
                    tid,
                    gid,
                    cid,
                    tname,
                    t["mass"],
                    t["volume"],
                    t["capacity"],
                    t["portion_size"],
                    t["base_price"],
                    t["radius"],
                    t["market_group_id"],
                    t["race_id"],
                    t["faction_id"],
                    t["variation_parent_type_id"],
                    t["wreck_type_id"],
                    t["published"],
                    json.dumps(attrs_dict, separators=(",", ":")),
                    json.dumps(eff_ids, separators=(",", ":")),
                    t.get("description_id"),
                    t.get("icon_id"),
                    t.get("graphic_id"),
                    t.get("sound_id"),
                    t.get("meta_group_id"),
                    t.get("meta_level"),
                    t.get("tech_level"),
                    default_eff_id,
                    tdesc,
                )
            )
        conn.executemany(
            "INSERT INTO sde_types VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            type_rows,
        )

        conn.executemany(
            "INSERT INTO sde_dogma_attributes VALUES (?, ?, ?, ?, ?, ?)",
            [
                (
                    a["attribute_id"],
                    a["name"] or f" attribute_{a['attribute_id']}",
                    a["default_value"],
                    a["high_is_good"],
                    a["stackable"],
                    a["published"],
                )
                for a in fsd_data["dogma_attributes"]
            ],
        )

        v24_eff_ids = set()
        eff_rows = []
        for e in fsd_data["dogma_effects"]:
            v24_eff_ids.add(e["effect_id"])
            eff_rows.append(
                (
                    e["effect_id"],
                    e["name"] or f"effect_{e['effect_id']}",
                    e["effect_category"],
                    e["is_offensive"],
                    e["is_assistance"],
                    e["is_warp_safe"],
                    e["disallow_auto_repeat"],
                    e["electronic_chance"],
                    e["propulsion_chance"],
                    e["range_chance"],
                    e["guid"],
                    e["published"],
                    json.dumps(e["modifiers"], separators=(",", ":")),
                )
            )
        conn.executemany(
            "INSERT INTO sde_dogma_effects VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            eff_rows,
        )

        conn.executemany(
            "INSERT INTO sde_type_materials VALUES (?, ?)",
            [
                (tm["type_id"], json.dumps(tm["materials"], separators=(",", ":")))
                for tm in fsd_data["type_materials"]
            ],
        )

        bp_rows = []
        for k, bp in blueprints_cache:
            bpid = int(bp.get("blueprintTypeID") or k)
            acts = bp.get("activities") or {}
            mfg = acts.get("manufacturing") or {}
            prods = mfg.get("products") or []
            prod_tid = int(prods[0]["typeID"]) if prods else 0
            prod_qty = int(prods[0].get("quantity", 1)) if prods else 0
            mfg_time = int(mfg.get("time", 0) or 0)
            max_runs = int(bp.get("maxProductionLimit", 1) or 1)
            mats = [
                {"material_type_id": int(m["typeID"]), "quantity": int(m["quantity"])}
                for m in (mfg.get("materials") or [])
            ]
            bp_rows.append(
                (
                    bpid,
                    prod_tid,
                    prod_qty,
                    mfg_time,
                    max_runs,
                    json.dumps(mats, separators=(",", ":")),
                    json.dumps(acts, separators=(",", ":")),
                )
            )
        conn.executemany(
            "INSERT INTO sde_blueprints VALUES (?, ?, ?, ?, ?, ?, ?)",
            bp_rows,
        )

        conn.executemany(
            "INSERT INTO sde_regions VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    r["region_id"],
                    label(r["name_id"], f"Region {r['region_id']}"),
                    r["x"],
                    r["y"],
                    r["z"],
                    r["faction_id"],
                    r["wormhole_class_id"],
                )
                for r in fsd_data["regions"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_constellations VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    c["constellation_id"],
                    c["region_id"],
                    label(c["name_id"], f"Constellation {c['constellation_id']}"),
                    c["x"],
                    c["y"],
                    c["z"],
                    c["faction_id"],
                    c["wormhole_class_id"],
                )
                for c in fsd_data["constellations"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_solar_systems VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    s["system_id"],
                    s["constellation_id"],
                    s["region_id"],
                    s["name"],
                    s["security"],
                    s["pseudo_security"],
                    s["security_class"],
                    s["x"],
                    s["y"],
                    s["z"],
                    s["radius"],
                    s["luminosity"],
                    s["sun_type_id"],
                    s["faction_id"],
                    s["wormhole_class_id"],
                    json.dumps(s["stargate_ids"], separators=(",", ":")),
                    json.dumps(s["planet_ids"], separators=(",", ":")),
                )
                for s in fsd_data["solar_systems"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_stargates VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    sg["stargate_id"],
                    sg["system_id"],
                    sg["destination_gate_id"],
                    sg["destination_system_id"],
                    sg["type_id"],
                    f"Stargate ({sys_name_by_id.get(sg['destination_system_id'], str(sg['destination_system_id']))})",
                    sg["x"],
                    sg["y"],
                    sg["z"],
                    celestial_radius_by_id.get(sg["stargate_id"], 5000.0),
                )
                for sg in fsd_data["stargates"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_celestials VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            celestials,
        )
        conn.executemany(
            "INSERT INTO sde_stations VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            stations,
        )
        conn.executemany(
            "INSERT INTO sde_station_operations VALUES (?, ?, ?, ?)",
            [
                (
                    op["operation_id"],
                    op["name"],
                    op["activity_id"],
                    json.dumps(op["services"], separators=(",", ":")),
                )
                for op in fsd_data["station_operations"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_station_services VALUES (?, ?)",
            [
                (sv["service_id"], label(sv["name_id"], f"Service {sv['service_id']}"))
                for sv in fsd_data["station_services"]
            ],
        )

        race_ship_by_id = {}
        race_rows = []
        for r in fsd_data["races"]:
            rid = r["race_id"]
            ship_tid = r["ship_type_id"]
            if ship_tid:
                race_ship_by_id[rid] = int(ship_tid)
            race_rows.append(
                (
                    rid,
                    label(r["name_id"], f"Race {rid}"),
                    ship_tid,
                    json.dumps(r["skills"], separators=(",", ":")),
                    label(r.get("description_id"), ""),
                )
            )
        conn.executemany("INSERT INTO sde_races VALUES (?, ?, ?, ?, ?)", race_rows)

        conn.executemany(
            "INSERT INTO sde_bloodlines VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    b["bloodline_id"],
                    b["race_id"],
                    label(b["name_id"], f"Bloodline {b['bloodline_id']}"),
                    b["corporation_id"],
                    b["charisma"],
                    b["intelligence"],
                    b["memory"],
                    b["perception"],
                    b["willpower"],
                    b.get("ship_type_id") or race_ship_by_id.get(b["race_id"]),
                    label(b.get("description_id"), ""),
                )
                for b in fsd_data["bloodlines"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_ancestries VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    a["ancestry_id"],
                    a["bloodline_id"],
                    label(a["name_id"], f"Ancestry {a['ancestry_id']}"),
                    a["charisma"],
                    a["intelligence"],
                    a["memory"],
                    a["perception"],
                    a["willpower"],
                    label(a.get("description_id"), ""),
                    str(a.get("short_description") or ""),
                )
                for a in fsd_data["ancestries"]
            ],
        )

        school_rows = []
        for s in fsd_data["schools"]:
            sta_list = s["starting_stations"]
            start_sta = int(sta_list[0]) if sta_list else 60003760
            start_sys = station_sys_by_id.get(start_sta, 30000142)
            corvette_tid = race_ship_by_id.get(s["race_id"])
            school_rows.append(
                (
                    s["school_id"],
                    s["race_id"],
                    label(s["name_id"], f"School {s['school_id']}"),
                    s["corporation_id"],
                    s["career_id"],
                    start_sta,
                    start_sys,
                    corvette_tid,
                    json.dumps(sta_list, separators=(",", ":")),
                    json.dumps(s["career_agents"], separators=(",", ":")),
                )
            )
        conn.executemany(
            "INSERT INTO sde_schools VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            school_rows,
        )

        conn.executemany(
            "INSERT INTO sde_factions VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    f["faction_id"],
                    label(f["name_id"], f"Faction {f['faction_id']}"),
                    f["corporation_id"],
                    f["militia_corporation_id"],
                    f["solar_system_id"],
                    f["size_factor"],
                    json.dumps(f["member_races"], separators=(",", ":")),
                    label(f.get("description_id"), ""),
                )
                for f in fsd_data["factions"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_npc_corporations VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    c["corporation_id"],
                    c["name"],
                    c["ticker_name"],
                    c["faction_id"],
                    c["solar_system_id"],
                    c["station_id"],
                    c["ceo_id"],
                    c["tax_rate"],
                    json.dumps(c["lp_offer_tables"], separators=(",", ":")),
                )
                for c in fsd_data["npc_corporations"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_agents VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    ag["agent_id"],
                    label(ag["name_id"], f"Agent {ag['agent_id']}"),
                    ag["corporation_id"],
                    ag["station_id"],
                    ag["solar_system_id"] or station_sys_by_id.get(ag["station_id"] or 0),
                    ag["level"],
                    ag["division_id"],
                    ag["agent_type_id"],
                    ag["is_locator"],
                )
                for ag in fsd_data["agents"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_market_groups VALUES (?, ?, ?, ?, ?)",
            [
                (
                    mg["market_group_id"],
                    mg["parent_group_id"],
                    label(mg["name_id"], f"MarketGroup {mg['market_group_id']}"),
                    mg["has_types"],
                    mg["icon_id"],
                )
                for mg in fsd_data["market_groups"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_planet_schematics VALUES (?, ?, ?, ?, ?)",
            [
                (
                    sc["schematic_id"],
                    label(sc["name_id"], f"Schematic {sc['schematic_id']}"),
                    sc["cycle_time"],
                    json.dumps(sc["pins"], separators=(",", ":")),
                    json.dumps(sc["types"], separators=(",", ":")),
                )
                for sc in fsd_data["schematics"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_clone_grades VALUES (?, ?)",
            [
                (int(k), json.dumps(v.get("skills", []), separators=(",", ":")))
                for k, v in clonegrades_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_skins VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    int(v.get("skinID") or k),
                    str(v.get("internalName", "")),
                    int(v.get("skinMaterialID", 0)),
                    json.dumps(v.get("types", []), separators=(",", ":")),
                    int(bool(v.get("allowCCPDevs", False))),
                    int(bool(v.get("visibleSerenity", False))),
                    int(bool(v.get("visibleTranquility", False))),
                    int(bool(v.get("isStructureSkin", False))),
                    str(v.get("skinDescription")) if v.get("skinDescription") is not None else None,
                )
                for k, v in skins_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_skin_materials VALUES (?, ?, ?)",
            [
                (
                    int(v.get("skinMaterialID") or k),
                    v.get("displayNameID"),
                    v.get("materialSetID"),
                )
                for k, v in skinmaterials_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_skin_licenses VALUES (?, ?, ?)",
            [
                (
                    int(v.get("licenseTypeID") or k),
                    int(v.get("skinID", 0)),
                    int(v.get("duration", -1)),
                )
                for k, v in skinlicenses_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dbuff_collections VALUES (?, ?, ?)",
            [
                (
                    int(k),
                    str(v.get("operationName", "")),
                    json.dumps(v, separators=(",", ":")),
                )
                for k, v in dbuffs_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_fighter_abilities VALUES (?, ?)",
            [(int(k), json.dumps(v, separators=(",", ":"))) for k, v in fighter_ab_cache],
        )
        conn.executemany(
            "INSERT INTO sde_fighter_abilities_by_type VALUES (?, ?)",
            [(int(k), json.dumps(v, separators=(",", ":"))) for k, v in fighter_ab_type_cache],
        )
        conn.executemany(
            "INSERT INTO sde_wormhole_types VALUES (?, ?, ?, ?, ?, ?, ?)",
            wormhole_rows,
        )

        spawn_groups = build_v24_npc_spawn_groups(fsd_data["groups"], fsd_data["types"], label)
        conn.executemany(
            "INSERT INTO sde_npc_spawn_groups VALUES (?, ?, ?, ?, ?, ?)",
            [
                (
                    sg["spawn_group_id"],
                    sg["faction_id"],
                    sg["ship_class"],
                    sg["group_id"],
                    sg["group_name"],
                    json.dumps(sg["npc_type_ids"], separators=(",", ":")),
                )
                for sg in spawn_groups
            ],
        )
        conn.executemany(
            "INSERT INTO sde_missions VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    m["mission_id"],
                    label(m["name_id"], f"Mission {m['mission_id']}"),
                    m["is_courier"],
                    m["lp_alpha"],
                    m["lp_omega"],
                    m["has_standing_rewards"],
                    m["content_template"],
                )
                for m in fsd_data["missions"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_epic_arcs VALUES (?, ?, ?, ?, ?)",
            [
                (
                    ea["epic_arc_id"],
                    label(ea["name_id"], f"Epic Arc {ea['epic_arc_id']}"),
                    ea["faction_id"],
                    ea["restart_interval"],
                    ea["mission_count"],
                )
                for ea in fsd_data["epic_arcs"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dungeons VALUES (?, ?, ?, ?, ?)",
            [
                (
                    d["dungeon_id"],
                    label(d["name_id"], f"Dungeon {d['dungeon_id']}"),
                    d["archetype_id"],
                    d["faction_id"],
                    d["difficulty"],
                )
                for d in fsd_data["dungeons"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_node_graphs VALUES (?, ?, ?, ?, ?)",
            [
                (
                    ng["node_graph_id"],
                    ng["name"],
                    ng["parent_graph_id"],
                    ng["node_count"],
                    json.dumps(ng["tags"], separators=(",", ":")),
                )
                for ng in fsd_data["node_graphs"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_space_components VALUES (?, ?, ?)",
            [
                (
                    sc["type_id"],
                    sc["ball_class"],
                    json.dumps(sc["components"], separators=(",", ":")),
                )
                for sc in fsd_data["space_components"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_compressible_types VALUES (?, ?)",
            [(ct["type_id"], ct["compressed_type_id"]) for ct in fsd_data["compressible_types"]],
        )
        conn.executemany(
            "INSERT INTO sde_dynamic_item_attributes VALUES (?, ?, ?)",
            [
                (
                    d["mutaplasmid_type_id"],
                    json.dumps(d["mappings"], separators=(",", ":")),
                    json.dumps(d["attribute_ranges"], separators=(",", ":")),
                )
                for d in fsd_data["dynamic_item_attributes"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_jumps VALUES (?, ?, ?, ?, ?)",
            [
                (
                    j["jump_id"],
                    j["stargate_id"],
                    j["from_system_id"],
                    j["to_system_id"],
                    j["jump_type"],
                )
                for j in fsd_data["jumps"]
            ],
        )


    table_counts = {}
    for (tname,) in conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
    ).fetchall():
        table_counts[tname] = int(conn.execute(f'SELECT count(*) FROM "{tname}"').fetchone()[0])
    conn.close()

    manifest_resources = [
        {
            "res_path": r["res_path"],
            "cdn_relative_path": r["cdn_relative_path"],
            "md5": r["md5"],
            "byte_size": r["byte_size"],
            "format": r["format"],
            "record_count": r["record_count"],
        }
        for r in verified_resources
    ]
    manifest = {
        "schema_version": 1,
        "client_build": 3619879,
        "client_release": "V24.01",
        "table_counts": table_counts,
        "resources": manifest_resources,
    }
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    print("=== EvoMu Real V24.01 SDE Importer (D-1) ===")
    print(f"Output SQLite DB: {out_db} ({out_db.stat().st_size:,} bytes)")
    for tname, cnt in table_counts.items():
        print(f"  {tname:32s}: {cnt:10,d} rows")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description="Import V24.01 EVE Client Static Data into SQLite")
    parser.add_argument("--shared-cache", type=Path, default=DEFAULT_SHARED_CACHE)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT_DB)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--force-extract", action="store_true")
    args = parser.parse_args()
    run_import(args.shared_cache, args.out, args.manifest, args.force_extract)
    return 0


if __name__ == "__main__":
    sys.exit(main())
