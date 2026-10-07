const test = require('node:test'), assert = require('node:assert/strict');
const fs = require('node:fs'), vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../desktop/desktop-interface.js'), 'utf8');
function boot(url, subframe = false, fail = false, confirmed = false) {
  const calls = [], status = {textContent: ''}, listeners = {}, button = {disabled: false, addEventListener: (type, fn) => {listeners[type] = fn;}};
  class Anchor {click() {calls.push('browser-click');}}
  let serial = 0;
  const URLMock = {createObjectURL: () => `blob:${++serial}`, revokeObjectURL() {}};
  const window = {__TAURI_INTERNALS__: {invoke: async (command, args) => {calls.push({command, args}); if (fail) throw new Error('write failed'); return command === 'save_export' ? '/tmp/voice.tfi' : false;}}};
  window.top = subframe ? {} : window;
  vm.runInNewContext(source, {window, location: new URL(url), URL: URLMock, HTMLAnchorElement: Anchor, Map, Uint8Array, Array,
    document: {readyState: 'complete', getElementById: () => status, querySelector: () => button,
      createElement: () => ({style: {}, children: [], listeners: {},
        addEventListener(type, fn) {this.listeners[type] = fn;}, append(...items) {this.children.push(...items);},
        close(value) {this.returnValue = value; this.listeners.close();}, remove() {}, showModal() {}, focus() {}}),
      body: {append(dialog) {setImmediate(() => dialog.children[confirmed ? 2 : 1].listeners.click());}}}});
  return {calls, status, listeners, button, Anchor, URL: URLMock};
}
test('local Synth exports original bytes even after immediate URL revocation', async () => {
  for (const url of ['tauri://localhost/index.html', 'http://tauri.localhost/index.html']) {
    const ui = boot(url), link = new ui.Anchor();
    link.href = ui.URL.createObjectURL(new Blob([new Uint8Array([1, 2, 255])])); link.download = 'voice.tfi';
    link.click(); ui.URL.revokeObjectURL(link.href);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(ui.calls[0].command, 'save_export');
    assert.deepEqual(Array.from(ui.calls[0].args.bytes), [1, 2, 255]);
    assert.equal(ui.calls[0].args.filename, 'voice.tfi'); assert.match(ui.status.textContent, /Saved/);
  }
});
test('remote pages and subframes retain browser behavior without native hooks', () => {
  for (const [url, frame] of [['https://example.com/', false], ['http://localhost:8890/', false], ['tauri://other/', false], ['tauri://localhost/', true]]) {
    const ui = boot(url, frame); const link = new ui.Anchor(); link.href = ui.URL.createObjectURL(new Blob(['x'])); link.download = 'x'; link.click();
    assert.deepEqual(ui.calls, ['browser-click']); assert.equal(ui.listeners.click, undefined);
  }
});
test('ordinary links use normal browser navigation and save errors are visible', async () => {
  const ui = boot('tauri://localhost/', false, true), link = new ui.Anchor(); link.href = 'https://example.com'; link.click();
  assert.equal(ui.calls[0], 'browser-click');
  link.href = ui.URL.createObjectURL(new Blob(['x'])); link.download = 'voice.vgi'; link.click();
  await new Promise(resolve => setImmediate(resolve)); assert.match(ui.status.textContent, /Could not save.*write failed/);
});
test('cache button confirms before native reload and cancellation re-enables it', async () => {
  for (const confirmed of [false, true]) {
    const ui = boot('tauri://localhost/', false, false, confirmed);
    await ui.listeners.click({preventDefault() {}, stopImmediatePropagation() {}});
    assert.equal(ui.calls.length, confirmed ? 1 : 0);
    if (confirmed) assert.equal(ui.calls[0].command, 'reload_window');
    assert.equal(ui.button.disabled, false);
  }
});
