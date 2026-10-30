#!/usr/bin/env python3
"""
sde_extractors.py — FSD binary, static schema, localization, and mapObjects.db extraction helpers
for the V24.01 EVE Client Static Data Importer (`tools/sde-import/import_sde.py`).
"""

from __future__ import annotations

import hashlib
import json
import pickle
import sqlite3
import subprocess
import zipfile
import zlib
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent

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
    """Compile cfsd_runner.c against EVE.app libpython2.7.dylib (or create ctypes python27.dll runner on Windows) and build code_pyc.zip."""
    import sys as _sys

    work_dir.mkdir(parents=True, exist_ok=True)
    bin64_dir = eve_build_dir / "bin64"
    if _sys.platform == "win32":
        runner_bin = work_dir / "cfsd_runner_win.py"
        runner_bin.write_text(
            "import ctypes, os, sys\n"
            "script, code_zip, bin64, res_dir, rfi, cat, out_json = sys.argv[1:8]\n"
            "os.add_dll_directory(bin64)\n"
            "os.environ['PATH'] = bin64 + os.pathsep + os.environ.get('PATH', '')\n"
            "py2 = ctypes.CDLL(os.path.join(bin64, 'python27.dll'))\n"
            "ctypes.c_int.in_dll(py2, 'Py_NoSiteFlag').value = 1\n"
            "ctypes.c_int.in_dll(py2, 'Py_DontWriteBytecodeFlag').value = 1\n"
            "py2.Py_Initialize()\n"
            "boot = (\n"
            "    'import sys\\n'\n"
            "    'sys.argv = %r\\n'\n"
            "    'execfile(sys.argv[0])\\n'\n"
            ") % ([script, code_zip, bin64, res_dir, rfi, cat, out_json],)\n"
            "rc = py2.PyRun_SimpleString(boot.encode('utf-8'))\n"
            "py2.Py_Finalize()\n"
            "sys.exit(rc)\n",
            encoding="utf-8",
        )
    else:
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
for p in glob.glob(os.path.join(bin64_dir, '*Loader.pyd')):
    stem = os.path.basename(p)[:-10].lower()
    loaders[stem] = os.path.basename(p)[:-4]

rfi = {}
for line in open(rfi_path, 'rb'):
    parts = line.strip().split(',')
    if len(parts) >= 2:
        rfi[parts[0].lower()] = parts[1]

cat = json.load(open(catalog_path, 'rb'))
cat_map = {x['res_path']: rfi.get(x['res_path'].lower(), x['cdn_relative_path']) for x in cat}

def load_fsd(name):
    rp = 'res:/staticdata/' + name + '.fsdbinary'
    mod = __import__(loaders[name.lower()])
    return mod.load(os.path.join(res_dir, cat_map[rp]))

def load_static_with_schema(name):
    rp = 'res:/staticdata/' + name + '.static'
    sp = 'res:/staticdata/' + name + '.schema'
    sch = persistence.LoadSchema(open(os.path.join(res_dir, rfi[sp.lower()]), 'rb').read())
    return binaryLoader.LoadFromString(open(os.path.join(res_dir, cat_map[rp]), 'rb').read(), sch)

# 1. Verify and count all 213 staticdata_catalog.json resources. Dispatch on the resource extension
# and the file's own magic bytes, not on the catalog's extension-derived `format` label (which calls
# every `.static` "SQLite3" although most are FSD data behind a pickled schema header).
resource_counts = {}
for item in cat:
    rp = item['res_path']
    p = os.path.join(res_dir, cat_map[rp])
    if rp.endswith('.fsdbinary'):
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
    elif rp.endswith('.static'):
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
    else:
        # `.schema` (YAML text) and `.pickle` resources
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
    rand_mats = []
    if getattr(tm, 'randomizedMaterials', None):
        for rm in tm.randomizedMaterials:
            rand_mats.append({
                'material_type_id': int(rm.materialTypeID),
                'quantity_min': int(rm.quantityMin),
                'quantity_max': int(rm.quantityMax),
            })
    type_materials.append({'type_id': int(tid), 'materials': mats, 'randomized_materials': rand_mats})

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
stars_raw = []

