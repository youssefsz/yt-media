<script lang="ts">
  import { onDestroy, tick } from 'svelte';

  import type { AnalysisState, DownloadDraft, OutputKind } from '../controller/workspace';
  import type { FormatOptionDto, OutputSelectionDto } from '../ipc/generated';
  import { formatCount, formatDuration, formatEstimatedSize, formatFormat } from '../presentation';
  import Icon from './Icon.svelte';

  export let analysis: AnalysisState;
  export let draft: DownloadDraft;
  export let busyActions: readonly string[];
  export let onSetUrl: (url: string) => void;
  export let onSetName: (name: string) => void;
  export let onSetOutputKind: (kind: OutputKind) => void;
  export let onSelectOutput: (output: OutputSelectionDto) => void;
  export let onAnalyze: () => Promise<void>;
  export let onCancelAnalysis: () => void;
  export let onChooseDestination: () => Promise<void>;
  export let onEnqueue: () => Promise<void>;

  let urlInput: HTMLInputElement;
  let mediaHeading: HTMLHeadingElement;
  let failedThumbnailId = '';
  let mounted = true;

  onDestroy(() => {
    mounted = false;
  });

  const qualityFor = (format: FormatOptionDto): number =>
    format.kind === 'mp3' ? format.bitrate_kbps : format.height;

  const outputFor = (format: FormatOptionDto): OutputSelectionDto =>
    format.kind === 'mp3'
      ? { format: 'mp3', quality: format.bitrate_kbps }
      : { format: 'mp4', quality: format.height };

  const safeThumbnail = (thumbnails: NonNullable<typeof media>['thumbnails']): string | null => {
    const candidate = thumbnails.at(-1)?.url;
    if (candidate === undefined) {
      return null;
    }
    try {
      const url = new URL(candidate);
      return url.protocol === 'https:' || url.protocol === 'http:' ? url.toString() : null;
    } catch {
      return null;
    }
  };

  $: media = analysis.media;
  $: formats = media?.formats.filter((format) => format.kind === draft.outputKind) ?? [];
  $: selectedFormat =
    formats.find(
      (format) =>
        draft.selectedOutputs[draft.outputKind]?.format === format.kind &&
        draft.selectedOutputs[draft.outputKind]?.quality === qualityFor(format),
    ) ?? null;
  $: thumbnail = safeThumbnail(media?.thumbnails ?? []);
  $: displayFilename = draft.name.length === 0 ? '' : `${draft.name}.${draft.outputKind}`;

  const updateFilename = (value: string): void => {
    const extension = `.${draft.outputKind}`;
    onSetName(value.endsWith(extension) ? value.slice(0, -extension.length) : value);
  };

  const handleAnalyze = async (): Promise<void> => {
    await onAnalyze();
    await tick();
    if (!mounted) {
      return;
    }
    if (analysis.status === 'ready' && mediaHeading?.isConnected) {
      mediaHeading.focus();
    } else if (urlInput?.isConnected) {
      urlInput.focus();
    }
  };
</script>

