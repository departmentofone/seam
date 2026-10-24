<script lang="ts">
  import { api, formatBytes, pickFiles, pickFolder, type Clipboard } from '../lib/api';
  import { app } from '../lib/state.svelte';
  import Icon from '../components/Icon.svelte';
  import Toggle from '../components/Toggle.svelte';

  const s = $derived(app.snap!);
  const online = $derived(s.peers.filter((p) => p.connected));

  function set(patch: Partial<Clipboard>) {
    api.updateSettings({ clipboard: { ...$state.snapshot(s.clipboard), ...patch } });
  }

  async function chooseFolder() {
    const dir = await pickFolder();
    if (dir) api.updateSettings({ receiveDir: dir });
  }

  async function send() {
    const paths = await pickFiles();
    if (paths.length) app.sendPaths = paths;
  }

  const pct = (b: number, t: number) => (t > 0 ? Math.min(100, Math.round((b / t) * 100)) : 100);
</script>

<header class="page">
  <div>
    <h1>Clipboard & files</h1>
    <p class="muted">Copy on one PC and paste on the other. Drag files across the edge, or send them from here.</p>
  </div>
  <button class="btn primary" disabled={!online.length} onclick={send}><Icon name="send" size={17} /> Send files</button>
</header>

<div class="card">
  <div class="setting">
    <div class="label"><span>Text and links</span><span>Includes formatting from apps like Word and your browser.</span></div>
    <div class="control"><Toggle label="Share text" checked={s.clipboard.text} onchange={(v) => set({ text: v })} /></div>
  </div>
  <div class="setting">
    <div class="label"><span>Images</span><span>Screenshots and copied pictures, up to {s.clipboard.max_image_mb} MB.</span></div>
    <div class="control"><Toggle label="Share images" checked={s.clipboard.images} onchange={(v) => set({ images: v })} /></div>
  </div>
  <div class="setting">
    <div class="label"><span>Copied files and folders</span><span>Copy in Explorer here, paste in Explorer there. Big files start copying right away and show Windows' usual progress.</span></div>
    <div class="control"><Toggle label="Share copied files" checked={s.clipboard.files} onchange={(v) => set({ files: v })} /></div>
  </div>
  <div class="setting">
    <div class="label"><span>Drag files across the edge</span><span>Drag a file from Explorer off the edge of the screen and drop it on the other PC.</span></div>
    <div class="control"><Toggle label="Drag files across" checked={s.clipboard.drag_files} onchange={(v) => set({ drag_files: v })} /></div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Keep passwords private</span>
      <span>Skip anything a password manager marks as private (Bitwarden, 1Password, KeePass and others do), so copied passwords stay on this PC.</span>
    </div>
    <div class="control"><Toggle label="Keep passwords private" checked={s.clipboard.respect_private} onchange={(v) => set({ respect_private: v })} /></div>
  </div>
  <div class="setting">
    <div class="label"><span>Received files go to</span><span class="mono path">{s.receiveDir}</span></div>
    <div class="control">
      <button class="btn small" onclick={() => api.reveal(s.receiveDir)}><Icon name="folder" size={15} /> Open</button>
      <button class="btn small ghost" onclick={chooseFolder}>Change</button>
    </div>
  </div>
</div>

<div class="section-title row between">
  <span>Transfers</span>
  {#if s.transfers.some((t) => t.done)}<button class="btn ghost small" onclick={() => api.clearTransfers()}>Clear finished</button>{/if}
</div>

{#if s.transfers.length === 0}
  <div class="card empty">
    <Icon name="arrowDown" size={22} />
    <p class="muted">
      Nothing yet. Files you send or receive show up here.
      {#if s.me.elevated}
        Seam is running as administrator, and Windows doesn't allow dropping files onto an administrator app's window, so use Send files or right-click a file and choose Send to.
      {:else}
        You can also drop files anywhere on this window to send them.
      {/if}
    </p>
  </div>
{:else}
  <div class="card list">
    {#each [...s.transfers].reverse() as t (t.id)}
      <div class="tr">
        <span class="dir" class:in={t.incoming}><Icon name={t.incoming ? 'arrowDown' : 'arrowUp'} size={16} /></span>
        <div class="tmain">
          <div class="row between">
            <span class="tname">{t.label}</span>
            <span class="faint fine">
              {#if t.error}Failed{:else if t.done}{formatBytes(t.total)}{:else}{formatBytes(t.bytes)} of {formatBytes(t.total)}{/if}
            </span>
          </div>
          <div class="faint fine">{t.incoming ? 'From' : 'To'} {t.peer}{t.error ? ` · ${t.error}` : ''}</div>
          {#if !t.done}
            <div class="bar"><span style:width="{pct(t.bytes, t.total)}%"></span></div>
          {/if}
        </div>
        {#if t.done && t.path}
          <button class="btn small ghost" onclick={() => api.reveal(t.path!)}>Show</button>
        {/if}
      </div>
    {/each}
  </div>
{/if}

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
  .path {
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .between {
    justify-content: space-between;
  }
  .section-title.row {
    display: flex;
    align-items: center;
  }
  .empty {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 18px 20px;
    color: var(--faint);
  }
  .list {
    padding: 4px 0;
  }
  .tr {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 12px 18px;
    border-top: 1px solid var(--line);
  }
  .tr:first-child {
    border-top: none;
  }
  .dir {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 10px;
    background: var(--paper-deep);
    color: var(--muted);
    flex: none;
  }
  .dir.in {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .tmain {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .tname {
    font-weight: 550;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bar {
    margin-top: 6px;
    height: 4px;
    border-radius: 99px;
    background: var(--paper-deep);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 300ms var(--ease);
  }
</style>
