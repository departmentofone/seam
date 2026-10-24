import { api, onEvent, type Snapshot, type UiEvent } from './api';

export interface Toast {
  id: number;
  level: 'info' | 'ok' | 'warn' | 'error';
  title: string;
  body: string;
}

export const app = $state({
  snap: null as Snapshot | null,
  toasts: [] as Toast[],
  pairResult: null as { ok: boolean; message: string } | null,
  /// Files waiting for the user to pick a device.
  sendPaths: [] as string[],
});

let nextToast = 1;

export function toast(level: Toast['level'], title: string, body = '') {
  const id = nextToast++;
  app.toasts.push({ id, level, title, body });
  setTimeout(() => dismiss(id), level === 'error' ? 9000 : 5000);
}

export function dismiss(id: number) {
  const i = app.toasts.findIndex((t) => t.id === id);
  if (i >= 0) app.toasts.splice(i, 1);
}

function handle(ev: UiEvent) {
  switch (ev.kind) {
    case 'state': {
      const { kind, ...snap } = ev;
      app.snap = snap;
      break;
    }
    case 'pairResult':
      app.pairResult = { ok: ev.ok, message: ev.message };
      break;
    case 'notice':
      toast(ev.level, ev.title, ev.body);
      break;
    case 'cursor':
      break;
  }
}

export async function connect() {
  onEvent(handle);
  const s = await api.state();
  if (s) app.snap = s;
  app.sendPaths = await api.takePendingSend();
}

export function peerName(id: string | null): string | null {
  if (!id || !app.snap) return null;
  if (id === app.snap.me.id) return app.snap.me.name;
  return app.snap.peers.find((p) => p.id === id)?.name ?? 'another device';
}
