<script lang="ts">
  import { api, pickFiles, type Peer } from '../lib/api';
  import { app } from '../lib/state.svelte';
  import Icon from '../components/Icon.svelte';
  import Modal from '../components/Modal.svelte';
  import PairDialog from '../components/PairDialog.svelte';

  let pairing = $state(false);
  let editing = $state(false);
  let name = $state('');
  let addrFor: Peer | null = $state(null);
  let addrValue = $state('');
  let removing: Peer | null = $state(null);

  const s = $derived(app.snap!);
  const others = $derived(s.discovered.filter((d) => !d.pairing));

  function startEdit() {
    name = s.me.name;
    editing = true;
  }

  function saveName(e: Event) {
    e.preventDefault();
    if (name.trim()) api.updateSettings({ deviceName: name.trim() });
    editing = false;
  }

  async function sendTo(p: Peer) {
    const paths = await pickFiles();
    if (paths.length) app.sendPaths = paths;
  }

  function openAddr(p: Peer) {
    addrFor = p;
    addrValue = p.manualAddr ?? '';
  }

  function saveAddr(e: Event) {
    e.preventDefault();
    if (addrFor) api.setManualAddr(addrFor.id, addrValue.trim() || null);
    addrFor = null;
  }

  function status(p: Peer) {
    if (!p.connected) return 'Offline';
    return p.latencyMs != null ? `Connected · ${p.latencyMs < 1 ? '<1' : p.latencyMs.toFixed(1)} ms` : 'Connected';
  }
</script>

<header class="page">
  <div>
    <h1>Devices</h1>
    <p class="muted">Pair your PCs once. After that they find each other on your network by themselves.</p>
  </div>
  <button class="btn primary" onclick={() => (pairing = true)}><Icon name="plus" size={18} /> Pair a device</button>
</header>

