<script lang="ts">
  import { tick } from 'svelte';

  import type { IpcErrorDto, JobDto } from '../ipc/generated';
  import { formatDate, formatOutput, jobDisplayName, progressDetail } from '../presentation';
  import StatusBadge from './StatusBadge.svelte';

  export let mode: 'queue' | 'history';
  export let connection: 'loading' | 'ready' | 'error';
  export let connectionError: IpcErrorDto | null;
  export let jobs: JobDto[];
  export let busyActions: readonly string[];
  export let onPause: (jobId: string) => Promise<void>;
  export let onResume: (jobId: string) => Promise<void>;
  export let onCancel: (jobId: string) => Promise<void>;
  export let onRetry: (jobId: string) => Promise<void>;
  export let onReveal: (jobId: string) => Promise<void>;
  export let onDelete: (jobId: string) => Promise<void>;

  let pendingDelete: string | null = null;

  const completeDelete = async (jobId: string): Promise<void> => {
    await onDelete(jobId);
    pendingDelete = null;
    await tick();
    document.getElementById('view-title')?.focus();
  };
</script>

<section class="job-view" aria-labelledby="view-title">
  <header class="view-heading">
    <h1 id="view-title" tabindex="-1">{mode === 'queue' ? 'Queue' : 'History'}</h1>
  </header>

  {#if connection === 'loading'}
    <div class="view-state" role="status">
      <div class="spinner" aria-hidden="true"></div>
      <h2>Loading local jobs</h2>
      <p>Recovering the durable queue and output status…</p>
    </div>
  {:else if connection === 'error'}
    <div class="view-state error-state" role="alert">
      <h2>Jobs are unavailable</h2>
      <p>{connectionError?.message ?? 'The native service did not respond.'}</p>
    </div>
  {:else if jobs.length === 0}
    <div class="view-state">
      <h2>{mode === 'queue' ? 'The queue is clear' : 'No history yet'}</h2>
      <p>
        {mode === 'queue'
          ? 'New downloads and interrupted recovery actions will appear here.'
          : 'Terminal jobs stay here until completed history is explicitly deleted.'}
      </p>
    </div>
  {:else}
    <ol class="job-list" aria-label={mode === 'queue' ? 'Ordered queue' : 'Download history'}>
      {#each jobs as job (job.id)}
        <li class="job-card" data-state={job.state}>
          <div class="job-main">
            <div class="job-heading">
              <div>
                <h2>{jobDisplayName(job)}</h2>
                <p>
                  {formatOutput(job.output)} <span aria-hidden="true">·</span>
                  {job.destination}
                </p>
              </div>
              <StatusBadge state={job.state} />
            </div>

            {#if mode === 'queue'}
              {#if job.state === 'interrupted'}
                <div class="recovery-note" role="status">
                  <strong>Recovery required</strong>
                  <span
                    >This job was active when the app closed. It will not use bandwidth until you
                    resume it.</span
                  >
                </div>
              {/if}
              {#if !job.destination_available}
                <div class="recovery-note warning" role="status">
                  <strong>Destination unavailable</strong>
                  <span>Reconnect or choose the original destination before resuming.</span>
                </div>
              {/if}
              <div class="job-progress">
                <div class="progress-copy">
                  <span>{job.progress?.stage ?? job.state}</span>
                  <span>{progressDetail(job.progress)}</span>
                </div>
                {#if job.progress?.percent !== null && job.progress?.percent !== undefined}
                  <progress max="100" value={job.progress.percent}>
                    {job.progress.percent}%
                  </progress>
                {:else}
                  <div
                    class="indeterminate-track"
                    role="progressbar"
                    aria-label="Progress is not yet available"
                  ></div>
                {/if}
              </div>
            {:else}
              <dl class="history-details">
                <div>
                  <dt>Finished</dt>
                  <dd>{formatDate(job.completed_at_ms ?? job.updated_at_ms)}</dd>
                </div>
                <div>
                  <dt>Attempts</dt>
                  <dd>{job.attempt_count}</dd>
                </div>
                <div>
                  <dt>Output</dt>
                  <dd>
                    {job.output_availability === 'present'
                      ? (job.final_output?.path ?? 'Present')
                      : job.output_availability === 'missing'
                        ? 'Missing or moved'
                        : 'No output'}
                  </dd>
                </div>
              </dl>
              {#if job.error !== null}
                <div class="job-error" role="status">
                  <strong>{job.error.class.replaceAll('-', ' ')}</strong>
                  <span>{job.error.message}</span>
                </div>
              {/if}
              {#if job.output_availability === 'missing'}
                <div class="recovery-note warning" role="status">
                  <strong>Output missing</strong>
                  <span
                    >The history entry is intact, but the published file is no longer at its
                    recorded location.</span
                  >
                </div>
              {/if}
            {/if}
          </div>

          <div class="job-actions" aria-label={`Actions for ${jobDisplayName(job)}`}>
            {#if mode === 'queue'}
              {#if job.available_actions.includes('pause')}
                <button
                  type="button"
                  disabled={busyActions.includes(`pause:${job.id}`)}
                  onclick={() => void onPause(job.id)}
                  >{busyActions.includes(`pause:${job.id}`) ? 'Pausing…' : 'Pause'}</button
                >
              {/if}
              {#if job.available_actions.includes('resume')}
                <button
                  class="accent-button"
                  type="button"
                  disabled={busyActions.includes(`resume:${job.id}`)}
                  onclick={() => void onResume(job.id)}
                  >{busyActions.includes(`resume:${job.id}`) ? 'Resuming…' : 'Resume'}</button
                >
              {/if}
              {#if job.available_actions.includes('cancel')}
                <button
                  type="button"
                  disabled={busyActions.includes(`cancel:${job.id}`)}
                  onclick={() => void onCancel(job.id)}
                  >{busyActions.includes(`cancel:${job.id}`) ? 'Cancelling…' : 'Cancel'}</button
                >
              {/if}
            {:else}
              {#if job.available_actions.includes('retry')}
                <button
                  class="accent-button"
                  type="button"
                  disabled={busyActions.includes(`retry:${job.id}`)}
                  onclick={() => void onRetry(job.id)}
                  >{busyActions.includes(`retry:${job.id}`) ? 'Retrying…' : 'Retry'}</button
                >
              {/if}
              {#if job.available_actions.includes('reveal')}
                <button
                  type="button"
                  disabled={busyActions.includes(`reveal:${job.id}`)}
                  onclick={() => void onReveal(job.id)}
                  >{busyActions.includes(`reveal:${job.id}`) ? 'Opening…' : 'Reveal'}</button
                >
              {/if}
              {#if job.available_actions.includes('delete-history')}
                {#if pendingDelete === job.id}
                  <div
                    class="delete-confirmation"
                    role="group"
                    aria-label="Confirm history deletion"
                  >
                    <span>Keep the output file and delete this record?</span>
                    <button
                      type="button"
                      onclick={() => {
                        pendingDelete = null;
                      }}>Keep record</button
                    >
                    <button
                      class="danger-button"
                      type="button"
                      disabled={busyActions.includes(`delete:${job.id}`)}
                      onclick={() => void completeDelete(job.id)}
                      >{busyActions.includes(`delete:${job.id}`)
                        ? 'Deleting…'
                        : 'Delete record'}</button
                    >
                  </div>
                {:else}
                  <button
                    type="button"
                    onclick={() => {
                      pendingDelete = job.id;
                    }}>Delete history</button
                  >
                {/if}
              {/if}
            {/if}
          </div>
        </li>
      {/each}
    </ol>
  {/if}
</section>
