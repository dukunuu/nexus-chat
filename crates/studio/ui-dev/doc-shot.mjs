// Run: node crates/studio/ui-dev/doc-shot.mjs [scenario ...]
// Writes 1440x900 shots of the doc editor to crates/studio/ui-dev/shots/ (gitignored).
import { open, shot, finish } from './doc-harness.mjs';
const only = process.argv.slice(2);
const scenarios = {
  async scroll() {
    const page = await open('dark');
    for (const [name, top] of [['mid', 760], ['end', 99999]]) {
      await page.evaluate(top => { document.querySelector('.page-scroll').scrollTop = top; }, top);
      await page.waitForTimeout(150); await shot(page, `doc-scroll-${name}`);
    }
    await page.close();
  },
  async overview() {
    for (const scheme of ['dark', 'light']) { const page = await open(scheme); await shot(page, `doc-overview-${scheme}`); await page.close(); }
  },
};
for (const [name, run] of Object.entries(scenarios)) if (!only.length || only.includes(name)) await run();
await finish();
