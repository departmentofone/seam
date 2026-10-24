<script lang="ts">
  import { api, formatBytes, pickFolder, type AiChoice, type AiProject } from '../lib/api';
  import { app } from '../lib/state.svelte';
  import Icon from '../components/Icon.svelte';
  import Toggle from '../components/Toggle.svelte';

  const s = $derived(app.snap!);
  const ai = $derived(s.ai);
  const online = $derived(s.peers.filter((p) => p.connected));

  let from: string | null = $state(null);
  $effect(() => {
    if (!from && online.length) from = online[0].id;
  });

  // What to bring, per project. Filled in when the other PC answers.
  let rows: Record<string, AiChoice> = $state({});
  let seeded = $state('');
  $effect(() => {
    const inv = ai.inventory;
    if (ai.phase !== 'choose' || !inv) return;
    const sig = `${ai.source}:${inv.projects.length}`;
    if (seeded === sig) return;
    seeded = sig;
    const defaults = new Map(ai.defaults);
    const next: Record<string, AiChoice> = {};
    for (const p of inv.projects) {
      next[p.key] = { key: p.key, sessions: true, files: p.filesAvailable, target: defaults.get(p.key) ?? '' };
    }
    rows = next;
  });

  let registerAll = $state(true);

  const projectName = (p: AiProject) => (p.scratch ? 'Claude app scratch folder' : (p.cwd ?? p.folder).split(/[\\/]/).filter(Boolean).pop() ?? p.folder);

  const total = $derived.by(() => {
    if (!ai.inventory) return 0;
    let n = ai.inventory.parts.filter((p) => p.label !== 'Session history').reduce((a, p) => a + p.bytes, 0);
    for (const p of ai.inventory.projects) {
      const r = rows[p.key];
      if (!r) continue;
      if (r.sessions) n += p.sessionBytes;
      if (r.files && p.filesAvailable) n += p.fileBytes;
    }
    return n;
  });

  const missingTarget = $derived(
    ai.inventory?.projects.some((p) => !p.scratch && rows[p.key] && (rows[p.key].sessions || rows[p.key].files) && !rows[p.key].target.trim()) ?? false,
  );

  async function browse(key: string) {
    const dir = await pickFolder();
    if (dir && rows[key]) rows[key].target = dir;
  }

  function start() {
    const choices = Object.values($state.snapshot(rows)).map((r) => ({ ...r, target: r.target.trim() }));
    api.aiRequest(choices);
  }

  const pct = (b: number, t: number) => (t > 0 ? Math.min(100, Math.round((b / t) * 100)) : 0);
  const ago = (ms: number) => {
    const d = Math.round((Date.now() - ms) / 86400000);
    return d <= 0 ? 'today' : d === 1 ? 'yesterday' : `${d} days ago`;
  };
</script>

<header class="page">
  <div>
    <h1>AI workspace</h1>
    <p class="muted">Bring your AI setup from another PC: settings, skills, memory, every past session and the project folders they worked in. Paths are fixed up for this PC, and you can undo the whole move.</p>
  </div>
</header>

