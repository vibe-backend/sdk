// Offline link check for site/dist: every relative href/src must resolve to a file,
// and every #fragment must match an id in the target page. Unsafe schemes fail.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dist = path.join(path.dirname(fileURLToPath(import.meta.url)), 'dist');
if (!fs.existsSync(dist)) { console.error('dist/ not found; run npm run build first'); process.exit(1); }
const files = fs.readdirSync(dist, { recursive: true }).map(String).filter((f) => f.endsWith('.html'));
const ids = new Map();
const idsOf = (abs) => {
  if (!ids.has(abs)) ids.set(abs, new Set([...fs.readFileSync(abs, 'utf8').matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
  return ids.get(abs);
};
const unescape = (s) => s.replace(/&amp;/g, '&').replace(/&quot;/g, '"').replace(/&lt;/g, '<').replace(/&gt;/g, '>');
let bad = 0, checked = 0;
for (const f of files) {
  const abs = path.join(dist, f);
  const html = fs.readFileSync(abs, 'utf8');
  for (const [, attr, raw] of html.matchAll(/\s(href|src)="([^"]*)"/g)) {
    const url = unescape(raw);
    checked++;
    if (/^(https?:|mailto:)/i.test(url)) continue;
    if (/^[a-z][a-z0-9+.-]*:/i.test(url) || url.startsWith('//')) { console.error(`${f}: disallowed URL ${attr}="${url}"`); bad++; continue; }
    const [pq, frag] = url.split('#');
    const p = pq.split('?')[0];
    const target = p ? path.resolve(path.dirname(abs), decodeURIComponent(p)) : abs;
    if ((target !== dist && !target.startsWith(dist + path.sep)) || !fs.existsSync(target) || !fs.statSync(target).isFile()) { console.error(`${f}: broken link ${url}`); bad++; continue; }
    if (frag && target.endsWith('.html') && !idsOf(target).has(decodeURIComponent(frag))) { console.error(`${f}: missing anchor ${url}`); bad++; }
  }
}
console.log(`Checked ${checked} links in ${files.length} pages: ${bad} problem(s).`);
process.exit(bad ? 1 : 0);