<section class="new-download" aria-labelledby="view-title">
  <h1 id="view-title" class="sr-only" tabindex="-1">New Download</h1>

  <form
    class="analyze-form"
    onsubmit={(event) => {
      event.preventDefault();
      void handleAnalyze();
    }}
  >
    <label class="sr-only" for="video-url">Video URL</label>
    <div class="url-control" class:invalid={analysis.status === 'error'}>
      <span aria-hidden="true"><Icon name="link" /></span>
      <input
        bind:this={urlInput}
        id="video-url"
        type="url"
        inputmode="url"
        autocomplete="url"
        placeholder="https://www.youtube.com/watch?v=…"
        value={draft.url}
        aria-describedby="url-help"
        aria-invalid={analysis.status === 'error'}
        disabled={analysis.status === 'loading'}
        oninput={(event) => onSetUrl(event.currentTarget.value)}
      />
      {#if draft.url.length > 0 && analysis.status !== 'loading'}
        <button
          class="clear-button"
          type="button"
          aria-label="Clear video URL"
          onclick={() => {
            onSetUrl('');
            urlInput.focus();
          }}
        >
          <Icon name="close" size={18} />
        </button>
      {/if}
      {#if analysis.status === 'loading'}
        <button class="analyze-button" type="button" onclick={onCancelAnalysis}> Cancel </button>
      {:else}
        <button class="analyze-button" type="submit">Analyze</button>
      {/if}
    </div>
    <p id="url-help" class="sr-only">
      Press Enter or choose Analyze. Public, on-demand videos are supported.
    </p>
  </form>

  {#if analysis.error !== null}
    <div class="inline-alert" role="alert">
      <strong>
        {analysis.error.code === 'invalid-request' ? 'Check the URL' : 'Action needed'}
      </strong>
      <span>{analysis.error.message}</span>
    </div>
  {/if}

  {#if analysis.status === 'loading'}
    <div class="analysis-loading" role="status">
      <div class="skeleton thumbnail-skeleton"></div>
      <div class="loading-copy">
        <div class="skeleton title-skeleton"></div>
        <div class="skeleton line-skeleton"></div>
        <p>Checking metadata and compatible formats…</p>
      </div>
    </div>
  {:else if media !== null}
    <div class="download-workspace">
      <div class="media-and-formats">
        <header class="media-summary">
          <div class="thumbnail-frame">
            {#if thumbnail !== null && failedThumbnailId !== media.id}
              <img
                src={thumbnail}
                alt=""
                width="320"
                height="180"
                referrerpolicy="no-referrer"
                onerror={() => {
                  failedThumbnailId = media?.id ?? '';
                }}
              />
            {:else}
              <span aria-label="Thumbnail unavailable">No preview</span>
            {/if}
            <span class="duration">{formatDuration(media.duration_ms)}</span>
          </div>
          <div class="media-copy">
            <h2 bind:this={mediaHeading} tabindex="-1">{media.title}</h2>
            <p>
              {formatCount(media.view_count)}
              {#if media.upload_date !== null}
                <span aria-hidden="true"> · </span>{media.upload_date}
              {/if}
            </p>
          </div>
        </header>

        {#if media.warnings.length > 0}
          <ul class="analysis-warnings" aria-label="Analysis warnings">
            {#each media.warnings as warning (warning)}
              <li>{warning}</li>
            {/each}
          </ul>
        {/if}

        <div class="format-tabs" role="tablist" aria-label="Output type">
          <button
            id="mp3-tab"
            type="button"
            role="tab"
            aria-selected={draft.outputKind === 'mp3'}
            aria-controls="formats-panel"
            tabindex={draft.outputKind === 'mp3' ? 0 : -1}
            onclick={() => onSetOutputKind('mp3')}
            onkeydown={(event) => {
              if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
                event.preventDefault();
                onSetOutputKind('mp4');
                document.getElementById('mp4-tab')?.focus();
              }
            }}>MP3</button
          >
          <button
            id="mp4-tab"
            type="button"
            role="tab"
            aria-selected={draft.outputKind === 'mp4'}
            aria-controls="formats-panel"
            tabindex={draft.outputKind === 'mp4' ? 0 : -1}
            onclick={() => onSetOutputKind('mp4')}
            onkeydown={(event) => {
              if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
                event.preventDefault();
                onSetOutputKind('mp3');
                document.getElementById('mp3-tab')?.focus();
              }
            }}>MP4</button
          >
        </div>

        <div
          id="formats-panel"
          class="formats-panel"
          role="tabpanel"
          aria-labelledby={`${draft.outputKind}-tab`}
        >
          <h3>Available formats</h3>
          {#if formats.length === 0}
            <div class="format-empty" role="status">
              <strong>{draft.outputKind.toUpperCase()} is unavailable for this video.</strong>
              <span>Choose the other output type or analyze a different video.</span>
            </div>
          {:else}
            <fieldset class="format-list">
              <legend class="sr-only">Choose a {draft.outputKind.toUpperCase()} format</legend>
              {#each formats as format (format.kind === 'mp3' ? `mp3:${format.bitrate_kbps}:${format.source.format_id}` : `mp4:${format.height}:${format.video_source.format_id}:${format.audio_source.format_id}`)}
                <label class="format-row" class:selected={selectedFormat === format}>
                  <input
                    type="radio"
                    name="format"
                    checked={selectedFormat === format}
                    onchange={() => onSelectOutput(outputFor(format))}
                  />
                  <span class="radio-mark" aria-hidden="true"></span>
                  <span class="format-name">{formatFormat(format)}</span>
                  <span
                    class:unknown-size={formatEstimatedSize(format).includes('Unknown') ||
                      format.kind === 'mp3'}
                    class="format-size"
                  >
                    {formatEstimatedSize(format)}
                  </span>
                </label>
              {/each}
            </fieldset>
          {/if}
        </div>
      </div>

      <aside class="output-panel" aria-labelledby="output-title">
        <h2 id="output-title">Output</h2>
        <div class="field">
          <label for="output-name">File name</label>
          <div class="filename-control">
            <input
              id="output-name"
              type="text"
              value={displayFilename}
              maxlength="185"
              placeholder="Use the video title automatically"
              oninput={(event) => updateFilename(event.currentTarget.value)}
            />
          </div>
        </div>
        <div class="field">
          <label for="destination-display">Save to</label>
          <div class="destination-control">
            <div class="destination-input">
              <Icon name="folder" size={18} />
              <input
                id="destination-display"
                type="text"
                readonly
                value={draft.destination}
                placeholder="Choose a folder"
              />
            </div>
            <button
              type="button"
              disabled={busyActions.includes('choose-destination')}
              onclick={() => void onChooseDestination()}
            >
              {busyActions.includes('choose-destination') ? 'Opening…' : 'Change'}
            </button>
          </div>
        </div>
        <dl class="output-summary">
          <div>
            <dt>Format</dt>
            <dd>{selectedFormat === null ? 'Not selected' : formatFormat(selectedFormat)}</dd>
          </div>
          <div>
            <dt>Estimated size</dt>
            <dd>{selectedFormat === null ? '—' : formatEstimatedSize(selectedFormat)}</dd>
          </div>
        </dl>
        <button
          class="primary-action"
          type="button"
          disabled={selectedFormat === null || busyActions.includes('enqueue')}
          onclick={() => void onEnqueue()}
        >
          {busyActions.includes('enqueue') ? 'Adding to queue…' : 'Start download'}
        </button>
      </aside>
    </div>
  {/if}
</section>
