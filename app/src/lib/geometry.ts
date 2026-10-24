// Layout math for the Displays editor. Mirrors seam-core's layout.rs so what
// the editor shows as connected is what the cursor actually does.

import type { Monitor, Placement } from './api';

export interface R { x: number; y: number; w: number; h: number }

export interface Dev {
  id: string;
  name: string;
  isMe: boolean;
  monitors: Monitor[];
  place: Placement;
}

/** Same tolerances as layout.rs. */
export const EDGE_GAP = 48;
export const EDGE_OVERLAP = 8;

export function origin(d: Dev) {
  return {
    x: Math.min(...d.monitors.map((m) => m.rect.x)),
    y: Math.min(...d.monitors.map((m) => m.rect.y)),
  };
}

export function monRect(d: Dev, m: Monitor, place = d.place): R {
  const o = origin(d);
  return {
    x: place.x + (m.rect.x - o.x) * place.scale,
    y: place.y + (m.rect.y - o.y) * place.scale,
    w: m.rect.w * place.scale,
    h: m.rect.h * place.scale,
  };
}

export function devRects(d: Dev, place = d.place): R[] {
  return d.monitors.map((m) => monRect(d, m, place));
}

export function bounds(rects: R[]): R {
  const x = Math.min(...rects.map((r) => r.x));
  const y = Math.min(...rects.map((r) => r.y));
  const r = Math.max(...rects.map((r) => r.x + r.w));
  const b = Math.max(...rects.map((r) => r.y + r.h));
  return { x, y, w: r - x, h: b - y };
}

export function defaultScale(d: Dev): number {
  const p = d.monitors.find((m) => m.primary) ?? d.monitors[0];
  return 1 / Math.max(0.5, p?.scale ?? 1);
}

export interface Link { x1: number; y1: number; x2: number; y2: number; a: string; b: string }

/** Edges where the cursor can pass from one device to another. */
export function links(devs: Dev[]): Link[] {
  const out: Link[] = [];
  for (let i = 0; i < devs.length; i++) {
    for (let j = i + 1; j < devs.length; j++) {
      for (const a of devRects(devs[i])) {
        for (const b of devRects(devs[j])) {
          const vy0 = Math.max(a.y, b.y);
          const vy1 = Math.min(a.y + a.h, b.y + b.h);
          const hx0 = Math.max(a.x, b.x);
          const hx1 = Math.min(a.x + a.w, b.x + b.w);
          const near = (gap: number) => gap >= -EDGE_OVERLAP && gap <= EDGE_GAP;
          const add = (x1: number, y1: number, x2: number, y2: number) => out.push({ x1, y1, x2, y2, a: devs[i].id, b: devs[j].id });
          if (vy1 > vy0) {
            if (near(b.x - (a.x + a.w))) add((a.x + a.w + b.x) / 2, vy0, (a.x + a.w + b.x) / 2, vy1);
            if (near(a.x - (b.x + b.w))) add((b.x + b.w + a.x) / 2, vy0, (b.x + b.w + a.x) / 2, vy1);
          }
          if (hx1 > hx0) {
            if (near(b.y - (a.y + a.h))) add(hx0, (a.y + a.h + b.y) / 2, hx1, (a.y + a.h + b.y) / 2);
            if (near(a.y - (b.y + b.h))) add(hx0, (b.y + b.h + a.y) / 2, hx1, (b.y + b.h + a.y) / 2);
          }
        }
      }
    }
  }
  return out;
}

/** Nudge `p` so the device's screens snap to nearby edges of `others`. */
export function snap(d: Dev, p: Placement, others: R[], threshold: number): Placement {
  const mine = devRects(d, p);
  let bestX: number | null = null;
  let bestY: number | null = null;
  for (const m of mine) {
    for (const o of others) {
      const xs = [o.x + o.w - m.x, o.x - (m.x + m.w), o.x - m.x, o.x + o.w - (m.x + m.w)];
      const ys = [
        o.y + o.h - m.y,
        o.y - (m.y + m.h),
        o.y - m.y,
        o.y + o.h - (m.y + m.h),
        o.y + o.h / 2 - (m.y + m.h / 2),
      ];
      for (const dx of xs) if (Math.abs(dx) < threshold && (bestX === null || Math.abs(dx) < Math.abs(bestX))) bestX = dx;
      for (const dy of ys) if (Math.abs(dy) < threshold && (bestY === null || Math.abs(dy) < Math.abs(bestY))) bestY = dy;
    }
  }
  return { ...p, x: p.x + (bestX ?? 0), y: p.y + (bestY ?? 0) };
}

function overlap(a: R, b: R) {
  const ox = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
  const oy = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
  return ox > 1 && oy > 1;
}

export function overlaps(d: Dev, p: Placement, others: R[]): boolean {
  return devRects(d, p).some((m) => others.some((o) => overlap(m, o)));
}

/**
 * Move the device to the closest spot where it overlaps nothing: flush
 * against one side of some other screen, keeping the other coordinate.
 */
export function resolve(d: Dev, p: Placement, others: R[]): Placement {
  if (!overlaps(d, p, others)) return p;
  const b = bounds(devRects(d, p));
  const cands: Placement[] = [];
  for (const o of others) {
    cands.push({ ...p, x: p.x + (o.x - (b.x + b.w)) });
    cands.push({ ...p, x: p.x + (o.x + o.w - b.x) });
    cands.push({ ...p, y: p.y + (o.y - (b.y + b.h)) });
    cands.push({ ...p, y: p.y + (o.y + o.h - b.y) });
  }
  const dist = (c: Placement) => Math.hypot(c.x - p.x, c.y - p.y);
  const free = cands.filter((c) => !overlaps(d, c, others)).sort((a, b) => dist(a) - dist(b));
  return free[0] ?? p;
}
