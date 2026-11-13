import "./style.css";
import { api, type FileInfo, type ServerInfo } from "./api";
import { connect, type Link, type LinkStatus } from "../shared/wsclient.js";
import { createTextMapper, READING_LINE } from "../shared/textpos.js";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

interface Settings { fontSize: number; marginPct: number; flipped: boolean; brightness: number; rotation: number }

let settings: Settings = { fontSize: 36, marginPct: 15, flipped: true, brightness: 25, rotation: 0 };
let link: Link;

const scriptEl = $<HTMLTextAreaElement>("script");
const mirror = $("mirror");
const readingLineEl = $("reading-line");
const fontSlider = $<HTMLInputElement>("font-slider");
const marginSlider = $<HTMLInputElement>("margin-slider");
const brightSlider = $<HTMLInputElement>("bright-slider");
const speedSlider = $<HTMLInputElement>("speed-slider");
const flipBtn = $("flip-btn");
const playStatus = $("play-status");
const scrollDisplay = $("scroll-display");

// ---------------------------------------------------------------- toast / confirm

let toastTimer: number | undefined;
function toast(msg: string, isError = false) {
  const el = $("toast");
  el.textContent = msg;
  el.className = isError ? "error" : "";
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), isError ? 6000 : 2200);
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

// An in-page confirm: window.confirm is not reliably available in every webview.
function confirmDialog(message: string, okLabel = "OK"): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = $("confirm-overlay");
    $("confirm-text").textContent = message;
    $("confirm-ok").textContent = okLabel;
    overlay.hidden = false;
    $("confirm-ok").focus();
    const done = (v: boolean) => {
      overlay.hidden = true;
      $("confirm-ok").onclick = $("confirm-cancel").onclick = null;
      resolve(v);
    };
    $("confirm-ok").onclick = () => done(true);
    $("confirm-cancel").onclick = () => done(false);
  });
}

// ---------------------------------------------------------------- shared position & reading line
// pos is a (fractional) character offset: the text on the reading line, on every
// device. Each device maps it to pixels with its own line wrapping (textpos.js).
// A textarea can't measure character positions, so we measure a hidden mirror
// div that has the same text, font and wrap width.
const deskLineHeight = () => parseFloat(getComputedStyle(scriptEl).lineHeight);
const mapper = createTextMapper(mirror, deskLineHeight);
let pos = 0;

const readingY = () => scriptEl.clientHeight * READING_LINE;
const padTop = () => parseFloat(scriptEl.style.paddingTop) || 0;

function layoutEditor() {
  const cs = getComputedStyle(scriptEl);
  const lh = deskLineHeight();
  // Padding lets the first line reach the reading line (top) and the last line too (bottom).
  scriptEl.style.paddingTop = readingY() - lh / 2 + "px";
  scriptEl.style.paddingBottom = scriptEl.clientHeight - readingY() + "px";
  mirror.style.width = scriptEl.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight) + "px";
  for (const prop of ["fontFamily", "fontSize", "fontWeight", "lineHeight", "letterSpacing", "wordSpacing", "tabSize"] as const) {
    mirror.style[prop] = cs[prop];
  }
  readingLineEl.style.top = readingY() + lh / 2 + "px";
}

const setMirrorText = () => { mirror.textContent = scriptEl.value; };

function updateScrollDisplay() {
  const n = scriptEl.value.length;
  scrollDisplay.textContent = "position: " + (n ? Math.round((pos / n) * 100) : 0) + "%";
}

// Outgoing positions are limited to ~30/s (the newest value always goes out), so fast
// wheel scrolling cannot flood the phones.
let lastPosSent = 0;
let posTimer: number | undefined;
function emitPos() {
  const wait = 33 - (performance.now() - lastPosSent);
  const send = () => {
    posTimer = undefined;
    lastPosSent = performance.now();
    link.send({ t: "position", pos });
  };
  if (wait <= 0) send();
  else if (posTimer === undefined) posTimer = window.setTimeout(send, wait);
}

function setPos(p: number, emit: boolean) {
  pos = p;
  updateScrollDisplay();
  if (emit) emitPos();
}

const posFromEditor = () => mapper.yToPos(scriptEl.scrollTop + readingY() - padTop());

