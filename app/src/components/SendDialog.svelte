<script lang="ts">
  import { api } from '../lib/api';
  import { app, toast } from '../lib/state.svelte';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';

  let open = $state(false);
  let to: string | null = $state(null);

  const online = $derived(app.snap ? app.snap.peers.filter((p) => p.connected) : []);
  const names = $derived(app.sendPaths.map((p) => p.split(/[\\/]/).pop() ?? p));

  $effect(() => {
    if (app.sendPaths.length) {
      open = true;
      if (online.length === 1) to = online[0].id;
    }
  });

  function close() {
    app.sendPaths = [];
    to = null;
    open = false;
  }

  function send() {
    if (!to) return;
    const peer = online.find((p) => p.id === to);
    api.sendFiles(to, app.sendPaths);
    toast('info', `Sending to ${peer?.name ?? 'device'}`, `${names.length} item${names.length === 1 ? '' : 's'}. Progress shows under Clipboard & files.`);
    close();
  }
</script>

<Modal bind:open title="Send files" width={460} onclose={close}>
  <div class="body">
    <ul class="files">
      {#each names.slice(0, 5) as n}
        <li><Icon name="file" size={16} /><span>{n}</span></li>
      {/each}
      {#if names.length > 5}<li class="faint">and {names.length - 5} more</li>{/if}
    </ul>

    {#if online.length === 0}
      <div class="banner info"><Icon name="info" size={18} /><span>None of your devices are online right now. Turn the other PC on and open Seam there.</span></div>
    {:else}
      <div class="picks">
        {#each online as p (p.id)}
          <button class="pick" class:on={to === p.id} onclick={() => (to = p.id)}>
            <Icon name="devices" size={18} />
            <span>{p.name}</span>
            {#if to === p.id}<span class="tick"><Icon name="check" size={16} /></span>{/if}
          </button>
        {/each}
      </div>
      <p class="faint fine">Files land in the Downloads\Seam folder on the other PC.</p>
    {/if}

    <div class="actions">
      <button class="btn ghost" onclick={close}>Cancel</button>
      <button class="btn primary" disabled={!to} onclick={send}><Icon name="send" size={16} /> Send</button>
    </div>
  </div>
</Modal>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 10px 12px;
    border-radius: var(--r-sm);
    background: var(--paper);
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
  }
  .files li {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .files span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
