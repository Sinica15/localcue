// LocalCue phone teleprompter. Served by the desktop app; talks to it over a WebSocket.
import { createTextMapper, READING_LINE } from './textpos.js';
import { connect } from './wsclient.js';
import { startVideoWake, videoWakeActive } from './nosleep.js';

const stage = document.getElementById('stage');
const display = document.getElementById('display');
const readingLine = document.getElementById('reading-line');
const hint = document.getElementById('hint');
const connIndicator = document.getElementById('conn-indicator');

let currentFontSize = 36;
const lineHeight = () => currentFontSize * 1.7;
// The stage may be rotated (see layoutStage), so viewport rects are converted back
// into the stage's own vertical axis before measuring text.
function toStage(r) {
  switch (rotation) {
    case 90: return { top: window.innerWidth - r.right, height: r.width };
    case 270: return { top: r.left, height: r.width };
    case 180: return { top: window.innerHeight - r.bottom, height: r.height };
    default: return { top: r.top, height: r.height };
  }
}
const mapper = createTextMapper(display, lineHeight, toStage);

// pos = shared script position (UTF-16 chars); y = where that is within #display (px).
// The text is placed so y sits on the reading line.
let pos = 0;
let y = 0;

let rotation = 0;
let stageH = window.innerHeight;

// Rotate the whole stage by a multiple of 90°. At 90°/270° the stage's width and
// height swap so the text wraps for the rotated screen.
function layoutStage() {
  const W = window.innerWidth, H = window.innerHeight;
  const sideways = rotation === 90 || rotation === 270;
  stage.style.background = '#000';
  stage.style.width = (sideways ? H : W) + 'px';
  stage.style.height = (sideways ? W : H) + 'px';
  stageH = sideways ? W : H;
  stage.style.transform = {
    0: 'none',
    90: `translateX(${W}px) rotate(90deg)`,
    180: `translate(${W}px, ${H}px) rotate(180deg)`,
    270: `translateY(${H}px) rotate(-90deg)`,
  }[rotation];
  placeReadingLine();
}

function readingY() { return stageH * READING_LINE; }

let flipped = true;

// Moving the text is a transform (compositor only). Changing `top` instead would make
// the browser re-lay-out the whole script every frame — and again before each text
// measurement — which is what made long scripts stutter on phones.
function render() {
  if (!Number.isFinite(y)) y = 0;  // never let a bad value freeze the display
  display.style.transform = `translate3d(0, ${readingY() - y}px, 0)${flipped ? ' scaleX(-1)' : ''}`;
}

// Rule just under the current line, like ruled paper (doesn't strike through text).
function placeReadingLine() {
  readingLine.style.top = (readingY() + lineHeight() / 2) + 'px';
}

function goToPos(p) {
  pos = p;
  glide = null;
  y = mapper.posToY(pos);
  render();
}

// ---- Smooth moves ----
// Positions from the desktop are absolute (character offsets). A small difference is
// animated over a fixed time, so slow scrolling looks smooth; a big jump (click-to-seek,
// fast wheel) lands immediately. A new target restarts the animation from where it is.
const GLIDE_MS = 160;
let glide = null;  // { from, to, start }

function glideToY(to) {
  const dist = Math.abs(to - y);
  if (dist < 0.5) { glide = null; y = to; render(); return; }
  if (dist > stageH * 1.5) { glide = null; y = to; render(); return; }
  glide = { from: y, to, start: null };
  ensureLoop();
}

function glideTo(p) {
  if (!Number.isFinite(p)) return;
  pos = p;
  glideToY(mapper.posToY(p));
}

function reportPos() {
  pos = mapper.yToPos(y);
  if (document.hidden) return;  // a hidden tab must not steer the desktop
  link.send({ t: 'position', pos });
}

// Positions can arrive far faster than a phone can lay text out (a mouse wheel on the
// desktop sends ~30/s, each needing text measurements). Only the newest one per
// animation frame is used.
let pendingPos = null;
let pendingCount = 0;
const BACKLOG = 4;  // this many positions within one frame = a backlog: just jump to the latest
function schedulePos(p) {
  const idle = pendingPos === null;
  pendingPos = p;
  pendingCount++;
  if (idle) {
    requestAnimationFrame(() => {
      const v = pendingPos;
      const backlog = pendingCount >= BACKLOG;
      pendingPos = null;
      pendingCount = 0;
      if (v === null) return;
      if (backlog) goToPos(v); else glideTo(v);
    });
  }
}

// ---- Auto-scroll: runs here on the phone (smooth, immune to network jitter). ----
// The desktop sends play/pause/skip/speed and follows our ~10 Hz position reports.
let playing = false;
let speed = 30;  // px/s
let lastFrame = null;
let lastReport = 0;
let loopOn = false;

const endY = () => mapper.posToY(mapper.length());

function ensureLoop() {
  if (loopOn) return;
  loopOn = true;
  lastFrame = null;
  requestAnimationFrame(frame);
}

// One animation loop for both the glide and auto-scroll.
function frame(t) {
  if (!playing && !glide) { loopOn = false; lastFrame = null; return; }
  const dt = lastFrame === null ? 0 : Math.min(t - lastFrame, 100) / 1000;  // cap: no jumps after a stall
  lastFrame = t;
  if (glide) {
    if (glide.start === null) glide.start = t;
    const k = Math.min(1, (t - glide.start) / GLIDE_MS);
    y = glide.from + (glide.to - glide.from) * (1 - (1 - k) ** 3);  // ease-out
    if (k >= 1) { y = glide.to; glide = null; }
    render();
  } else if (playing) {
    const yEnd = endY();
    y = Math.min(yEnd, y + speed * dt);
    render();
    if (y >= yEnd) {  // reached the end of the script
      playing = false;
      reportPos();
      link.send({ t: 'play_state', playing: false });
    } else if (t - lastReport >= 100) {
      reportPos();
      lastReport = t;
    }
  }
  requestAnimationFrame(frame);
}

