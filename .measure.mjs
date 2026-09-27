import puppeteer from 'puppeteer-core';
import fs from 'fs';
const out = process.argv[2];
const wait = ms => new Promise(r => setTimeout(r, ms));
const b = await puppeteer.launch({ executablePath: '/usr/bin/chromium', headless: true, args: ['--no-sandbox'] });
const p = await b.newPage();
await p.setViewport({ width: 1920, height: 1080 });
const files = fs.readdirSync('export').filter(f => /^slide-\d+-.*\.html$/.test(f)).sort((a, b) => parseInt(a.split('-')[1]) - parseInt(b.split('-')[1]));
for (const f of files) {
  await p.goto('http://127.0.0.1:8765/' + f, { waitUntil: 'networkidle0' });
  await wait(900);
  const m = await p.evaluate(() => {
    const r = s => { const e = document.querySelector(s); return e ? e.getBoundingClientRect() : null; };
    const d = r('.diagram'), lede = r('.slide > p.dim'), note = r('.bd-note, .race-result, .timer-readout, .cy-verdict, .ut-note'), foot = r('.game-footer');
    const rd = x => x && Math.round(x);
    return { box: d && [rd(d.top), rd(d.bottom)], lede: lede && rd(lede.bottom), noteBottom: note && rd(note.bottom), footTop: foot && rd(foot.top), docH: document.documentElement.scrollHeight };
  });
  console.log(f.replace('.html','').padEnd(32), JSON.stringify(m));
  if (out) await p.screenshot({ path: `${out}/L-${f.replace('.html', '')}.png` });
}
await b.close();
