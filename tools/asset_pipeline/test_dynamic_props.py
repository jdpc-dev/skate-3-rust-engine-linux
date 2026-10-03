import json
import struct
import unittest
from unittest.mock import patch
from pathlib import Path
from tempfile import TemporaryDirectory
import numpy as np

from .dynamic_props import (locators, transform_mesh, template_meshes, save_catalog,
                            load_catalog, classify_prop, export_templates)


def resource(kind, payload):
    raw = bytearray(88+len(payload))
    raw[:7] = b'\x89RW4xb2'
    struct.pack_into('>I', raw, 32, 1)
    struct.pack_into('>I', raw, 48, 64)
    struct.pack_into('>6I', raw, 64, 88, 0, len(payload), 0, 0, kind)
    raw[88:] = payload
    return raw


class DynamicPropsTests(unittest.TestCase):
    def test_transform_does_not_read_other_meshes(self):
        class LazyArrays(dict):
            def __getitem__(self, key):
                if key.endswith('_1'):
                    raise AssertionError('Unrelated mesh was decompressed')
                return super().__getitem__(key)
        arrays = LazyArrays(vertices_0=np.zeros((3, 3)), faces_0=np.array([[0, 1, 2]]), vertices_1=None)
        result = transform_mesh(arrays, 0, np.eye(4))
        np.testing.assert_array_equal(result['faces_0'], [[0, 1, 2]])

    def test_catalog_roundtrip_preserves_double_precision_and_bindings(self):
        template = dict(matrix=np.arange(16, dtype=float).reshape(4, 4)/7,
                        model_matrix=np.eye(4), npz=Path('model.npz'),
                        meshes=[dict(retail_texture_ids={'diffuse': '0xf123456789abcdef'})],
                        asset_id='original', mesh_info=[112])
        textures={'0xf123456789abcdef': dict(width=4, height=4, rgba='original.rgba')}
        with TemporaryDirectory() as work:
            path=Path(work)/'catalog.json'
            with patch('tools.asset_pipeline.dynamic_props.catalog', return_value=({'template':template}, textures)):
                save_catalog([], path)
            templates, actual_textures=load_catalog(path)
        actual=templates['template']
        for key in ('matrix','model_matrix'):
            self.assertEqual(actual[key].tobytes(),template[key].tobytes())
        self.assertEqual(actual['meshes'],template['meshes'])
        self.assertEqual(actual_textures,textures)

    def test_locator_id_and_row_matrix(self):
        payload = bytearray(166)
        struct.pack_into('>5I', payload, 0, 0, 1, 1, 32, 160)
        m = np.eye(4); m[3, :3] = [10, 20, -30]
        struct.pack_into('>16f', payload, 32, *m.ravel())
        struct.pack_into('>3Q2I', payload, 128, 123, 456, 789, 0, 160)
        payload[160:] = b'bench\0'
        rows = locators(resource(0xEB001D, payload))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['template_id'], '0000000000000315')
        self.assertEqual(rows[0]['matrix'][3], [10, 20, -30, 1])
        self.assertEqual(rows[0]['name'], 'bench')
        struct.pack_into('>I', payload, 16, 159)
        with self.assertRaisesRegex(ValueError, 'layout'):
            locators(resource(0xEB001D, payload))

    def test_scaled_rotation_and_normal_transform(self):
        m = np.array([[0, 0, -2, 0], [0, 3, 0, 0], [4, 0, 0, 0], [10, 20, 30, 1]], dtype=float)
        arrays = {'vertices_0': np.array([[1, 2, 3]]), 'normals_0': np.array([[0, 0, 1]]), 'faces_0': np.array([[0, 0, 0]])}
        transformed = transform_mesh(arrays, 0, m)
        np.testing.assert_allclose(transformed['vertices_0'], [[22, 26, 28]])
        np.testing.assert_allclose(transformed['normals_0'], [[1, 0, 0]])
        np.testing.assert_array_equal(arrays['vertices_0'], [[1, 2, 3]])

    def test_missing_template_reference_is_rejected(self):
        payload = bytearray(192)
        struct.pack_into('>5I', payload, 0, 0, 1, 1, 32, 192)
        struct.pack_into('>I', payload, 160, 100)
        with self.assertRaisesRegex(ValueError, 'model reference'):
            template_meshes(resource(0xEB000D, payload))

    def test_dropper_classification_excludes_ground_and_lids(self):
        self.assertEqual(classify_prop('DMO_Glbl_BarrierPlastic_1004_0x1'), ('Barrier', 'Plastic Barrier'))
        self.assertEqual(classify_prop('ak_DMO_UN_GuardRailGrind_1000_0x1'), ('Rail', 'Grind Rail'))
        self.assertEqual(classify_prop('AD_Bench_DMO_1007_0x1'), ('Bench', 'Bench'))
        self.assertIsNone(classify_prop('af_DMO_UN_CrackedGroundJump_1000_0x1'))
        self.assertIsNone(classify_prop('dd_DMO_glbl_TinTrashLid_1007_0x1'))

    def _locator_stream(self, template_id, name):
        payload = bytearray(160 + len(name) + 1)
        struct.pack_into('>5I', payload, 0, 0, 1, 1, 32, 160)
        struct.pack_into('>16f', payload, 32, *np.eye(4).ravel())
        struct.pack_into('>3Q2I', payload, 128, 0, 0, template_id, 0, 160)
        payload[160:160 + len(name)] = name.encode()
        return resource(0xEB001D, payload)

    def test_export_templates_writes_base_centred_catalog(self):
        template_id = 0x123
        name = 'AD_Bench_DMO_1007_0x1'
        template = dict(matrix=np.eye(4), model_matrix=np.eye(4), npz=Path('unused.npz'),
                        meshes=[dict(index=0, retail_texture_ids={'diffuse': '0xabc'},
                                     texture_id='0xabc', source_offsets={'mesh_info': 1})])
        # A triangle floating 2 m above the model origin: recentring must drop it to y=0.
        arrays = {'vertices_0': np.array([[0., 2., 0.], [2., 2., 0.], [0., 2., 2.]]),
                  'faces_0': np.array([[0, 1, 2]]), 'normals_0': np.array([[0., 1., 0.]] * 3)}

        class FakeNpz:
            def __init__(self, values): self.values = values
            def __enter__(self): return self
            def __exit__(self, *a): return False
            def __iter__(self): return iter(self.values)
            def __getitem__(self, key):
                if key not in self.values: raise KeyError(key)
                return self.values[key]

        def fake_write(manifest_path, output, collision, **kwargs):
            Path(output).write_bytes(b'dummy')

        with TemporaryDirectory() as work:
            work = Path(work)
            district = work / 'district.json'
            district.write_text(json.dumps(dict(map_name='University', district_name='University',
                                                simulation_assets=[dict(rx2='loc.rx2')])))
            (work / 'loc.rx2').write_bytes(self._locator_stream(template_id, name))
            output = work / 'dropper-templates'
            with patch('tools.asset_pipeline.dynamic_props.catalog',
                       return_value=({f'{template_id:016X}': template}, {'0xabc': dict(width=1, height=1, rgba='x.rgba')})), \
                 patch('tools.asset_pipeline.dynamic_props.write', side_effect=fake_write), \
                 patch('tools.asset_pipeline.dynamic_props.np.load', return_value=FakeNpz(arrays)):
                count = export_templates(district, [], output, report=lambda _: None)
            self.assertEqual(count, 1)
            catalog = json.loads((output / 'catalog.json').read_text())
            entry = catalog['templates'][0]
            self.assertEqual(entry['category'], 'Bench')
            self.assertEqual(entry['name'], 'Bench')
            self.assertEqual(entry['file'], f'{template_id:016X}.skate')
            self.assertAlmostEqual(entry['bounds'][0][1], 0.0, places=6)
            self.assertTrue((output / entry['file']).is_file())


if __name__ == '__main__':
    unittest.main()
