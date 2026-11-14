"""Import an unmodified Synth release; pin archive and extracted file hashes."""
import argparse
import hashlib
import importlib.util
import json
import re
from pathlib import Path, PurePosixPath
import shutil
import stat
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def unpack(archive, destination):
    hashes = {}
    with zipfile.ZipFile(archive) as source:
        entries = source.infolist()
        for entry in entries:
            path = PurePosixPath(entry.filename)
            if (path.is_absolute() or '..' in path.parts or '\\' in entry.filename
                    or ':' in entry.filename or stat.S_ISLNK(entry.external_attr >> 16)):
                raise ValueError(f'Unsafe ZIP entry: {entry.filename}')
        for entry in entries:
            if entry.is_dir():
                continue
            name = str(PurePosixPath(entry.filename))
            if name in hashes:
                raise ValueError(f'Duplicate ZIP entry: {name}')
            data = source.read(entry)
            target = destination / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            hashes[name] = digest(data)
    if 'index.html' not in hashes or 'synth.js' not in hashes:
        raise ValueError('Expected Synth ZIP with index.html and synth.js at its root')
    return hashes


LOCAL_REQUIRED = {'index.html', 'synth.js'}
LOCAL_MANIFEST = 'local-changes.json'


def safe_name(name):
    if not isinstance(name, str) or not name or any(ord(char) < 32 or ord(char) == 127 for char in name):
        raise ValueError('Expected a safe relative release file path')
    path = PurePosixPath(name)
    if (path.is_absolute() or name != path.as_posix() or '..' in path.parts
            or name == '.' or '\\' in name or ':' in name):
        raise ValueError(f'Unsafe local change path: {name}')
    return name


def valid_hash(value):
    return isinstance(value, str) and re.fullmatch(r'[0-9a-f]{64}', value) is not None


def snapshot_dist(root):
    directory = root / 'dist'
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError('Expected a real dist directory')
    hashes = {}
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            raise ValueError(f'Symlinks are not release files: {path.relative_to(directory)}')
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError(f'Expected a regular release file: {path.relative_to(directory)}')
        name = safe_name(path.relative_to(directory).as_posix())
        hashes[name] = digest(path.read_bytes())
    return hashes


def baseline_files(lock):
    files = lock.get('files') if isinstance(lock, dict) else None
    if not isinstance(files, dict) or not valid_hash(lock.get('sha256')):
        raise ValueError('Expected a pinned release SHA-256 and file hashes')
    for name, sha in files.items():
        safe_name(name)
        if not valid_hash(sha):
            raise ValueError(f'Invalid imported file SHA-256: {name}')
    return files


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'Duplicate local manifest property: {key}')
        result[key] = value
    return result


def local_changes(root, lock):
    path = root / LOCAL_MANIFEST
    if path.is_symlink():
        raise ValueError('Local change manifest must not be a symlink')
    if not path.exists():
        return None
    baseline = baseline_files(lock)
    manifest = json.loads(path.read_text(), object_pairs_hook=unique_object)
    if (not isinstance(manifest, dict) or set(manifest) != {'schemaVersion', 'baseSha256', 'files'}
            or type(manifest['schemaVersion']) is not int or manifest['schemaVersion'] != 1
            or not isinstance(manifest['files'], dict)):
        raise ValueError('Invalid local change manifest schema')
    if manifest['baseSha256'] != lock['sha256']:
        raise ValueError('Local changes belong to a different imported release; import and reapply edits')
    for name, sha in manifest['files'].items():
        safe_name(name)
        if sha is None:
            if name not in baseline:
                raise ValueError(f'Cannot delete an unknown imported file: {name}')
        elif not valid_hash(sha):
            raise ValueError(f'Invalid local file SHA-256: {name}')
    return manifest


def require_entries(files):
    missing = LOCAL_REQUIRED - files.keys()
    if missing:
        raise ValueError('Local edits must keep application entry files: ' + ', '.join(sorted(missing)))


def record_local_changes(root):
    lock = json.loads((root / 'release.lock.json').read_text())
    baseline = baseline_files(lock)
    local_changes(root, lock)  # Reject stale or malformed records before replacing them.
    actual = snapshot_dist(root)
    require_entries(actual)
    changes = {name: actual.get(name) for name in sorted(baseline.keys() | actual.keys())
               if actual.get(name) != baseline.get(name)}
    manifest = {'schemaVersion': 1, 'baseSha256': lock['sha256'], 'files': changes}
    destination = root / LOCAL_MANIFEST
    if changes:
        with tempfile.TemporaryDirectory(prefix='.local-edits-', dir=root) as temporary:
            staging = Path(temporary) / LOCAL_MANIFEST
            staging.write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
            staging.replace(destination)
        print(f"Recorded {len(changes)} local changes over Synth {lock['version']}; imported release hashes kept")
    else:
        destination.unlink(missing_ok=True)
        print(f"Restored imported Synth {lock['version']}; no local changes")
    return manifest


