import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it } from 'vitest';
import { axe } from 'vitest-axe';

import App from './App.svelte';
import type { AnalyzeResponseDto, BootstrapStateDto } from './lib/ipc/generated';
import {
  analysisFixture,
  bootstrapFixture,
  FixtureDesktopClient,
  fixtureError,
} from './lib/testing/fixtures';

afterEach(() => {
  cleanup();
});

const renderApp = (client = new FixtureDesktopClient()) => {
  const result = render(App, { client });
  return { client, ...result };
};

const expectNoA11yViolations = async (container: HTMLElement): Promise<void> => {
  const violations = (await axe(container)).violations.map((violation) => ({
    id: violation.id,
    help: violation.help,
    targets: violation.nodes.map((node) => node.target),
  }));
  expect(violations).toEqual([]);
};

const analyzeFixtureVideo = async (): Promise<void> => {
  const user = userEvent.setup();
  const input = await screen.findByLabelText('Video URL');
  await user.type(input, analysisFixture.media.url);
  await user.keyboard('{Enter}');
  await screen.findByRole('heading', {
    level: 2,
    name: analysisFixture.media.title,
  });
};

describe('desktop workspace', () => {
  it('renders recovered state, primary navigation, and the global shelf accessibly', async () => {
    const { container } = renderApp();

    expect(await screen.findByRole('heading', { level: 1, name: 'New Download' })).toBeDefined();
    expect(screen.getByRole('navigation', { name: 'Workspace' })).toBeDefined();
    expect(container.querySelector('.brand-logo')).toHaveAttribute('src', '/brand-mark.svg');
    expect(screen.getByRole('button', { name: /^Queue/ })).toHaveTextContent('3');
    expect(screen.getByRole('button', { name: /2 transfers/ })).toBeDefined();
    await expectNoA11yViolations(container);
  });

  it('supports empty validation, Enter-key analysis, format selection, and enqueue', async () => {
    const { client } = renderApp();
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'Analyze' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Enter a public video URL to analyze.',
    );

    const input = screen.getByLabelText('Video URL');
    await user.type(input, analysisFixture.media.url);
    await user.keyboard('{Enter}');
    expect(
      await screen.findByRole('heading', {
        level: 2,
        name: analysisFixture.media.title,
      }),
    ).toHaveFocus();
    expect(screen.getByRole('tab', { name: 'MP4' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('radio', { name: /1080p/ })).toBeChecked();

    await user.click(screen.getByRole('radio', { name: /720p/ }));
    await user.click(screen.getByRole('tab', { name: 'MP3' }));
    expect(screen.getAllByRole('radio')).toHaveLength(2);
    expect(screen.getByRole('radio', { name: /128 kbps/ })).toBeChecked();
    await user.click(screen.getByRole('radio', { name: /192 kbps/ }));
    await user.click(screen.getByRole('tab', { name: 'MP4' }));
    expect(screen.getByRole('radio', { name: /720p/ })).toBeChecked();
    await user.click(screen.getByRole('tab', { name: 'MP3' }));
    expect(screen.getByRole('radio', { name: /192 kbps/ })).toBeChecked();
    await user.click(screen.getByRole('tab', { name: 'MP4' }));
    await user.click(screen.getByRole('button', { name: 'Start download' }));
    await waitFor(() => {
      expect(client.calls).toContain('enqueue');
    });
  });

  it('uses keyboard tabs and restores focus when navigation changes content', async () => {
    renderApp();
    const user = userEvent.setup();

    const queue = await screen.findByRole('button', { name: /^Queue/ });
    queue.focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('heading', { level: 1, name: 'Queue' })).toHaveFocus();
    expect(screen.getByText('Recovery required')).toBeDefined();

    await user.click(screen.getByRole('button', { name: 'History' }));
    expect(await screen.findByRole('heading', { level: 1, name: 'History' })).toHaveFocus();
    expect(screen.getByText('Output missing')).toBeDefined();

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    expect(await screen.findByRole('heading', { level: 1, name: 'Settings' })).toHaveFocus();
    expect(screen.getByRole('combobox', { name: 'Concurrent downloads' })).toHaveValue('2');
  });

  it('announces analysis failures and exposes unavailable output and unknown size states', async () => {
    const client = new FixtureDesktopClient();
    client.analysisError = fixtureError('Analysis was rejected by the fixture analyzer.');
    renderApp(client);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Video URL'), analysisFixture.media.url);
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Analysis was rejected by the fixture analyzer.',
    );

    cleanup();
    const unavailable: AnalyzeResponseDto = {
      ...analysisFixture,
      media: {
        ...analysisFixture.media,
        formats: analysisFixture.media.formats
          .filter((format) => format.kind === 'mp4')
          .map((format) =>
            format.kind === 'mp4' && format.height === 480
              ? { ...format, estimated_size_bytes: null }
              : format,
          ),
      },
    };
    const unavailableClient = new FixtureDesktopClient();
    unavailableClient.analysisResponse = unavailable;
    renderApp(unavailableClient);
    await analyzeFixtureVideo();
    expect(screen.getByText('Unknown size')).toBeDefined();
    await user.click(screen.getByRole('tab', { name: 'MP3' }));
    expect(screen.getByText('MP3 is unavailable for this video.')).toBeDefined();
  });

  it('keeps native command failures actionable and does not optimistically remove history', async () => {
    const client = new FixtureDesktopClient();
    client.commandError = fixtureError('The native reveal command could not find the output.');
    renderApp(client);
    const user = userEvent.setup();

    await user.click(await screen.findByRole('button', { name: 'History' }));
    await user.click(screen.getByRole('button', { name: 'Reveal' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The native reveal command could not find the output.',
    );
    expect(screen.getByText('Completed city walk.mp4')).toBeDefined();
  });

  it('leaves an in-flight analysis, returns to New Download, and ignores the stale result', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([]));
    let resolveAnalysis: ((response: AnalyzeResponseDto) => void) | undefined;
    client.analysisPromise = new Promise((resolve) => {
      resolveAnalysis = resolve;
    });
    renderApp(client);
    const user = userEvent.setup();
    const input = await screen.findByLabelText('Video URL');
    await user.type(input, analysisFixture.media.url);
    await user.click(screen.getByRole('button', { name: 'Analyze' }));
    expect(await screen.findByRole('button', { name: 'Cancel' })).toBeDefined();

    await user.click(screen.getByRole('button', { name: /^Queue/ }));
    expect(await screen.findByRole('heading', { level: 1, name: 'Queue' })).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'New Download' }));
    expect(await screen.findByLabelText('Video URL')).toHaveValue(analysisFixture.media.url);
    expect(screen.getByRole('button', { name: 'Analyze' })).toBeDefined();
    await waitFor(() => {
      expect(client.calls).toContain('cancel-analysis');
    });

    resolveAnalysis?.(analysisFixture);
    await Promise.resolve();
    await Promise.resolve();
    expect(
      screen.queryByRole('heading', {
        level: 2,
        name: analysisFixture.media.title,
      }),
    ).toBeNull();
  });

  it('lets the user cancel analysis directly and returns the form to idle', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([]));
    let resolveAnalysis: ((response: AnalyzeResponseDto) => void) | undefined;
    client.analysisPromise = new Promise((resolve) => {
      resolveAnalysis = resolve;
    });
    renderApp(client);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Video URL'), analysisFixture.media.url);
    await user.click(screen.getByRole('button', { name: 'Analyze' }));
    await user.click(await screen.findByRole('button', { name: 'Cancel' }));

    expect(await screen.findByRole('button', { name: 'Analyze' })).toBeDefined();
    expect(screen.queryByRole('status')).toBeNull();
    await waitFor(() => {
      expect(client.calls).toContain('cancel-analysis');
    });
    resolveAnalysis?.(analysisFixture);
  });

  it('has no automated accessibility violations in analyzed, queue, history, and settings states', async () => {
    const { container } = renderApp();
    const user = userEvent.setup();

    await analyzeFixtureVideo();
    await expectNoA11yViolations(container);

    for (const name of ['Queue', 'History', 'Settings']) {
      await user.click(
        screen.getByRole('button', {
          name: name === 'Queue' ? /^Queue/ : name,
        }),
      );
      await screen.findByRole('heading', { level: 1, name });
      await expectNoA11yViolations(container);
    }
  });

  it('shows truthful startup state until recovered data is ready', async () => {
    class DelayedClient extends FixtureDesktopClient {
      private resolveConnection: ((unlisten: () => void) => void) | undefined;
      private applySnapshot: ((snapshot: BootstrapStateDto) => void) | undefined;

      override connectJobEvents(
        onSnapshot: (snapshot: BootstrapStateDto) => void,
      ): Promise<() => void> {
        this.applySnapshot = onSnapshot;
        return new Promise((resolve) => {
          this.resolveConnection = resolve;
        });
      }

      finish(): void {
        this.applySnapshot?.(this.snapshot);
        this.resolveConnection?.(() => undefined);
      }
    }

    const client = new DelayedClient(bootstrapFixture([]));
    const { container } = renderApp(client);
    expect(await screen.findByRole('status')).toHaveTextContent('Starting YT Media');
    expect(container.querySelector('.startup-logo')).toHaveAttribute('src', '/brand-mark.svg');
    expect(screen.queryByRole('navigation', { name: 'Workspace' })).toBeNull();
    expect(screen.queryByRole('progressbar')).toBeNull();

    client.finish();
    expect(await screen.findByRole('heading', { level: 1, name: 'New Download' })).toBeDefined();

    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /^Queue/ }));
    expect(screen.getByText('The queue is clear')).toBeDefined();
  });
});