// Scroll the editor so pos is on its reading line. Remembers the resulting
// scrollTop so the scroll event this causes isn't mistaken for a user scroll.
// (If scrollTop doesn't change there's no scroll event, so nothing to expect.)
let expectedScrollTop: number | null = null;
function scrollEditorTo(p: number) {
  const before = scriptEl.scrollTop;
  scriptEl.scrollTop = padTop() + mapper.posToY(p) - readingY();
  expectedScrollTop = scriptEl.scrollTop !== before ? scriptEl.scrollTop : null;
}

// Any user scroll of the editor (wheel, scrollbar, keyboard, caret) moves the phones.
// The editor's scroll position is driven from two sides: the phones (we follow them)
// and the user. Telling those apart from scroll events alone is unreliable (webviews
// round and re-fire them), and a mistake bounces the position back to the phones as a
// jump — so a scroll event only counts as the user's own right after real input.
// Phone-driven scrolling never produces input, so it can never loop back.
let lastUserInput = 0;
for (const ev of ["wheel", "pointerdown", "pointermove", "keydown"]) {
  document.addEventListener(
    ev,
    (e) => {
      if (ev === "pointermove" && !(e as PointerEvent).buttons) return; // dragging a scrollbar
      // In play mode the arrow keys control the prompter, not the editor's scroll.
      if (ev === "keydown" && playMode) return;
      lastUserInput = performance.now();
    },
    { capture: true, passive: true },
  );
}

scriptEl.addEventListener("scroll", () => {
  if (performance.now() - lastUserInput > 400) return;
  if (expectedScrollTop !== null && Math.abs(scriptEl.scrollTop - expectedScrollTop) < 2) return;
  expectedScrollTop = null;
  setPos(posFromEditor(), true);
});

// Re-layout whenever the editor changes size — the control bar wraps onto more or
// fewer rows as the window is resized.
new ResizeObserver(() => {
  layoutEditor();
  scrollEditorTo(pos);
}).observe(scriptEl);

// Wheel: scrolls the editor (and so the phones) from anywhere on the page.
// Speed scales with horizontal mouse position, for fine positioning:
//   left 25% of window → 1.0 (normal),  right edge → 0.2 (5× finer)
let mouseX = window.innerWidth / 2;
document.addEventListener("mousemove", (e) => { mouseX = e.clientX; });
let wheelRemainder = 0;
document.addEventListener(
  "wheel",
  (e) => {
    if (e.ctrlKey || anyOverlayOpen()) return; // pinch-zoom / dialogs
    e.preventDefault();
    let dy = e.deltaY;
    if (e.deltaMode === 1) dy *= 32;
    if (e.deltaMode === 2) dy *= window.innerHeight;
    const leftZone = 0.25 * window.innerWidth;
    const scale = mouseX <= leftZone ? 1 : 1 - (0.8 * (mouseX - leftZone)) / (window.innerWidth - leftZone);
    wheelRemainder += dy * scale;
    const step = Math.trunc(wheelRemainder); // keep sub-pixel amounts for touchpads
    wheelRemainder -= step;
    scriptEl.scrollTop += step;
  },
  { passive: false },
);

// Click on a line (without dragging a selection) → put that line on the reading line.
scriptEl.addEventListener("click", () => {
  if (scriptEl.selectionStart !== scriptEl.selectionEnd) return;
  const p = mapper.lineStart(scriptEl.selectionStart);
  setPos(p, true);
  scrollEditorTo(p);
});

// ---------------------------------------------------------------- play mode
// The phones run the auto-scroll themselves; we send commands and follow the
// position reports of one of them. Speed is px/s on the phone.
let playMode = false;
let playing = false;
let playSpeed = 30;

const sendPlay = (action: string, extra: Record<string, unknown> = {}) =>
  link.send({ t: "play_control", action, speed: playSpeed, ...extra });

function setPlaying(on: boolean) {
  playing = on;
  playStatus.textContent = on ? "▶ playing" : "⏸ paused";
  playStatus.className = on ? "playing" : "paused";
}

function setSpeed(v: number) {
  playSpeed = Math.max(1, Math.min(100, v));
  speedSlider.value = String(playSpeed);
  $("speed-display").textContent = String(playSpeed);
}

const HINTS = {
  edit: "Type to edit the script · click a line to put it on the reading line · mouse wheel scrolls · Play switches to presenting",
  play: "Paused until you press Space · Space start/pause · ↓ ↑ move the text one line · ← → speed · Esc or Edit: pause and go back to editing",
};

