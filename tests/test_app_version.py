# SPDX-License-Identifier: BSD-3-Clause
"""Version preparation and rollback fixtures; no real app metadata is changed."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location(
    'app_version_fixture', Path(__file__).resolve().parents[1] / 'scripts/app_version.py')
versions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(versions)


class AppVersionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'src-tauri').mkdir()
        for name, value in {
            'package.json': {'name': 'fixture', 'version': '1.2.3'},
            'package-lock.json': {
                'name': 'fixture', 'version': '0.8.0', 'lockfileVersion': 3,
                'packages': {'': {'name': 'fixture', 'version': '0.8.0'},
                             'node_modules/example': {'version': '4.5.6'}},
            },
            'src-tauri/tauri.conf.json': {'productName': 'Fixture', 'version': '1.2.3'},
        }.items():
            (self.root / name).write_text(json.dumps(value, indent=2) + '\n')
        (self.root / 'src-tauri/Cargo.toml').write_text(
            '[package]\nname = "fixture"\nversion = "1.2.3" # keep this comment\nedition = "2021"\n'
            '\n[dependencies]\nexample = { version = "4.5.6" }\n')
        (self.root / 'src-tauri/Cargo.lock').write_text(
            '# generated fixture\nversion = 4\n\n'
            '[[package]]\nname = "fixture"\nversion = "1.2.3"\ndependencies = ["example"]\n\n'
            '[[package]]\nname = "example"\nversion = "4.5.6"\n'
            'source = "registry+https://example.invalid/index"\nchecksum = "abc123"\n\n'
            '[[package]]\nname = "fixture"\nversion = "9.0.0"\n'
            'source = "registry+https://example.invalid/index"\nchecksum = "def456"\n')

    def snapshot(self):
        return {name: (self.root / name).read_bytes() for name in versions.FILES}

    def test_preparation_is_read_only_and_apply_syncs_all_five_files(self):
        before = self.snapshot()
        foreign = versions.dependency_snapshot(self.root)
        update = versions.prepare_version_update(self.root, '2.0.4')
        self.assertEqual(update.version, '2.0.4')
        self.assertEqual(self.snapshot(), before)
        update.apply()
        versions.verify_metadata(self.root, '2.0.4')
        self.assertEqual(versions.dependency_snapshot(self.root), foreign)
        self.assertEqual(foreign[-1]['version'], '9.0.0')
        for name in versions.FILES:
            self.assertNotEqual((self.root / name).read_bytes(), before[name])
        self.assertIn('version = "2.0.4" # keep this comment',
                      (self.root / 'src-tauri/Cargo.toml').read_text())
        npm_lock = json.loads((self.root / 'package-lock.json').read_text())
        self.assertEqual(npm_lock['packages']['node_modules/example']['version'], '4.5.6')

    def test_invalid_versions_never_write(self):
        before = self.snapshot()
        for version in ('01.2.3', '1.02.3', '1.2.03', '1.2', 'v1.2.3', '../1.2.3',
                        '1.2.3-beta', '1.2.3+build', '1.2.3\n', None):
            with self.subTest(version=version), self.assertRaises(ValueError):
                versions.prepare_version_update(self.root, version)
            self.assertEqual(self.snapshot(), before)

    def test_missing_and_malformed_metadata_never_write(self):
        cases = (
            ('package-lock.json', '{broken'),
            ('package-lock.json', json.dumps({'version': '1.2.3', 'packages': {}})),
            ('src-tauri/tauri.conf.json', json.dumps({'productName': 'Fixture', 'version': '01.2.3'})),
        )
        for name, value in cases:
            path = self.root / name
            original = path.read_bytes()
            path.write_text(value)
            before = self.snapshot()
            with self.subTest(name=name, value=value), self.assertRaises(ValueError):
                versions.prepare_version_update(self.root, '2.0.0')
            self.assertEqual(self.snapshot(), before)
            path.write_bytes(original)

    def test_explicit_version_repairs_mismatched_numeric_app_versions(self):
        for name, old_version in (
            ('src-tauri/Cargo.toml', b'1.2.4'),
            ('src-tauri/tauri.conf.json', b'1.2.5'),
            ('src-tauri/Cargo.lock', b'1.2.6'),
        ):
            path = self.root / name
            path.write_bytes(path.read_bytes().replace(b'1.2.3', old_version))
        before = self.snapshot()
        foreign = versions.dependency_snapshot(self.root)
        update = versions.prepare_version_update(self.root, '2.0.0')
        self.assertEqual(self.snapshot(), before)
        update.apply()
        versions.verify_metadata(self.root, '2.0.0')
        self.assertEqual(versions.dependency_snapshot(self.root), foreign)

    def test_changed_originals_abort_before_any_replacement(self):
        update = versions.prepare_version_update(self.root, '2.0.0')
        path = self.root / 'package.json'
        path.write_text(path.read_text() + '\n')
        before = self.snapshot()
        with self.assertRaisesRegex(ValueError, 'changed after'):
            update.apply()
        self.assertEqual(self.snapshot(), before)

    def test_replacement_failure_restores_replaced_metadata(self):
        before = self.snapshot()
        update = versions.prepare_version_update(self.root, '2.0.0')
        replace = os.replace
        calls = 0

        def fail_second(source, destination):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError('fixture replacement failure')
            return replace(source, destination)

        with mock.patch.object(versions.os, 'replace', side_effect=fail_second):
            with self.assertRaisesRegex(OSError, 'fixture replacement failure'):
                update.apply()
        self.assertGreater(calls, 2)
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(list(self.root.glob('.app-version-*')), [])

    def test_foreign_lock_bytes_and_crlf_are_preserved(self):
        path = self.root / 'src-tauri/Cargo.lock'
        original = path.read_bytes().replace(b'\n', b'\r\n')
        path.write_bytes(original)
        versions.prepare_version_update(self.root, '2.0.0').apply()
        expected = original.replace(b'name = "fixture"\r\nversion = "1.2.3"',
                                    b'name = "fixture"\r\nversion = "2.0.0"', 1)
        self.assertEqual(path.read_bytes(), expected)
        self.assertEqual(tomllib.loads(path.read_text())['version'], 4)


if __name__ == '__main__':
    unittest.main()
