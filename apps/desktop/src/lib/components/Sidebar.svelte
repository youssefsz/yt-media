<script lang="ts">
  import type { WorkspaceView } from '../controller/workspace';
  import Icon from './Icon.svelte';

  export let activeView: WorkspaceView;
  export let queueCount: number;
  export let onNavigate: (view: WorkspaceView) => void;

  const items: ReadonlyArray<{
    view: WorkspaceView;
    label: string;
    icon: 'download' | 'queue' | 'history';
  }> = [
    { view: 'new-download', label: 'New Download', icon: 'download' },
    { view: 'queue', label: 'Queue', icon: 'queue' },
    { view: 'history', label: 'History', icon: 'history' },
  ];
</script>

<aside class="sidebar" aria-label="Primary">
  <div class="brand" aria-hidden="true">
    <img class="brand-logo" src="/brand-mark.svg" alt="" />
  </div>
  <nav aria-label="Workspace">
    {#each items as item (item.view)}
      <button
        type="button"
        class:active={activeView === item.view}
        aria-current={activeView === item.view ? 'page' : undefined}
        onclick={() => onNavigate(item.view)}
      >
        <span class="nav-glyph" aria-hidden="true"><Icon name={item.icon} /></span>
        <span>{item.label}</span>
        {#if item.view === 'queue' && queueCount > 0}
          <span class="count" aria-label={`${queueCount} non-terminal jobs`}>
            {queueCount}
          </span>
        {/if}
      </button>
    {/each}
  </nav>
  <button
    type="button"
    class="settings-link"
    class:active={activeView === 'settings'}
    aria-current={activeView === 'settings' ? 'page' : undefined}
    onclick={() => onNavigate('settings')}
  >
    <span class="nav-glyph" aria-hidden="true"><Icon name="settings" /></span>
    <span>Settings</span>
  </button>
</aside>
