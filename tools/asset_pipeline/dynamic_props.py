"""Export authored district DMO locators and their referenced model geometry.

This preserves initial placement, not DMO simulation. Native 0x825876D0 reads
the locator matrix and template ID at +112; RX2 EB001D records are 128 bytes.
No name matching, random placement, ground snapping or collision synthesis.
"""
import argparse
import copy
import hashlib
import json
import struct
import sys
import tempfile
from pathlib import Path

import numpy as np
from .map_writer import write
from tools.owned_game.big import BigArchive


def sections(raw):
    if raw[:7] != b'\x89RW4xb2':
        raise ValueError('Expected Xbox RX2')
    count = struct.unpack_from('>I', raw, 32)[0]
    table = struct.unpack_from('>I', raw, 48)[0]
    if table + count*24 > len(raw):
        raise ValueError('Truncated RX2 section table')
    return [struct.unpack_from('>6I', raw, table+i*24) for i in range(count)]


def matrix(raw, at):
    value = np.array(struct.unpack_from('>16f', raw, at)).reshape(4, 4)
    if not np.isfinite(value).all() or not np.allclose(value[:, 3], [0, 0, 0, 1], atol=1e-5):
        raise ValueError('Invalid DMO affine matrix')
    if abs(np.linalg.det(value[:3, :3])) < 1e-8:
        raise ValueError('Singular DMO matrix')
    return value


def records(raw, kind, stride):
    for offset, _, size, _, _, type_id in sections(raw):
        if type_id != kind:
            continue
        if offset+size > len(raw) or size < 32:
            raise ValueError('Invalid DMO section extent')
        count = struct.unpack_from('>I', raw, offset+4)[0]
        start, strings = struct.unpack_from('>2I', raw, offset+12)
        if start != 32 or strings != start+count*stride or strings > size:
            raise ValueError('Unexpected DMO record layout')
        for index in range(count):
            yield offset, offset+start+index*stride, size


def locators(raw):
    result = []
    for base, at, size in records(raw, 0xEB001D, 128):
        name_offset = struct.unpack_from('>I', raw, at+124)[0]
        if not 32 <= name_offset < size:
            raise ValueError('Invalid DMO locator name offset')
        end = raw.index(b'\0', base+name_offset, base+size)
        result.append(dict(instance_id=f'{struct.unpack_from(">Q", raw, at+96)[0]:016X}',
            locator_id=f'{struct.unpack_from(">Q", raw, at+104)[0]:016X}',
            template_id=f'{struct.unpack_from(">Q", raw, at+112)[0]:016X}',
            matrix=matrix(raw, at).tolist(), name=raw[base+name_offset:end].decode('utf-8'),
            source_offset=at, bounds=np.array(struct.unpack_from('>8f', raw, at+64)).reshape(2, 4)[:, :3].tolist()))
    return result


def template_meshes(raw):
    """Resolve EB000D tInstance -> EB0001 model -> EB0023 mesh -> declaration."""
    table = sections(raw)
    result = {}
    for _, at, _ in records(raw, 0xEB000D, 160):
        key = f'{struct.unpack_from(">Q", raw, at+104)[0]:016X}'
        index = struct.unpack_from('>I', raw, at+128)[0]
        if index >= len(table) or table[index][5] != 0xEB0001:
            raise ValueError('Unresolved DMO model reference')
        base, _, size, _, _, _ = table[index]
        start = struct.unpack_from('>I', raw, base+36)[0]
        count = struct.unpack_from('>H', raw, base+48)[0]
        if base+size > len(raw) or start+count*8 > size:
            raise ValueError('Invalid DMO model mesh table')
        mesh_info = []
        for j in range(count):
            mesh_index = struct.unpack_from('>I', raw, base+start+j*8)[0]
            if mesh_index >= len(table) or table[mesh_index][5] != 0xEB0023:
                raise ValueError('Unresolved DMO mesh reference')
            descriptor = struct.unpack_from('>I', raw, table[mesh_index][0]+40)[0]
            if descriptor >= len(table) or table[descriptor][5] != 0x200E9:
                raise ValueError('Unresolved DMO vertex declaration')
            mesh_info.append(table[descriptor][0])
        if key in result:
            raise ValueError('Duplicate DMO template ID')
        result[key] = dict(mesh_info=mesh_info, matrix=matrix(raw, at),
                          model_matrix=matrix(raw, base+struct.unpack_from('>I', raw, base+32)[0]))
    return result


