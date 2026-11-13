// Typed wrappers around the Tauri commands defined in src-tauri/src/lib.rs.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Iface {
  name: string;
  ip: string;
  virtualLink: boolean;
}

export interface ServerInfo {
  mobileUrl: string | null;
  qrSvg: string | null;
  ip: string | null;
  port: number | null;
  desktopWs: string | null;
  loopbackPort: number | null;
  interfaces: Iface[];
  selectedInterface: string | null;
  /** The window cannot sync with the built-in server. */
  error: string | null;
  /** Phones cannot connect (no network / no port). */
  warning: string | null;
  voiceAvailable: boolean;
}

export interface FileInfo {
  name: string | null;
  path: string | null;
  savedText: string;
}

const inTauri = "__TAURI_INTERNALS__" in window;

// `npm run dev` in a plain browser (no Tauri): talk to `cargo run --example headless`
// via ?ws=<desktop websocket url>. Lets the UI be developed without the native shell.
function devInfo(): ServerInfo {
  const ws = new URLSearchParams(location.search).get("ws");
  return {
    mobileUrl: null, qrSvg: null, ip: null, port: null, desktopWs: ws, loopbackPort: null,
    interfaces: [], selectedInterface: null,
    error: ws ? null : "Dev mode: add ?ws=<desktop websocket url> (see `cargo run --example headless`)",
    warning: "Dev mode: no QR code outside the desktop app", voiceAvailable: false,
  };
}

function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return invoke<T>(cmd, args);
  if (import.meta.env.DEV && cmd === "get_server_info") return Promise.resolve(devInfo() as T);
  if (import.meta.env.DEV && cmd === "get_file_info") {
    return Promise.resolve({ name: null, path: null, savedText: "" } as T);
  }
  return Promise.reject(new Error("This action needs the LocalCue desktop app."));
}

export const api = {
  serverInfo: () => call<ServerInfo>("get_server_info"),
  restartServer: () => call<ServerInfo>("restart_server"),
  setInterface: (ip: string | null) => call<ServerInfo>("set_interface", { ip }),
  rotateToken: () => call<ServerInfo>("rotate_token"),
  fileInfo: () => call<FileInfo>("get_file_info"),
  openScript: () => call<FileInfo | null>("open_script"),
  saveScript: (text: string, saveAs: boolean) => call<FileInfo | null>("save_script", { text, saveAs }),
  reloadScript: () => call<FileInfo>("reload_script"),
  /** Fired when the network changes and the QR code must be regenerated. */
  onServerInfo: (fn: (info: ServerInfo) => void) =>
    inTauri ? listen<ServerInfo>("server-info", (e) => fn(e.payload)) : Promise.resolve(() => {}),
};
