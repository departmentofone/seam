<script lang="ts">
  import { api, type Crossing, type LayoutDoc, type Placement } from '../lib/api';
  import { app } from '../lib/state.svelte';
  import { bounds, defaultScale, devRects, links, overlaps, resolve, snap, type Dev, type R } from '../lib/geometry';
  import Icon from '../components/Icon.svelte';
  import Toggle from '../components/Toggle.svelte';

  const s = $derived(app.snap!);

  // Local copy of the layout while editing; replaced whenever the engine
  // sends a newer revision.
  let local: LayoutDoc = $state({ revision: -1, placements: [] });
  $effect(() => {
    if (s.layout.revision !== local.revision && !drag) local = structuredClone($state.snapshot(s.layout));
  });

  const placeOf = (id: string): Placement | undefined => local.placements.find(([d]) => d === id)?.[1];

  const devs: Dev[] = $derived.by(() => {
    const out: Dev[] = [];
    const me = placeOf(s.me.id);
    if (me && s.me.monitors.length) out.push({ id: s.me.id, name: s.me.name, isMe: true, monitors: s.me.monitors, place: me });
    for (const p of s.peers) {
      const pl = placeOf(p.id);
      if (pl && p.connected && p.monitors.length) out.push({ id: p.id, name: p.name, isMe: false, monitors: p.monitors, place: pl });
    }
    return out;
  });
  const waiting = $derived(s.peers.filter((p) => !p.connected || !p.monitors.length));

  let selected: string | null = $state(null);
  const sel = $derived(devs.find((d) => d.id === selected) ?? null);

  // View transform, frozen while dragging so the canvas does not rescale
  // under the pointer.
  let cw = $state(800);
  let ch = $state(380);
  let frozen: { k: number; tx: number; ty: number } | null = $state(null);
  const view = $derived.by(() => {
    if (frozen) return frozen;
    const all = devs.flatMap((d) => devRects(d));
    if (!all.length) return { k: 1, tx: 0, ty: 0 };
    const b = bounds(all);
    const pad = 56;
    const k = Math.min((cw - pad * 2) / b.w, (ch - pad * 2) / b.h, 0.5);
    return { k, tx: (cw - b.w * k) / 2 - b.x * k, ty: (ch - b.h * k) / 2 - b.y * k };
  });
  const sx = (x: number) => view.tx + x * view.k;
  const sy = (y: number) => view.ty + y * view.k;

  const palette = ['var(--accent)', 'var(--sky)', 'var(--grape)', 'var(--ok)', 'var(--warn)'];
  const colorOf = (i: number) => palette[i % palette.length];

  let drag: { id: string; px: number; py: number; start: Placement; others: R[] } | null = $state(null);
  let bad = $state(false);

  function down(e: PointerEvent, d: Dev) {
    selected = d.id;
    frozen = { ...view };
    const others = devs.filter((o) => o.id !== d.id).flatMap((o) => devRects(o));
    drag = { id: d.id, px: e.clientX, py: e.clientY, start: { ...d.place }, others };
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
  }

  function move(e: PointerEvent) {
    if (!drag) return;
    const d = devs.find((x) => x.id === drag!.id);
    if (!d) return;
    const raw = {
      ...drag.start,
      x: drag.start.x + (e.clientX - drag.px) / view.k,
      y: drag.start.y + (e.clientY - drag.py) / view.k,
    };
    const p = e.altKey ? raw : snap(d, raw, drag.others, 14 / view.k);
    bad = overlaps(d, p, drag.others);
    setPlace(d.id, p);
  }

  function up() {
    if (!drag) return;
    const d = devs.find((x) => x.id === drag!.id);
    if (d) setPlace(d.id, resolve(d, d.place, drag.others));
    drag = null;
    frozen = null;
    bad = false;
    commit();
  }

  function setPlace(id: string, p: Placement) {
    const slot = local.placements.find(([d]) => d === id);
    if (slot) slot[1] = { x: Math.round(p.x), y: Math.round(p.y), scale: p.scale };
  }

  function commit() {
    api.setLayout($state.snapshot(local) as LayoutDoc);
  }

  function nudge(e: KeyboardEvent, d: Dev) {
    const step = e.shiftKey ? 1 : 20;
    const dir: Record<string, [number, number]> = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
    const v = dir[e.key];
    if (!v) return;
    e.preventDefault();
    setPlace(d.id, { ...d.place, x: d.place.x + v[0] * step, y: d.place.y + v[1] * step });
    commit();
  }

  function setSize(d: Dev, pct: number) {
    const others = devs.filter((o) => o.id !== d.id).flatMap((o) => devRects(o));
    const p = { ...d.place, scale: defaultScale(d) * (pct / 100) };
    setPlace(d.id, resolve(d, p, others));
  }

  function arrange() {
    const me = devs.find((d) => d.isMe);
    if (!me) return;
    let right = bounds(devRects(me)).x + bounds(devRects(me)).w;
    const mb = bounds(devRects(me));
    for (const d of devs.filter((x) => !x.isMe)) {
      const scale = defaultScale(d);
      const b = bounds(devRects(d, { ...d.place, scale }));
      const p = { x: right, y: mb.y + (mb.h - b.h) / 2, scale };
      setPlace(d.id, p);
      right += b.w;
    }
    commit();
  }

  const conn = $derived(links(devs));
  const loose = $derived(devs.length > 1 ? devs.filter((d) => !conn.some((l) => l.a === d.id || l.b === d.id)) : []);

  // Crossing settings ------------------------------------------------------
  function setCrossing(patch: Partial<Crossing>) {
    api.updateSettings({ crossing: { ...$state.snapshot(s.crossing), ...patch } });
  }
  let delay = $state(0);
  let corner = $state(0);
  let speed = $state(1);
  $effect(() => {
    delay = s.crossing.delay_ms;
    corner = s.crossing.corner_px;
    speed = s.crossing.speed;
  });
  const sizePct = $derived(sel ? Math.round((sel.place.scale / defaultScale(sel)) * 100) : 100);
