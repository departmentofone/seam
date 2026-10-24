// Types mirror seam-core's engine snapshot. Nested settings structs keep
// their Rust snake_case field names.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { mock } from './mock';

export type Id = string;

export interface Rect { x: number; y: number; w: number; h: number }
export interface Monitor { id: string; name: string; rect: Rect; scale: number; primary: boolean }
export interface Placement { x: number; y: number; scale: number }
export interface LayoutDoc { revision: number; placements: [Id, Placement][] }

export interface Crossing {
  delay_ms: number;
  corner_px: number;
  cross_with_buttons: boolean;
  speed: number;
  home_hotkey: string;
  lock_key: string;
}

export interface Clipboard {
  text: boolean;
  images: boolean;
  files: boolean;
  drag_files: boolean;
  respect_private: boolean;
  max_image_mb: number;
}

export interface Peer {
  id: Id;
  name: string;
  fingerprint: string;
  connected: boolean;
  addr: string | null;
  latencyMs: number | null;
  monitors: Monitor[];
  manualAddr: string | null;
}

export interface Found { id: Id; name: string; addrs: string[]; pairing: boolean }

export interface Transfer {
  id: number;
  incoming: boolean;
  peer: string;
  label: string;
  bytes: number;
  total: number;
  done: boolean;
  error: string | null;
  path: string | null;
}

export interface AiPart { label: string; files: number; bytes: number }
export interface AiProject {
  key: string;
  folder: string;
  cwd: string | null;
  sessions: number;
  sessionBytes: number;
  lastActive: number;
  filesAvailable: boolean;
  fileBytes: number;
  fileCount: number;
  scratch: boolean;
}
export interface AiInventory { tool: string; machine: string; home: string; appdata: string; found: boolean; parts: AiPart[]; projects: AiProject[] }
export interface AiReport {
  added: number;
  updated: number;
  merged: number;
  keptLocal: number;
  unchanged: number;
  conflicts: number;
  skipped: number;
  projectFiles: number;
  projectFilesKept: number;
  sessionsRegistered: number;
  notes: string[];
  backup: string | null;
}
export interface AiChoice { key: string; sessions: boolean; files: boolean; target: string }
export interface AiView {
  phase: 'idle' | 'scanning' | 'choose' | 'preparing' | 'downloading' | 'ready' | 'applying' | 'done' | 'error';
  source: Id | null;
  sourceName: string | null;
  inventory: AiInventory | null;
  defaults: [string, string][];
  bytes: number;
  total: number;
  preview: AiReport | null;
  report: AiReport | null;
  error: string | null;
  claudeRunning: boolean;
  unlisted: number | null;
  lastBackup: string | null;
  tools: [string, string, boolean][];
}

export interface Snapshot {
  me: { id: Id; name: string; fingerprint: string; port: number; monitors: Monitor[]; elevated: boolean; version: string };
  peers: Peer[];
  discovered: Found[];
  layout: LayoutDoc;
  cursorOn: Id | null;
  controlledBy: Id | null;
  locked: boolean;
  publicNetwork: boolean;
  pairingCode: string | null;
  pairingEndsIn: number | null;
  crossing: Crossing;
  clipboard: Clipboard;
  sharing: boolean;
  startAtLogin: boolean;
  runElevated: boolean;
  onboarded: boolean;
  receiveDir: string;
  transfers: Transfer[];
  ai: AiView;
}

export type UiEvent =
  | ({ kind: 'state' } & Snapshot)
  | { kind: 'pairResult'; ok: boolean; message: string }
  | { kind: 'notice'; level: 'info' | 'ok' | 'warn' | 'error'; title: string; body: string }
  | { kind: 'cursor'; on: string | null };

export interface SettingsPatch {
  crossing?: Crossing;
  clipboard?: Clipboard;
  sharing?: boolean;
  startAtLogin?: boolean;
  runElevated?: boolean;
  onboarded?: boolean;
  receiveDir?: string;
  deviceName?: string;
}