function setPlayMode(on: boolean) {
  playMode = on;
  $("mode-edit").classList.toggle("active", !on);
  $("mode-play").classList.toggle("active", on);
  $("editor").classList.toggle("present", on);
  $("mode-bar").className = on ? "play" : "edit";
  $("mode-title").textContent = on ? "PLAY MODE" : "EDIT MODE";
  $("mode-hint").textContent = on ? HINTS.play : HINTS.edit;
  // Read-only in play mode so arrow keys drive the prompter rather than the caret.
  scriptEl.readOnly = on;
  if (on) scriptEl.blur();
  if (!on) { playStatus.textContent = ""; playStatus.className = ""; }
}

// Play: enter play mode, paused. Space starts the scrolling.
function enterPlay() {
  if (playMode) return;
  setPlayMode(true);
  setPlaying(false);
  sendPlay("pause");
}

// Edit: pause the phones and go back to editing (arrow keys move the caret again).
function enterEdit() {
  if (!playMode) return;
  setPlayMode(false);
  setPlaying(false);
  sendPlay("pause");
  scriptEl.focus();
}

$("mode-play").addEventListener("click", () => { enterPlay(); $("mode-play").blur(); });
$("mode-edit").addEventListener("click", () => { enterEdit(); $("mode-edit").blur(); });
setPlayMode(false);

speedSlider.addEventListener("input", () => {
  setSpeed(parseInt(speedSlider.value));
  sendPlay("speed");
});

// ---------------------------------------------------------------- settings
function showSettings() {
  fontSlider.value = String(settings.fontSize);
  $("font-display").textContent = settings.fontSize + "px";
  marginSlider.value = String(settings.marginPct);
  $("margin-display").textContent = settings.marginPct + "%";
  brightSlider.value = String(settings.brightness);
  $("bright-display").textContent = settings.brightness + "%";
  flipBtn.classList.toggle("active", settings.flipped);
  $("rotate-btn").textContent = `⟳ Rotate ${settings.rotation}°`;
  $("rotate-btn").classList.toggle("active", settings.rotation !== 0);
}

const emitSettings = () => link.send({ t: "settings_update", ...settings });

fontSlider.addEventListener("input", () => { settings.fontSize = parseInt(fontSlider.value); showSettings(); emitSettings(); });
marginSlider.addEventListener("input", () => { settings.marginPct = parseInt(marginSlider.value); showSettings(); emitSettings(); });
brightSlider.addEventListener("input", () => { settings.brightness = parseInt(brightSlider.value); showSettings(); emitSettings(); });
$("rotate-btn").addEventListener("click", () => {
  settings.rotation = (settings.rotation + 90) % 360;
  showSettings();
  emitSettings();
  $("rotate-btn").blur();
});
flipBtn.addEventListener("click", () => { settings.flipped = !settings.flipped; showSettings(); emitSettings(); flipBtn.blur(); });

// ---------------------------------------------------------------- script text & files
let fileName: string | null = null;
let savedText = "";
let debounceTimer: number | undefined;
let textUnsynced = false; // local edits the server has not acknowledged (yet)

function updateFileStatus() {
  const el = $("file-status");
  const dirty = fileName ? scriptEl.value !== savedText : scriptEl.value.length > 0;
  el.textContent = (dirty ? "● " : "") + (fileName ?? "Untitled");
  el.title = dirty ? "Unsaved changes (Ctrl/Cmd+S to save)" : "Saved";
  el.className = dirty ? "dirty" : "";
  $("reload-btn").hidden = !fileName;
}

function setScript(text: string) {
  scriptEl.value = text;
  setMirrorText();
  updateFileStatus();
  setPos(Math.min(pos, text.length), false);
  scrollEditorTo(pos);
}

function applyFile(f: FileInfo) {
  fileName = f.name;
  savedText = f.savedText;
  updateFileStatus();
}

function sendText() {
  clearTimeout(debounceTimer);
  debounceTimer = undefined;
  textUnsynced = !link.send({ t: "text_update", text: scriptEl.value });
  setPos(posFromEditor(), true);
}

// Text sync (debounced). The editor is the reference for position while editing.
scriptEl.addEventListener("input", () => {
  setMirrorText();
  updateFileStatus();
  textUnsynced = true;
  clearTimeout(debounceTimer);
  debounceTimer = window.setTimeout(sendText, 300);
});

