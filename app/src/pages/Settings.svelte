<script lang="ts">
  import { api, openUrl } from '../lib/api';
  import { app, toast } from '../lib/state.svelte';
  import Icon from '../components/Icon.svelte';
  import Toggle from '../components/Toggle.svelte';

  const s = $derived(app.snap!);

  async function setLogin(v: boolean) {
    try {
      await api.updateSettings({ startAtLogin: v });
    } catch (e) {
      toast('error', 'Could not change startup', String(e));
    }
  }

  async function setElevated(v: boolean) {
    try {
      await api.updateSettings({ runElevated: v });
      if (v && !s.me.elevated) toast('info', 'Saved', 'Seam starts as administrator from your next sign-in. Use "Restart as administrator" to switch now.');
    } catch (e) {
      toast('error', 'Not changed', String(e));
    }
  }

  async function restartAdmin() {
    try {
      await api.restartAsAdmin();
    } catch (e) {
      toast('error', 'Not restarted', String(e));
    }
  }
</script>

<header class="page">
  <h1>Settings</h1>
</header>

<div class="section-title">General</div>
<div class="card">
  <div class="setting">
    <div class="label"><span>Start Seam when I sign in</span><span>It starts quietly in the tray.</span></div>
    <div class="control"><Toggle label="Start at sign-in" checked={s.startAtLogin} onchange={setLogin} /></div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Run as administrator</span>
      <span>Windows doesn't let normal apps control windows that run as administrator, such as Task Manager, installers and some games' launchers. Turn this on to make the cursor and keyboard work in those too. Windows asks for permission once.</span>
    </div>
    <div class="control"><Toggle label="Run as administrator" checked={s.runElevated} onchange={setElevated} /></div>
  </div>
  <div class="setting">
    <div class="label">
      <span>Right now</span>
      <span>{s.me.elevated ? 'Seam is running as administrator.' : 'Seam is running without administrator rights.'}</span>
    </div>
    <div class="control">
      {#if !s.me.elevated}<button class="btn small" onclick={restartAdmin}><Icon name="shield" size={15} /> Restart as administrator</button>{/if}
    </div>
  </div>
</div>

<div class="section-title">Privacy & security</div>
<div class="card prose">
  <ul class="facts">
    <li>
      <Icon name="lock" size={18} />
      <div>Seam has no account and no servers. Your devices talk to each other directly over your local network.</div>
    </li>
    <li>
      <Icon name="shield" size={18} />
      <div>Connections are encrypted with TLS 1.3. At pairing, each device saves the other's certificate and refuses any other, so a third machine can't pretend to be one of your PCs.</div>
    </li>
    <li>
      <Icon name="keyboard" size={18} />
      <div>While the cursor is on another PC, what you type is sent to that PC only. Seam doesn't record or store keystrokes.</div>
    </li>
    <li>
      <Icon name="link" size={18} />
      <div>A pairing code lasts three minutes and stops working after five wrong tries. The code itself never crosses the network, so someone listening in can't learn it.</div>
    </li>
  </ul>
  <div class="setting">
    <div class="label"><span>This PC's security code</span><span>The other device shows the same code next to this PC's name. If they ever differ, unpair and pair again.</span></div>
    <div class="control"><span class="mono">{s.me.fingerprint}</span></div>
  </div>
</div>

<div class="section-title">Things to know</div>
<div class="card prose">
  <ul class="facts">
    <li>
      <Icon name="warn" size={18} />
      <div>Some online games' anti-cheat software objects to apps that read the mouse and keyboard. Pause sharing from the tray icon before you play, or lock the cursor with Scroll Lock.</div>
    </li>
    <li>
      <Icon name="warn" size={18} />
      <div>The Windows sign-in screen and UAC prompts run on a secure desktop that no app can reach. Use that PC's own mouse and keyboard for those.</div>
    </li>
    <li>
      <Icon name="wifi" size={18} />
      <div>The installer allows Seam through Windows Firewall on private networks only. On public Wi-Fi it stays blocked.</div>
    </li>
  </ul>
</div>

<div class="section-title">Help</div>
<div class="card">
  <div class="setting">
    <div class="label"><span>Logs</span><span>Useful if something isn't working. They never contain what you type or copy.</span></div>
    <div class="control"><button class="btn small" onclick={() => api.openLogs()}><Icon name="folder" size={15} /> Open logs</button></div>
  </div>
  <div class="setting">
    <div class="label"><span>Seam {s.me.version}</span><span>Free and open source, made by Department of One.</span></div>
    <div class="control">
      <button class="btn small ghost" onclick={() => openUrl('https://www.departmentofone.net/seam')}>Website</button>
      <button class="btn small ghost" onclick={() => api.updateSettings({ onboarded: false })}>Show welcome</button>
    </div>
  </div>
  <div class="setting">
    <div class="label"><span>Quit Seam</span><span>Closing the window keeps Seam running in the tray. This stops it completely.</span></div>
    <div class="control"><button class="btn small danger" onclick={() => api.quit()}>Quit</button></div>
  </div>
</div>

<style>
  .page {
    margin-bottom: 8px;
  }
  .facts {
    list-style: none;
    margin: 0;
    padding: 6px 18px;
  }
  .facts li {
    display: flex;
    gap: 12px;
    padding: 10px 0;
    color: var(--muted);
    font-size: 13px;
  }
  .facts li :global(svg) {
    flex: none;
    margin-top: 1px;
    color: var(--ink);
  }
  .prose .setting {
    border-top: 1px solid var(--line);
  }
</style>
