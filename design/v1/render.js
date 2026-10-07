// Renders the mock-ups to PNG at 2x: every [data-shot] element of the given
// pages becomes <out>/<data-shot>.png, and every crop in its data-crops
// ("name:x,y,w,h;…", frame coordinates) becomes <out>/<name>.png.
//
//   NODE_PATH=/path/to/node_modules node design/v1/render.js OUT_DIR 01-downtimes.html [...]
//
// Needs playwright-core and a Chromium (PLAYWRIGHT_BROWSERS_PATH, or
// CHROMIUM=/path/to/chrome).
const path = require('path');
const fs = require('fs');
const { chromium } = require('playwright-core');

async function main() {
  const [out, ...pages] = process.argv.slice(2);
  fs.mkdirSync(out, { recursive: true });
  const executablePath = process.env.CHROMIUM || (() => {
    const base = process.env.PLAYWRIGHT_BROWSERS_PATH || '/opt/pw-browsers';
    const dir = fs.readdirSync(base).find((d) => /^chromium-\d+$/.test(d));
    return path.join(base, dir, 'chrome-linux', 'chrome');
  })();
  const browser = await chromium.launch({ executablePath });
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 2 });
  for (const page of pages) {
    const tab = await context.newPage();
    const errors = [];
    tab.on('pageerror', (e) => errors.push(e.message));
    tab.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
    await tab.goto('file://' + path.resolve(__dirname, page));
    await tab.evaluate(() => document.fonts.ready);
    await tab.waitForTimeout(150);
    if (errors.length) console.error(page, errors);
    const shots = await tab.$$('[data-shot]');
    for (const el of shots) {
      const name = await el.getAttribute('data-shot');
      await el.screenshot({ path: path.join(out, name + '.png') });
      console.log(name);
      const crops = (await el.getAttribute('data-crops')) || '';
      const box = await el.evaluate((e) => { const r = e.getBoundingClientRect(); return { x: r.x + window.scrollX, y: r.y + window.scrollY }; });
      for (const spec of crops.split(';').filter(Boolean)) {
        const [cname, rect] = spec.split(':');
        const [x, y, w, h] = rect.split(',').map(Number);
        await tab.screenshot({ path: path.join(out, cname + '.png'), clip: { x: box.x + x, y: box.y + y, width: w, height: h }, fullPage: true });
        console.log('  ' + cname);
      }
    }
    await tab.close();
  }
  await browser.close();
}
main().catch((e) => { console.error(e); process.exit(1); });
