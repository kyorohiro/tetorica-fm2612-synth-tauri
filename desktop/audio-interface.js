// Appended to Synth's module by the desktop resource server. The synthesis,
// effects, visualisers and recorder stay in Web Audio; Rust owns device output.
(() => {
  'use strict';
  const local = (location.protocol === 'tauri:' && location.hostname === 'localhost') ||
    (['http:', 'https:'].includes(location.protocol) && location.hostname === 'tauri.localhost');
  if (window.top !== window || !local || !window.__TAURI_INTERNALS__) return;
  const invoke = (command, args = {}, options) => window.__TAURI_INTERNALS__.invoke(command, args, options);
  const clockNow = () => globalThis.performance?.now?.() ?? Date.now();
  let mounted = false;

  function mount() {
    const bridge = window.tetoricaSynthAudio;
    const row = document.querySelector('.volume-row');
    if (mounted || !bridge || !row) return;
    mounted = true;
    const panel = document.createElement('details');
    panel.id = 'desktop-audio-output';
    panel.style.cssText = 'margin:0 0 8px;font-size:.75rem;color:#d5dfd5';
    const summary = document.createElement('summary');
    summary.textContent = 'Audio output';
    summary.style.cssText = 'cursor:pointer;width:max-content;padding:4px 0';
    const controls = document.createElement('div');
    controls.style.cssText = 'display:flex;align-items:center;gap:6px;flex-wrap:wrap;padding:6px 0';
    const mode = document.createElement('select');
    mode.id = 'desktop-audio-mode';
    mode.setAttribute('aria-label', 'Audio output');
    mode.add(new Option('Web Audio', 'web'));
    mode.add(new Option('Native (Rust)', 'native'));
    const device = document.createElement('select');
    device.id = 'desktop-audio-device';
    device.setAttribute('aria-label', 'Native audio output device');
    device.add(new Option('System default', ''));
    const refresh = document.createElement('button'), apply = document.createElement('button');
    refresh.type = apply.type = 'button';
    refresh.textContent = 'Refresh'; apply.textContent = 'Apply device';
    refresh.className = apply.className = 'toolbar-button';
    for (const select of [mode, device]) {
      select.style.cssText = 'max-width:100%;min-height:26px;border:1px solid #52605b;border-radius:8px;padding:2px 6px;background:#242d2e;color:#d5dfd5;font:inherit';
    }
    const status = document.createElement('p');
    status.id = 'desktop-audio-status'; status.setAttribute('role', 'status');
    status.style.cssText = 'margin:2px 0;white-space:pre-wrap;overflow-wrap:anywhere';
    status.textContent = 'Web Audio output';
    const hint = document.createElement('p');
    hint.textContent = 'Changing output stops notes, recording and loop playback.';
    hint.style.cssText = 'margin:4px 0;color:#aebcad';
    controls.append(mode, device, refresh, apply);
    panel.append(summary, controls, status, hint);
    row.after(panel);
    // Button pointer interaction must not take the playing keyboard's focus.
    for (const button of [refresh, apply, summary]) button.addEventListener('pointerdown', event => event.preventDefault());
    for (const select of [mode, device]) select.addEventListener('change', () => select.blur());

    let session = null, busy = false, closed = false, switching = 0;
    const loadedContexts = new WeakSet();
    const beginSwitch = () => { if (switching++ === 0) bridge.setSwitching?.(true); };
    const endSwitch = () => { if (--switching === 0) bridge.setSwitching?.(false); };
    function show() {
      device.hidden = refresh.hidden = apply.hidden = mode.value !== 'native';
    }
    show();

    function detach(current = session) {
      if (!current) return;
      if (session === current) session = null;
      current.tap?.port.postMessage({type: 'stop'});
      if (current.tap) current.tap.port.onmessage = null;
      if (current.routed) {
        try { current.output.disconnect(current.tap); } catch {}
        try { current.tap.disconnect(); } catch {}
        if (current.context.state !== 'closed') {
          try { current.output.connect(current.context.destination); } catch {}
        }
        current.routed = false;
      }
    }

    async function stopNative() {
      const old = session;
      detach(old);
      if (old) await invoke('audio_stop', {session: old.id});
    }

    function errorText(error) {
      return String(error?.message ?? error ?? 'Native audio failed');
    }

    async function failed(current, error) {
      if (closed || session !== current || current.failed) return;
      current.failed = true;
      beginSwitch();
      try {
        try { await bridge.stopPlayback(); } catch {}
        // A delayed failure from a replaced stream cannot affect the new output.
        if (closed || session !== current) return;
        detach(current);
        void invoke('audio_stop', {session: current.id}).catch(() => {});
        mode.value = 'web'; show();
        status.textContent = `Web Audio output — ${errorText(error)}`;
        status.title = errorText(error);
      } finally { endSwitch(); }
    }

    async function refreshDevices() {
      const previous = device.value;
      const devices = await invoke('audio_devices');
      if (closed) return;
      device.replaceChildren(new Option('System default', ''));
      for (const entry of devices) {
        device.add(new Option(`${entry.name}${entry.isDefault ? ' — default' : ''}`, entry.id));
      }
      if ([...device.options].some(option => option.value === previous)) device.value = previous;
      if (!devices.length) throw new Error('No audio output devices found');
    }

    async function poll(current) {
      if (current.polling) return;
      current.polling = true;
      try {
        while (!closed && session === current && !current.failed) {
          // Native waits for consumed packet credits/error (up to 100 ms when
          // idle). This IPC continuation avoids WebView background timer
          // throttling, while only one request per session remains in flight.
          const result = await invoke('audio_poll', {session: current.id});
          if (closed || session !== current || current.failed) return;
          if (result.error) throw new Error(result.error);
          if (!Number.isInteger(result.credits) || result.credits < 0 || result.credits > current.maxCredits) {
            throw new Error('Invalid native audio credits');
          }
          if (result.credits) current.tap.port.postMessage({type: 'credit', count: result.credits});
          const now = clockNow();
          if (Number.isFinite(result.queuedFrames)) {
            current.tap.port.postMessage({type: 'clock', queuedFrames: result.queuedFrames,
              intervalSeconds: Math.max(0.001, Math.min(1, (now - current.pollTime) / 1000))});
          }
          current.pollTime = now;
          if (now - current.lastStatsTime >= 1000) {
            current.tap.port.postMessage({type: 'stats'});
            status.title = `Device buffer: ${result.queuedFrames ?? 0} frames; buffer underruns: ${result.underruns ?? 0}; capture drops: ${current.droppedFrames} frames.`;
            current.lastStatsTime = now;
          }
        }
      } catch (error) { void failed(current, error); }
      finally { current.polling = false; }
    }

    async function connect() {
      const state = await bridge.ensureReady();
      if (closed) return;
      const {context, output} = state;
      if (!context?.audioWorklet || !output) throw new Error('Synth audio output is unavailable');
      if (!loadedContexts.has(context)) {
        await context.audioWorklet.addModule(new URL('./desktop-audio-worklet.js', import.meta.url));
        loadedContexts.add(context);
      }
      const info = await invoke('audio_start', {sampleRate: context.sampleRate, deviceId: device.value || null});
      if (closed) { void invoke('audio_stop', {session: info.session}).catch(() => {}); return; }
      const current = {id: info.session, context, output, tap: null, routed: false,
        failed: false, pending: 0, writes: Promise.resolve(), maxCredits: info.credits,
        frames: info.frames, droppedFrames: 0, polling: false, pollTime: clockNow(), lastStatsTime: clockNow()};
      session = current;
      try {
        if (typeof info.session !== 'string' || !info.session || !Number.isInteger(info.frames) || info.frames < 128 || info.frames > 4096 || info.frames % 128 ||
            !Number.isInteger(info.credits) || info.credits < 1 || info.credits > 32 ||
            !Number.isFinite(info.sampleRate) || info.sampleRate < 8000 || info.sampleRate > 384000) {
          throw new Error('Invalid native audio session');
        }
        const tap = new AudioWorkletNode(context, 'tetorica-synth-desktop-output', {
          numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [2],
          processorOptions: {frames: info.frames, maxCredits: info.credits, outputSampleRate: info.sampleRate},
        });
        current.tap = tap;
        tap.port.onmessage = event => {
          if (closed || session !== current || current.failed) return;
          if (event.data?.type === 'stats') { current.droppedFrames = event.data.droppedFrames; return; }
          const packet = event.data;
          if (!(packet instanceof ArrayBuffer) || packet.byteLength !== info.frames * 8 ||
              current.pending >= info.credits * 2) {
            void failed(current, new Error('Invalid or stalled native audio packet')); return;
          }
          current.pending++;
          current.writes = current.writes.then(async () => {
            if (closed || session !== current || current.failed) return;
            await invoke('audio_write', packet, {headers: {'x-synth-audio-session': current.id}});
          }).catch(error => { void failed(current, error); }).finally(() => { current.pending--; });
        };
        tap.onprocessorerror = () => { void failed(current, new Error('Native audio capture stopped')); };
        await bridge.stopPlayback();
        if (closed || session !== current) return;
        // Remove only the speaker edge: analyser and recorder connections stay.
        output.disconnect(context.destination);
        current.routed = true;
        output.connect(tap); tap.connect(context.destination);
        tap.port.postMessage({type: 'credit', count: info.credits});
        await context.resume();
        status.textContent = `Native → ${device.selectedOptions[0]?.textContent ?? 'System default'} (${info.sampleRate.toLocaleString()} Hz)`;
        status.title = '';
        void poll(current);
      } catch (error) {
        detach(current);
        await invoke('audio_stop', {session: current.id}).catch(() => {});
        throw error;
      }
    }

    async function change(action) {
      if (busy || closed) return;
      busy = true;
      beginSwitch();
      mode.disabled = device.disabled = refresh.disabled = apply.disabled = true;
      try { await action(); }
      catch (error) {
        try { await bridge.stopPlayback(); } catch {}
        try { await stopNative(); } catch {}
        if (!closed) {
          mode.value = 'web'; show();
          status.textContent = `Web Audio output — ${errorText(error)}`;
          status.title = errorText(error);
        }
      } finally {
        busy = false;
        endSwitch();
        mode.disabled = device.disabled = refresh.disabled = apply.disabled = false;
      }
    }

    mode.addEventListener('change', () => {
      show();
      void change(async () => {
        await bridge.stopPlayback();
        await stopNative();
        if (mode.value === 'native') { await refreshDevices(); await connect(); }
        else { status.textContent = 'Web Audio output'; status.title = ''; }
      });
    });
    refresh.addEventListener('click', () => { void change(refreshDevices); });
    apply.addEventListener('click', () => { void change(async () => {
      await bridge.stopPlayback(); await stopNative(); await connect();
    }); });
    device.addEventListener('change', () => {
      if (!busy) status.textContent = 'Select Apply device to change output.';
    });
    window.addEventListener('pagehide', () => {
      closed = true;
      const old = session; detach(old);
      if (old) void invoke('audio_stop', {session: old.id}).catch(() => {});
    }, {once: true});
  }

  window.addEventListener('tetorica-synth-ready', mount);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount, {once: true});
  else mount();
})();
