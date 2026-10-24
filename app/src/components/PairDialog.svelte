<script lang="ts">
  import { api, type Found } from '../lib/api';
  import { app, toast } from '../lib/state.svelte';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';

  let { open = $bindable(false) }: { open?: boolean } = $props();

  let mode: 'show' | 'enter' = $state('show');
  let code = $state('');
  let addr = $state('');
  let target: Found | null = $state(null);
  let busy = $state(false);
  let error = $state('');

  const s = $derived(app.snap);
  const nearby = $derived(s ? s.discovered.filter((d) => d.pairing) : []);
  const shown = $derived(s?.pairingCode ?? null);
  const secs = $derived(s?.pairingEndsIn ?? 0);

  $effect(() => {
    if (open && mode === 'show' && !shown) api.beginPairing();
  });

  $effect(() => {
    if (!open && shown) api.endPairing();
  });

  $effect(() => {
    if (nearby.length === 1 && !target && !addr) target = nearby[0];
  });

  // Results arrive as events, for either side of the pairing.
  $effect(() => {
    const r = app.pairResult;
    if (!r) return;
    app.pairResult = null;
    busy = false;
    if (r.ok) {
      toast('ok', r.message, 'They will connect automatically from now on.');
      reset();
      open = false;
    } else if (mode === 'enter') {
      error = r.message;
    } else {
      toast('warn', 'Pairing attempt', r.message);
    }
  });

  function reset() {
    code = '';
    addr = '';
    target = null;
    error = '';
    busy = false;
  }

  function switchTo(m: 'show' | 'enter') {
    mode = m;
    error = '';
    if (m === 'enter' && shown) api.endPairing();
  }

  async function submit(e: Event) {
    e.preventDefault();
    error = '';
    if (code.length !== 6) {
      error = 'The code has 6 digits.';
      return;
    }
    if (!target && !addr.trim()) {
      error = 'Pick a device, or type its address.';
      return;
    }
    busy = true;
    await api.pairWith(code, target?.id, addr.trim() || undefined);
  }

  function onCode(e: Event) {
    const v = (e.target as HTMLInputElement).value.replace(/\D/g, '').slice(0, 6);
    code = v;
    (e.target as HTMLInputElement).value = v;
  }

  const fmt = (c: string) => `${c.slice(0, 3)} ${c.slice(3)}`;
  const mmss = (n: number) => `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`;
</script>

<Modal bind:open title="Pair a device" width={500} onclose={reset}>
  <div class="tabs" role="tablist">
    <button role="tab" aria-selected={mode === 'show'} class:on={mode === 'show'} onclick={() => switchTo('show')}>Show a code</button>
    <button role="tab" aria-selected={mode === 'enter'} class:on={mode === 'enter'} onclick={() => switchTo('enter')}>Enter a code</button>
  </div>

  {#if mode === 'show'}
    <div class="show">
      <p class="muted">On your other PC, open Seam, choose <b>Pair a device</b>, then <b>Enter a code</b> and type:</p>
      <div class="code mono" aria-live="polite">{shown ? fmt(shown) : '··· ···'}</div>
      {#if shown}
        <div class="timer">
          <div class="bar"><span style:width="{(secs / 180) * 100}%"></span></div>
          <span class="faint mono">{mmss(secs)}</span>
        </div>
      {:else}
        <button class="btn small" onclick={() => api.beginPairing()}><Icon name="refresh" size={16} /> New code</button>
      {/if}
      <p class="fine faint">The code only works for the next three minutes. Five wrong tries and it stops working.</p>
    </div>
  {:else}
    <form onsubmit={submit} class="enter">
      <div class="field">
        <span class="lbl">Device</span>
        {#if nearby.length}
          <div class="picks">
            {#each nearby as d (d.id)}
              <button type="button" class="pick" class:on={target?.id === d.id} onclick={() => ((target = d), (addr = ''))}>
                <Icon name="devices" size={18} />
                <span>{d.name}</span>
                {#if target?.id === d.id}<span class="tick"><Icon name="check" size={16} /></span>{/if}
              </button>
            {/each}
          </div>
        {:else}
          <p class="faint fine">No device is showing a code on this network yet. Choose <b>Show a code</b> on the other PC, or type its address below.</p>
        {/if}
        <input
          type="text"
          placeholder="Or type an address, like 192.168.1.20"
          bind:value={addr}
          oninput={() => (target = null)}
          spellcheck="false"
          autocomplete="off"
        />
      </div>

      <div class="field">
        <span class="lbl">Code</span>
        <input class="codein mono" type="text" inputmode="numeric" placeholder="000000" value={code} oninput={onCode} autocomplete="one-time-code" />
      </div>

      {#if error}<div class="banner error"><Icon name="warn" size={18} /><span>{error}</span></div>{/if}

      <div class="actions">
        <button type="button" class="btn ghost" onclick={() => (open = false)}>Cancel</button>
        <button type="submit" class="btn primary" disabled={busy}>{busy ? 'Pairing…' : 'Pair'}</button>
      </div>
    </form>
  {/if}
</Modal>

<style>
  .tabs {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px;
    padding: 4px;
    border-radius: 999px;
    background: var(--paper-deep);
    margin-bottom: 20px;
  }
  .tabs button {
    height: 32px;
    border: none;
    border-radius: 999px;
    background: transparent;
    color: var(--muted);
    font: 500 13px var(--body);
    cursor: pointer;
    transition: background-color 160ms var(--ease), color 160ms var(--ease);
  }
  .tabs button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }
  .show {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 16px;
    text-align: center;
  }
  .code {
    font-size: 44px;
    font-weight: 600;
    letter-spacing: 0.12em;
    padding: 14px 26px;
    border-radius: var(--r-md);
    background: var(--paper);
    border: 1px solid var(--line);
    user-select: text;
  }
  .timer {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 220px;
  }
  .bar {
    flex: 1;
    height: 4px;
    border-radius: 99px;
    background: var(--paper-deep);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 1s linear;
  }
  .enter {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .lbl {
    font: 600 11.5px var(--mono);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
  }
  .codein {
    height: 52px;
    font-size: 26px;
    letter-spacing: 0.3em;
    text-align: center;
  }
</style>
