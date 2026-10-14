const test = require('node:test'), assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../desktop/audio-interface.js'), 'utf8')
  .replace('import.meta.url', JSON.stringify('tauri://localhost/synth.js'));
const flush = async () => { for (let index = 0; index < 3; index++) await new Promise(resolve => setImmediate(resolve)); };
function deferred() { let resolve, reject; const promise = new Promise((ok, fail) => { resolve = ok; reject = fail; }); return {promise, resolve, reject}; }

function boot({url = 'tauri://localhost/index.html', subframe = false, initiallyReady = true, override = {}} = {}) {
  const nodes = new Map(), calls = [], taps = [], modules = [], switches = [], timers = new Map(), pendingPolls = [];
  let nextTimer = 1, nextSession = 0, stopCount = 0, clockTime = 0;
  class Element {
    constructor(tag) { this.tag = tag; this.children = []; this.listeners = new Map(); this.style = {}; this.value = ''; this.hidden = false; this.disabled = false; }
    set id(value) { this._id = value; nodes.set(value, this); } get id() { return this._id; }
    setAttribute() {}
    append(...elements) { this.children.push(...elements); }
    after(element) { this.afterElement = element; }
    add(option) { this.children.push(option); if (this.children.length === 1) this.value = option.value; }
    replaceChildren(...elements) { this.children = elements; this.value = elements[0]?.value ?? ''; }
    get options() { return this.children; }
    get selectedOptions() { return this.children.filter(option => option.value === this.value); }
    addEventListener(type, fn) { const list = this.listeners.get(type) ?? []; list.push(fn); this.listeners.set(type, list); }
    dispatch(type, event = {}) { for (const fn of this.listeners.get(type) ?? []) fn({preventDefault() { event.prevented = true; }, ...event}); }
    blur() { this.blurred = true; }
  }
  class Option { constructor(text, value) { this.textContent = text; this.value = value; } }
  const destination = {}, analyser = {}, recorder = {};
  const context = {sampleRate: 53267, state: 'running', destination,
    audioWorklet: {addModule: async module => {modules.push(String(module)); if (override.moduleError) throw new Error(override.moduleError);}},
    resume: async () => { if (override.resumeError) throw new Error(override.resumeError); }};
  const edges = new Set([destination, analyser, recorder]);
  const output = {connect(target) {edges.add(target);}, disconnect(target) {if (!edges.delete(target)) throw new Error('Missing edge');}};
  class AudioWorkletNode {
    constructor(_context, name, settings) {
      this.name = name; this.settings = settings; this.sent = []; this.edges = new Set();
      this.port = {postMessage: message => {this.sent.push(message);}, onmessage: null}; taps.push(this);
    }
    connect(target) {this.edges.add(target);} disconnect() {this.edges.clear();}
  }
  const window = new Element('window'); window.top = subframe ? {} : window;
  const bridge = {ensureReady: async () => ({context, output}), stopPlayback: async () => {stopCount++;}, setSwitching: value => switches.push(value)};
  if (initiallyReady) window.tetoricaSynthAudio = bridge;
  window.__TAURI_INTERNALS__ = {invoke: async (command, args, options) => {
    calls.push({command, args, options});
    if (override[command]) return override[command](args, options);
    if (command === 'audio_devices') return [{id: 'device-a', name: 'Speakers', isDefault: true}];
    if (command === 'audio_start') return {session: `session-${++nextSession}`, frames: 512, sampleRate: 48000, credits: 4};
    if (command === 'audio_poll') {
      const reply = deferred(); pendingPolls.push({session: args.session, ...reply}); return reply.promise;
    }
    return null;
  }};
  const row = new Element('row'), document = new Element('document'); document.readyState = 'complete';
  document.createElement = tag => new Element(tag); document.querySelector = () => row;
  vm.runInNewContext(source, {window, document, location: new URL(url), URL, Option, AudioWorkletNode, ArrayBuffer, Uint8Array, WeakSet,
    performance: {now: () => clockTime},
    setTimeout(fn) {const id = nextTimer++; timers.set(id, fn); return id;}, clearTimeout(id) {timers.delete(id);}});
  const mode = () => nodes.get('desktop-audio-mode');
  const choose = async value => {mode().value = value; mode().dispatch('change'); await flush();};
  return {nodes, calls, taps, modules, switches, timers, pendingPolls, window, row, bridge, context, edges, analyser, recorder, destination,
    advance(milliseconds) {clockTime += milliseconds;},
    choose, stopCount: () => stopCount,
    poll: async (result = {credits: 0, error: null, queuedFrames: 1024, underruns: 0}, session) => {
      const index = session ? pendingPolls.findIndex(reply => reply.session === session) : pendingPolls.length - 1;
      assert.ok(index >= 0, 'one native long poll must be pending');
      pendingPolls.splice(index, 1)[0].resolve(result); await flush();
    }};
}