def transform_mesh(arrays, index, transform):
    # NpzFile.items() reads every array before the filter runs.
    values = {key: np.array(arrays[key], copy=True) for key in arrays if key.endswith('_'+str(index))}
    key = f'vertices_{index}'
    values[key] = (values[key] @ transform[:3, :3] + transform[3, :3]).astype('f4')
    normal_matrix = np.linalg.inv(transform[:3, :3]).T
    for prefix in ('normals', 'retail_normals'):
        key = f'{prefix}_{index}'
        if key in values:
            n = values[key] @ normal_matrix
            values[key] = (n/np.maximum(np.linalg.norm(n, axis=1, keepdims=True), 1e-20)).astype('f4')
    if np.linalg.det(transform[:3, :3]) < 0:
        values[f'faces_{index}'] = values[f'faces_{index}'][:, [0, 2, 1]]
    return values


def catalog(cache_roots):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'vendor/utt'))
    import rx2_parser
    from .backdrop import texture_groups
    templates, textures = {}, {}
    for root in cache_roots:
        m = json.loads((root/'manifest.json').read_text())
        for key, value in m['textures'].items():
            entry = dict(value)
            for path_key in ('rgba', 'png'):
                if path_key in entry:
                    entry[path_key] = str((root/entry[path_key]).resolve())
            if key in textures and Path(textures[key]['rgba']).read_bytes() != Path(entry['rgba']).read_bytes():
                raise ValueError('Conflicting DMO texture '+key)
            textures[key] = entry
        for model in m['models']:
            raw = (root/model['rx2']).read_bytes()
            table = rx2_parser.RX2File(raw)
            table.parse()
            roles = set(role for mesh in model['meshes'] for role in mesh['retail_texture_ids'])
            bindings = texture_groups(raw, table, roles)
            for key, definition in template_meshes(raw).items():
                meshes = [copy.deepcopy(mesh) for mesh in model['meshes'] if mesh['source_offsets']['mesh_info'] in definition['mesh_info']]
                for mesh in meshes:
                    # DMO resources have a high-bit namespace that display names
                    # omit. Bind the binary channel GUID; never strip that bit.
                    group = bindings[mesh['retail_material_group_index']]
                    mesh['retail_texture_ids'] = {role:f'0x{group[role]:016x}' for role in mesh['retail_texture_ids']}
                    mesh['texture_id'] = mesh['retail_texture_ids'].get('diffuse', mesh['retail_texture_ids'].get('transparent'))
                if len(meshes) != len(definition['mesh_info']):
                    raise ValueError('DMO model geometry omitted '+key)
                if key in templates:
                    raise ValueError('Conflicting DMO template '+key)
                templates[key] = dict(definition, meshes=meshes, npz=root/model['npz'], asset_id=model['asset_id'])
    return templates, textures


def save_catalog(cache_roots, output):
    """Build once per installation; JSON keeps this local cache non-executable."""
    templates, textures = catalog(cache_roots)
    serialised = {key: dict(value, matrix=value['matrix'].tolist(),
        model_matrix=value['model_matrix'].tolist(), npz=str(value['npz'].resolve()))
        for key, value in templates.items()}
    temporary = output.with_suffix('.new')
    temporary.write_text(json.dumps(dict(version=1, templates=serialised, textures=textures)), encoding='utf-8')
    temporary.replace(output)


def load_catalog(path):
    data = json.loads(path.read_text(encoding='utf-8'))
    if data['version'] != 1:
        raise ValueError('Unsupported DMO catalog version')
    for value in data['templates'].values():
        value['matrix'] = np.array(value['matrix'], dtype=np.float64)
        value['model_matrix'] = np.array(value['model_matrix'], dtype=np.float64)
        value['npz'] = Path(value['npz'])
    return data['templates'], data['textures']