{#if s.publicNetwork}
  <div class="banner warn">
    <Icon name="wifi" size={18} />
    <div>
      <strong>This network is set to Public</strong>
      <span>Windows Firewall blocks Seam on public networks, so your devices can't reach each other. If this is your home or office network, open Windows Settings, go to Network & internet, and set it to Private.</span>
    </div>
  </div>
{/if}

<section class="me card">
  <div class="me-tile" aria-hidden="true"><Icon name="devices" size={26} /></div>
  <div class="me-main">
    <div class="row">
      {#if editing}
        <form class="row" onsubmit={saveName}>
          <!-- svelte-ignore a11y_autofocus -->
          <input type="text" bind:value={name} maxlength="40" autofocus onblur={saveName} />
        </form>
      {:else}
        <h2>{s.me.name}</h2>
        <button class="btn ghost small" onclick={startEdit} aria-label="Rename this PC"><Icon name="edit" size={15} /></button>
      {/if}
      <span class="pill accent">This PC</span>
      {#if s.me.elevated}<span class="pill"><Icon name="shield" size={13} /> Administrator</span>{/if}
    </div>
    <div class="meta">
      <span><span class="faint">Security code</span> <span class="mono">{s.me.fingerprint}</span></span>
      <span><span class="faint">Port</span> <span class="mono">UDP {s.me.port}</span></span>
      <span><span class="faint">Screens</span> {s.me.monitors.length}</span>
    </div>
  </div>
</section>

<div class="section-title">Paired devices</div>

{#if s.peers.length === 0}
  <div class="empty card">
    <div class="pair-art" aria-hidden="true">
      <span class="scr a"></span><span class="seam"></span><span class="scr b"></span>
    </div>
    <h2>No devices yet</h2>
    <p class="muted">Install Seam on your other PC, open it, then press <b>Pair a device</b> on either one.</p>
    <button class="btn primary" onclick={() => (pairing = true)}><Icon name="plus" size={18} /> Pair a device</button>
  </div>
{:else}
  <div class="list">
    {#each s.peers as p (p.id)}
      <article class="peer card" class:offline={!p.connected}>
        <div class="peer-tile" aria-hidden="true"><Icon name="devices" size={22} /></div>
        <div class="peer-main">
          <div class="row">
            <h3>{p.name}</h3>
            <span class="pill" class:ok={p.connected}><span class="dot"></span>{status(p)}</span>
            {#if s.cursorOn === p.id}<span class="pill accent"><Icon name="cursor" size={13} /> Cursor here</span>{/if}
            {#if s.controlledBy === p.id}<span class="pill accent">Using this PC</span>{/if}
          </div>
          <div class="meta">
            {#if p.addr}<span class="mono">{p.addr}</span>{/if}
            <span><span class="faint">Security code</span> <span class="mono">{p.fingerprint}</span></span>
            {#if p.manualAddr}<span class="faint">Fixed address set</span>{/if}
          </div>
        </div>
        <div class="peer-actions">
          <button class="btn small" disabled={!p.connected} onclick={() => sendTo(p)}><Icon name="send" size={15} /> Send files</button>
          <button class="btn ghost small" onclick={() => openAddr(p)}>Address</button>
          <button class="btn ghost small danger" onclick={() => (removing = p)}>Unpair</button>
        </div>
      </article>
    {/each}
  </div>
{/if}

{#if others.length}
  <div class="section-title">Also on this network</div>
  <div class="list">
    {#each others as d (d.id)}
      <div class="nearby card">
        <Icon name="wifi" size={18} />
        <span class="grow">{d.name}</span>
        <span class="faint fine">Not paired</span>
        <button class="btn small" onclick={() => (pairing = true)}>Pair</button>
      </div>
    {/each}
  </div>
{/if}

<PairDialog bind:open={pairing} />

<Modal open={!!addrFor} title="Fixed address" width={440} onclose={() => (addrFor = null)}>
  <form class="stack" onsubmit={saveAddr}>
    <p class="muted">Seam finds {addrFor?.name} automatically. If your network blocks that (some routers and VPNs do), type its address here. You'll find it on that PC's Devices page.</p>
    <input type="text" placeholder="192.168.1.20 or 192.168.1.20:47420" bind:value={addrValue} spellcheck="false" />
    <div class="actions">
      <button type="button" class="btn ghost" onclick={() => (addrFor = null)}>Cancel</button>
      <button type="submit" class="btn primary">Save</button>
    </div>
  </form>
</Modal>

<Modal open={!!removing} title="Unpair {removing?.name}?" width={420} onclose={() => (removing = null)}>
  <div class="stack">
    <p class="muted">They stop sharing a mouse, keyboard and clipboard right away. To use them together again you'll need to pair with a new code.</p>
    <div class="actions">
      <button class="btn ghost" onclick={() => (removing = null)}>Keep</button>
      <button
        class="btn primary"
        onclick={() => {
          if (removing) api.unpair(removing.id);
          removing = null;
        }}>Unpair</button
      >
    </div>
  </div>
</Modal>

<style>
  .page {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 24px;
    margin-bottom: 24px;
  }
  .page p {
    margin-top: 6px;
  }
  .banner {
    margin-bottom: 16px;
  }
  .banner span {
    color: var(--muted);
  }
  .me {
    display: flex;
    gap: 16px;
    align-items: center;
    padding: 18px 20px;
  }
  .me-tile,
  .peer-tile {
    flex: none;
    display: grid;
    place-items: center;
    width: 52px;
    height: 52px;
    border-radius: 14px;
    background: var(--accent);
    color: var(--accent-ink);
  }
  .peer-tile {
    width: 44px;
    height: 44px;
    border-radius: 12px;
    background: var(--paper-deep);
    color: var(--ink);
  }
  .me-main,
  .peer-main {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    flex: 1;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
    font-size: 12.5px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .peer {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 14px 16px;
    transition: opacity 200ms var(--ease);
  }
  .peer.offline .peer-main {
    opacity: 0.7;
  }
  .peer-actions {
    display: flex;
    gap: 4px;
    flex: none;
  }
  .nearby {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 12px 10px 16px;
    color: var(--muted);
  }
  .grow {
    flex: 1;
    color: var(--ink);
    font-weight: 500;
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 36px 24px 30px;
    text-align: center;
  }
  .empty p {
    max-width: 380px;
    margin-bottom: 8px;
  }
  .pair-art {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
  }
  .scr {
    width: 64px;
    height: 44px;
    border-radius: 9px;
    border: 2px solid var(--ink);
    background: var(--paper);
  }
  .scr.b {
    width: 52px;
    height: 36px;
  }
  .seam {
    width: 10px;
    height: 10px;
    border-radius: 3px;
    background: var(--accent);
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
</style>
