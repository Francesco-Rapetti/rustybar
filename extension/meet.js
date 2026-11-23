// RustyBar, in a Meet tab: tells the bar about the call while it's on (see background.js), and
// presses its buttons when the bar asks. The page says when something changes, so nothing is read
// on a timer. Meet's buttons are found by what doesn't change with its language: the mic and
// camera toggles have `data-is-muted`, the others the names of their Material icons.
(() => {
  const CLICKABLE = 'button, [role="button"]';
  const icons = el => Array.from(el.querySelectorAll('i')).map(i => i.textContent.trim()).join(' ');
  const has = (el, re) => re.test(' ' + icons(el) + ' ');

  // The buttons of the call, or null outside one. Only in the bar of the controls (the closest of
  // Leave's containers with the mute toggles too): raised hands show elsewhere too, with the same
  // icon, and open the list of people.
  function controls() {
    const leave = Array.from(document.querySelectorAll(CLICKABLE)).find(b => has(b, / call_end /));
    if (!leave) return null;
    let bar = leave.parentElement;
    while (bar && !bar.querySelector('[data-is-muted]')) bar = bar.parentElement;
    bar = bar || document;
    const toggles = Array.from(bar.querySelectorAll('[data-is-muted]'));
    return {
      leave,
      mic: toggles.find(t => has(t, / mic(_off|_none)? /)) || toggles[0],
      camera: toggles.find(t => has(t, / videocam(_off)? /)) || toggles[1],
      hand: Array.from(bar.querySelectorAll(CLICKABLE)).find(b => has(b, / (back_hand|front_hand) /)),
    };
  }

  // Raised, for the hand: Meet has no state on it but its label, in the page's language. Pressed,
  // it draws the button in another style, in place of some of the classes it made it with
  // (`data-idom-class`), as the mic's while muted.
  function pressed(b) {
    if (b.getAttribute('aria-pressed') === 'true') return true;
    const made = (b.getAttribute('data-idom-class') || '').split(/\s+/).filter(Boolean);
    return made.some(c => !b.classList.contains(c));
  }

  function state() {
    const c = controls();
    if (!c) return { call: false };
    const on = t => (t ? t.getAttribute('data-is-muted') !== 'true' : null);
    const title = document.querySelector('[data-meeting-title]');
    const code = (location.pathname.match(/^\/([a-z]{3}-[a-z]{4}-[a-z]{3})/) || [])[1] || '';
    const name = title ? title.getAttribute('data-meeting-title') : document.title.replace(/^(Google )?Meet\s*[-–:]\s*/, '');
    return {
      call: true,
      mic: on(c.mic),
      camera: on(c.camera),
      hand: c.hand ? pressed(c.hand) : null,
      name: name.includes(code) ? '' : name.trim(),
      code,
    };
  }

  function press(what) {
    const c = controls();
    const t = c && c[what];
    if (!t) return;
    const b = t.matches(CLICKABLE) ? t : t.closest(CLICKABLE) || t.querySelector(CLICKABLE) || t;
    b.click();
  }

  // What the bar's `msg trigger call_dump` writes: every button with its label, icons and states,
  // to see what changed when Meet changes its page.
  function dump() {
    const out = ['copy: ' + me, 'title: ' + document.title, 'state: ' + JSON.stringify(state())];
    for (const b of document.querySelectorAll('button, [role="button"], [data-is-muted]')) {
      const a = n => (b.hasAttribute(n) ? n + '=' + b.getAttribute(n) : '');
      out.push([b.tagName.toLowerCase(), JSON.stringify(b.getAttribute('aria-label') || ''), 'icons=' + icons(b), a('data-is-muted'), a('aria-pressed'), b.querySelector('svg') ? 'svg' : ''].filter(Boolean).join(' '));
    }
    // The controls whole, with every attribute, and where raised hands show
    const c = controls();
    for (const what of ['mic', 'camera', 'hand', 'leave']) {
      const el = c && c[what];
      if (el) out.push(what + ': ' + el.outerHTML.replace(/<path[^>]*>/g, '').slice(0, 3000));
    }
    for (const i of document.querySelectorAll('i')) {
      if (!/front_hand|back_hand/.test(i.textContent) || (c && c.hand && c.hand.contains(i))) continue;
      const up = [];
      for (let el = i.parentElement, n = 0; el && n < 8; el = el.parentElement, n++) {
        up.push(el.tagName.toLowerCase() + (el.getAttribute('aria-label') ? ' "' + el.getAttribute('aria-label') + '"' : '') + Array.from(el.attributes).filter(a => a.name.startsWith('data-')).map(a => ' ' + a.name + '=' + a.value.slice(0, 40)).join(''));
      }
      out.push('elsewhere ' + i.textContent.trim() + ': ' + up.join(' < '));
    }
    return out.join('\n');
  }

  // The line to the worker, only while there's a call: it ends with it, and the worker's line
  // to the bar with the last one
  let port = null;
  let last = '';
  let stopped = false;
  // This copy: the page's mark says which one is on (see `stop`)
  const VERSION = chrome.runtime.getManifest().version;
  const me = VERSION + '-' + Math.random().toString(36).slice(2, 8);
  document.documentElement.dataset.rustybar = me;

  function connect() {
    if (stopped || !chrome.runtime?.id) return; // the extension was reloaded: this script is an orphan
    port = chrome.runtime.connect({ name: 'meet:' + VERSION });
    port.onMessage.addListener(m => {
      if (m.stop) return stop();
      if (m.press) press(m.press);
      if (m.dump) port.postMessage({ dump: dump() });
    });
    port.onDisconnect.addListener(() => {
      port = null;
      if (stopped) return;
      // The worker stopped (Chrome stops idle ones): again, if the call goes on
      if (JSON.parse(last || '{}').call) setTimeout(connect, 1000);
    });
    port.postMessage(JSON.parse(last));
  }

  function check() {
    if (stopped) return;
    // A newer copy is on (the extension loaded again over this tab)
    if (document.documentElement.dataset.rustybar !== me) return stop();
    const now = JSON.stringify(state());
    if (now === last) return;
    last = now;
    const call = JSON.parse(now).call;
    if (call && !port) connect();
    else if (port) {
      port.postMessage(JSON.parse(now));
      if (!call) {
        port.disconnect();
        port = null;
      }
    }
  }

  // At most every 200 ms, however much the page changes
  let timer = 0;
  const observer = new MutationObserver(() => {
    if (!timer) timer = setTimeout(() => { timer = 0; check(); }, 200);
  });
  observer.observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['data-is-muted', 'aria-pressed', 'aria-label'] });
  function stop() {
    stopped = true;
    observer.disconnect();
    if (port) port.disconnect();
    port = null;
  }

  check();
})();
