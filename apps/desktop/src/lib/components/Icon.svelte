<script lang="ts">
  export let name:
    | 'download'
    | 'queue'
    | 'history'
    | 'settings'
    | 'link'
    | 'folder'
    | 'close'
    | 'pause'
    | 'play'
    | 'chevron';
  export let size = 22;
</script>

<svg
  aria-hidden="true"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.7"
  stroke-linecap="round"
  stroke-linejoin="round"
>
  {#if name === 'download'}
    <path d="M12 3v12m0 0 4-4m-4 4-4-4M4 17v3h16v-3" />
  {:else if name === 'queue'}
    <path d="M4 6h10M4 12h10M4 18h7M18 14v7m-3.5-3.5h7" />
  {:else if name === 'history'}
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7v5l3.5 2" />
  {:else if name === 'settings'}
    <circle cx="12" cy="12" r="3" />
    <path
      d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H2.8v-4H3a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1A1.7 1.7 0 0 0 9 4.6 1.7 1.7 0 0 0 10 3V2.8h4V3a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1Z"
    />
  {:else if name === 'link'}
    <path
      d="m10.5 13.5 3-3M8.2 15.8l-1 1a3.5 3.5 0 0 1-5-5l3-3a3.5 3.5 0 0 1 5 0M15.8 8.2l1-1a3.5 3.5 0 0 1 5 5l-3 3a3.5 3.5 0 0 1-5 0"
    />
  {:else if name === 'folder'}
    <path d="M3 6.5h7l2 2h9v10H3z" />
  {:else if name === 'close'}
    <path d="m7 7 10 10M17 7 7 17" />
  {:else if name === 'pause'}
    <path d="M8 5v14M16 5v14" />
  {:else if name === 'play'}
    <path d="m9 6 9 6-9 6z" />
  {:else if name === 'chevron'}
    <path d="m7 14 5-5 5 5" />
  {/if}
</svg>
