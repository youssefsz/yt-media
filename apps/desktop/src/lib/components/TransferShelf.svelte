<script lang="ts">
  import type { JobDto } from '../ipc/generated';
  import {
    formatBytes,
    formatEta,
    jobDisplayName,
    progressDetail,
    stateLabel,
  } from '../presentation';
  import Icon from './Icon.svelte';

  export let jobs: JobDto[];
  export let expanded: boolean;
  export let busyActions: readonly string[];
  export let onToggle: () => void;
  export let onPause: (jobId: string) => Promise<void>;
  export let onResume: (jobId: string) => Promise<void>;
  export let onCancel: (jobId: string) => Promise<void>;

  $: activeCount = jobs.filter((job) => job.state !== 'queued').length;
  $: shelfLabel =
    jobs.length === 0
      ? 'No active transfers'
      : activeCount === jobs.length
        ? `${activeCount} active`
        : `${jobs.length} transfers`;
</script>

<section class="transfer-shelf" class:expanded aria-labelledby="transfer-shelf-title">
  <button
    type="button"
    class="shelf-toggle"
    aria-expanded={expanded}
    aria-controls="transfer-list"
    onclick={onToggle}
  >
    <span id="transfer-shelf-title">{shelfLabel}</span>
    <span class:expanded aria-hidden="true"><Icon name="chevron" size={16} /></span>
  </button>
  {#if expanded}
    <div id="transfer-list" class="transfer-list">
      {#if jobs.length > 0}
        {#each jobs as job (job.id)}
          <article class="transfer-row">
            <div class="transfer-identity">
              <strong>{jobDisplayName(job)}</strong>
              <span
                >{job.progress?.stage === undefined
                  ? stateLabel(job.state)
                  : stateLabel(job.progress.stage)}</span
              >
            </div>
            <div class="transfer-progress">
              {#if job.progress?.percent !== null && job.progress?.percent !== undefined}
                <progress max="100" value={job.progress.percent}>
                  {job.progress.percent}%
                </progress>
                <strong>{Math.round(job.progress.percent)}%</strong>
              {:else}
                <div
                  class="indeterminate-track"
                  role="progressbar"
                  aria-label="Progress is not supplied"
                ></div>
              {/if}
              <span>{progressDetail(job.progress)}</span>
            </div>
            <div class="transfer-metrics">
              {#if job.progress?.stage === 'downloading' && job.progress.bytes_per_second !== null}
                <span>{formatBytes(job.progress.bytes_per_second)}/s</span>
              {/if}
              {#if job.progress?.stage === 'downloading' && formatEta(job.progress.eta_seconds) !== null}
                <span>{formatEta(job.progress.eta_seconds)}</span>
              {/if}
            </div>
            <div
              class="transfer-actions"
              aria-label={`Transfer actions for ${jobDisplayName(job)}`}
            >
              {#if job.available_actions.includes('pause')}
                <button
                  type="button"
                  aria-label={`Pause ${jobDisplayName(job)}`}
                  disabled={busyActions.includes(`pause:${job.id}`)}
                  onclick={() => void onPause(job.id)}><Icon name="pause" size={19} /></button
                >
              {/if}
              {#if job.available_actions.includes('resume')}
                <button
                  type="button"
                  aria-label={`Resume ${jobDisplayName(job)}`}
                  disabled={busyActions.includes(`resume:${job.id}`)}
                  onclick={() => void onResume(job.id)}><Icon name="play" size={18} /></button
                >
              {/if}
              {#if job.available_actions.includes('cancel')}
                <button
                  type="button"
                  aria-label={`Cancel ${jobDisplayName(job)}`}
                  disabled={busyActions.includes(`cancel:${job.id}`)}
                  onclick={() => void onCancel(job.id)}><Icon name="close" size={18} /></button
                >
              {/if}
            </div>
          </article>
        {/each}
      {/if}
    </div>
  {/if}
</section>