def check(root):
    lock = json.loads((root / 'release.lock.json').read_text())
    actual = snapshot_dist(root)
    manifest = local_changes(root, lock)
    expected = dict(lock['files'])
    if manifest is not None:
        for name, sha in manifest['files'].items():
            if sha is None:
                expected.pop(name)
            else:
                expected[name] = sha
        require_entries(actual)
    if actual != expected:
        raise ValueError('dist has unrecorded changes; run npm run record:edits or import the pinned ZIP again')
    return lock


def prepare_app_version(root, version):
    """Load this checkout's standalone metadata updater only when requested."""
    spec = importlib.util.spec_from_file_location(
        'tetorica_app_version', Path(__file__).with_name('app_version.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.prepare_version_update(root, version)


def import_release(root, archive, version=None):
    version_update = prepare_app_version(root, version) if version is not None else None
    if not archive.is_file():
        raise FileNotFoundError(f'Synth release ZIP not found: {archive}')
    sha = digest(archive.read_bytes())
    previous = root / 'release.lock.json'
    old_lock_bytes = previous.read_bytes() if previous.exists() else None
    old_lock = json.loads(old_lock_bytes) if old_lock_bytes is not None else {}
    local_manifest = root / LOCAL_MANIFEST
    old_local_bytes = local_manifest.read_bytes() if local_manifest.exists() else None
    release_version = old_lock.get('version') if old_lock.get('sha256') == sha else f'sha256-{sha[:12]}'
    with tempfile.TemporaryDirectory(prefix='.import-', dir=root) as temporary:
        staging = Path(temporary) / 'dist'
        staging.mkdir()
        hashes = unpack(archive, staging)
        lock = {'version': release_version, 'archive': archive.name, 'sha256': sha, 'files': hashes}
        destination = root / 'dist'
        backup = Path(temporary) / 'previous-dist'
        installed = False
        try:
            if destination.exists():
                destination.rename(backup)
            staging.rename(destination)
            installed = True
            previous.write_text(json.dumps(lock, indent=2, sort_keys=True) + '\n')
            local_manifest.unlink(missing_ok=True)
            if version_update is not None:
                version_update.apply()
        except BaseException:
            # Metadata applies transactionally too. Restore the imported assets
            # and provenance if either installation or the app update fails.
            if installed:
                shutil.rmtree(destination)
            if backup.exists():
                backup.rename(destination)
            if old_lock_bytes is None:
                previous.unlink(missing_ok=True)
            else:
                previous.write_bytes(old_lock_bytes)
            if old_local_bytes is None:
                local_manifest.unlink(missing_ok=True)
            else:
                local_manifest.write_bytes(old_local_bytes)
            raise
    print(f'Imported {release_version}: {len(hashes)} files, SHA256 {sha}')
    if version_update is not None:
        print(f'App version set to {version_update.version}')
    return lock


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('zip', nargs='?', type=Path, help='Path to the Synth release ZIP')
    parser.add_argument('--version', help='Set the app version to X.Y.Z; independent of the ZIP filename')
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument('--check', action='store_true')
    actions.add_argument('--record-local-changes', action='store_true')
    args = parser.parse_args()
    if (args.check or args.record_local_changes) and (args.zip is not None or args.version is not None):
        parser.error('choose a ZIP import, --check, or --record-local-changes')
    if not args.check and not args.record_local_changes and args.zip is None:
        parser.error('provide the ZIP path: npm run import:release -- ./xxx.zip')
    try:
        if args.check:
            lock = check(ROOT)
            manifest = local_changes(ROOT, lock)
            if manifest is None:
                print(f"Verified Synth {lock['version']}: {len(lock['files'])} files")
            else:
                print(f"Verified locally edited Synth {lock['version']}: {len(manifest['files'])} recorded local changes over SHA256 {lock['sha256']}")
        elif args.record_local_changes:
            record_local_changes(ROOT)
        else:
            import_release(ROOT, args.zip, version=args.version)
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        parser.error(str(error))


if __name__ == '__main__':
    main()
