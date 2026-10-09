import { expect, test } from '@playwright/test';

test('ping replies', async ({ request }) => {
  const response = await request.get('/api/ping');
  expect(await response.json()).toEqual({ reply: 'pong' });
});

test('widget test connection labels the count', async ({ request }) => {
  const response = await request.post('/api/widget/test', { data: { count: 2 } });
  expect(await response.json()).toEqual({ ok: true, label: 'many' });
});

test('home page greets', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('h1')).toHaveText('Hello pong');
});

test('edge route grades a value', async ({ request }) => {
  const response = await request.get('/api/edge?value=11');
  expect(await response.json()).toEqual({ grade: 'high' });
});
