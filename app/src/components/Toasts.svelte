<script lang="ts">
  import { app, dismiss } from '../lib/state.svelte';
  import Icon from './Icon.svelte';

  const icons = { info: 'info', ok: 'check', warn: 'warn', error: 'warn' } as const;
</script>

<div class="stack" aria-live="polite">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.level}">
      <span class="icon"><Icon name={icons[t.level]} size={18} /></span>
      <div class="text">
        <strong>{t.title}</strong>
        {#if t.body}<span>{t.body}</span>{/if}
      </div>
      <button class="x" aria-label="Dismiss" onclick={() => dismiss(t.id)}><Icon name="close" size={14} /></button>
    </div>
  {/each}
</div>

<style>
  .stack {
    position: fixed;
    right: 20px;
    bottom: 20px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 50;
    width: 340px;
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 12px 12px 12px 14px;
    border-radius: var(--r-sm);
    background: var(--surface);
    border: 1px solid var(--line);
    box-shadow: var(--shadow-lift);
    animation: enter 220ms var(--ease);
  }
  @keyframes enter {
    from {
      opacity: 0;
      transform: translateY(6px) scale(0.98);
    }
  }
  .icon {
    flex: none;
    margin-top: 1px;
    color: var(--muted);
  }
  .ok .icon {
    color: var(--ok);
  }
  .warn .icon {
    color: var(--warn);
  }
  .error .icon {
    color: var(--error);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
    font-size: 13px;
  }
  .text span {
    color: var(--muted);
  }
  .x {
    border: none;
    background: none;
    color: var(--faint);
    cursor: pointer;
    padding: 2px;
    border-radius: 6px;
  }
  .x:hover {
    color: var(--ink);
  }
</style>
