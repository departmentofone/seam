<script lang="ts">
  import { api } from '../lib/api';
  import { app } from '../lib/state.svelte';
  import Icon from '../components/Icon.svelte';
  import PairDialog from '../components/PairDialog.svelte';

  let step = $state(0);
  let pairing = $state(false);
  const s = $derived(app.snap!);

  function finish() {
    api.updateSettings({ onboarded: true });
  }
</script>

<div class="wrap">
  <div class="panel card">
    <div class="steps" aria-hidden="true">
      {#each [0, 1, 2] as i}<span class:on={i <= step}></span>{/each}
    </div>

    {#if step === 0}
      <div class="art" aria-hidden="true">
        <span class="scr a"><span class="cur"></span></span>
        <span class="scr b"></span>
      </div>
      <h1>One mouse for all your PCs</h1>
      <p class="lead muted">Move the cursor off the edge of this screen and it carries on onto your other PC. Your keyboard follows it. Copy on one, paste on the other, or drag files across.</p>
      <ul class="facts">
        <li><Icon name="lock" size={18} /><span>Works on your own network only. No account, no cloud.</span></li>
        <li><Icon name="shield" size={18} /><span>Every connection is encrypted, and only devices you pair can connect.</span></li>
        <li><Icon name="keyboard" size={18} /><span>To do its job, Seam reads your mouse and keyboard. It never records or stores what you type.</span></li>
      </ul>
      <div class="actions"><button class="btn primary" onclick={() => (step = 1)}>Continue</button></div>
    {:else if step === 1}
      <h1>A few things to know</h1>
      <ul class="facts long">
        <li>
          <Icon name="wifi" size={18} />
          <span>Both PCs need Seam, on the same network. The installer lets Seam through Windows Firewall on private networks.{#if s.publicNetwork} <b class="warn">This PC's network is set to Public right now</b>, which blocks it. Switch it to Private in Windows Settings under Network & internet.{/if}</span>
        </li>
        <li>
          <Icon name="shield" size={18} />
          <span>Windows stops normal apps from controlling apps that run as administrator. If you need that, turn on "Run as administrator" in Settings.</span>
        </li>
        <li>
          <Icon name="lock" size={18} />
          <span>No app can reach the sign-in screen or UAC prompts. Use that PC's own keyboard for those.</span>
        </li>
        <li>
          <Icon name="warn" size={18} />
          <span>Anti-cheat in some online games may object to apps that read input. Pause Seam from the tray icon while you play.</span>
        </li>
      </ul>
      <div class="actions">
        <button class="btn ghost" onclick={() => (step = 0)}>Back</button>
        <button class="btn primary" onclick={() => (step = 2)}>Continue</button>
      </div>
    {:else}
      <h1>Pair your other PC</h1>
      <p class="lead muted">Install Seam on the other PC and open it. Then press <b>Pair a device</b> here and follow the steps. You only do this once.</p>
      {#if s.peers.length}
        <div class="banner info"><Icon name="check" size={18} /><span>Paired with {s.peers.map((p) => p.name).join(', ')}.</span></div>
      {/if}
      <div class="actions">
        <button class="btn ghost" onclick={finish}>Later</button>
        {#if s.peers.length}
          <button class="btn primary" onclick={finish}>Done</button>
        {:else}
          <button class="btn primary" onclick={() => (pairing = true)}><Icon name="plus" size={18} /> Pair a device</button>
        {/if}
      </div>
    {/if}
  </div>
</div>

<PairDialog bind:open={pairing} />

<style>
  .wrap {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 32px;
    background-image: radial-gradient(var(--line-strong) 1px, transparent 1px);
    background-size: 18px 18px;
  }
  .panel {
    width: min(560px, 100%);
    padding: 32px 36px 28px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    animation: in 260ms var(--ease);
  }
  @keyframes in {
    from {
      opacity: 0;
      transform: scale(0.98);
    }
  }
  .steps {
    display: flex;
    gap: 6px;
  }
  .steps span {
    width: 22px;
    height: 4px;
    border-radius: 2px;
    background: var(--line-strong);
    transition: background-color 200ms var(--ease);
  }
  .steps span.on {
    background: var(--accent);
  }
  .lead {
    font-size: 15px;
  }
  .art {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 8px 0 4px;
  }
  .scr {
    position: relative;
    width: 120px;
    height: 76px;
    border-radius: 12px;
    border: 2px solid var(--ink);
    background: var(--paper);
  }
  .scr.b {
    width: 96px;
    height: 62px;
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .cur {
    position: absolute;
    right: -9px;
    top: 26px;
    width: 0;
    height: 0;
    border-left: 9px solid var(--ink);
    border-top: 6px solid transparent;
    border-bottom: 6px solid transparent;
    animation: slide 2.4s var(--ease) infinite;
  }
  @keyframes slide {
    0%,
    30% {
      transform: translateX(-26px);
      opacity: 1;
    }
    60%,
    100% {
      transform: translateX(26px);
      opacity: 1;
    }
  }
  .facts {
    list-style: none;
    margin: 4px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .facts li {
    display: flex;
    gap: 12px;
    color: var(--muted);
  }
  .facts li :global(svg) {
    flex: none;
    margin-top: 2px;
    color: var(--ink);
  }
  .facts b {
    color: var(--ink);
    font-weight: 600;
  }
  .facts .warn {
    color: var(--warn);
  }
  .actions {
    margin-top: 10px;
  }
</style>