<div class="tools">
  {#each ai.tools as [id, name, ready] (id)}
    <span class="pill" class:ok={ready}>{#if ready}<Icon name="check" size={13} />{/if}{name}{ready ? '' : ' · later'}</span>
  {/each}
</div>

{#if ai.phase === 'idle'}
  <section class="card block">
    <h2>Move Claude to this PC</h2>
    <p class="muted">Seam copies Claude Code's settings, instructions, skills, plugins, memory and session history, plus the Claude app's session list. Sign-in data never moves, so you'll sign in to Claude here once.</p>
    {#if online.length === 0}
      <div class="banner info"><Icon name="info" size={18} /><span>Pair the PC you're moving from and keep Seam open on it. It shows up here once it's connected.</span></div>
    {:else}
      <div class="row">
        <select bind:value={from} aria-label="Move from">
          {#each online as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
        </select>
        <button class="btn primary" disabled={!from} onclick={() => from && api.aiScan(from)}><Icon name="spark" size={17} /> See what's there</button>
      </div>
    {/if}
  </section>
{:else if ai.phase === 'scanning'}
  <section class="card block center"><span class="spinner"></span><p>Looking at Claude on {ai.sourceName}…</p></section>
{:else if ai.phase === 'choose' && ai.inventory}
  {#if !ai.inventory.found}
    <section class="card block">
      <h2>Claude isn't set up on {ai.sourceName}</h2>
      <p class="muted">There's no Claude Code folder for this user on that PC.</p>
      <div class="actions"><button class="btn" onclick={() => api.aiReset()}>Back</button></div>
    </section>
  {:else}
    <section class="card block">
      <h2>From {ai.sourceName}</h2>
      <div class="parts">
        {#each ai.inventory.parts as part (part.label)}
          <div class="part"><span>{part.label}</span><span class="faint mono">{part.files.toLocaleString()} files · {formatBytes(part.bytes)}</span></div>
        {/each}
      </div>
    </section>

    <div class="section-title">Projects</div>
    <div class="card projects">
      {#each ai.inventory.projects as p (p.key)}
        {@const r = rows[p.key]}
        {#if r}
          <div class="proj" class:off={!r.sessions && !r.files}>
            <div class="pmain">
              <div class="row tight">
                <strong>{projectName(p)}</strong>
                <span class="faint fine">{p.sessions} session{p.sessions === 1 ? '' : 's'} · last used {ago(p.lastActive)}</span>
              </div>
              <span class="mono path faint">{p.cwd ?? p.folder}</span>
              {#if !p.scratch && (r.sessions || r.files)}
                <div class="row tight dest">
                  <span class="fine faint">Goes to</span>
                  <input type="text" bind:value={r.target} spellcheck="false" aria-label="Destination for {projectName(p)}" />
                  <button class="btn small ghost" onclick={() => browse(p.key)}>Browse</button>
                </div>
              {:else if p.scratch}
                <span class="fine faint">Comes along with the Claude app's data.</span>
              {/if}
            </div>
            <div class="checks">
              <label class="check"><Toggle label="Sessions" checked={r.sessions} onchange={(v) => (r.sessions = v)} /><span>Sessions <span class="faint">{formatBytes(p.sessionBytes)}</span></span></label>
              {#if p.filesAvailable}
                <label class="check"><Toggle label="Files" checked={r.files} onchange={(v) => (r.files = v)} /><span>Files <span class="faint">{formatBytes(p.fileBytes)}</span></span></label>
              {/if}
            </div>
          </div>
        {/if}
      {/each}
    </div>
    <p class="fine faint note">Project copies skip folders you can rebuild, like node_modules and .venv. Git history comes along. Files that are newer on this PC are never replaced.</p>

    <div class="bar-actions">
      <span class="muted">About {formatBytes(total)} to copy</span>
      <div class="row">
        <button class="btn ghost" onclick={() => api.aiReset()}>Cancel</button>
        <button class="btn primary" disabled={missingTarget} onclick={start}>Copy to this PC</button>
      </div>
    </div>
  {/if}
{:else if ai.phase === 'preparing' || ai.phase === 'downloading'}
  <section class="card block">
    <h2>Copying from {ai.sourceName}</h2>
    {#if ai.phase === 'preparing'}
      <p class="muted">Gathering files on {ai.sourceName}…</p>
      <div class="progress"><span class="indeterminate"></span></div>
    {:else}
      <p class="muted">{formatBytes(ai.bytes)} of {formatBytes(ai.total)}. Nothing on this PC changes until you press Move.</p>
      <div class="progress"><span style:width="{pct(ai.bytes, ai.total)}%"></span></div>
    {/if}
    <div class="actions"><button class="btn ghost" onclick={() => api.aiReset()}>Cancel</button></div>
  </section>
{:else if ai.phase === 'ready' && ai.preview}
  {@const pv = ai.preview}
  <section class="card block">
    <h2>Ready to move</h2>
    <div class="stats">
      <div><b>{pv.added.toLocaleString()}</b><span>new files</span></div>
      <div><b>{(pv.updated + pv.merged).toLocaleString()}</b><span>updated or merged</span></div>
      <div><b>{pv.projectFiles.toLocaleString()}</b><span>project files</span></div>
      <div><b>{pv.conflicts}</b><span>kept as this PC's</span></div>
    </div>
    <p class="muted fine">Settings and lists are merged, and where both PCs set something different, this PC's choice stays. A memory that differs is kept as a second file. Everything that changes is backed up first.</p>
    <label class="check line"><Toggle label="Show every session in the Claude app" checked={registerAll} onchange={(v) => (registerAll = v)} /><span>Show every session in the Claude app's list, including ones started from the terminal</span></label>
    {#if ai.claudeRunning}
      <div class="banner warn">
        <Icon name="warn" size={18} />
        <div><strong>Quit Claude first</strong><span>Claude rewrites these files while it runs. Quit the Claude app (right-click its tray icon, then Quit) and close any claude terminals.</span></div>
        <button class="btn small" onclick={() => api.aiRefresh()}>Check again</button>
      </div>
    {/if}
    <div class="actions">
      <button class="btn ghost" onclick={() => api.aiReset()}>Cancel</button>
      <button class="btn primary" disabled={ai.claudeRunning} onclick={() => api.aiApply(registerAll)}>Move workspace</button>
    </div>
  </section>
{:else if ai.phase === 'applying'}
  <section class="card block center"><span class="spinner"></span><p>Moving the workspace into place…</p></section>
{:else if ai.phase === 'done' && ai.report}
  {@const r = ai.report}
  <section class="card block">
    <h2><Icon name="check" size={20} /> Moved</h2>
    <p class="muted">{(r.added + r.updated + r.merged).toLocaleString()} files are in place and {r.sessionsRegistered} more sessions were added to the Claude app's list. Open Claude and your history is there.</p>
    {#if r.notes.length}
      <ul class="notes">{#each r.notes as n}<li>{n}</li>{/each}</ul>
    {/if}
    <p class="fine faint">Python projects need their virtual environment again (python -m venv .venv, then pip install -r requirements.txt) and Node projects need npm install.</p>
    <div class="actions">
      {#if r.backup}<button class="btn ghost" onclick={() => api.reveal(r.backup!)}><Icon name="folder" size={15} /> Backup</button>{/if}
      {#if ai.lastBackup}<button class="btn" onclick={() => api.aiUndo()}><Icon name="undo" size={15} /> Undo the move</button>{/if}
      <button class="btn primary" onclick={() => api.aiReset()}>Done</button>
    </div>
  </section>
{:else if ai.phase === 'error'}
  <section class="card block">
    <div class="banner error"><Icon name="warn" size={18} /><span>{ai.error}</span></div>
    <div class="actions">
      {#if ai.lastBackup}<button class="btn" onclick={() => api.aiUndo()}><Icon name="undo" size={15} /> Undo changes</button>{/if}
      <button class="btn primary" onclick={() => api.aiReset()}>Start over</button>
    </div>
  </section>
{/if}

<div class="section-title">On this PC</div>
<div class="card">
  <div class="setting">
    <div class="label">
      <span>Show every session in the Claude app</span>
      <span>
        {#if ai.unlisted === null}Counting…{:else if ai.unlisted === 0}Every Claude session on this PC is already in the app's list.{:else}{ai.unlisted} session{ai.unlisted === 1 ? '' : 's'} on this PC {ai.unlisted === 1 ? "isn't" : "aren't"} in the Claude app's list, usually ones started from a terminal. Adding them lets you open them from the app.{/if}
        {#if ai.claudeRunning && ai.unlisted}Quit Claude first.{/if}
      </span>
    </div>
    <div class="control">
      <button class="btn small" disabled={!ai.unlisted || ai.claudeRunning} onclick={() => api.aiRegisterLocal()}>Add them</button>
    </div>
  </div>
  {#if ai.lastBackup && ai.phase !== 'done' && ai.phase !== 'error'}
    <div class="setting">
      <div class="label"><span>Undo the last change</span><span>Puts back every file Seam's last workspace change touched.</span></div>
      <div class="control"><button class="btn small" disabled={ai.claudeRunning} onclick={() => api.aiUndo()}><Icon name="undo" size={15} /> Undo</button></div>
    </div>
  {/if}
</div>

<style>
  .page {
    margin-bottom: 14px;
  }
  .page p {
    margin-top: 6px;
    max-width: 600px;
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 18px;
  }
  .block {
    padding: 20px 22px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .block h2 {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .center {
    align-items: center;
    text-align: center;
    padding: 36px;
  }
  .spinner {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    border: 3px solid var(--paper-deep);
    border-top-color: var(--accent);
    animation: spin 800ms linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: spin 1600ms linear infinite !important;
    }
  }
  .parts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px 24px;
  }
  .part {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 0;
    border-bottom: 1px solid var(--line);
    font-size: 13px;
  }
  .projects {
    padding: 2px 0;
  }
  .proj {
    display: flex;
    gap: 18px;
    padding: 14px 18px;
    border-top: 1px solid var(--line);
    transition: opacity 160ms var(--ease);
  }
  .proj:first-child {
    border-top: none;
  }
  .proj.off .pmain {
    opacity: 0.5;
  }
  .pmain {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .tight {
    gap: 8px;
  }
  .path {
    overflow-wrap: anywhere;
    font-size: 11.5px;
  }
  .dest {
    margin-top: 4px;
  }
  .dest input {
    flex: 1;
    height: 30px;
    font-family: var(--mono);
    font-size: 12px;
  }
  .checks {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: none;
    width: 180px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    cursor: pointer;
  }
  .check.line {
    padding: 6px 0;
  }
  .note {
    margin: 10px 4px 0;
  }
  .bar-actions {
    position: sticky;
    bottom: -48px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin-top: 16px;
    padding: 14px 0;
    background: linear-gradient(to bottom, transparent, var(--paper) 30%);
  }
  .progress {
    height: 6px;
    border-radius: 99px;
    background: var(--paper-deep);
    overflow: hidden;
    position: relative;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 300ms var(--ease);
  }
  .progress .indeterminate {
    width: 30%;
    animation: slide 1.2s ease-in-out infinite alternate;
  }
  @keyframes slide {
    from {
      transform: translateX(-10%);
    }
    to {
      transform: translateX(240%);
    }
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 10px;
  }
  .stats div {
    display: flex;
    flex-direction: column;
    padding: 12px 14px;
    border-radius: var(--r-sm);
    background: var(--paper);
  }
  .stats b {
    font: 700 22px var(--display);
  }
  .stats span {
    font-size: 12px;
    color: var(--muted);
  }
  .banner span {
    color: var(--muted);
  }
  .banner div {
    flex: 1;
  }
  .notes {
    margin: 0;
    padding-left: 18px;
    color: var(--muted);
    font-size: 12.5px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
</style>
