<script lang="ts">
  import type {
    IpcErrorDto,
    SettingsDto,
    ToolStatusDto,
    UpdatePreferenceDto,
  } from '../ipc/generated';

  export let settings: SettingsDto | null;
  export let tools: ToolStatusDto[];
  export let status: 'idle' | 'saving' | 'error';
  export let error: IpcErrorDto | null;
  export let busyActions: readonly string[];
  export let onSetConcurrency: (value: number) => Promise<void>;
  export let onSetUpdatePreference: (value: UpdatePreferenceDto) => Promise<void>;
  export let onChooseDefaultDestination: () => Promise<void>;
  export let onClearDefaultDestination: () => Promise<void>;
  export let onRefreshTools: () => Promise<void>;

  const toolLabel = (tool: ToolStatusDto['tool']): string =>
    tool === 'yt-dlp' ? 'yt-dlp' : tool[0]?.toUpperCase() + tool.slice(1);

  const sourceLabel = (source: ToolStatusDto['source']): string =>
    source === null ? 'Not resolved' : source.replaceAll('-', ' ');

  const updatePreference = (value: string): void => {
    if (value === 'notify' || value === 'automatic' || value === 'disabled') {
      void onSetUpdatePreference(value);
    }
  };
</script>

<section class="settings-view" aria-labelledby="view-title">
  <header class="view-heading">
    <h1 id="view-title" tabindex="-1">Settings</h1>
  </header>

  {#if settings === null}
    <div class="view-state error-state" role="alert">
      <h2>Settings are unavailable</h2>
      <p>The local persistence service needs attention before preferences can be changed.</p>
    </div>
  {:else}
    {#if error !== null}
      <div class="inline-alert" role="alert">
        <strong>Settings not saved</strong>
        <span>{error.message}</span>
      </div>
    {/if}
    <div class="settings-layout" aria-busy={status === 'saving'}>
      <section class="settings-section" aria-labelledby="download-settings-title">
        <header>
          <h2 id="download-settings-title">Downloads</h2>
          <p>Defaults apply to new work and never restart interrupted jobs.</p>
        </header>
        <div class="setting-row">
          <div>
            <label for="default-destination">Default destination</label>
            <p>Selected through the native system folder picker.</p>
          </div>
          <div class="setting-control destination-setting">
            <input
              id="default-destination"
              type="text"
              readonly
              value={settings.default_destination ?? ''}
              placeholder="Not set"
            />
            <button
              type="button"
              disabled={busyActions.includes('choose-default-destination') || status === 'saving'}
              onclick={() => void onChooseDefaultDestination()}
            >
              {busyActions.includes('choose-default-destination') ? 'Opening…' : 'Choose'}
            </button>
            <button
              type="button"
              disabled={status === 'saving'}
              onclick={() => void onClearDefaultDestination()}>Use Downloads</button
            >
          </div>
        </div>
        <div class="setting-row">
          <div>
            <label for="queue-concurrency">Concurrent downloads</label>
            <p>One through four engine-owned queue slots.</p>
          </div>
          <select
            id="queue-concurrency"
            value={String(settings.queue_concurrency)}
            disabled={status === 'saving'}
            onchange={(event) => void onSetConcurrency(Number(event.currentTarget.value))}
          >
            <option value="1">1 download</option>
            <option value="2">2 downloads</option>
            <option value="3">3 downloads</option>
            <option value="4">4 downloads</option>
          </select>
        </div>
        <div class="setting-row">
          <div>
            <label for="update-preference">Verified tool updates</label>
          </div>
          <select
            id="update-preference"
            value={settings.update_preference}
            disabled={status === 'saving'}
            onchange={(event) => updatePreference(event.currentTarget.value)}
          >
            <option value="notify">Notify me</option>
            <option value="automatic">Automatic</option>
            <option value="disabled">Disabled</option>
          </select>
        </div>
      </section>

      <section class="settings-section tool-section" aria-labelledby="tools-title">
        <header>
          <div>
            <h2 id="tools-title">Media tool health</h2>
            <p>
              Only verified engine-resolved executables are shown. Arbitrary arguments are never
              exposed.
            </p>
          </div>
          <button
            type="button"
            disabled={busyActions.includes('refresh-tools')}
            onclick={() => void onRefreshTools()}
          >
            {busyActions.includes('refresh-tools') ? 'Checking…' : 'Check again'}
          </button>
        </header>
        <ul class="tool-list">
          {#each tools as tool (tool.tool)}
            <li>
              <span class:ready={tool.ready} class="tool-indicator" aria-hidden="true"></span>
              <div>
                <strong>{toolLabel(tool.tool)}</strong>
                <span
                  >{tool.ready ? sourceLabel(tool.source) : (tool.message ?? 'Unavailable')}</span
                >
              </div>
              <span class="tool-state">{tool.ready ? 'Ready' : 'Needs attention'}</span>
            </li>
          {/each}
        </ul>
      </section>
    </div>
  {/if}
</section>
