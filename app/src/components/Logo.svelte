<script lang="ts">
  // The Seam mark: a pointer cut by the seam it just crossed.
  let { size = 30 }: { size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 100 100" aria-hidden="true">
  <path fill="var(--accent)" d="M23 0H77C89.7 0 100 10.3 100 23V77C100 89.7 89.7 100 77 100H23C10.3 100 0 89.7 0 77V23C0 10.3 10.3 0 23 0Z" />
  <path fill="#f6f3ec" d="M45.8 59.6 31.4 72V16L50.6 33.42V69.85L45.8 59.6ZM53.8 41.93 74.6 60.8H57L65.8 78.8L54.6 84L53.8 82.29V41.93Z" />
</svg>