async function openFile() {
  if (scriptEl.value && (!fileName || scriptEl.value !== savedText)) {
    if (!(await confirmDialog("Replace the current script with another file? Unsaved edits will be lost.", "Open file…"))) return;
  }
  try {
    const f = await api.openScript();
    if (!f) return; // dialog cancelled
    applyFile(f);
    setScript(f.savedText); // the server also broadcasts it to every client (see "text" below)
    toast("Opened " + f.name);
  } catch (e) {
    toast(errText(e), true);
  }
}

async function saveFile(saveAs: boolean) {
  if (debounceTimer !== undefined) sendText(); // keep the server copy current
  try {
    const f = await api.saveScript(scriptEl.value, saveAs);
    if (!f) return;
    applyFile(f);
    toast("Saved " + f.name);
  } catch (e) {
    toast(errText(e), true);
  }
}

async function reloadFile() {
  if (scriptEl.value !== savedText && !(await confirmDialog(`Discard your unsaved edits and reload ${fileName} from disk?`, "Reload"))) return;
  try {
    const f = await api.reloadScript();
    applyFile(f);
    setScript(f.savedText);
    toast("Reloaded " + f.name);
  } catch (e) {
    toast(errText(e), true);
  }
}

$("open-btn").addEventListener("click", () => { $("open-btn").blur(); void openFile(); });
$("save-btn").addEventListener("click", () => { $("save-btn").blur(); void saveFile(false); });
$("reload-btn").addEventListener("click", () => { $("reload-btn").blur(); void reloadFile(); });

// ---------------------------------------------------------------- connection to the built-in server
let info: ServerInfo | null = null;

function setConn(s: LinkStatus) {
  const el = $("conn");
  el.className = s;
  el.textContent = s === "open" ? "synced" : s === "connecting" ? "connecting…" : "disconnected, retrying…";
}

function setMobileCount(n: number) {
  const el = $("devices");
  el.textContent = n === 0 ? "no phones" : n === 1 ? "1 phone connected" : `${n} phones connected`;
  el.className = n > 0 ? "some" : "";
}

function onMessage(m: any) {
  switch (m.t) {
    case "init": {
      // Full state on every (re)connect. Local edits the server has not seen win.
      settings = { rotation: 0, ...m.settings };
      showSettings();
      setSpeed(m.speed);
      setMobileCount(m.mobile);
      if (textUnsynced) {
        sendText();
        emitSettings();
      } else {
        setScript(m.text);
        setPos(m.pos, false);
        scrollEditorTo(m.pos);
      }
      if (m.playing) setPlayMode(true); // phones are scrolling (e.g. after a restart): take control
      if (playMode) setPlaying(m.playing);
      break;
    }
    case "clients": setMobileCount(m.mobile); break;
    // Positions from a phone while playing, or after it paused / skipped.
    case "position": setPos(m.pos, false); scrollEditorTo(m.pos); break;
    // Text from elsewhere: opening or reloading a file.
    case "text": setScript(m.text); break;
    case "settings": settings = { rotation: 0, ...m }; showSettings(); break;
    case "play_control": setSpeed(m.speed); break; // speed as stored on the server
    case "play_state": if (playMode) setPlaying(m.playing); break;
  }
}

function startLink() {
  link = connect({
    url: async () => {
      const i = await api.serverInfo();
      applyInfo(i);
      if (!i.desktopWs) throw new Error(i.error ?? "server not running");
      return i.desktopWs;
    },
    onMessage,
    onStatus: setConn,
  });
}

// ---------------------------------------------------------------- QR dialog & server info
const overlay = $("qr-overlay");
const anyOverlayOpen = () => !overlay.hidden || !$("confirm-overlay").hidden;

function applyInfo(i: ServerInfo) {
  info = i;
  $("banner").hidden = !i.error;
  $("banner-text").textContent = i.error ?? "";
  $("qr-btn").classList.toggle("active", !!i.warning && !i.mobileUrl);
  renderQr();
}