function startPlaying() {
  if (playing) return;
  playing = true;
  ensureLoop();
}

function stopPlaying() { playing = false; }

function setText(text) {
  if (display.textContent !== text) display.textContent = text;
  goToPos(Math.min(pos, text.length));
}

function setSettings({ fontSize, marginPct, flipped: flip, brightness, rotation: rot = 0 }) {
  currentFontSize = fontSize;
  flipped = flip;
  rotation = rot;
  layoutStage();
  display.style.fontSize = fontSize + 'px';
  display.style.padding = `20px ${marginPct}%`;
  display.style.color = `rgba(255, 255, 255, ${brightness / 100})`;
  goToPos(pos);  // re-wrapped, so recompute where pos is
}

// ---- Messages from the desktop ----
let haveState = false;

function onMessage(m) {
  switch (m.t) {
    case 'init': {
      // The app was updated while this page stayed open: load the matching page.
      const mine = document.querySelector('meta[name=localcue-build]')?.content;
      if (m.build && mine && m.build !== mine) { location.reload(); return; }
      // Full state: on first connect and after every reconnect (Wi-Fi drop, desktop restart).
      const keepLocal = haveState && playing && m.playing;
      const localPos = keepLocal ? mapper.yToPos(y) : null;
      haveState = true;
      speed = m.speed;
      setText(m.text);
      setSettings(m.settings);
      if (keepLocal) {
        // We kept scrolling while offline; our position is newer than the server's.
        goToPos(localPos);
        reportPos();
      } else {
        goToPos(m.pos);
        m.playing ? startPlaying() : stopPlaying();
      }
      break;
    }
    case 'text': setText(m.text); break;
    case 'settings': setSettings(m); break;
    case 'position': schedulePos(m.pos); break;  // jump (and keep playing, if playing)
    case 'play_state': if (!m.playing) stopPlaying(); break;
    case 'play_control':
      speed = m.speed ?? speed;
      switch (m.action) {
        case 'play': startPlaying(); break;
        case 'pause': if (playing) { stopPlaying(); reportPos(); } break;
        case 'skip': {
          // The desktop's arrow keys skip by lines; older clients by seconds of scrolling.
          const dy = m.lines != null ? m.lines * lineHeight() : speed * m.seconds;
          if (!Number.isFinite(dy)) break;
          const to = Math.max(0, Math.min(endY(), y + dy));
          glideToY(to);
          pos = mapper.yToPos(to);
          if (!document.hidden) link.send({ t: 'position', pos });
          break;
        }
      }
      break;
  }
}

// ---- Connection ----
const token = new URLSearchParams(location.search).get('t') || '';
const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const wsUrl = `${proto}://${location.host}/ws?role=mobile&t=${encodeURIComponent(token)}`;

let failures = 0;
const link = connect({
  url: wsUrl,
  onMessage,
  onStatus(s) {
    connIndicator.className = s;
    if (s === 'open') {
      if (failures >= 3) hint.style.opacity = '0';  // the "can't reach" message
      failures = 0;
    } else if (s === 'closed' && ++failures >= 3) {
      hint.textContent = 'Can’t reach LocalCue — check Wi-Fi, or scan the QR code again';
      hint.style.opacity = '1';
    }
  },
});

// Viewport changes (fullscreen, rotation, browser chrome) move the reading line.
window.addEventListener('resize', () => { layoutStage(); goToPos(pos); });

// ---- Screen wake lock ----
// The API releases the lock whenever the page is hidden, so we re-request
// on every visibilitychange back to visible and on a 30 s heartbeat.
let wakeLock = null;
const wakeIndicator = document.getElementById('wake-indicator');

async function requestWakeLock() {
  if (!('wakeLock' in navigator)) {
    // Not available on plain http: fall back to a silent looping video (needs a tap).
    const ok = await startVideoWake();
    wakeIndicator.className = ok ? 'active' : 'inactive';
    return;
  }
  try {
    wakeLock = await navigator.wakeLock.request('screen');
    wakeIndicator.className = 'active';
    wakeLock.addEventListener('release', () => {
      wakeLock = null;
      wakeIndicator.className = 'inactive';
    });
  } catch (err) {
    console.warn('Wake lock failed:', err.name, err.message);
    wakeIndicator.className = 'inactive';
  }
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') requestWakeLock();
});
setInterval(() => {
  if (document.visibilityState !== 'visible') return;
  if ('wakeLock' in navigator ? !wakeLock : !videoWakeActive()) requestWakeLock();
}, 30_000);

// Tap anywhere to toggle fullscreen (where supported), and (re-)acquire the wake lock.
const root = document.documentElement;
document.addEventListener('click', () => {
  requestWakeLock();
  if (!root.requestFullscreen) return;  // e.g. iPhone Safari: use "Add to Home Screen"
  if (!document.fullscreenElement) root.requestFullscreen().catch(() => {});
  else document.exitFullscreen().catch(() => {});
});

if (root.requestFullscreen) {
  hint.textContent = 'tap for fullscreen';
} else {
  hint.textContent = 'for fullscreen: Share → Add to Home Screen';
}
setTimeout(() => { if (failures < 3) hint.style.opacity = '0'; }, 4000);

requestWakeLock();
