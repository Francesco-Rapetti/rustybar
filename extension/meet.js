// RustyBar, in a Meet tab: tells the bar about the call while it's on (see background.js), from
// the page before joining (`lobby`), and presses its buttons when the bar asks. The page says when
// something changes, so nothing is read on a timer. Meet's buttons are found by what doesn't
// change with its language: the mic and camera toggles have `data-is-muted`, the others the names
// of their Material icons, Join its `jsname`.
(() => {
  const CLICKABLE = 'button, [role="button"]';
  const icons = el => Array.from(el.querySelectorAll('i')).map(i => i.textContent.trim()).join(' ');
  const has = (el, re) => re.test(' ' + icons(el) + ' ');

  const CODE = /^\/([a-z]{3}-[a-z]{4}-[a-z]{3})/;
  // Join, before joining ("Join now", or "Ask to join" where someone must let you in): the button
  // in this wrapper, whose only name is its text
  const JOIN = '[jsname="Qx7uuf"]';

  // The buttons of the call, or null outside one. Only in the bar of the controls (the closest of
  // Leave's containers with the mute toggles too): raised hands show elsewhere too, with the same
  // icon, and open the list of people. Before joining (`lobby`), a meeting's page with the mic and
  // camera of the preview, and no Leave.
  function controls() {
    const leave = Array.from(document.querySelectorAll(CLICKABLE)).find(b => has(b, / call_end /));
    if (!leave) {
      const toggles = Array.from(document.querySelectorAll('[data-is-muted]'));
      if (!toggles.length || !CODE.test(location.pathname)) return null;
      const wrap = document.querySelector(JOIN);
      const join = wrap && (wrap.matches(CLICKABLE) ? wrap : wrap.querySelector(CLICKABLE));
      return {
        lobby: true,
        mic: toggles.find(t => has(t, / mic(_off|_none)? /)) || toggles[0],
        camera: toggles.find(t => has(t, / videocam(_off)? /)) || toggles[1],
        join: join && !join.disabled && join.getAttribute('aria-disabled') !== 'true' ? join : null,
      };
    }
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
    const code = (location.pathname.match(CODE) || [])[1] || '';
    const name = title ? title.getAttribute('data-meeting-title') : document.title.replace(/^(Google )?Meet\s*[-–:]\s*/, '');
    return {
      call: true,
      lobby: !!c.lobby,
      join: !!c.join,
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
      const text = b.textContent.replace(/\s+/g, ' ').trim().slice(0, 40);
      out.push([b.tagName.toLowerCase(), JSON.stringify(b.getAttribute('aria-label') || ''), 'text=' + JSON.stringify(text), 'icons=' + icons(b), a('jsname'), a('data-is-muted'), a('aria-pressed'), a('aria-disabled'), b.disabled ? 'disabled' : '', b.querySelector('svg') ? 'svg' : ''].filter(Boolean).join(' '));
    }
    // The controls whole, with every attribute, and where raised hands show
    const c = controls();
    for (const what of ['mic', 'camera', 'hand', 'leave', 'join']) {
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

  // The line to the worker, only on a meeting's page (before joining, in the call, after
  // leaving): it ends with it, and the worker's line to the bar with the last one. Not only during
  // the call, so that the bar can ask what the page has (`call_dump`) even when it isn't read as one
  const meeting = () => CODE.test(location.pathname);
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
      // The worker stopped (Chrome stops idle ones): again, if the page is still a meeting's
      if (JSON.parse(last || '{}').call || meeting()) setTimeout(connect, 1000);
    });
    port.postMessage(JSON.parse(last));
  }

  // When the call went missing right after the page before joining (see `check`)
  let held = 0;

  function check() {
    if (stopped) return;
    // A newer copy is on (the extension loaded again over this tab)
    if (document.documentElement.dataset.rustybar !== me) return stop();
    const st = state();
    // Joining: the page before joining goes before the call's controls come, and for a moment
    // there's neither. The call goes on meanwhile, for up to 10 s, or the bar would see one end
    // and another start
    if (!st.call && meeting() && JSON.parse(last || '{}').lobby) {
      held = held || Date.now();
      if (Date.now() - held < 10000) return void setTimeout(check, 500);
    }
    held = 0;
    const now = JSON.stringify(st);
    if (now === last) return;
    last = now;
    const wanted = JSON.parse(now).call || meeting();
    if (wanted && !port) connect();
    else if (port) {
      port.postMessage(JSON.parse(now));
      if (!wanted) {
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
