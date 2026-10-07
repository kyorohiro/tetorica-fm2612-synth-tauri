(() => {
  'use strict';
  const local = (location.protocol === 'tauri:' && location.hostname === 'localhost') ||
    (['http:', 'https:'].includes(location.protocol) && location.hostname === 'tauri.localhost');
  if (window.top !== window || !local) return;
  const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args);
  const status = text => {const element = document.getElementById('status'); if (element) element.textContent = text;};
  const blobs = new Map();
  const create = URL.createObjectURL.bind(URL), revoke = URL.revokeObjectURL.bind(URL);
  URL.createObjectURL = blob => {const url = create(blob); blobs.set(url, blob); return url;};
  URL.revokeObjectURL = url => {blobs.delete(url); return revoke(url);};
  const click = HTMLAnchorElement.prototype.click;
  HTMLAnchorElement.prototype.click = function () {
    const blob = blobs.get(this.href);
    if (!this.download || !blob) return click.call(this);
    // Hold the Blob before the Synth immediately revokes its temporary URL.
    const filename = this.download;
    void blob.arrayBuffer().then(bytes => invoke('save_export', {filename, bytes: Array.from(new Uint8Array(bytes))}))
      .then(path => status(path ? `Saved ${path}` : 'Save cancelled.'))
      .catch(error => status(`Could not save: ${String(error)}`));
  };
  function confirmReload() {
    return new Promise(resolve => {
      const dialog = document.createElement('dialog');
      dialog.style.cssText = 'padding:24px;border:1px solid #92c9c3;border-radius:12px;background:#202928;color:#fff;font:16px system-ui';
      const message = document.createElement('p');
      message.textContent = 'Reload Synth? Playback and imported presets will be cleared.';
      const cancel = document.createElement('button'), confirm = document.createElement('button');
      cancel.textContent = 'Cancel'; confirm.textContent = 'Reload';
      cancel.addEventListener('click', () => dialog.close('cancel'));
      confirm.addEventListener('click', () => dialog.close('reload'));
      dialog.addEventListener('close', () => {const accepted = dialog.returnValue === 'reload'; dialog.remove(); resolve(accepted);}, {once:true});
      dialog.append(message, cancel, confirm); document.body.append(dialog); dialog.showModal(); cancel.focus();
    });
  }
  function mount() {
    const button = document.querySelector('[data-reset-offline-cache]');
    button?.addEventListener('click', async event => {
      event.preventDefault(); event.stopImmediatePropagation();
      if (button.disabled) return;
      button.disabled = true;
      try {if (await confirmReload()) await invoke('reload_window');}
      catch (error) {status(`Could not reload: ${String(error)}`);}
      finally {button.disabled = false;}
    }, {capture: true});
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount, {once: true});
  else mount();
})();
