<script lang="ts">
  import { onMount } from 'svelte';
  import { api, onFileDrop, onSendRequest } from './lib/api';
  import { app, connect, peerName } from './lib/state.svelte';
  import Icon from './components/Icon.svelte';
  import Toggle from './components/Toggle.svelte';
  import Toasts from './components/Toasts.svelte';
  import Logo from './components/Logo.svelte';
  import Devices from './pages/Devices.svelte';
  import Displays from './pages/Displays.svelte';
  import Files from './pages/Files.svelte';
  import Workspace from './pages/Workspace.svelte';
  import Settings from './pages/Settings.svelte';
  import Welcome from './pages/Welcome.svelte';
  import SendDialog from './components/SendDialog.svelte';

  type Page = 'devices' | 'displays' | 'files' | 'workspace' | 'settings';
  let page: Page = $state('devices');
  let dropping = $state(false);

  const nav: { id: Page; label: string; icon: string }[] = [
    { id: 'devices', label: 'Devices', icon: 'devices' },
    { id: 'displays', label: 'Displays', icon: 'displays' },
    { id: 'files', label: 'Clipboard & files', icon: 'clipboard' },
    { id: 'workspace', label: 'AI workspace', icon: 'spark' },
    { id: 'settings', label: 'Settings', icon: 'settings' },
  ];

  onMount(() => {
    connect();
    const stopSend = onSendRequest((paths) => (app.sendPaths = paths));
    let stopDrop: (() => void) | undefined;
    onFileDrop(
      (paths) => (app.sendPaths = paths),
      (on) => (dropping = on),
    ).then((s) => (stopDrop = s));
    return () => {
      stopSend();
      stopDrop?.();
    };
  });

  const s = $derived(app.snap);
  const where = $derived.by(() => {
    if (!s) return '';
    if (s.controlledBy) return `${peerName(s.controlledBy)} is using this PC`;
    if (s.cursorOn) return `Cursor on ${peerName(s.cursorOn)}`;
    return 'Cursor on this PC';
  });
  const online = $derived(s ? s.peers.filter((p) => p.connected).length : 0);
</script>

{#if s && !s.onboarded}
  <Welcome />
{:else if s}
  <div class="shell">
    <aside>
      <div class="brand">
        <Logo size={30} />
        <span class="word">Seam</span>
      </div>

      <nav>
        {#each nav as item}
          <button class="nav" class:active={page === item.id} onclick={() => (page = item.id)} aria-current={page === item.id ? 'page' : undefined}>
            <Icon name={item.icon} size={18} />
            <span>{item.label}</span>
          </button>
        {/each}
      </nav>

      <div class="status card">
        <div class="where">
          <span class="pulse" class:away={!!s.cursorOn || !!s.controlledBy} class:paused={!s.sharing}></span>
          <span>{s.sharing ? where : 'Sharing paused'}</span>
        </div>
        <div class="faint small">{online === 0 ? 'No devices connected' : `${online} device${online === 1 ? '' : 's'} connected`}</div>
        <div class="share">
          <span>Share mouse & keyboard</span>
          <Toggle label="Share mouse and keyboard" checked={s.sharing} onchange={(v) => api.updateSettings({ sharing: v })} />
        </div>
        {#if s.cursorOn}
          <button class="btn small" onclick={() => api.goHome()}><Icon name="home" size={16} /> Bring cursor home</button>
        {/if}
      </div>
    </aside>

    <main>
      {#if page === 'devices'}
        <Devices />
      {:else if page === 'displays'}
        <Displays />
      {:else if page === 'files'}
        <Files />
      {:else if page === 'workspace'}
        <Workspace />
      {:else}
        <Settings />
      {/if}
    </main>
  </div>

  {#if dropping}
    <div class="drop">
      <div class="drop-card"><Icon name="send" size={28} /><span>Drop to send to another device</span></div>
    </div>
  {/if}
  <SendDialog />
{:else}
  <div class="loading"><Logo size={64} /></div>
{/if}

<Toasts />

<style>
  .shell {
    display: grid;
    grid-template-columns: 236px 1fr;
    height: 100vh;
  }
  aside {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 20px 14px 14px;
    border-right: 1px solid var(--line);
    background: var(--paper-deep);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 8px;
  }
  .word {
    font: 750 22px var(--display);
    letter-spacing: -0.03em;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 38px;
    padding: 0 10px;
    border: none;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--muted);
    font: 500 13.5px var(--body);
    text-align: left;
    cursor: pointer;
    transition: background-color 150ms var(--ease), color 150ms var(--ease), transform 150ms var(--ease);
  }
  .nav:active {
    transform: scale(0.98);
  }
  .nav.active {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }
  @media (hover: hover) and (pointer: fine) {
    .nav:not(.active):hover {
      background: color-mix(in srgb, var(--surface) 55%, transparent);
      color: var(--ink);
    }
  }
  .status {
    margin-top: auto;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .where {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 550;
    font-size: 13px;
  }
  .small {
    font-size: 12px;
  }
  .share {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    font-size: 12.5px;
    padding-top: 8px;
    border-top: 1px solid var(--line);
  }
  .pulse {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--ok);
    box-shadow: 0 0 0 3px var(--ok-soft);
    flex: none;
  }
  .pulse.away {
    background: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .pulse.paused {
    background: var(--faint);
    box-shadow: 0 0 0 3px var(--paper-deep);
  }
  main {
    overflow-y: auto;
    padding: 32px 40px 48px;
  }
  .drop {
    position: fixed;
    inset: 0;
    background: rgb(23 23 26 / 25%);
    backdrop-filter: blur(3px);
    display: grid;
    place-items: center;
    z-index: 40;
    animation: fade 150ms var(--ease);
  }
  .drop-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 36px 48px;
    border-radius: var(--r-lg);
    border: 2px dashed var(--accent);
    background: var(--surface);
    color: var(--accent);
    font: 600 15px var(--body);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  .loading {
    height: 100vh;
    display: grid;
    place-items: center;
  }
</style>
