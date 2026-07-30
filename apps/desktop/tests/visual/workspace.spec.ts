import { expect, test, type Page } from '@playwright/test';

const sizes = [
  { width: 960, height: 640 },
  { width: 1280, height: 800 },
  { width: 1600, height: 1000 },
] as const;

const motions = ['no-preference', 'reduce'] as const;

const states = [
  'new-download',
  'queue',
  'history',
  'settings',
  'active-shelf',
  'errors',
  'interrupted',
] as const;

type VisualState = (typeof states)[number];

test('first paint is the dark startup surface before frontend JavaScript runs', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.route('**/src/main.ts', async (route) => route.abort());
  await page.goto('/');

  const startup = page.getByRole('status');
  await expect(startup).toHaveText('Starting YT Media');
  await expect(startup).toBeVisible();
  await expect(startup.locator('.startup-fallback-logo')).toHaveAttribute('src', '/brand-mark.svg');
  await expect(startup.locator('.startup-fallback-spinner')).toHaveCount(0);
  expect(await startup.evaluate((element) => getComputedStyle(element).backgroundColor)).toBe(
    'rgb(14, 17, 19)',
  );
  await expect(page).toHaveScreenshot('startup-first-paint-1280x800.png', { fullPage: true });
});

const openState = async (page: Page, state: VisualState): Promise<void> => {
  const fixtureScenario =
    state === 'errors'
      ? 'errors'
      : state === 'interrupted'
        ? 'interrupted'
        : state === 'queue' || state === 'history' || state === 'settings'
          ? state
          : 'default';
  await page.goto(`/visual.html?scenario=${fixtureScenario}`);
  await expect(page.getByRole('heading', { level: 1, name: 'New Download' })).toBeVisible();
  await expect(page.locator('.brand-logo')).toHaveAttribute('src', '/brand-mark.svg');

  if (state === 'new-download') {
    await page.getByLabel('Video URL').fill('https://www.youtube.com/watch?v=dQw4w9WgXcQ');
    await page.getByRole('button', { name: 'Analyze' }).click();
    await expect(page.getByText('City Lights Timelapse', { exact: false }).first()).toBeVisible();
  } else if (state === 'queue' || state === 'interrupted') {
    await page.getByRole('button', { name: /^Queue/ }).click();
    await expect(page.getByRole('heading', { level: 1, name: 'Queue' })).toBeVisible();
  } else if (state === 'history') {
    await page.getByRole('button', { name: 'History' }).click();
    await expect(page.getByRole('heading', { level: 1, name: 'History' })).toBeVisible();
  } else if (state === 'settings') {
    await page.getByRole('button', { name: 'Settings' }).click();
    await expect(page.getByRole('heading', { level: 1, name: 'Settings' })).toBeVisible();
  } else if (state === 'errors') {
    await page.getByLabel('Video URL').fill('https://www.youtube.com/watch?v=dQw4w9WgXcQ');
    await page.getByRole('button', { name: 'Analyze' }).click();
    await expect(page.getByRole('alert')).toBeVisible();
  }
};

for (const size of sizes) {
  for (const motion of motions) {
    for (const state of states) {
      test(`${state} at ${size.width}x${size.height} with ${motion} motion`, async ({ page }) => {
        await page.setViewportSize(size);
        await page.emulateMedia({ reducedMotion: motion });
        await openState(page, state);

        const overflow = await page.evaluate(() => ({
          document: document.documentElement.scrollWidth - window.innerWidth,
          body: document.body.scrollWidth - window.innerWidth,
        }));
        expect(overflow.document).toBeLessThanOrEqual(0);
        expect(overflow.body).toBeLessThanOrEqual(0);
        const shelfBounds = await page.locator('.transfer-shelf').boundingBox();
        expect(shelfBounds).not.toBeNull();
        if (shelfBounds !== null) {
          expect(shelfBounds.height).toBeGreaterThanOrEqual(130);
          expect(Math.ceil(shelfBounds.y + shelfBounds.height)).toBeLessThanOrEqual(size.height);
        }
        const firstTransfer = page.locator('.transfer-row').first();
        if ((await firstTransfer.count()) > 0) {
          const transferBounds = await firstTransfer.boundingBox();
          expect(transferBounds).not.toBeNull();
          if (transferBounds !== null) {
            expect(Math.ceil(transferBounds.y + transferBounds.height)).toBeLessThanOrEqual(
              size.height,
            );
          }
        }

        const motionLabel = motion === 'reduce' ? 'reduced' : 'normal';
        await expect(page).toHaveScreenshot(
          `${state}-${size.width}x${size.height}-${motionLabel}.png`,
          { fullPage: true },
        );
      });
    }
  }
}

test('keyboard navigation, tab behavior, focus restoration, and large text remain usable', async ({
  page,
}) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await page.goto('/visual.html?scenario=long-title');
  await page.getByRole('button', { name: /^Queue/ }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { level: 1, name: 'Queue' })).toBeFocused();

  await page.getByRole('button', { name: 'New Download' }).click();
  const urlInput = page.getByRole('textbox', { name: 'Video URL' });
  await urlInput.fill('https://www.youtube.com/watch?v=dQw4w9WgXcQ');
  await expect(urlInput).toBeFocused();
  expect(await urlInput.evaluate((element) => getComputedStyle(element).boxShadow)).toBe('none');
  expect(
    await urlInput.evaluate(
      (element) => getComputedStyle(element.parentElement as HTMLElement).boxShadow,
    ),
  ).not.toBe('none');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { level: 2, name: /City Lights/ })).toBeFocused();
  await expect(page.getByRole('radio', { name: /1080p/ })).toBeChecked();
  await page.getByRole('radio', { name: /720p/ }).click();
  await page.getByRole('tab', { name: 'MP4' }).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(page.getByRole('tab', { name: 'MP3' })).toBeFocused();
  await expect(page.getByRole('radio', { name: /128 kbps/ })).toBeChecked();
  await page.getByRole('radio', { name: /192 kbps/ }).click();
  await page.getByRole('tab', { name: 'MP4' }).click();
  await expect(page.getByRole('radio', { name: /720p/ })).toBeChecked();
  await page.getByRole('tab', { name: 'MP3' }).click();
  await expect(page.getByRole('radio', { name: /192 kbps/ })).toBeChecked();

  await page.evaluate(() => {
    document.documentElement.style.fontSize = '150%';
  });
  const horizontalOverflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  expect(horizontalOverflow).toBeLessThanOrEqual(0);
  await expect(page.getByRole('button', { name: 'Start download' })).toBeVisible();
});
