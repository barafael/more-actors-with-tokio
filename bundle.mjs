import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = dirname(fileURLToPath(import.meta.url));

const marp = spawnSync(
  'marp',
  [
    '--allow-local-files',
    '--html',
    '--pdf-outlines.headings',
    'true',
    '--pdf-outlines.pages',
    'true',
    'slides.md',
    '--html',
    'true',
    '--theme',
    'marp-theme-rhea/rhea.css',
  ],
  {
    cwd: root,
    stdio: 'inherit',
  },
);
if (marp.status !== 0) {
  throw new Error(`marp failed with exit code ${marp.status}`);
}

const escapeAttr = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;');

function inlineGame(src) {
  const file = join(root, src);
  let html = readFileSync(file, 'utf8');
  html = html.replace(
    /<link\s+rel="stylesheet"\s+href="([^"]+)">/g,
    (_, href) => `<style>\n${readFileSync(join(dirname(file), href), 'utf8')}\n</style>`,
  );
  html = html.replace(
    /<script\s+src="([^"]+)"><\/script>/g,
    (_, js) => `<script>\n${readFileSync(join(dirname(file), js), 'utf8')}\n</script>`,
  );
  return html;
}

const deck = readFileSync(join(root, 'slides.html'), 'utf8');
const bundled = deck.replace(
  /(<iframe[^>]*)\bsrc="([^"]*\.html)"([^>]*>)/g,
  (_, pre, src, post) => `${pre}srcdoc="${escapeAttr(inlineGame(src))}"${post}`,
);
writeFileSync(join(root, 'slides.standalone.html'), bundled);
console.log('Wrote slides.standalone.html');
