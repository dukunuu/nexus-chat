// Run: node crates/studio/ui-dev/library-shot.mjs [scenario ...]
// Shots of the Library at 1440x900 (and narrower) in both terminal palettes, empty and
// full, filtered, with a card menu open and as a start page, into shots/ (gitignored).
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { open, shot, finish, libraryMock, browser, base, errors } from './doc-harness.mjs';
const here = path.dirname(fileURLToPath(import.meta.url));
const only = process.argv.slice(2);
const full = [libraryMock];
const show = async page => { await page.keyboard.press('Control+o'); await page.locator('#library .lib-card').first().waitFor(); await page.waitForTimeout(450); };
const scenarios = {
  async full() {
    for (const scheme of ['dark', 'light']) {
      const page = await open(scheme, undefined, { scripts: full });
      await show(page);
      await page.mouse.move(520, 300);
      await page.waitForTimeout(250);
      await shot(page, `library-full-${scheme}`);
      await page.close();
    }
  },
  async empty() {
    for (const scheme of ['dark', 'light']) {
      const page = await open(scheme, undefined, { scripts: ['window.__libraryItems = [];', libraryMock] });
      await page.keyboard.press('Control+o');
      await page.locator('#library .lib-empty h2').waitFor();
      await page.waitForTimeout(200);
      await shot(page, `library-empty-${scheme}`);
      await page.close();
    }
  },
  async filtered() {
    for (const scheme of ['dark', 'light']) {
      const page = await open(scheme, undefined, { scripts: full });
      await show(page);
      await page.getByRole('button', { name: /^Decks/ }).click();
      await page.locator('#library .lib-search').fill('re');
      await page.waitForTimeout(250);
      await shot(page, `library-filtered-${scheme}`);
      await page.locator('#library .lib-search').fill('zzz');
      await page.waitForTimeout(150);
      await shot(page, `library-nomatch-${scheme}`);
      await page.close();
    }
  },
  async menu() {
    for (const scheme of ['dark', 'light']) {
      const page = await open(scheme, undefined, { scripts: full });
      await show(page);
      const card = page.locator('#library .lib-card').nth(1);
      await card.hover();
      await card.locator('.lib-more').click();
      await page.getByRole('menuitem', { name: 'Export' }).click();
      await page.waitForTimeout(250);
      await shot(page, `library-menu-${scheme}`);
      await page.close();
    }
  },
  async home() {
    for (const scheme of ['dark', 'light']) {
      const page = await open(scheme, undefined, { scripts: ['window.__libraryHome = true; window.__libraryCurrent = null;', libraryMock] });
      await page.locator('#library .lib-card').first().waitFor();
      await page.waitForTimeout(450);
      await shot(page, `library-home-${scheme}`);
      await page.close();
    }
  },
  async narrow() {
    const page = await open('dark', { width: 720, height: 900 }, { scripts: full });
    await show(page);
    await shot(page, 'library-narrow-dark');
    await page.close();
  },
};
/** Deck mode: the Library button, the project menu and the export menu, against mock.js. */
scenarios.deck = async () => {
  for (const scheme of ['dark', 'light']) {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, colorScheme: scheme, reducedMotion: 'reduce' });
    page.setDefaultTimeout(6000);
    page.on('pageerror', error => errors.push(error.message));
    const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/workspace.json'), 'utf8'));
    await page.addInitScript(value => { window.__studioFixture = value; }, fixture);
    await page.addInitScript({ content: fs.readFileSync(path.join(here, 'mock.js'), 'utf8') });
    await page.addInitScript({ content: libraryMock });
    await page.goto(base);
    await page.waitForFunction(() => document.querySelectorAll('#strip .slide-item').length === 3);
    const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${scheme === 'dark' ? 'default' : 'light'}.json`), 'utf8'));
    await page.evaluate(colors => window.studioAsyncEvent({ event: 'Theme', colors }), colors);
    const sent = () => page.evaluate(() => window.__library.commands);
    await shot(page, `library-deck-bar-${scheme}`);
    // The Library button and Cmd/Ctrl+O.
    await page.locator('#open-library').click();
    await page.locator('#library .lib-card').first().waitFor();
    assert.equal(await page.locator('#topbar').evaluate(n => n.inert), true);
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('#library').isVisible(), false);
    await page.keyboard.press('Control+o');
    assert.equal(await page.locator('#library').isVisible(), true);
    await page.keyboard.press('Control+o');
    // Opening a file from the library returns to the deck.
    await page.locator('#open-library').click();
    await page.locator('#library .lib-card').nth(1).click();
    await page.locator('#library').waitFor({ state: 'hidden' });
    assert.deepEqual((await sent()).find(c => c.command === 'LibraryOpen'), { command: 'LibraryOpen', path: 'decks/launch.deck.json' });
    // The project menu: native Open and Save a copy, no typed paths.
    await page.locator('#deck-title').click();
    assert.deepEqual(await page.locator('#deck-menu button').allTextContents(), ['Rename', 'New deck', 'New doc', 'Open…', 'Save a copy…']);
    await shot(page, `library-deck-menu-${scheme}`);
    await page.locator('#open-deck').click();
    await page.waitForTimeout(80);
    assert.equal(await page.locator('dialog[open]').count(), 0, 'Open… is the native dialog');
    assert.deepEqual((await sent()).filter(c => c.command === 'OpenDialog'), [{ command: 'OpenDialog' }]);
    await page.locator('#deck-title').click();
    await page.locator('#save-copy').click();
    await page.locator('#status', { hasText: 'Saved a copy: copy.deck.json with 2 assets' }).waitFor();
    // Export asks where; the old behavior is the last item.
    await page.locator('#export').click();
    assert.equal(await page.locator('#export-quick').getAttribute('aria-checked'), 'false');
    await shot(page, `library-deck-export-${scheme}`);
    await page.getByRole('menuitem', { name: 'PDF' }).click();
    await page.locator('#status', { hasText: 'Exported Export.pdf' }).waitFor();
    assert.deepEqual((await sent()).filter(c => c.command === 'ExportAs').at(-1), { command: 'ExportAs', format: 'pdf' });
    await page.locator('#export').click();
    await page.locator('#export-quick').click();
    assert.equal(await page.locator('#export-quick').getAttribute('aria-checked'), 'true');
    await page.getByRole('menuitem', { name: 'PDF' }).click();
    assert.deepEqual((await page.evaluate(() => window.__mock.commands.filter(c => c.command === 'Export').at(-1))), { command: 'Export', format: 'pdf' });
    // Add template… is the native dialog.
    await page.locator('#deck-design').click();
    await page.locator('#add-template').click();
    await page.waitForFunction(() => window.__library.commands.some(c => c.command === 'PickTemplate'));
    // A toast raised while the library is open shows above it.
    await page.locator('#open-library').click();
    await page.locator('#library .lib-card').first().waitFor();
    const card = page.locator('#library .lib-card').nth(1);
    await card.hover(); await card.locator('.lib-more').click();
    await page.getByRole('menuitem', { name: 'Export' }).click();
    await page.locator('.lib-menu .sub', { hasText: 'PowerPoint' }).click();
    await page.locator('.lib-toast', { hasText: 'Exported' }).waitFor();
    await shot(page, `library-deck-toast-${scheme}`);
    await page.close();
  }
};
for (const [name, run] of Object.entries(scenarios)) if (!only.length || only.includes(name)) await run();
await finish();
