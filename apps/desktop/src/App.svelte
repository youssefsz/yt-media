<script lang="ts">
  import { onMount, tick } from 'svelte';

  import JobList from './lib/components/JobList.svelte';
  import NewDownload from './lib/components/NewDownload.svelte';
  import Settings from './lib/components/Settings.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import TransferShelf from './lib/components/TransferShelf.svelte';
  import { createWorkspaceController, type WorkspaceView } from './lib/controller/workspace';
  import { desktopClient, type DesktopClient } from './lib/ipc/client';

  export let client: DesktopClient = desktopClient;

  const controller = createWorkspaceController(client);
  const workspace = controller.state;

  $: queueJobs = $workspace.jobs.filter((job) => !job.is_terminal);
  $: historyJobs = $workspace.jobs.filter((job) => job.is_terminal);
  $: shelfJobs = queueJobs.filter((job) => job.state !== 'interrupted');

  const navigate = async (view: WorkspaceView): Promise<void> => {
    controller.navigate(view);
    await tick();
    const urlInput = view === 'new-download' ? document.getElementById('video-url') : null;
    const target =
      urlInput instanceof HTMLInputElement && !urlInput.disabled
        ? urlInput
        : document.getElementById('view-title');
    target?.focus();
  };

  onMount(() => {
    void controller.connect();
    return () => {
      controller.disconnect();
    };
  });
</script>

<svelte:head>
  <meta
    name="description"
    content="YT Media local-first download queue and media conversion workspace."
  />
</svelte:head>

{#if $workspace.connection === 'loading'}
  <div class="startup-screen" role="status" aria-live="polite">
    <span class="startup-mark" aria-hidden="true">
      <img class="startup-logo" src="/brand-mark.svg" alt="" />
    </span>
    <span>Starting YT Media</span>
  </div>
{:else}
  <div class="app-shell">
    <Sidebar
      activeView={$workspace.view}
      queueCount={queueJobs.length}
      onNavigate={(view) => void navigate(view)}
    />
    <div class="app-body" class:has-expanded-shelf={$workspace.shelfExpanded}>
      {#if $workspace.diagnostic !== null}
        <div class="service-banner" role="status">
          <strong>Local service needs attention</strong>
          <span>{$workspace.diagnostic.message}</span>
          <button type="button" onclick={() => void controller.reconnect()}>Retry startup</button>
        </div>
      {:else if $workspace.connectionError !== null}
        <div class="service-banner" role="alert">
          <strong>
            {$workspace.connection === 'error'
              ? 'Native service unavailable'
              : 'Action needs attention'}
          </strong>
          <span>{$workspace.connectionError.message}</span>
          {#if $workspace.connection === 'error'}
            <button type="button" onclick={() => void controller.reconnect()}>Reconnect</button>
          {/if}
        </div>
      {/if}

      <main>
        {#if $workspace.view === 'new-download'}
          <NewDownload
            analysis={$workspace.analysis}
            draft={$workspace.draft}
            busyActions={$workspace.busyActions}
            onSetUrl={controller.setUrl}
            onSetName={controller.setName}
            onSetOutputKind={controller.setOutputKind}
            onSelectOutput={controller.selectOutput}
            onAnalyze={controller.analyze}
            onCancelAnalysis={controller.cancelAnalysis}
            onChooseDestination={controller.chooseDestination}
            onEnqueue={controller.enqueue}
          />
        {:else if $workspace.view === 'queue'}
          <JobList
            mode="queue"
            connection={$workspace.connection}
            connectionError={$workspace.connectionError}
            jobs={queueJobs}
            busyActions={$workspace.busyActions}
            onPause={controller.pause}
            onResume={controller.resume}
            onCancel={controller.cancel}
            onRetry={controller.retry}
            onReveal={controller.reveal}
            onDelete={controller.deleteHistory}
          />
        {:else if $workspace.view === 'history'}
          <JobList
            mode="history"
            connection={$workspace.connection}
            connectionError={$workspace.connectionError}
            jobs={historyJobs}
            busyActions={$workspace.busyActions}
            onPause={controller.pause}
            onResume={controller.resume}
            onCancel={controller.cancel}
            onRetry={controller.retry}
            onReveal={controller.reveal}
            onDelete={controller.deleteHistory}
          />
        {:else}
          <Settings
            settings={$workspace.settings}
            tools={$workspace.tools}
            status={$workspace.settingsStatus}
            error={$workspace.settingsError}
            busyActions={$workspace.busyActions}
            onSetConcurrency={controller.setConcurrency}
            onSetUpdatePreference={controller.setUpdatePreference}
            onChooseDefaultDestination={controller.chooseDefaultDestination}
            onClearDefaultDestination={controller.clearDefaultDestination}
            onRefreshTools={controller.refreshTools}
            onCheckForToolUpdates={controller.checkForToolUpdates}
            onResetToolUpdates={controller.resetToolUpdates}
          />
        {/if}
      </main>

      <TransferShelf
        jobs={shelfJobs}
        expanded={$workspace.shelfExpanded}
        busyActions={$workspace.busyActions}
        onToggle={controller.toggleShelf}
        onPause={controller.pause}
        onResume={controller.resume}
        onCancel={controller.cancel}
      />
    </div>
    <p class="sr-only" aria-live="polite" aria-atomic="true">
      {$workspace.announcement}
    </p>
  </div>
{/if}