# Object Dropper catalog classification. First matching token wins; more
# specific tokens precede broader ones. This is a host presentation mapping of
# authored DMO model names, not a recovered park-item category assignment.
DROPPER_RULES = (
    ('quarterpipe', 'Ramp', 'Quarter Pipe'),
    ('guardrailgrind', 'Rail', 'Grind Rail'),
    ('railfancymed', 'Rail', 'Fancy Rail'),
    ('raillong_flatlow', 'Rail', 'Long Rail (Low)'),
    ('raillong_flatmed', 'Rail', 'Long Rail (Med)'),
    ('safetybarrierlong', 'Barrier', 'Safety Barrier'),
    ('barrierplastic', 'Barrier', 'Plastic Barrier'),
    ('barriertraffic', 'Barrier', 'Traffic Barrier'),
    ('constructionsignstand', 'Barrier', 'Construction Sign'),
    ('picnictable', 'Bench', 'Picnic Table'),
    ('pinnictable', 'Bench', 'Picnic Table'),
    ('patiotable', 'Bench', 'Patio Table'),
    ('patiochair', 'Bench', 'Patio Chair'),
    ('bench', 'Bench', 'Bench'),
    ('couch', 'Bench', 'Couch'),
    ('garbagecan', 'Bin', 'Garbage Can'),
    ('recyclingbin', 'Bin', 'Recycling Bin'),
    ('recyclebin', 'Bin', 'Recycle Bin'),
    ('tintrashbody', 'Bin', 'Trash Can'),
    ('garbagebag', 'Bin', 'Garbage Bag'),
    ('dumpsteropen', 'Bin', 'Dumpster (Open)'),
    ('dumpsterclosed', 'Bin', 'Dumpster (Closed)'),
    ('newspaperbox', 'Misc', 'Newspaper Box'),
    ('newspaper', 'Misc', 'Newspaper Dispenser'),
    ('mailbox', 'Misc', 'Mail Box'),
    ('vendingmachine', 'Misc', 'Vending Machine'),
    ('bikerack', 'Misc', 'Bike Rack'),
    ('woodenpalette', 'Misc', 'Wooden Palette'),
    ('beachball', 'Misc', 'Beach Ball'),
    ('basketballstantion', 'Misc', 'Basketball Hoop'),
    ('basketball', 'Misc', 'Basketball'),
    ('soccernet', 'Misc', 'Soccer Net'),
    ('hurdleshort', 'Misc', 'Hurdle (Short)'),
    ('hurdletall', 'Misc', 'Hurdle (Tall)'),
)
# Ground features and fragments that are not standalone placeable props.
DROPPER_EXCLUDE = ('crackedground', 'tintrashlid')
DROPPER_CATEGORY_ORDER = ('Ramp', 'Rail', 'Barrier', 'Bench', 'Bin', 'Misc')


def classify_prop(name):
    low = name.lower()
    if any(token in low for token in DROPPER_EXCLUDE):
        return None
    for token, category, label in DROPPER_RULES:
        if token in low:
            return category, label
    return None


def _used_templates(manifest_path, templates):
    """template_id -> first authored locator name that references it."""
    district = json.loads(manifest_path.read_text())
    used = {}
    for source in district['simulation_assets']:
        raw = (manifest_path.parent / source['rx2']).read_bytes()
        for item in locators(raw):
            key = item['template_id']
            if key in templates and key not in used:
                used[key] = item['name']
    return district, used


def export_templates(manifest_path, cache_roots, output_dir, *, catalog_path=None, report=print):
    """Emit one model-local, base-centred render-only .skate per droppable prop.

    The runtime reuses the normal map parser and material binding. Geometry is
    baked from the same authored DMO catalog as the native-props supplement; no
    new models, collision or behaviour are invented here.
    """
    templates, textures = catalog(cache_roots) if catalog_path is None else load_catalog(catalog_path)
    district, referenced = _used_templates(manifest_path, templates)
    selected = []
    for template_id, name in referenced.items():
        classified = classify_prop(name)
        if classified is not None:
            selected.append((classified[0], classified[1], template_id))
    selected.sort(key=lambda row: (DROPPER_CATEGORY_ORDER.index(row[0]), row[1], row[2]))
    # Disambiguate repeated display labels deterministically.
    counts = {}
    entries = []
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='skate-dropper-') as work:
        root = Path(work)
        for category, label, template_id in selected:
            counts[label] = counts.get(label, 0) + 1
            if counts[label] > 1:
                label = f'{label} ({counts[label]})'
            template = templates[template_id]
            transform = template['model_matrix'] @ template['matrix']
            arrays = {}
            meshes = copy.deepcopy(template['meshes'])
            used_textures = set()
            with np.load(template['npz'], allow_pickle=False) as original:
                for mesh in meshes:
                    arrays.update(transform_mesh(original, mesh['index'], transform))
                    used_textures.update(mesh.get('retail_texture_ids', {}).values())
                    if mesh.get('texture_id'):
                        used_textures.add(mesh['texture_id'])
            vertex_keys = [key for key in arrays if key.startswith('vertices_')]
            if not vertex_keys:
                raise ValueError('DMO template has no geometry: ' + template_id)
            combined = np.concatenate([arrays[key] for key in vertex_keys], axis=0)
            low, high = combined.min(axis=0), combined.max(axis=0)
            offset = np.array([-(low[0] + high[0]) / 2, -low[1], -(low[2] + high[2]) / 2], dtype='f4')
            for key in vertex_keys:
                arrays[key] = (arrays[key] + offset).astype('f4')
            model_npz = root / (template_id + '.npz')
            np.savez(model_npz, **arrays)
            missing = used_textures - set(textures)
            if missing:
                raise ValueError('Missing DMO textures ' + ', '.join(sorted(missing)))
            manifest = dict(map_name=district['map_name'], district_name=district['district_name'],
                            models=[dict(asset_id=template_id, meshes=meshes, npz=str(model_npz))],
                            textures={key: textures[key] for key in sorted(used_textures)},
                            normal_texture_policy=dict(excluded_texture_ids=[]), grind_splines=[])
            (root / 'manifest.json').write_text(json.dumps(manifest))
            name = template_id + '.skate'
            temporary = output_dir / (name + '.new')
            try:
                write(root / 'manifest.json', temporary, None, render_only=True)
                temporary.replace(output_dir / name)
            finally:
                temporary.unlink(missing_ok=True)
            bounds = [(low + offset).tolist(), (high + offset).tolist()]
            entries.append(dict(id=template_id, name=label, category=category, file=name,
                                bounds=bounds, meshes=len(meshes)))
            report(f'Object dropper template: {category} / {label} ({template_id})')
    catalog_file = output_dir / 'catalog.json'
    temporary = catalog_file.with_suffix('.new')
    temporary.write_text(json.dumps(dict(version=1, map=district['map_name'], templates=entries), indent=2) + '\n')
    temporary.replace(catalog_file)
    return len(entries)


