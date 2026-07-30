import { mount } from 'svelte';

import App from './App.svelte';
import './app.css';
import type { JobDto } from './lib/ipc/generated';
import {
  bootstrapFixture,
  activeJobFixture,
  cancelledJobFixture,
  completedJobFixture,
  failedJobFixture,
  FixtureDesktopClient,
  fixtureError,
  interruptedJobFixture,
  missingJobFixture,
  queuedJobFixture,
} from './lib/testing/fixtures';

const scenario = new URL(window.location.href).searchParams.get('scenario');
const activeJob = activeJobFixture();
const referenceActiveJob: JobDto = {
  ...activeJob,
  state: 'merging',
  progress: activeJob.progress === null ? null : { ...activeJob.progress, stage: 'merging' },
};
const jobs =
  scenario === 'queue'
    ? [referenceActiveJob, queuedJobFixture, interruptedJobFixture]
    : scenario === 'history'
      ? [completedJobFixture, missingJobFixture, failedJobFixture, cancelledJobFixture]
      : scenario === 'interrupted'
        ? [interruptedJobFixture]
        : [referenceActiveJob];
const client = new FixtureDesktopClient(bootstrapFixture(jobs));

if (scenario === 'errors') {
  client.analysisError = fixtureError(
    'The video could not be analyzed. Check that it is public and try again.',
  );
}
if (scenario === 'long-title') {
  client.analysisResponse = {
    ...client.analysisResponse,
    media: {
      ...client.analysisResponse.media,
      title:
        'City Lights Timelapse — a deliberately long localized title that remains readable at large text sizes',
    },
  };
}

mount(App, {
  target: document.getElementById('app') ?? document.body,
  props: { client },
});