test('desktop-only controls wait for the Synth bridge and do not mount in remote pages or subframes', () => {
  for (const options of [{url: 'https://example.com'}, {url: 'http://localhost:8888'}, {url: 'tauri://other/index.html'}, {subframe: true}]) {
    assert.equal(boot(options).nodes.size, 0);
  }
  const ui = boot({initiallyReady: false}); assert.equal(ui.nodes.size, 0);
  ui.window.tetoricaSynthAudio = ui.bridge; ui.window.dispatch('tetorica-synth-ready');
  assert.ok(ui.nodes.get('desktop-audio-mode'));
  const original = ui.row.afterElement; ui.window.dispatch('tetorica-synth-ready'); assert.equal(ui.row.afterElement, original);
});

test('switching to native reroutes only the speaker edge and switching back cleans up', async () => {
  const ui = boot(); await ui.choose('native');
  const tap = ui.taps[0];
  assert.equal(ui.edges.has(ui.destination), false); assert.equal(ui.edges.has(tap), true);
  assert.equal(ui.edges.has(ui.analyser), true); assert.equal(ui.edges.has(ui.recorder), true);
  assert.deepEqual(ui.switches, [true, false]); assert.equal(ui.stopCount(), 2);
  assert.deepEqual(ui.modules, ['tauri://localhost/desktop-audio-worklet.js']);
  assert.equal(tap.settings.processorOptions.outputSampleRate, 48000);
  assert.equal(tap.sent[0].count, 4); assert.match(ui.nodes.get('desktop-audio-status').textContent, /Native/);
  await ui.choose('web');
  assert.equal(ui.edges.has(ui.destination), true); assert.equal(ui.edges.has(tap), false); assert.equal(tap.edges.size, 0);
  assert.equal(ui.timers.size, 0);
  assert.equal(ui.calls.filter(call => call.command === 'audio_stop')[0].args.session, 'session-1');
});

test('PCM writes are raw binary with a session header and are serialised', async () => {
  const write = deferred(); let count = 0;
  const ui = boot({override: {audio_write: () => ++count === 1 ? write.promise : null}});
  await ui.choose('native');
  const packet = new ArrayBuffer(512 * 8), tap = ui.taps[0];
  tap.port.onmessage({data: packet}); tap.port.onmessage({data: packet}); await flush();
  let writes = ui.calls.filter(call => call.command === 'audio_write'); assert.equal(writes.length, 1);
  assert.equal(writes[0].args, packet); assert.equal(writes[0].options.headers['x-synth-audio-session'], 'session-1');
  write.resolve(); await flush(); writes = ui.calls.filter(call => call.command === 'audio_write'); assert.equal(writes.length, 2);
});

test('a stale write error cannot stop a replacement session or steal its credits', async () => {
  const write = deferred();
  const ui = boot({override: {audio_write: () => write.promise}});
  await ui.choose('native'); const oldTap = ui.taps[0];
  oldTap.port.onmessage({data: new ArrayBuffer(512 * 8)}); await flush();
  await ui.choose('web'); await ui.choose('native'); const currentTap = ui.taps[1];
  write.reject(new Error('old disconnected')); await ui.poll({credits: 4, error: 'old error'}, 'session-1');
  await ui.poll({credits: 2, error: null}, 'session-2');
  assert.equal(ui.nodes.get('desktop-audio-mode').value, 'native');
  assert.equal(ui.edges.has(currentTap), true); assert.equal(ui.edges.has(ui.destination), false);
  assert.equal(ui.calls.filter(call => call.command === 'audio_stop').length, 1);
  assert.deepEqual(currentTap.sent.filter(message => message.type === 'credit').map(message => message.count), [4, 2]);
});

test('device errors stop playback, restore Web Audio and show the native error', async () => {
  const ui = boot();
  await ui.choose('native'); await ui.poll({credits: 0, error: 'Device disconnected'});
  assert.equal(ui.nodes.get('desktop-audio-mode').value, 'web'); assert.equal(ui.edges.has(ui.destination), true);
  assert.match(ui.nodes.get('desktop-audio-status').textContent, /Device disconnected/);
  assert.equal(ui.stopCount(), 3); assert.deepEqual(ui.switches, [true, false, true, false]); assert.equal(ui.timers.size, 0);
});