export const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return inTauri ? invoke<T>(cmd, args) : mock.call<T>(cmd, args);
}

export const api = {
  state: () => call<Snapshot | null>('get_state'),
  beginPairing: () => call<void>('begin_pairing'),
  endPairing: () => call<void>('end_pairing'),
  pairWith: (code: string, id?: Id, addr?: string) => call<void>('pair_with', { code, id: id ?? null, addr: addr ?? null }),
  unpair: (id: Id) => call<void>('unpair', { id }),
  setManualAddr: (id: Id, addr: string | null) => call<void>('set_manual_addr', { id, addr }),
  setLayout: (layout: LayoutDoc) => call<void>('set_layout', { layout }),
  updateSettings: (patch: SettingsPatch) => call<void>('update_settings', { patch }),
  sendFiles: (to: Id, paths: string[]) => call<void>('send_files', { to, paths }),
  takePendingSend: () => call<string[]>('take_pending_send'),
  goHome: () => call<void>('go_home'),
  clearTransfers: () => call<void>('clear_transfers'),
  reveal: (path: string) => call<void>('reveal', { path }),
  openLogs: () => call<void>('open_logs'),
  restartAsAdmin: () => call<void>('restart_as_admin'),
  identify: () => call<void>('identify_monitors'),
  aiScan: (from: Id) => call<void>('ai_scan', { from }),
  aiRequest: (projects: AiChoice[]) => call<void>('ai_request', { selection: { tool: 'claude', projects } }),
  aiApply: (registerAll: boolean) => call<void>('ai_apply', { registerAll }),
  aiUndo: () => call<void>('ai_undo'),
  aiReset: () => call<void>('ai_reset'),
  aiRegisterLocal: () => call<void>('ai_register_local'),
  aiRefresh: () => call<void>('ai_refresh'),
  quit: () => call<void>('quit'),
};

export function onEvent(f: (ev: UiEvent) => void): () => void {
  if (!inTauri) return mock.listen(f);
  let stop: (() => void) | null = null;
  listen<UiEvent>('seam', (e) => f(e.payload)).then((u) => (stop = u));
  return () => stop?.();
}

export function onSendRequest(f: (paths: string[]) => void): () => void {
  if (!inTauri) return () => {};
  let stop: (() => void) | null = null;
  listen<{ paths: string[] }>('seam-send', (e) => f(e.payload.paths)).then((u) => (stop = u));
  return () => stop?.();
}

export async function pickFiles(): Promise<string[]> {
  if (!inTauri) return ['C:\\Users\\you\\Pictures\\holiday.jpg'];
  const { open } = await import('@tauri-apps/plugin-dialog');
  const res = await open({ multiple: true, directory: false, title: 'Choose files to send' });
  if (!res) return [];
  return Array.isArray(res) ? res : [res];
}

export async function pickFolder(): Promise<string | null> {
  if (!inTauri) return null;
  const { open } = await import('@tauri-apps/plugin-dialog');
  const res = await open({ directory: true, multiple: false, title: 'Where should received files go?' });
  return typeof res === 'string' ? res : null;
}

export async function openUrl(url: string) {
  if (!inTauri) return void window.open(url, '_blank');
  const { openUrl } = await import('@tauri-apps/plugin-opener');
  await openUrl(url);
}

export async function onFileDrop(f: (paths: string[]) => void, hover: (on: boolean) => void): Promise<() => void> {
  if (!inTauri) return () => {};
  const { getCurrentWebview } = await import('@tauri-apps/api/webview');
  return getCurrentWebview().onDragDropEvent((e) => {
    const p = e.payload;
    if (p.type === 'enter' || p.type === 'over') hover(true);
    else if (p.type === 'leave') hover(false);
    else if (p.type === 'drop') {
      hover(false);
      if (p.paths.length) f(p.paths);
    }
  });
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}
