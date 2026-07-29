<script lang="ts">
  import { onMount } from 'svelte';

  import type { BootstrapStateDto, IpcErrorDto } from './lib/ipc/generated';
  import { connectJobEvents, isIpcError } from './lib/ipc/client';

  let bootstrapState: BootstrapStateDto | undefined;
  let bootstrapError: IpcErrorDto | undefined;
  let lastEventSequence = '0';

  onMount(() => {
    let disposed = false;
    let disconnect: (() => void) | undefined;
    void connectJobEvents(
      (snapshot) => {
        if (!disposed) {
          bootstrapState = snapshot;
          lastEventSequence = snapshot.last_event_sequence;
        }
      },
      (event) => {
        if (!disposed) {
          lastEventSequence = event.sequence;
        }
      },
    )
      .then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          disconnect = unlisten;
        }
      })
      .catch((error: unknown) => {
        if (disposed) {
          return;
        }
        bootstrapError = isIpcError(error)
          ? error
          : {
              code: 'internal',
              message: 'The native application service did not respond. Restart YT Media.',
              details: [],
            };
      });
    return () => {
      disposed = true;
      disconnect?.();
    };
  });
</script>

<svelte:head>
  <meta
    name="description"
    content="YT Media local desktop service bootstrap and recovery diagnostics."
  />
</svelte:head>

<main aria-labelledby="bootstrap-title">
  <section class="bootstrap-panel" aria-live="polite">
    <p class="eyebrow">Local media workspace</p>
    <h1 id="bootstrap-title">YT Media</h1>
    {#if bootstrapError !== undefined}
      <div class="diagnostic" role="alert">
        <h2>Native service unavailable</h2>
        <p>{bootstrapError.message}</p>
      </div>
    {:else if bootstrapState === undefined}
      <p class="status">Recovering local jobs and checking media tools…</p>
    {:else}
      <p class:healthy={bootstrapState.health === 'healthy'} class="status">
        {bootstrapState.health === 'healthy'
          ? 'Desktop integration ready'
          : bootstrapState.health === 'degraded'
            ? 'Local history ready; media tools need attention'
            : 'Local service needs attention'}
      </p>
      <dl>
        <div>
          <dt>Recovered jobs</dt>
          <dd>{bootstrapState.jobs.length}</dd>
        </div>
        <div>
          <dt>Verified tools</dt>
          <dd>{bootstrapState.tools.filter((tool) => tool.ready).length} / 4</dd>
        </div>
        <div>
          <dt>Event boundary</dt>
          <dd>{lastEventSequence}</dd>
        </div>
        <div>
          <dt>IPC schema</dt>
          <dd>v{bootstrapState.schema_version}</dd>
        </div>
      </dl>
      {#if bootstrapState.diagnostic !== null}
        <div class="diagnostic" role="status">
          <h2>Recoverable diagnostic</h2>
          <p>{bootstrapState.diagnostic.message}</p>
        </div>
      {/if}
    {/if}
    <p class="scope-note">
      Download controls and the full desktop workspace arrive in the next UI milestone.
    </p>
  </section>
</main>