def export(manifest_path, cache_roots, output, *, catalog_path=None):
    district = json.loads(manifest_path.read_text())
    templates, textures = catalog(cache_roots) if catalog_path is None else load_catalog(catalog_path)
    placements = {}
    for source in district['simulation_assets']:
        raw = (manifest_path.parent/source['rx2']).read_bytes()
        for item in locators(raw):
            item['source_asset'] = source['asset_id']
            item['source_sha256'] = hashlib.sha256(raw).hexdigest()
            key = item['instance_id']
            if key in placements and placements[key] != item:
                raise ValueError('Conflicting DMO locator '+key)
            placements[key] = item
    report = dict(map=district['map_name'], instances=[], unresolved=[], simulation='initial placement only')
    with tempfile.TemporaryDirectory(prefix='skate-dmo-') as work:
        root = Path(work); models = []; used = set()
        for number, item in enumerate(placements.values()):
            template = templates.get(item['template_id'])
            if template is None:
                report['unresolved'].append(item)
                continue
            transform = template['model_matrix'] @ template['matrix'] @ np.array(item['matrix'])
            arrays = {}; meshes = copy.deepcopy(template['meshes'])
            with np.load(template['npz'], allow_pickle=False) as original:
                for mesh in meshes:
                    arrays.update(transform_mesh(original, mesh['index'], transform))
                    used.update(mesh['retail_texture_ids'].values())
            path = f'{number}.npz'; np.savez(root/path, **arrays)
            models.append(dict(asset_id=item['instance_id'], meshes=meshes, npz=path))
            report['instances'].append(dict(item, model_asset=template['asset_id'], meshes=len(meshes)))
        if models:
            manifest = dict(map_name=district['map_name'], district_name=district['district_name'],
                models=models, textures={key:textures[key] for key in sorted(used)},
                normal_texture_policy=dict(excluded_texture_ids=[]), grind_splines=[])
            (root/'manifest.json').write_text(json.dumps(manifest))
            # Publish only a complete package; preserve the previous one on failure.
            temporary = output.with_suffix('.skate.new')
            try:
                write(root/'manifest.json', temporary, None, render_only=True)
                temporary.replace(output)
            finally:
                temporary.unlink(missing_ok=True)
        else:
            output.unlink(missing_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.with_suffix('.json').write_text(json.dumps(report, indent=2))
    return len(report['instances']), len(report['unresolved'])


def prepare_catalog(game_root, work):
    vendor = Path(__file__).resolve().parents[1]/'vendor'
    sys.path.insert(0, str(vendor/'university/tools/vanilla_map_extraction/tools'))
    from prepare_hawaiian_dream import prepare
    archive = BigArchive(game_root/'data/content/worlddmo.big')
    archive.extract_entries(archive.entries, work/'raw')
    roots = []
    for stream in (work/'raw/data/content/world/dmo').iterdir():
        root = work/'cache'/stream.name
        prepare(stream_directory=stream, output_root=root, utt_root=vendor/'utt',
                district_name=stream.name, map_name=stream.name, raw_texture_cache=True)
        roots.append(root)
    save_catalog(roots, work/'catalog.json')
    return roots


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--manifest', type=Path, required=True)
    p.add_argument('--cache', type=Path, nargs='+', required=True)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    print('DMO placed/unresolved:', export(a.manifest, a.cache, a.output))
