// Stand-in engine for previewing the interface in a plain browser.

import type { Snapshot, UiEvent } from './api';

type Listener = (ev: UiEvent) => void;
const listeners = new Set<Listener>();

const PC = '1b7f0c3e-0000-4000-8000-000000000001';
const LAPTOP = '9a2d44c1-0000-4000-8000-000000000002';
const DEN = 'c0ffee00-0000-4000-8000-000000000003';

const snap: Snapshot = {
  me: {
    id: PC,
    name: 'Studio PC',
    fingerprint: '4F2A 91C0 7DE3 18B5',
    port: 47420,
    elevated: false,
    version: '0.1.0',
    monitors: [
      { id: '\\\\.\\DISPLAY1', name: 'DELL U2723QE', rect: { x: 0, y: 0, w: 2560, h: 1440 }, scale: 1, primary: true },
      { id: '\\\\.\\DISPLAY2', name: 'LG 24MK430H', rect: { x: 2560, y: 180, w: 1920, h: 1080 }, scale: 1, primary: false },
    ],
  },
  peers: [
    {
      id: LAPTOP,
      name: 'Laptop',
      fingerprint: 'A1C9 0E44 B7F2 6D13',
      connected: true,
      addr: '192.168.1.24:47420',
      latencyMs: 1.4,
      monitors: [{ id: '\\\\.\\DISPLAY1', name: 'Built-in display', rect: { x: 0, y: 0, w: 2880, h: 1800 }, scale: 2, primary: true }],
      manualAddr: null,
    },
    {
      id: DEN,
      name: 'Living room PC',
      fingerprint: '77D0 3A1F C58E 0B92',
      connected: false,
      addr: '192.168.1.31:47420',
      latencyMs: null,
      monitors: [],
      manualAddr: null,
    },
  ],
  discovered: [{ id: 'ffff', name: 'Office laptop', addrs: ['192.168.1.40:47420'], pairing: true }],
  layout: {
    revision: 3,
    placements: [
      [PC, { x: 0, y: 0, scale: 1 }],
      [LAPTOP, { x: -1440, y: 420, scale: 0.5 }],
    ],
  },
  cursorOn: null,
  controlledBy: null,
  locked: false,
  publicNetwork: false,
  pairingCode: null,
  pairingEndsIn: null,
  crossing: { delay_ms: 0, corner_px: 12, cross_with_buttons: false, speed: 1, home_hotkey: 'Ctrl+Alt+Home', lock_key: 'ScrollLock' },
  clipboard: { text: true, images: true, files: true, drag_files: true, respect_private: true, max_image_mb: 32 },
  sharing: true,
  startAtLogin: true,
  runElevated: false,
  onboarded: new URLSearchParams(location.search).has('onboarded'),
  receiveDir: 'C:\\Users\\you\\Downloads\\Seam',
  ai: {
    phase: 'idle',
    source: null,
    sourceName: null,
    inventory: null,
    defaults: [],
    bytes: 0,
    total: 0,
    preview: null,
    report: null,
    error: null,
    claudeRunning: true,
    unlisted: 3,
    lastBackup: null,
    tools: [
      ['claude', 'Claude', true],
      ['codex', 'Codex CLI', false],
      ['gemini', 'Gemini CLI', false],
      ['cursor', 'Cursor', false],
    ],
  },
  transfers: [
    { id: 1, incoming: true, peer: 'Laptop', label: 'Holiday photos', bytes: 412_000_000, total: 1_240_000_000, done: false, error: null, path: null },
    { id: 2, incoming: false, peer: 'Laptop', label: 'report-final.pdf', bytes: 2_400_000, total: 2_400_000, done: true, error: null, path: null },
  ],
};

let countdown: number | undefined;

function emit(ev: UiEvent) {
  for (const l of listeners) l(ev);
}

function push() {
  emit({ kind: 'state', ...structuredClone(snap) });
}