</script>

<header class="page">
  <div>
    <h1>Displays</h1>
    <p class="muted">Arrange the screens the way they sit on your desk. The cursor crosses wherever two devices touch.</p>
  </div>
  <div class="row">
    <button class="btn" onclick={() => api.identify()}><Icon name="eye" size={17} /> Identify screens</button>
    {#if devs.length > 1}<button class="btn ghost" onclick={arrange}>Auto-arrange</button>{/if}
  </div>
</header>

<div class="canvas card" bind:clientWidth={cw} bind:clientHeight={ch}>
  <svg width={cw} height={ch} role="application" aria-label="Screen layout" onpointermove={move} onpointerup={up} onpointercancel={up}>
    <defs>
      <pattern id="dots" width="18" height="18" patternUnits="userSpaceOnUse">
        <circle cx="1" cy="1" r="1" fill="var(--line-strong)" />
      </pattern>
    </defs>
    <rect width={cw} height={ch} fill="url(#dots)" />

    {#each devs as d, i (d.id)}
      {@const rects = devRects(d)}
      {@const b = bounds(rects)}
      {@const c = colorOf(i)}
      <g
        class="dev"
        class:dragging={drag?.id === d.id}
        class:selected={selected === d.id}
        class:bad={drag?.id === d.id && bad}
        style:--c={c}
        tabindex="0"
        role="button"
        aria-label="{d.name}, drag to position. Arrow keys nudge."
        onpointerdown={(e) => down(e, d)}
        onkeydown={(e) => nudge(e, d)}
        onfocus={() => (selected = d.id)}
      >
        <text class="devname" x={sx(b.x)} y={sy(b.y) - 10}>{d.name}{d.isMe ? ' · this PC' : ''}{s.cursorOn === d.id || (d.isMe && !s.cursorOn && !s.controlledBy) ? '  ●' : ''}</text>
        {#each d.monitors as m, mi}
          {@const r = rects[mi]}
          {@const w = r.w * view.k}
          {@const h = r.h * view.k}
          <rect class="mon" x={sx(r.x) + 2} y={sy(r.y) + 2} width={Math.max(0, w - 4)} height={Math.max(0, h - 4)} rx="8" />
          {#if d.isMe && w > 40 && h > 40}<text class="num" x={sx(r.x) + 14} y={sy(r.y) + 30}>{mi + 1}</text>{/if}
          {#if w > 90 && h > 50}
            <text class="mname" x={sx(r.x) + w / 2} y={sy(r.y) + h / 2 - 2} text-anchor="middle">{m.name}</text>
          {/if}
          {#if w > 140 && h > 50}
            <text class="mres" x={sx(r.x) + w / 2} y={sy(r.y) + h / 2 + 15} text-anchor="middle">{m.rect.w} × {m.rect.h} · {Math.round(m.scale * 100)}%</text>
          {/if}
        {/each}
      </g>
    {/each}

    {#each conn as l}
      <line class="link" x1={sx(l.x1)} y1={sy(l.y1)} x2={sx(l.x2)} y2={sy(l.y2)} />
    {/each}
  </svg>

  {#if devs.length < 2}
    <div class="hint">
      <Icon name="link" size={18} />
      <span>{s.peers.length ? 'Your other devices show up here once they connect.' : 'Pair another device to place its screens here.'}</span>
    </div>
  {:else}
    <div class="legend">
      <span><span class="swatch"></span> Cursor crosses here</span>
      <span class="faint">Drag to move · hold Alt to skip snapping</span>
    </div>
  {/if}
</div>

{#if loose.length}
  <div class="banner warn" style="margin-top:12px">
    <Icon name="warn" size={18} />
    <span>{loose.map((d) => d.name).join(' and ')} {loose.length === 1 ? "doesn't" : "don't"} touch any other device, so the cursor can't reach {loose.length === 1 ? 'it' : 'them'}. Drag it against another screen's edge.</span>
  </div>
{/if}
{#if waiting.length}
  <p class="faint fine waiting">{waiting.map((p) => p.name).join(', ')} {waiting.length === 1 ? 'is' : 'are'} offline. {waiting.length === 1 ? 'Its' : 'Their'} screens appear here when {waiting.length === 1 ? 'it connects' : 'they connect'}.</p>
{/if}

{#if sel && devs.length > 1}
  <div class="card sizecard">
    <div class="setting">
      <div class="label">
        <span>Size match for {sel.name}</span>
        <span>If the cursor lands higher or lower than you expect after crossing, change this until the screens on the map match their real sizes.</span>
      </div>
      <div class="control slider">
        <input type="range" min="50" max="200" step="5" value={sizePct} oninput={(e) => setSize(sel, Number((e.target as HTMLInputElement).value))} onchange={commit} aria-label="Size match" />
        <span class="mono val">{sizePct}%</span>
      </div>
    </div>
  </div>
{/if}

<div class="section-title">Crossing</div>
<div class="card">
  <div class="setting">
    <div class="label">
      <span>Wait at the edge</span>
      <span>Hold the cursor against the edge this long before it crosses. Useful if you often reach for scrollbars at the edge.</span>
    </div>
    <div class="control slider">
      <input type="range" min="0" max="500" step="25" bind:value={delay} onchange={() => setCrossing({ delay_ms: delay })} aria-label="Edge delay" />
      <span class="mono val">{delay === 0 ? 'Off' : `${delay} ms`}</span>
    </div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Corner dead zone</span>
      <span>Corners never cross, so Start, close buttons and hot corners stay easy to hit.</span>
    </div>
    <div class="control slider">
      <input type="range" min="0" max="80" step="4" bind:value={corner} onchange={() => setCrossing({ corner_px: corner })} aria-label="Corner dead zone" />
      <span class="mono val">{corner === 0 ? 'Off' : `${corner} px`}</span>
    </div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Pointer speed on other devices</span>
      <span>Seam already matches speed across different screen sizes. Fine-tune it here.</span>
    </div>
    <div class="control slider">
      <input type="range" min="0.5" max="2" step="0.05" bind:value={speed} onchange={() => setCrossing({ speed })} aria-label="Pointer speed" />
      <span class="mono val">{speed.toFixed(2)}×</span>
    </div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Cross while holding a mouse button</span>
      <span>Off keeps window drags and text selection on one screen. Dragging files always crosses.</span>
    </div>
    <div class="control">
      <Toggle label="Cross while holding a mouse button" checked={s.crossing.cross_with_buttons} onchange={(v) => setCrossing({ cross_with_buttons: v })} />
    </div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Bring the cursor home</span>
      <span>Press this anywhere to snap the cursor back to this PC.</span>
    </div>
    <div class="control">
      <select value={s.crossing.home_hotkey} onchange={(e) => setCrossing({ home_hotkey: (e.target as HTMLSelectElement).value })} aria-label="Home hotkey">
        <option value="Ctrl+Alt+Home">Ctrl + Alt + Home</option>
        <option value="Ctrl+Alt+End">Ctrl + Alt + End</option>
        <option value="Ctrl+Alt+F12">Ctrl + Alt + F12</option>
        <option value="Ctrl+Shift+Home">Ctrl + Shift + Home</option>
        <option value="None">Off</option>
      </select>
    </div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Lock the cursor to one device</span>
      <span>Press once to stop crossing (handy for games), press again to allow it.</span>
    </div>
    <div class="control">
      <select value={s.crossing.lock_key} onchange={(e) => setCrossing({ lock_key: (e.target as HTMLSelectElement).value })} aria-label="Lock key">
        <option value="ScrollLock">Scroll Lock</option>
        <option value="Pause">Pause</option>
        <option value="None">Off</option>
      </select>
      {#if s.locked}<span class="pill warn"><Icon name="lock" size={13} /> Locked</span>{/if}
    </div>
  </div>
</div>

<style>
  .page {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 24px;
    margin-bottom: 20px;
  }
  .page p {
    margin-top: 6px;
    max-width: 520px;
  }
  .canvas {
    position: relative;
    height: 380px;
    overflow: hidden;
    background: var(--surface-2);
  }
  svg {
    display: block;
    touch-action: none;
  }
  .dev {
    cursor: grab;
    outline: none;
  }
  .dev.dragging {
    cursor: grabbing;
  }
  .mon {
    fill: color-mix(in srgb, var(--c) 14%, var(--surface));
    stroke: color-mix(in srgb, var(--c) 55%, var(--line-strong));
    stroke-width: 1.5;
    transition: fill 160ms var(--ease), stroke 160ms var(--ease);
  }
  .dev.selected .mon,
  .dev:focus-visible .mon {
    stroke: var(--c);
    stroke-width: 2.5;
  }
  .dev.dragging .mon {
    fill: color-mix(in srgb, var(--c) 22%, var(--surface));
  }
  .dev.bad .mon {
    stroke: var(--error);
    fill: var(--error-soft);
  }
  .devname {
    font: 600 12.5px var(--body);
    fill: var(--ink);
  }
  .num {
    font: 750 20px var(--display);
    fill: var(--c);
  }
  .mname {
    font: 550 12px var(--body);
    fill: var(--ink);
  }
  .mres {
    font: 400 11px var(--mono);
    fill: var(--muted);
  }
  .link {
    stroke: var(--accent);
    stroke-width: 5;
    stroke-linecap: round;
    pointer-events: none;
  }
  .hint {
    position: absolute;
    left: 50%;
    bottom: 18px;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 999px;
    background: var(--surface);
    border: 1px solid var(--line);
    color: var(--muted);
    font-size: 13px;
    white-space: nowrap;
  }
  .legend {
    position: absolute;
    left: 14px;
    bottom: 12px;
    right: 14px;
    display: flex;
    justify-content: space-between;
    font-size: 12px;
    color: var(--muted);
    pointer-events: none;
  }
  .swatch {
    display: inline-block;
    width: 14px;
    height: 4px;
    border-radius: 2px;
    background: var(--accent);
    vertical-align: middle;
    margin-right: 4px;
  }
  .waiting {
    margin: 10px 4px 0;
  }
  .sizecard {
    margin-top: 12px;
  }
  .slider input {
    width: 180px;
  }
  .val {
    width: 64px;
    text-align: right;
    color: var(--muted);
  }
</style>
