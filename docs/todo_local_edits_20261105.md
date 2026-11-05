# Readable source and local edits

- [x] Keep imported release hashes and source receipts unchanged when editing the browser app.
- [x] Add `npm run record:edits` to record explicit local changes separately.
- [x] Reject stale manifests, unsafe paths, symlinks, missing entry files, and unrecorded later edits.
- [x] Clear local changes only after a successful official ZIP import.
- [x] Document edit → record → dev/build and the generated WASM factory exception.
- [x] Run Python and JavaScript tests for the desktop shell.
- [ ] Browser-check a locally edited build before distribution; no publication is part of this work.

The user requested GitHub commit/push after verification. No new application tag or binary release is part of this update.
