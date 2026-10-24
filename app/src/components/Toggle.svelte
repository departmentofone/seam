<script lang="ts">
  let {
    checked = false,
    disabled = false,
    label,
    onchange,
  }: { checked?: boolean; disabled?: boolean; label: string; onchange: (v: boolean) => void } = $props();
</script>

<button
  class="switch"
  class:on={checked}
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob"></span>
</button>

<style>
  .switch {
    position: relative;
    width: 40px;
    height: 24px;
    border-radius: 999px;
    border: none;
    padding: 0;
    background: var(--line-strong);
    cursor: pointer;
    transition: background-color 180ms var(--ease);
    flex: none;
  }
  .switch.on {
    background: var(--accent);
  }
  .switch:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 200ms var(--ease);
  }
  .on .knob {
    transform: translateX(16px);
  }
  .switch:active .knob {
    width: 21px;
  }
  .on:active .knob {
    transform: translateX(13px);
  }
</style>