for sid, s in systems_raw.iteritems():
    isid = int(sid)
    ssc = ssc_raw[isid] if isid in ssc_raw else None
    sg_ids = []
    pl_ids = [int(p) for p in s.planetItemIDs] if s.planetItemIDs else []
    st_obj = getattr(ssc, 'star', None) if ssc is not None else None
    if st_obj is not None:
        st_stats = getattr(st_obj, 'statistics', None)
        stars_raw.append({
            'star_id': int(st_obj.id),
            'system_id': isid,
            'type_id': int(st_obj.typeID),
            'radius': int(round(float(getattr(st_obj, 'radius', 0.0) or 0.0))),
            'age': int(round(float(getattr(st_stats, 'age', 0.0) or 0.0))) if st_stats is not None else 0,
            'luminosity': float(getattr(st_stats, 'luminosity', 0.0) or 0.0) if st_stats is not None else float(getattr(ssc, 'luminosity', 0.0) or 0.0),
            'temperature': int(round(float(getattr(st_stats, 'temperature', 0.0) or 0.0))) if st_stats is not None else 0,
            'spectral_class': str(getattr(st_stats, 'spectralClass', '') or '') if st_stats is not None else '',
        })
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

# Client type lists (`res:/staticdata/typelist.fsdbinary`): inclusion/exclusion rules referenced by
# dogma attributes such as `3255` (compressibleItemsTypeList) on shipboard compressors.
tl_raw = load_fsd('typelist')
type_lists = []
for tlid, tl in tl_raw.iteritems():
    def _ids(attr):
        v = getattr(tl, attr, None)
        return sorted(int(x) for x in v) if v is not None else []
    type_lists.append({
        'type_list_id': int(tlid),
        'name': str(getattr(tl, 'name', '') or ''),
        'included_category_ids': _ids('includedCategoryIDs'),
        'included_group_ids': _ids('includedGroupIDs'),
        'included_type_ids': _ids('includedTypeIDs'),
        'excluded_category_ids': _ids('excludedCategoryIDs'),
        'excluded_group_ids': _ids('excludedGroupIDs'),
        'excluded_type_ids': _ids('excludedTypeIDs'),
    })

# Per-dungeon asteroid layout (`res:/staticdata/asteroidcountbytypeidroomiddungeonid.static`):
# dungeonID -> roomID -> asteroid typeID -> number of asteroids of that type in the room.
ac_raw = binaryLoader.LoadFromString(open(os.path.join(res_dir, cat_map['res:/staticdata/asteroidcountbytypeidroomiddungeonid.static']), 'rb').read())
dungeon_asteroid_counts = []
for did, rooms in ac_raw.iteritems():
    for rid, tcounts in rooms.iteritems():
        for tid, cnt in tcounts.iteritems():
            dungeon_asteroid_counts.append({
                'dungeon_id': int(did),
                'room_id': int(rid),
                'type_id': int(tid),
                'asteroid_count': int(cnt),
            })

