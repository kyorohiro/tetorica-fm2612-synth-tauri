"""Explicit local edits keep the imported release and source receipts intact."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/import_release.py'
spec = importlib.util.spec_from_file_location('local_importer', SCRIPT)
importer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(importer)
ENTRY = next(name for name in importer.LOCAL_REQUIRED if name != 'index.html')


def release_files():
    files = {'index.html': b'official', ENTRY: b'// official'}
    if hasattr(importer, 'REQUIRED'):
        files.update({
            'synth-dmg/index.html': b'official', 'synth-dmg/processor.js': b'// processor',
            'synth-dmg/synth.css': b'body {}', 'generated/gameboy_apu_wasm.js': b'// factory',
            'generated/gameboy_apu_wasm.wasm': b'\x00asm', 'sources/GameBoy.md': b'native sources',
            'sources/native/gameboy.cpp': b'// native', 'sources/dmg-synth-browser-inputs.zip': b'inputs',
            'LICENSE': b'BSD', 'licenses/mame-gameboy/LICENSE': b'BSD',
        })
        def record(name):
            return {'path': name, 'bytes': len(files[name]), 'sha256': importer.digest(files[name])}
        files['sources/dmg-synth-browser.json'] = json.dumps({
            'productId': 'tetorica-dmg-synth',
            'bundles': [record(name) for name in ['synth-dmg/synth.js', 'synth-dmg/processor.js', 'synth-dmg/synth.css']],
            'sourceArchive': record('sources/dmg-synth-browser-inputs.zip'),
        }).encode()
        files['sources/GameBoy.json'] = json.dumps({
            'component': 'gameboy_apu', 'inputs': [record('sources/native/gameboy.cpp')],
            'generated': {name: record('generated/' + name) for name in ['gameboy_apu_wasm.js', 'gameboy_apu_wasm.wasm']},
        }).encode()
    return files


class LocalChangesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.files = {'index.html': b'original', ENTRY: b'// source', 'optional.txt': b'optional',
                      'sources/original.json': b'original source receipt'}
        for name, data in self.files.items():
            self.write(name, data)
        self.lock = {'version': 'test', 'archive': 'official.zip', 'sha256': importer.digest(b'official.zip'),
                     'files': {name: importer.digest(data) for name, data in self.files.items()}}
        self.lock_path = self.root / 'release.lock.json'
        self.lock_path.write_text(json.dumps(self.lock, indent=2) + '\n')
        self.lock_bytes = self.lock_path.read_bytes()
        self.manifest_path = self.root / 'local-changes.json'

    def write(self, name, data):
        path = self.root / 'dist' / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def manifest(self, changes, **overrides):
        value = {'schemaVersion': 1, 'baseSha256': self.lock['sha256'], 'files': changes}
        value.update(overrides)
        self.manifest_path.write_text(json.dumps(value))
        return value

    def test_records_modified_added_deleted_bytes_without_rewriting_imported_hashes_or_receipts(self):
        self.write(ENTRY, b'// locally edited source')
        self.write('helpers/new.js', b'export const local = true;')
        (self.root / 'dist/optional.txt').unlink()
        manifest = importer.record_local_changes(self.root)
        self.assertEqual(manifest, {'schemaVersion': 1, 'baseSha256': self.lock['sha256'], 'files': {
            ENTRY: importer.digest(b'// locally edited source'),
            'helpers/new.js': importer.digest(b'export const local = true;'), 'optional.txt': None}})
        self.assertEqual(importer.check(self.root), self.lock)
        self.assertEqual(self.lock_path.read_bytes(), self.lock_bytes)
        self.assertEqual((self.root / 'dist/sources/original.json').read_bytes(), self.files['sources/original.json'])
        self.assertEqual(json.loads(self.manifest_path.read_text()), manifest)

    def test_unrecorded_later_edits_are_rejected_until_explicitly_recorded(self):
        self.write(ENTRY, b'// first edit')
        importer.record_local_changes(self.root)
        self.write(ENTRY, b'// later edit')
        with self.assertRaisesRegex(ValueError, 'unrecorded'):
            importer.check(self.root)
        importer.record_local_changes(self.root)
        self.assertEqual(importer.check(self.root), self.lock)
        self.write(ENTRY, self.files[ENTRY])
        self.assertEqual(importer.record_local_changes(self.root)['files'], {})
        self.assertFalse(self.manifest_path.exists())
        self.assertEqual(importer.check(self.root), self.lock)

    def test_recording_and_local_checks_require_application_entries(self):
        for name in importer.LOCAL_REQUIRED:
            with self.subTest(name=name):
                path = self.root / 'dist' / name
                saved = path.read_bytes(); path.unlink()
                with self.assertRaisesRegex(ValueError, 'entry'):
                    importer.record_local_changes(self.root)
                self.manifest({name: None})
                with self.assertRaises(ValueError):
                    importer.check(self.root)
                self.manifest_path.unlink(); self.write(name, saved)
        self.assertFalse(self.manifest_path.exists())

    def test_stale_manifest_is_not_rebased_or_silently_accepted(self):
        self.manifest({}, baseSha256='0' * 64)
        before = self.manifest_path.read_bytes()
        for action in [importer.check, importer.record_local_changes]:
            with self.assertRaisesRegex(ValueError, 'different imported release'):
                action(self.root)
        self.assertEqual(self.manifest_path.read_bytes(), before)
        self.assertEqual(self.lock_path.read_bytes(), self.lock_bytes)

    def test_manifest_rejects_unsafe_paths_invalid_hashes_schema_and_unknown_deletions(self):
        for name in ['../escape', '/absolute', 'a/../escape', './index.html', 'a//b', 'a\\b', 'C:/x', '.', '', 'a\x00b']:
            with self.subTest(name=name):
                self.manifest({name: '1' * 64})
                with self.assertRaises(ValueError): importer.check(self.root)
        for value in ['not-a-hash', '', True, 42, {}, []]:
            with self.subTest(value=value):
                self.manifest({ENTRY: value})
                with self.assertRaises(ValueError): importer.check(self.root)
        for overrides in [{'schemaVersion': True}, {'schemaVersion': 2}, {'files': []}, {'extra': 1}]:
            self.manifest({}, **overrides)
            with self.assertRaises(ValueError): importer.check(self.root)
        self.manifest({'unknown.js': None})
        with self.assertRaises(ValueError): importer.check(self.root)
        self.manifest_path.write_text('{"schemaVersion":1,"schemaVersion":1,"baseSha256":"' + self.lock['sha256'] + '","files":{}}')
        with self.assertRaisesRegex(ValueError, 'Duplicate'): importer.check(self.root)

    def test_symlinks_are_not_recorded_or_followed(self):
        link = self.root / 'dist/link.js'
        try:
            link.symlink_to(self.root / 'release.lock.json')
        except (OSError, NotImplementedError):
            self.skipTest('creating symlinks is not available on this platform')
        for action in [importer.check, importer.record_local_changes]:
            with self.assertRaisesRegex(ValueError, 'Symlinks'):
                action(self.root)
        link.unlink()
        self.manifest_path.symlink_to(self.lock_path)
        with self.assertRaisesRegex(ValueError, 'symlink'):
            importer.record_local_changes(self.root)
        self.assertEqual(self.lock_path.read_bytes(), self.lock_bytes)

    def test_successful_official_import_clears_local_manifest_but_invalid_import_keeps_it(self):
        self.write(ENTRY, b'// local'); importer.record_local_changes(self.root)
        local_bytes = self.manifest_path.read_bytes()
        archive = self.root / 'replacement.zip'
        with zipfile.ZipFile(archive, 'w') as output:
            output.writestr('index.html', b'wrong app')
        with self.assertRaises(ValueError): importer.import_release(self.root, archive)
        self.assertEqual(self.manifest_path.read_bytes(), local_bytes)
        self.assertEqual(importer.check(self.root), self.lock)
        with zipfile.ZipFile(archive, 'w') as output:
            for name, data in release_files().items(): output.writestr(name, data)
        imported = importer.import_release(self.root, archive)
        self.assertFalse(self.manifest_path.exists())
        self.assertEqual(importer.check(self.root), imported)
        self.assertEqual((self.root / 'dist/index.html').read_bytes(), b'official')

    def test_cli_reports_that_verified_bytes_include_recorded_local_changes(self):
        self.write(ENTRY, b'// local'); importer.record_local_changes(self.root)
        program = ('import importlib.util,sys; from pathlib import Path; '
                   'spec=importlib.util.spec_from_file_location("probe",sys.argv[1]); '
                   'module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); '
                   'module.ROOT=Path(sys.argv[2]); sys.argv=[sys.argv[1],"--check"]; module.main()')
        result = subprocess.run([sys.executable, '-c', program, str(SCRIPT), str(self.root)],
                                text=True, capture_output=True, check=True)
        self.assertIn('locally edited Synth', result.stdout)
        self.assertIn('1 recorded local changes', result.stdout)
        self.assertIn(self.lock['sha256'], result.stdout)


if __name__ == '__main__':
    unittest.main()
