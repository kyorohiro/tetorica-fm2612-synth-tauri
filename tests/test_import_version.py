"""Versioned imports use only temporary fixtures, with no build or Git actions."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import sys
import types
import unittest
from unittest import mock
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/import_release.py'
spec = importlib.util.spec_from_file_location('version_importer', SCRIPT)
importer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(importer)


def snapshot(root):
    return {p.relative_to(root).as_posix(): p.read_bytes()
            for p in root.rglob('*') if p.is_file()}


def metadata_fixture(root):
    (root / 'src-tauri').mkdir()
    files = {
        'package.json': {'name': 'fixture-app', 'version': '0.1.0'},
        'package-lock.json': {'name': 'fixture-app', 'version': '0.0.9',
            'lockfileVersion': 3, 'packages': {
                '': {'name': 'fixture-app', 'version': '0.0.9'},
                'node_modules/foreign': {'version': '4.5.6', 'integrity': 'fixed'},
            }},
        'src-tauri/tauri.conf.json': {'productName': 'Fixture App', 'version': '0.1.0'},
    }
    for name, value in files.items():
        (root / name).write_text(json.dumps(value, indent=2) + '\n')
    (root / 'src-tauri/Cargo.toml').write_text(
        '[package]\nname = "fixture-app"\nversion = "0.1.0"\nedition = "2021"\n'
        '\n[dependencies]\nforeign = "1"\n')
    (root / 'src-tauri/Cargo.lock').write_text(
        'version = 4\n\n[[package]]\nname = "fixture-app"\nversion = "0.1.0"\n'
        'dependencies = ["foreign"]\n\n[[package]]\nname = "foreign"\nversion = "1.2.3"\n'
        'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
        'checksum = "' + 'a' * 64 + '"\n')


def release_files(text=b'readable fixture'):
    if hasattr(importer, 'BROWSER_RECEIPT'):
        fixture_spec = importlib.util.spec_from_file_location(
            'version_release_fixture', Path(__file__).with_name('test_import_release.py'))
        fixture = importlib.util.module_from_spec(fixture_spec)
        fixture_spec.loader.exec_module(fixture)
        files = fixture.release_files(text)
        receipt = json.loads(files[importer.BROWSER_RECEIPT])
        receipt['version'] = '0.8.7'
        files[importer.BROWSER_RECEIPT] = json.dumps(receipt).encode()
        return files
    files = {name: b'// readable fixture' for name in importer.LOCAL_REQUIRED}
    files['index.html'] = text
    return files


def write_archive(path, files):
    with zipfile.ZipFile(path, 'w') as archive:
        for name, content in files.items():
            archive.writestr(name, content)


class VersionImportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='import version fixture ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        metadata_fixture(self.root)
        # The ZIP deliberately has no relationship to the requested app version.
        self.archive = self.root / 'web-assets-other-version.zip'
        write_archive(self.archive, release_files())

    def assert_app_version(self, version):
        self.assertEqual(json.loads((self.root / 'package.json').read_text())['version'], version)
        lock = json.loads((self.root / 'package-lock.json').read_text())
        self.assertEqual(lock['version'], version)
        self.assertEqual(lock['packages']['']['version'], version)
        self.assertEqual(lock['packages']['node_modules/foreign'],
                         {'version': '4.5.6', 'integrity': 'fixed'})
        self.assertEqual(json.loads((self.root / 'src-tauri/tauri.conf.json').read_text())['version'], version)
        import tomllib
        self.assertEqual(tomllib.loads((self.root / 'src-tauri/Cargo.toml').read_text())['package']['version'], version)
        packages = tomllib.loads((self.root / 'src-tauri/Cargo.lock').read_text())['package']
        self.assertEqual(packages[0]['version'], version)
        self.assertEqual(packages[1], {'name': 'foreign', 'version': '1.2.3',
            'source': 'registry+https://github.com/rust-lang/crates.io-index', 'checksum': 'a' * 64})

    def test_version_import_updates_all_five_files_and_keeps_web_provenance(self):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            lock = importer.import_release(self.root, self.archive, version='1.2.3')
        self.assert_app_version('1.2.3')
        self.assertEqual(lock['archive'], self.archive.name)
        expected = '0.8.7' if hasattr(importer, 'BROWSER_RECEIPT') else 'sha256-' + lock['sha256'][:12]
        self.assertEqual(lock['version'], expected)
        self.assertEqual(importer.check(self.root), lock)
        self.assertIn('App version set to 1.2.3', output.getvalue())

    def test_invalid_version_rejects_before_any_write(self):
        for version in ('../bad', '01.2.3', '1.2', '1.2.3-beta'):
            with self.subTest(version=version):
                before = snapshot(self.root)
                with self.assertRaises(ValueError):
                    importer.import_release(self.root, self.archive, version=version)
                self.assertEqual(snapshot(self.root), before)

    def test_malformed_metadata_rejects_before_assets_change(self):
        (self.root / 'package-lock.json').write_text('{invalid')
        before = snapshot(self.root)
        with self.assertRaises(ValueError):
            importer.import_release(self.root, self.archive, version='1.2.3')
        self.assertEqual(snapshot(self.root), before)

    def test_bad_zip_leaves_all_versions_and_assets_unchanged(self):
        write_archive(self.archive, {'index.html': b'wrong product'})
        before = snapshot(self.root)
        with self.assertRaises(ValueError):
            importer.import_release(self.root, self.archive, version='1.2.3')
        self.assertEqual(snapshot(self.root), before)

    def test_metadata_apply_failure_restores_dist_receipt_and_local_changes(self):
        with contextlib.redirect_stdout(io.StringIO()):
            importer.import_release(self.root, self.archive)
        (self.root / importer.LOCAL_MANIFEST).write_bytes(b'{"saved-local-edit": true}\n')
        write_archive(self.archive, release_files(b'new imported sound'))
        before = snapshot(self.root)
        for failure in (OSError('fixture write failure'), KeyboardInterrupt()):
            with self.subTest(failure=type(failure).__name__):
                update = types.SimpleNamespace(version='1.2.3', apply=mock.Mock(side_effect=failure))
                with mock.patch.object(importer, 'prepare_app_version', return_value=update):
                    with self.assertRaises(type(failure)):
                        importer.import_release(self.root, self.archive, version='1.2.3')
                self.assertEqual(snapshot(self.root), before)

    def test_legacy_import_does_not_require_or_create_app_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'legacy.zip'
            write_archive(archive, release_files())
            with contextlib.redirect_stdout(io.StringIO()) as output:
                lock = importer.import_release(root, archive)
            self.assertEqual(importer.check(root), lock)
            self.assertFalse((root / 'package.json').exists())
            self.assertNotIn('App version', output.getvalue())

    def test_cli_passes_optional_version_and_rejects_non_import_actions(self):
        with mock.patch.object(importer, 'ROOT', self.root), \
             mock.patch.object(sys, 'argv', ['import_release.py', str(self.archive), '--version', '1.2.3']), \
             contextlib.redirect_stdout(io.StringIO()):
            importer.main()
        self.assert_app_version('1.2.3')
        for args in (['--check', '--version', '1.2.3'],
                     ['--record-local-changes', '--version', '1.2.3'],
                     ['--version', '1.2.3'],
                     [str(self.archive), '--check', '--version', '1.2.3']):
            with self.subTest(args=args):
                before = snapshot(self.root)
                with mock.patch.object(sys, 'argv', ['import_release.py', *args]), \
                     contextlib.redirect_stderr(io.StringIO()), \
                     self.assertRaises(SystemExit) as caught:
                    importer.main()
                self.assertEqual(caught.exception.code, 2)
                self.assertEqual(snapshot(self.root), before)


if __name__ == '__main__':
    unittest.main()