# Gas cloud types per gas-site dungeon (`res:/staticdata/gastypesbydungeonid.static`).
gt_raw = binaryLoader.LoadFromString(open(os.path.join(res_dir, cat_map['res:/staticdata/gastypesbydungeonid.static']), 'rb').read())
dungeon_gas_types = []
for did, tids in gt_raw.iteritems():
    for tid in tids:
        dungeon_gas_types.append({'dungeon_id': int(did), 'type_id': int(tid)})

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
    'stars': stars_raw,
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
    'type_lists': type_lists,
    'dungeon_asteroid_counts': dungeon_asteroid_counts,
    'dungeon_gas_types': dungeon_gas_types,
    'dynamic_item_attributes': dynamic_item_attributes,
}
json.dump(out, open(out_json_path, 'wb'))
"""


def build_v24_npc_spawn_groups(
    fsd_groups: list[dict],
    fsd_types: list[dict],
    label_fn: Callable[[int | None, str], str],
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


# Resource container formats, classified from each client file's own magic bytes (T-2). The
# extension-based labels in `docs/index/staticdata_catalog.json` call every `.static` "SQLite3" and
# every `.schema`/`.pickle` "Pickle/Binary", which is wrong for most files.
FORMAT_SQLITE = "SQLite3"
FORMAT_FSD_BINARY = "FSD Binary"
FORMAT_FSD_STATIC_PICKLED_SCHEMA = "FSD Static (pickled schema header)"
FORMAT_FSD_STATIC_EXTERNAL_SCHEMA = "FSD Static (external .schema)"
FORMAT_FSD_SCHEMA_TEXT = "FSD Schema (YAML text)"
FORMAT_PICKLE_PROTOCOL_0 = "Pickle (protocol 0)"
FORMAT_PICKLE_PROTOCOL_2 = "Pickle (protocol 2)"


def detect_resource_format(raw: bytes, has_sibling_schema: bool) -> str:
    """Classify a client resource from its magic bytes; raises on an unrecognised container.

    - `SQLite format 3\\0` header: SQLite3 database.
    - `cfsd` FSD binary: 24-byte header hash, then a u64 LE payload length equal to `len - 32`.
    - FSD static with pickled schema: u32 LE length `n`, then a protocol-0 pickle of the schema
      dict (`(dp`) occupying exactly bytes `4..4+n` and ending with the pickle STOP opcode `.`.
    - Pickle protocol 2 starts with the PROTO opcode `\\x80\\x02`; protocol 0 starts with MARK `(`
      and ends with STOP `.`.
    - FSD schema: printable YAML text (`keyFooter:` / `fixedItemSize:` ...).
    - Anything else is headerless FSD static data whose schema lives in the sibling `.schema`.
    """
    if raw.startswith(b"SQLite format 3\x00"):
        return FORMAT_SQLITE
    if len(raw) >= 40 and int.from_bytes(raw[24:32], "little") == len(raw) - 32:
        return FORMAT_FSD_BINARY
    if len(raw) >= 8:
        n = int.from_bytes(raw[0:4], "little")
        if 4 < n < len(raw) - 4 and raw[4:7] == b"(dp" and raw[3 + n : 4 + n] == b".":
            return FORMAT_FSD_STATIC_PICKLED_SCHEMA
    if raw.startswith(b"\x80\x02"):
        return FORMAT_PICKLE_PROTOCOL_2
    if raw.startswith(b"(") and raw.rstrip().endswith(b"."):
        return FORMAT_PICKLE_PROTOCOL_0
    if raw and all(32 <= c < 127 or c in (9, 10, 13) for c in raw):
        return FORMAT_FSD_SCHEMA_TEXT
    if has_sibling_schema:
        return FORMAT_FSD_STATIC_EXTERNAL_SCHEMA
    raise ValueError(f"Unrecognised client resource container (first bytes {raw[:16].hex()})")


def verify_sde_resources(
    catalog: list[dict],
    res_dir: Path,
    resource_counts: dict,
) -> list[dict]:
    verified_resources = []
    res_paths = {item["res_path"] for item in catalog}
    for item in catalog:
        rp = item["res_path"]
        p = res_dir / item["cdn_relative_path"]
        raw = p.read_bytes()
        actual_md5 = hashlib.md5(raw).hexdigest()
        if actual_md5 != item["md5"] and not item.get("_rfi_override"):
            raise ValueError(f"MD5 mismatch for {rp}: expected {item['md5']}, got {actual_md5}")
        has_sibling_schema = rp.endswith(".static") and (rp[: -len(".static")] + ".schema") in res_paths
        fmt = detect_resource_format(raw, has_sibling_schema)
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
                "format": fmt,
                "record_count": int(cnt),
                "spot": str(spot),
            }
        )
    return verified_resources


def extract_celestials_and_stations(
    map_objects_db: Path,
    sys_name_by_id: dict[int, str],
    sys_const_by_id: dict[int, int],
    sys_reg_by_id: dict[int, int],
    corp_name_by_id: dict[int, str],
    op_name_by_id: dict[int, str],
    label_fn: Callable[[int | None, str], str],
) -> tuple[dict[int, float], list[tuple], list[tuple], dict[int, int]]:
    map_conn = sqlite3.connect(f"file:{map_objects_db}?mode=ro", uri=True)
    celestial_rows = map_conn.execute(
        "SELECT celestialID, celestialNameID, solarSystemID, typeID, groupID, radius, x, y, z, orbitID, orbitIndex, celestialIndex FROM celestials"
    ).fetchall()
    station_rows = map_conn.execute(
        "SELECT stationID, orbitID, solarSystemID, ownerID, typeID, x, y, z, operationID, reprocessingEfficiency, reprocessingStationsTake, isConquerable, useOperationName, orbitIndex, celestialIndex FROM npcStations"
    ).fetchall()
    map_conn.close()

    celestial_radius_by_id: dict[int, float] = {}
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
            cname = label_fn(int(cname_id), f"{sname} {roman}".strip())
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
    station_sys_by_id: dict[int, int] = {}
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

    return celestial_radius_by_id, celestials, stations, station_sys_by_id


def build_types_wormholes_and_blueprints(
    fsd_types: list[dict],
    blueprints_cache: list[tuple[str, dict]],
    label_fn: Callable[[int | None, str], str],
) -> tuple[list[tuple], list[tuple], list[tuple]]:
    wormhole_rows = []
    type_rows = []
    for t in fsd_types:
        tid = t["type_id"]
        gid = t["group_id"]
        cid = t["category_id"]
        tname = label_fn(t["name_id"], f"Type {tid}")
        attrs_dict = {int(k): float(v) for k, v in t["attrs"]}
        eff_ids = [int(k) for k, _ in t["effs"]]
        default_eff_id = next((int(k) for k, is_def in t["effs"] if is_def), None)
        tdesc = label_fn(t.get("description_id"), "")
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

    return type_rows, wormhole_rows, bp_rows
