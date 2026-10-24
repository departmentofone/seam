<script lang="ts">
  let { n, name }: { n: number; name: string } = $props();
  document.documentElement.style.background = 'transparent';
  document.body.style.background = 'transparent';
</script>

<div class="tile">
  <span class="n">{n}</span>
  <span class="name">{name}</span>
</div>

<style>
  .tile {
    position: fixed;
    inset: 8px;
    border-radius: 28px;
    background: var(--accent);
    color: #fff;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
    animation: pop 220ms var(--ease);
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.94);
    }
  }
  .n {
    font: 750 96px/1 var(--display);
  }
  .name {
    margin-top: 6px;
    font: 500 13px var(--body);
    opacity: 0.9;
    max-width: 90%;
    text-align: center;
  }
</style>