export const mock = {
  listen(f: Listener) {
    listeners.add(f);
    return () => listeners.delete(f);
  },

  async call<T>(cmd: string, args: Record<string, any> = {}): Promise<T> {
    switch (cmd) {
      case 'get_state':
        return structuredClone(snap) as T;
      case 'begin_pairing':
        snap.pairingCode = String(Math.floor(Math.random() * 1e6)).padStart(6, '0');
        snap.pairingEndsIn = 180;
        clearInterval(countdown);
        countdown = window.setInterval(() => {
          if (!snap.pairingEndsIn) return clearInterval(countdown);
          snap.pairingEndsIn -= 1;
          if (snap.pairingEndsIn <= 0) snap.pairingCode = null;
          push();
        }, 1000);
        break;
      case 'end_pairing':
        snap.pairingCode = null;
        snap.pairingEndsIn = null;
        break;
      case 'pair_with':
        setTimeout(() => {
          const ok = args.code === '123456';
          emit({ kind: 'pairResult', ok, message: ok ? 'Paired with Office laptop' : "That code didn't match. Check the number on the other device and try again." });
        }, 700);
        break;
      case 'unpair':
        snap.peers = snap.peers.filter((p) => p.id !== args.id);
        snap.layout.placements = snap.layout.placements.filter(([d]) => d !== args.id);
        break;
      case 'set_layout':
        snap.layout = { ...args.layout, revision: snap.layout.revision + 1 };
        break;
      case 'update_settings': {
        const p = args.patch;
        if (p.crossing) snap.crossing = p.crossing;
        if (p.clipboard) snap.clipboard = p.clipboard;
        if (p.sharing !== undefined) snap.sharing = p.sharing;
        if (p.startAtLogin !== undefined) snap.startAtLogin = p.startAtLogin;
        if (p.runElevated !== undefined) snap.runElevated = p.runElevated;
        if (p.onboarded !== undefined) snap.onboarded = p.onboarded;
        if (p.receiveDir !== undefined) snap.receiveDir = p.receiveDir;
        if (p.deviceName) snap.me.name = p.deviceName;
        break;
      }
      case 'set_manual_addr': {
        const peer = snap.peers.find((x) => x.id === args.id);
        if (peer) peer.manualAddr = args.addr;
        break;
      }
      case 'send_files':
        emit({ kind: 'notice', level: 'ok', title: 'Sending', body: `${args.paths.length} file(s) on the way` });
        break;
      case 'clear_transfers':
        snap.transfers = snap.transfers.filter((t) => !t.done);
        break;
      case 'ai_scan':
        Object.assign(snap.ai, { phase: 'scanning', source: args.from, sourceName: 'Laptop', error: null, report: null, preview: null });
        setTimeout(() => {
          snap.ai.inventory = {
            tool: 'claude',
            machine: 'OLD-LAPTOP',
            home: 'C:\\Users\\alex',
            appdata: 'C:\\Users\\alex\\AppData\\Roaming',
            found: true,
            parts: [
              { label: 'Settings and instructions', files: 41, bytes: 2_100_000 },
              { label: 'Skills', files: 577, bytes: 20_100_000 },
              { label: 'Plugins', files: 5328, bytes: 138_700_000 },
              { label: 'Memory', files: 13, bytes: 41_000 },
              { label: 'Session history', files: 1821, bytes: 898_700_000 },
              { label: 'Claude app sessions', files: 180, bytes: 55_100_000 },
            ],
            projects: [
              { key: 'C:\\Users\\alex\\Projects\\Client work', folder: 'C--Users-alex-Projects-Client-work', cwd: 'C:\\Users\\alex\\Projects\\Client work', sessions: 17, sessionBytes: 412_000_000, lastActive: Date.now() - 86400000, filesAvailable: true, fileBytes: 1_097_000_000, fileCount: 4210, scratch: false },
              { key: 'C:\\Users\\alex\\Projects\\Budget tracker', folder: 'C--Users-alex-Projects-Budget-tracker', cwd: 'C:\\Users\\alex\\Projects\\Budget tracker', sessions: 5, sessionBytes: 96_000_000, lastActive: Date.now() - 3 * 86400000, filesAvailable: true, fileBytes: 266_000_000, fileCount: 980, scratch: false },
              { key: 'C:\\Users\\alex\\Projects\\Recipe app', folder: 'C--Users-alex-Projects-Recipe-app', cwd: 'C:\\Users\\alex\\Projects\\Recipe app', sessions: 12, sessionBytes: 180_000_000, lastActive: Date.now() - 6 * 86400000, filesAvailable: true, fileBytes: 521_000_000, fileCount: 2304, scratch: false },
              { key: 'C:\\Users\\alex\\AppData\\Roaming\\Claude\\scratch-workspaces\\6ab9\\8ee6\\scratch-2026-10-13-78a9c2', folder: 'scratch', cwd: 'scratch', sessions: 1, sessionBytes: 37_000_000, lastActive: Date.now() - 9 * 86400000, filesAvailable: false, fileBytes: 0, fileCount: 0, scratch: true },
            ],
          };
          snap.ai.defaults = snap.ai.inventory.projects.map((p) => [p.key, p.key.replace('alex', 'sam')] as [string, string]);
          snap.ai.phase = 'choose';
          push();
        }, 700);
        break;
      case 'ai_request': {
        snap.ai.phase = 'preparing';
        snap.ai.total = 1_480_000_000;
        snap.ai.bytes = 0;
        setTimeout(() => {
          snap.ai.phase = 'downloading';
          const t = window.setInterval(() => {
            snap.ai.bytes = Math.min(snap.ai.total, snap.ai.bytes + 160_000_000);
            if (snap.ai.bytes >= snap.ai.total) {
              clearInterval(t);
              snap.ai.phase = 'ready';
              snap.ai.preview = { added: 8446, updated: 3, merged: 2, keptLocal: 1, unchanged: 40, conflicts: 1, skipped: 0, projectFiles: 7494, projectFilesKept: 0, sessionsRegistered: 6, notes: [], backup: null };
            }
            push();
          }, 300);
        }, 600);
        break;
      }
      case 'ai_apply':
        if (snap.ai.claudeRunning) {
          emit({ kind: 'notice', level: 'warn', title: 'Claude is still open', body: 'Quit the Claude app (also from the tray), then press Move again.' });
          break;
        }
        snap.ai.phase = 'applying';
        setTimeout(() => {
          snap.ai.phase = 'done';
          snap.ai.report = { ...snap.ai.preview!, backup: 'C:\\Users\\you\\AppData\\Roaming\\Seam\\ai-backups\\20261024-1900', notes: ['Kept this PC\'s copy of claude\\home\\CLAUDE.md; the other version is in the backup\'s conflicts folder', 'Repaired git worktrees in C:\\Users\\sam\\Projects\\website'] };
          snap.ai.lastBackup = snap.ai.report.backup;
          push();
        }, 1200);
        break;
      case 'ai_refresh':
        snap.ai.claudeRunning = !snap.ai.claudeRunning;
        break;
      case 'ai_reset':
        Object.assign(snap.ai, { phase: 'idle', inventory: null, preview: null, report: null, error: null });
        break;
      case 'ai_undo':
        snap.ai.lastBackup = null;
        emit({ kind: 'notice', level: 'ok', title: 'Undone', body: '8,446 files were put back the way they were.' });
        break;
      case 'ai_register_local':
        snap.ai.unlisted = 0;
        emit({ kind: 'notice', level: 'ok', title: 'Sessions added', body: '3 sessions now show in the Claude app.' });
        break;
      case 'take_pending_send':
        return [] as T;
      case 'identify_monitors':
        emit({ kind: 'notice', level: 'info', title: 'Identify', body: 'Numbers show on each screen of this PC.' });
        break;
      default:
        break;
    }
    queueMicrotask(push);
    return undefined as T;
  },
};
