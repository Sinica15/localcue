// Small WebSocket client with automatic reconnection, shared by the desktop
// window and the phone page.
//
//   const link = connect({ url, onMessage, onStatus });
//   link.send({ t: 'ping' });   // false if not connected (state is resynced on reconnect)
//   link.close();
//
// - `url` is a string or a (possibly async) function, called before every attempt.
// - Reconnects with a short, capped back-off, immediately when the network comes
//   back or the page becomes visible again.
// - A heartbeat detects half-dead connections (e.g. Wi-Fi dropped without a close).
// - onStatus receives 'connecting' | 'open' | 'closed'.

const PING_EVERY_MS = 4000;
const DEAD_AFTER_MS = 11000;

export function connect({ url, onMessage, onStatus }) {
  let ws = null;
  let stopped = false;
  let retryTimer = null;
  let heartbeat = null;
  let attempt = 0;
  let lastHeard = 0;

  const status = (s) => onStatus && onStatus(s);

  async function open() {
    clearTimeout(retryTimer);
    if (stopped || (ws && ws.readyState <= WebSocket.OPEN)) return;
    status('connecting');
    let target;
    try {
      target = typeof url === 'function' ? await url() : url;
    } catch {
      return scheduleRetry();
    }
    if (stopped) return;
    const sock = new WebSocket(target);
    ws = sock;
    sock.onopen = () => {
      attempt = 0;
      lastHeard = Date.now();
      status('open');
      clearInterval(heartbeat);
      heartbeat = setInterval(() => {
        if (Date.now() - lastHeard > DEAD_AFTER_MS) sock.close();
        else send({ t: 'ping' });
      }, PING_EVERY_MS);
    };
    sock.onmessage = (ev) => {
      lastHeard = Date.now();
      let msg;
      try { msg = JSON.parse(ev.data); } catch { return; }
      if (msg.t !== 'pong') onMessage(msg);
    };
    sock.onclose = () => {
      if (ws !== sock) return;
      clearInterval(heartbeat);
      ws = null;
      status('closed');
      scheduleRetry();
    };
    sock.onerror = () => { /* onclose follows */ };
  }

  function scheduleRetry() {
    if (stopped) return;
    const delay = Math.min(4000, 400 * 1.6 ** attempt++) * (0.8 + Math.random() * 0.4);
    clearTimeout(retryTimer);
    retryTimer = setTimeout(open, delay);
  }

  function send(obj) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify(obj));
    return true;
  }

  const wake = () => { if (!ws) { attempt = 0; open(); } };
  window.addEventListener('online', wake);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') wake();
  });

  open();

  return {
    send,
    close() {
      stopped = true;
      clearTimeout(retryTimer);
      clearInterval(heartbeat);
      if (ws) ws.close();
    },
  };
}