function renderQr() {
  const i = info;
  if (!i) return;
  const img = $<HTMLImageElement>("qr-img");
  if (i.qrSvg) {
    img.src = "data:image/svg+xml;utf8," + encodeURIComponent(i.qrSvg);
    img.hidden = false;
  } else {
    img.hidden = true;
  }
  $("qr-url").textContent = i.mobileUrl ?? "";
  const note = $("qr-note");
  note.textContent = i.warning ?? "";
  note.hidden = !i.warning;

  const sel = $<HTMLSelectElement>("iface-select");
  sel.replaceChildren();
  sel.add(new Option("Automatic" + (i.selectedInterface ? "" : i.ip ? ` (${i.ip})` : ""), ""));
  for (const f of i.interfaces) {
    sel.add(new Option(`${f.name} — ${f.ip}${f.virtualLink ? " (virtual)" : ""}`, f.ip));
  }
  sel.value = i.selectedInterface ?? "";
  $("iface-row").hidden = i.interfaces.length === 0;
}

function showQr(show: boolean) {
  overlay.hidden = !show;
  if (show) void api.serverInfo().then(applyInfo).catch(() => {});
}

$("qr-btn").addEventListener("click", () => { $("qr-btn").blur(); showQr(true); });
$("qr-close").addEventListener("click", () => showQr(false));
overlay.addEventListener("click", (e) => { if (e.target === overlay) showQr(false); });

async function copyLink() {
  const url = info?.mobileUrl;
  if (!url) return;
  try {
    await navigator.clipboard.writeText(url);
    toast("Link copied");
  } catch {
    const r = document.createRange();
    r.selectNodeContents($("qr-url"));
    getSelection()?.removeAllRanges();
    getSelection()?.addRange(r);
  }
}
$("qr-copy").addEventListener("click", () => void copyLink());
$("qr-url").addEventListener("click", () => void copyLink());

$<HTMLSelectElement>("iface-select").addEventListener("change", async (e) => {
  try {
    applyInfo(await api.setInterface((e.target as HTMLSelectElement).value || null));
  } catch (err) {
    toast(errText(err), true);
  }
});

$("qr-rotate").addEventListener("click", async () => {
  if (!(await confirmDialog("Create a new access key? Phones that are connected will be disconnected and must scan the new QR code.", "New key"))) return;
  try {
    applyInfo(await api.rotateToken());
    toast("New access key created");
  } catch (e) {
    toast(errText(e), true);
  }
});

$("banner-retry").addEventListener("click", async () => {
  try { applyInfo(await api.restartServer()); } catch (e) { toast(errText(e), true); }
});

void api.onServerInfo(applyInfo);

// ---------------------------------------------------------------- keyboard
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!$("confirm-overlay").hidden) $("confirm-cancel").click();
    else if (!overlay.hidden) showQr(false);
    else enterEdit();
    return;
  }
  if (!$("confirm-overlay").hidden) return;

  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === "s") { e.preventDefault(); void saveFile(e.shiftKey); return; }
  if (mod && e.key.toLowerCase() === "o") { e.preventDefault(); void openFile(); return; }

  // Arrow keys only act in play mode (the editor is read-only then, so it's safe
  // to take them even when it has focus — e.g. after click-to-seek).
  if (!playMode || mod || !overlay.hidden) return;

  switch (e.key) {
    case "ArrowLeft": e.preventDefault(); setSpeed(playSpeed - 5); sendPlay("speed"); break;
    case "ArrowRight": e.preventDefault(); setSpeed(playSpeed + 5); sendPlay("speed"); break;
    // Up/down only move the text by one line; they never start or stop playback.
    case "ArrowDown": e.preventDefault(); sendPlay("skip", { lines: 1 }); break;
    case "ArrowUp": e.preventDefault(); sendPlay("skip", { lines: -1 }); break;
    case " ":
      e.preventDefault();
      if (e.repeat) break;
      setPlaying(!playing);
      sendPlay(playing ? "play" : "pause");
      break;
  }
});

// ---------------------------------------------------------------- start
window.addEventListener("error", (e) => {
  // "ResizeObserver loop…" is a harmless browser notice about layout in the same frame.
  if (e.message.startsWith("ResizeObserver loop")) return;
  toast("Error: " + e.message, true);
});
window.addEventListener("unhandledrejection", (e) => toast("Error: " + errText(e.reason), true));
layoutEditor();
updateScrollDisplay();
showSettings();
setSpeed(playSpeed);
updateFileStatus();
startLink();
void api.fileInfo().then(applyFile).catch(() => {});
