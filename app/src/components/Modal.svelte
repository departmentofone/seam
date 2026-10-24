<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    open = $bindable(false),
    title,
    width = 480,
    onclose,
    children,
  }: { open?: boolean; title: string; width?: number; onclose?: () => void; children: Snippet } = $props();

  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });

  function close() {
    open = false;
    onclose?.();
  }
</script>

<dialog
  bind:this={dialog}
  style:width="{width}px"
  onclose={() => {
    if (open) close();
  }}
  onclick={(e) => {
    if (e.target === dialog) close();
  }}
>
  <div class="inner">
    <header>
      <h2>{title}</h2>
      <button class="btn ghost small icon" aria-label="Close" onclick={close}><Icon name="close" size={18} /></button>
    </header>
    {@render children()}
  </div>
</dialog>

<style>
  dialog {
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 0;
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow-lift);
    max-width: calc(100vw - 48px);
    opacity: 0;
    transform: scale(0.96);
    transition:
      opacity 200ms var(--ease),
      transform 200ms var(--ease),
      overlay 200ms allow-discrete,
      display 200ms allow-discrete;
  }
  dialog[open] {
    opacity: 1;
    transform: scale(1);
  }
  @starting-style {
    dialog[open] {
      opacity: 0;
      transform: scale(0.96);
    }
  }
  dialog::backdrop {
    background: rgb(23 23 26 / 28%);
    backdrop-filter: blur(4px);
  }
  .inner {
    padding: 22px 24px 24px;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }
  .icon {
    width: 32px;
    padding: 0;
  }
</style>
