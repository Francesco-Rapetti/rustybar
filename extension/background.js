// RustyBar's worker: carries what the Meet tabs say (see meet.js) to the bar, over a WebSocket to
// it on this Mac, and the bar's clicks back to them. Open only while a tab is on a meeting's page;
// the bar takes only this extension (by its id, fixed by the key in the manifest).
const BAR = 'ws://127.0.0.1:47823/meet';
const tabs = new Map(); // tab id → { port, state }
let ws = null;
let wait = 1000;
let ping = 0;

function say(m) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m));
}

function open() {
  if (ws || tabs.size === 0) return;
  ws = new WebSocket(BAR);
  ws.onopen = () => {
    wait = 1000;
    say({ type: 'hello', version: chrome.runtime.getManifest().version });
    for (const [tab, t] of tabs) if (t.state) say({ type: 'call', tab, ...t.state });
    // A message every 20 s keeps the worker on (Chrome stops it after 30 s without one)
    ping = setInterval(() => say({ type: 'ping' }), 20000);
  };
  ws.onmessage = e => {
    let m;
    try { m = JSON.parse(e.data); } catch { return; }
    const t = tabs.get(m.tab);
    if (m.type === 'press' && t) t.port.postMessage({ press: m.control });
    if (m.type === 'show' && t) {
      chrome.tabs.update(m.tab, { active: true });
      chrome.tabs.get(m.tab).then(tab => chrome.windows.update(tab.windowId, { focused: true }));
    }
    if (m.type === 'dump') for (const t of tabs.values()) t.port.postMessage({ dump: true });
    // The app has a newer version of this extension: loaded again from its folder, which puts
    // the new script in the Meet tabs (see onInstalled)
    if (m.type === 'reload') chrome.runtime.reload();
    // The bar reloaded: what each tab has, again
    if (m.type === 'resend') for (const [tab, t] of tabs) if (t.state) say({ type: 'call', tab, ...t.state });
  };
  ws.onclose = () => {
    ws = null;
    clearInterval(ping);
    // The bar isn't there (yet): again, a little later each time
    if (tabs.size) setTimeout(open, wait);
    wait = Math.min(wait * 2, 10000);
  };
}

// Installed or loaded again with a call already on: the Meet tabs open get the script now, not
// at their next load (meet.js stops the copy it replaces)
chrome.runtime.onInstalled.addListener(async () => {
  for (const tab of await chrome.tabs.query({ url: 'https://meet.google.com/*' })) {
    chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ['meet.js'] }).catch(() => {});
  }
});

// Which of two versions of the script is newer: `meet:1.3` against `meet` (before versions)
const version = port => (port.name.split(':')[1] || '0').split('.').map(Number);
const newer = (a, b) => {
  for (let i = 0; i < Math.max(a.length, b.length); i++) if ((a[i] || 0) !== (b[i] || 0)) return (a[i] || 0) > (b[i] || 0);
  return false;
};

chrome.runtime.onConnect.addListener(port => {
  if (!/^meet(:|$)/.test(port.name) || !port.sender?.tab) return;
  const tab = port.sender.tab.id;
  // Two copies of the script in a tab (the extension loaded again over it): the newest version
  // is heard, the other is told to stop. One from before versions doesn't stop, and isn't
  // disconnected, or it would connect again every second: it stays, unheard.
  const before = tabs.get(tab);
  if (before && newer(version(before.port), version(port))) {
    port.postMessage({ stop: true });
    return;
  }
  if (before) before.port.postMessage({ stop: true });
  tabs.set(tab, { port, state: null });
  port.onMessage.addListener(m => {
    const t = tabs.get(tab);
    if (!t || t.port !== port) return;
    if (m.dump !== undefined) return say({ type: 'dump', tab, text: m.dump });
    t.state = m;
    say({ type: 'call', tab, ...m });
  });
  port.onDisconnect.addListener(() => {
    if (tabs.get(tab)?.port !== port) return;
    tabs.delete(tab);
    say({ type: 'call', tab, call: false });
    if (tabs.size === 0 && ws) ws.close();
  });
  open();
});