test('pagehide disposes the stream and suppresses late poll responses', async () => {
  const ui = boot();
  await ui.choose('native'); ui.window.dispatch('pagehide');
  await ui.poll({credits: 4, error: null});
  assert.equal(ui.edges.has(ui.destination), true); assert.equal(ui.timers.size, 0);
  assert.equal(ui.calls.filter(call => call.command === 'audio_stop').length, 1);
  assert.equal(ui.taps[0].port.onmessage, null);
});

test('selected device is applied explicitly and the worklet module is reused per AudioContext', async () => {
  const ui = boot(); await ui.choose('native');
  const select = ui.nodes.get('desktop-audio-device'); select.value = 'device-a'; select.dispatch('change');
  assert.equal(select.blurred, true); assert.match(ui.nodes.get('desktop-audio-status').textContent, /Apply device/);
  ui.nodes.get('desktop-audio-output').children[1].children[3].dispatch('click'); await flush();
  const starts = ui.calls.filter(call => call.command === 'audio_start'); assert.equal(starts.length, 2);
  assert.equal(starts[1].args.deviceId, 'device-a'); assert.equal(ui.modules.length, 1);
});

test('native setup failure leaves one Web Audio speaker route and re-enables controls', async () => {
  for (const override of [{moduleError: 'worklet unavailable'}, {resumeError: 'context cannot resume'},
    {audio_start: () => {throw new Error('permission denied');}}, {audio_devices: () => []}]) {
    const ui = boot({override}); await ui.choose('native');
    assert.equal(ui.nodes.get('desktop-audio-mode').value, 'web'); assert.equal(ui.edges.has(ui.destination), true);
    assert.equal(ui.nodes.get('desktop-audio-mode').disabled, false); assert.equal(ui.timers.size, 0);
    assert.equal(ui.switches.at(-1), false); assert.match(ui.nodes.get('desktop-audio-status').textContent, /Web Audio output —/);
  }
});

test('malformed credit or PCM messages fail safely rather than filling an unbounded queue', async () => {
  const ui = boot(); await ui.choose('native'); await ui.poll({credits: Infinity, error: null});
  assert.equal(ui.edges.has(ui.destination), true); assert.match(ui.nodes.get('desktop-audio-status').textContent, /credits/);
  const pcm = boot(); await pcm.choose('native'); pcm.taps[0].port.onmessage({data: new ArrayBuffer(4)}); await flush();
  assert.equal(pcm.edges.has(pcm.destination), true); assert.match(pcm.nodes.get('desktop-audio-status').textContent, /packet/);
});

test('dynamic device packet sizes retain one pending native poll without JavaScript timers', async () => {
  for (const [sampleRate, frames] of [[8000, 128], [16000, 256], [48000, 512],
    [96000, 1024], [192000, 2048], [384000, 4096]]) {
    const ui = boot({override: {audio_start: () => ({session: 'session-1', frames, sampleRate, credits: 4})}});
    await ui.choose('native');
    assert.equal(ui.pendingPolls.length, 1); assert.equal(ui.timers.size, 0);
    assert.equal(ui.taps[0].settings.processorOptions.frames, frames);
    await ui.choose('web');
  }
});

test('native long-poll continuations replenish credits immediately and measure actual elapsed time', async () => {
  const ui = boot();
  await ui.choose('native'); assert.equal(ui.timers.size, 0);
  assert.equal(ui.calls.filter(call => call.command === 'audio_poll').length, 1);
  ui.advance(3); await ui.poll({credits: 1, queuedFrames: 1024, error: null});
  assert.equal(ui.pendingPolls.length, 1); assert.equal(ui.timers.size, 0);
  assert.equal(ui.calls.filter(call => call.command === 'audio_poll').length, 2);
  assert.equal(ui.taps[0].sent.filter(message => message.type === 'clock')[0].intervalSeconds, .003);
  ui.advance(25); await ui.poll();
  assert.equal(ui.taps[0].sent.filter(message => message.type === 'clock')[1].intervalSeconds, .025);
});

test('idle native polls remain bounded and detaching suppresses another request', async () => {
  const ui = boot(); await ui.choose('native'); ui.advance(1000); await flush();
  assert.equal(ui.calls.filter(call => call.command === 'audio_poll').length, 1);
  await ui.poll(); assert.equal(ui.calls.filter(call => call.command === 'audio_poll').length, 2);
  await ui.choose('web'); await ui.poll();
  assert.equal(ui.calls.filter(call => call.command === 'audio_poll').length, 2);
  assert.equal(ui.pendingPolls.length, 0); assert.equal(ui.timers.size, 0);
});
