// Static site builder for the vibe-backend product site (English + Traditional Chinese).
//
// Output layout (site/dist):
//   index.html, features.html, ...   language pickers: choose /en/ or /zh-hant/ from localStorage or the browser language
//   en/...          English pages
//   zh-hant/...     Traditional Chinese pages
//
// Docs are generated at build time from every docs/spec-v*.md (one page per version, plus
// docs/spec/latest/ = newest) and docs/best-practices/*.md.
// Translations live in a mirrored tree: docs/<locale>/<same relative path>, for example
// docs/zh-hant/best-practices/rate-limit.md. When a locale has no translation, the original
// is shown with a notice banner in that locale.
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Marked } from 'marked';
import hljs from 'highlight.js';
import sanitizeHtml from 'sanitize-html';
import { LOCALES, DEFAULT_LOCALE } from './src/i18n.mjs';
import { fetchActivity } from './activity.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const out = path.join(here, 'dist');
const src = path.join(here, 'src');
const REPO = 'https://github.com/vibe-backend/sdk';
const SITE_URL = (process.env.SITE_URL || 'https://vibe-backend.github.io/sdk/').replace(/\/?$/, '/');
const locales = Object.keys(LOCALES);

const slugify = (s) => s.toLowerCase().trim()
  .replace(/<[^>]+>/g, '').replace(/[`*_~]/g, '')
  .replace(/[^\p{L}\p{N}\s-]/gu, '').replace(/\s+/g, '-') || 'section';
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

// ---------- markdown ----------
function highlight(text, lang) {
  if (lang && hljs.getLanguage(lang)) {
    try { return hljs.highlight(text, { language: lang, ignoreIllegals: true }).value; } catch { /* fall through */ }
  }
  return esc(text); // unsupported or missing language: plain, escaped text
}

// Only http(s), mailto, relative URLs and #fragments survive; everything else is stripped.
function sanitize(html) {
  return sanitizeHtml(html, {
    allowedTags: sanitizeHtml.defaults.allowedTags.concat(['h1', 'h2', 'img', 'del', 'input', 'span']),
    allowedAttributes: {
      a: ['href', 'title', 'class'], img: ['src', 'alt', 'title', 'referrerpolicy', 'loading'],
      h1: ['id'], h2: ['id'], h3: ['id'], h4: ['id'], h5: ['id'], h6: ['id'],
      code: ['class'], span: ['class'], pre: ['class'], th: ['align'], td: ['align'],
      input: ['type', 'checked', 'disabled'],
    },
    allowedSchemes: ['http', 'https', 'mailto'],
    allowedSchemesByTag: { img: ['http', 'https'] },
    allowProtocolRelative: false,
    transformTags: {
      img: (tagName, attribs) => ({ tagName, attribs: { ...attribs, referrerpolicy: 'no-referrer', loading: 'lazy' } }),
    },
    allowedClasses: { code: ['hljs', /^language-[\w+-]+$/], span: [/^hljs-[\w-]+$/, /^[\w-]+_{1,2}$/], a: ['anchor'] },
  });
}

// docs/<locale>/x.md is a translation of docs/x.md: both map to the same page.
const canonicalPath = (repoPath) => {
  const m = /^docs\/([a-z-]+)\/(.+)$/.exec(repoPath);
  return m && LOCALES[m[1]] ? `docs/${m[2]}` : repoPath;
};

// md files with a generated page -> that page (same locale); other repo paths -> GitHub blob URL.
function rewriteLink(href, sourcePath, fromRel) {
  if (!href || href.startsWith('#') || /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('//')) return href;
  const [p, frag = ''] = href.split(/(?=#)/);
  const repoPath = p.startsWith('/')
    ? path.posix.normalize(p).replace(/^\/+/, '') // repo-root-relative, e.g. /README.md
    : path.posix.normalize(path.posix.join(path.posix.dirname(sourcePath), p));
  if (repoPath.startsWith('..')) return href;
  const page = pageBySource.get(canonicalPath(repoPath));
  if (page) return relHref(fromRel, page) + frag;
  return `${REPO}/blob/main/${repoPath}${frag}`;
}

function render(md, sourcePath, fromRel) {
  const toc = [];
  const seen = new Map();
  const marked = new Marked({
    gfm: true,
    renderer: {
      code({ text, lang }) {
        const l = (lang || '').split(/\s/)[0];
        return `<pre><code class="hljs${l ? ' language-' + esc(l) : ''}">${highlight(text, l)}</code></pre>\n`;
      },
      heading({ tokens, depth }) {
        const inner = this.parser.parseInline(tokens);
        let id = slugify(inner);
        const n = seen.get(id) || 0; seen.set(id, n + 1);
        if (n) id += '-' + n;
        if (depth >= 2 && depth <= 3) toc.push({ depth, id, text: inner.replace(/<[^>]+>/g, '') });
        return `<h${depth} id="${id}"><a class="anchor" href="#${id}">#</a>${inner}</h${depth}>\n`;
      },
      link({ href, title, tokens }) {
        const h = rewriteLink(href || '', sourcePath, fromRel);
        return `<a href="${esc(h)}"${title ? ` title="${esc(title)}"` : ''}>${this.parser.parseInline(tokens)}</a>`;
      },
    },
  });
  return { html: sanitize(marked.parse(md)), toc };
}

function titleOf(md, fallback) {
  const m = /^#\s+(.+)$/m.exec(md);
  return m ? m[1].replace(/[`*_]/g, '').trim() : fallback;
}

// Rough language detection for untranslated originals: mostly CJK text -> zh-hant.
function detectLang(md) {
  const text = md.replace(/```[\s\S]*?```/g, '').replace(/`[^`]*`/g, '');
  const cjk = (text.match(/[\u3400-\u9fff]/g) || []).length;
  const latin = (text.match(/[A-Za-z]+/g) || []).length;
  return cjk > latin ? 'zh-hant' : 'en';
}

// ---------- collect docs ----------
const read = (rel) => fs.readFileSync(path.join(root, rel), 'utf8');
const exists = (rel) => fs.existsSync(path.join(root, rel));
const docs = [];
// Spec versions: docs/spec-vX.Y(.Z).md, newest first (numeric semver sort).
const semver = (v) => v.split('.').map(Number);
const cmpVer = (a, b) => { const x = semver(a), y = semver(b); for (let i = 0; i < Math.max(x.length, y.length); i++) { const d = (y[i] || 0) - (x[i] || 0); if (d) return d; } return 0; };
const specVersions = fs.readdirSync(path.join(root, 'docs')).map((f) => /^spec-v(\d+(?:\.\d+){0,2})\.md$/.exec(f)).filter(Boolean)
  .map((m) => m[1]).sort(cmpVer);
const LATEST = specVersions[0];
for (const v of specVersions) {
  docs.push({ slug: `spec-v${v}`, rel: `docs/spec/v${v}/index.html`, group: 'spec', version: v, isLatest: v === LATEST, source: `docs/spec-v${v}.md` });
}
if (exists('docs/best-practices')) {
  const files = fs.readdirSync(path.join(root, 'docs/best-practices')).filter((f) => f.endsWith('.md'));
  // README.md is the best-practices index page and is listed first.
  files.sort((a, b) => (b.toLowerCase() === 'readme.md') - (a.toLowerCase() === 'readme.md') || a.localeCompare(b));
  for (const f of files) {
    const isIndex = f.toLowerCase() === 'readme.md';
    docs.push({
      slug: isIndex ? 'best-practices' : f.replace(/\.md$/, '').toLowerCase().replace(/[^a-z0-9-]+/g, '-'),
      group: 'bestPractices', isIndex, source: `docs/best-practices/${f}`,
    });
  }
}
const used = new Set(['index', 'spec']);
for (const d of docs) {
  if (!d.rel) { let s = d.slug, i = 2; while (used.has(s)) s = `${d.slug}-${i++}`; d.slug = s; used.add(s); d.rel = `docs/${s}.html`; }
  d.md = read(d.source);
  d.lang = detectLang(d.md);
  d.variants = {};
  for (const loc of locales) {
    const tr = d.source.replace(/^docs\//, `docs/${loc}/`);
    if (exists(tr)) d.variants[loc] = { source: tr, md: read(tr), translated: true };
    else d.variants[loc] = { source: d.source, md: d.md, translated: d.lang === loc };
  }
}
const pageBySource = new Map(docs.map((d) => [d.source, d.rel]));
const LATEST_REL = 'docs/spec/latest/index.html';
// Relative href between two page paths under a locale root.
const relHref = (from, to) => path.posix.relative(path.posix.dirname(from), to) || path.posix.basename(to);

const docTitle = (d, loc) => {
  const t = LOCALES[loc].docs;
  if (d.group === 'spec') return `${t.specTitle} v${d.version}`;
  if (d.isIndex) return t.overview;
  return titleOf(d.variants[loc].md, d.slug);
};

// Cache-busting: ?v=<content hash> for every file in src/assets (GitHub Pages caches for 10 minutes).
const assetVersion = Object.fromEntries(fs.readdirSync(path.join(src, 'assets'), { recursive: true }).map(String)
  .filter((f) => fs.statSync(path.join(src, 'assets', f)).isFile())
  .map((f) => [f.split(path.sep).join('/'), createHash('sha256').update(fs.readFileSync(path.join(src, 'assets', f))).digest('hex').slice(0, 10)]));

// File tree for hand-written pages: "├── name/  # comment" lines -> coloured spans, comments aligned.
function renderTree(text, attrs = '') {
  const rows = text.split('\n').map((line) => {
    const [, guide, name, comment = ''] = /^([│├└─\s]*)(\S+)(?:\s+#\s*(.*))?$/.exec(line) || [null, '', line];
    return { guide, name, comment, depth: Math.round([...guide].length / 4) };
  });
  const label = (/data-label="([^"]*)"/.exec(attrs) || [])[1] || 'File tree';
  attrs = attrs.replace(/\s*data-label="[^"]*"/, '');
  const width = Math.max(...rows.map((r) => [...(r.guide + r.name)].length)) + 2;
  const html = rows.map(({ guide, name, comment }) => {
    const pad = ' '.repeat(Math.max(1, width - [...(guide + name)].length));
    const cls = name.endsWith('/') ? 'dir' : 'file';
    return `<span class="g">${esc(guide)}</span><span class="${cls}">${esc(name)}</span>${comment ? `${pad}<span class="cm"># ${esc(comment)}</span>` : ''}`;
  }).join('\n');
  // Screen readers get a plain nested list; the drawn tree is exposed as one labelled image.
  let list = '', depth = -1;
  for (const r of rows) {
    if (r.depth > depth) list += '<ul>'.repeat(r.depth - depth);
    else list += '</li>' + '</ul></li>'.repeat(depth - r.depth);
    list += `<li><code>${esc(r.name)}</code>${r.comment ? ` – ${esc(r.comment)}` : ''}`;
    depth = r.depth;
  }
  list += '</li>' + '</ul></li>'.repeat(depth) + '</ul>';
  // Merge any class="..." from the source <pre> into ours (avoid a duplicate class attribute).
  const extra = (/\sclass="([^"]*)"/.exec(attrs) || [])[1];
  attrs = attrs.replace(/\s*class="[^"]*"/, '');
  return `<pre class="tree${extra ? ' ' + esc(extra) : ''}"${attrs} role="img" aria-label="${esc(label)}"><code>${html}</code></pre><div class="sr">${list}</div>`;
}

// ---------- layout ----------
const NAV = [['index.html', 'home'], ['features.html', 'features'], ['quick-start.html', 'quickStart'], ['docs/index.html', 'docs']];
const THEME_BOOT = "try{var t=localStorage.getItem('vb-theme');if(t)document.documentElement.dataset.theme=t}catch(e){}document.documentElement.classList.add('js')";
const ICON_THEME = '<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><path fill="currentColor" d="M12 3a9 9 0 1 0 9 9c0-.46-.04-.92-.1-1.36a5.39 5.39 0 0 1-4.4 2.26 5.4 5.4 0 0 1-3.14-9.8c-.44-.06-.9-.1-1.36-.1z"/></svg>';

function layout({ loc, rel, title, body, active, sidebar = '', wide = false, canonicalRel = rel }) {
  const t = LOCALES[loc];
  const depth = rel.split('/').length - 1;
  const toRoot = '../'.repeat(depth + 1);       // dist/
  const toLoc = '../'.repeat(depth);            // dist/<loc>/
  const links = NAV.map(([h, k]) => `<a href="${toLoc}${h}"${active === k ? ' aria-current="page"' : ''}>${t.nav[k]}</a>`).join('');
  // hreflang alternates use the same (versioned, index.html-less) URL as the canonical.
  const canon = canonicalRel.replace(/(^|\/)index\.html$/, '$1');
  const alternates = locales.map((l) => `<link rel="alternate" hreflang="${LOCALES[l].hreflang}" href="${SITE_URL}${l}/${canon}">`).join('')
    + `<link rel="alternate" hreflang="x-default" href="${SITE_URL}${canon}">`;
  const switcher = locales.map((l) => l === loc
    ? `<span class="cur" aria-current="true">${LOCALES[l].short}</span>`
    : `<a href="${toRoot}${l}/${rel}" hreflang="${LOCALES[l].hreflang}" lang="${LOCALES[l].htmlLang}" data-lang="${l}" title="${LOCALES[l].name}">${LOCALES[l].short}</a>`).join('');
  return `<!doctype html>
<html lang="${t.htmlLang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(title)} · vibe-backend</title>
<meta name="description" content="${esc(t.description)}">
<meta name="color-scheme" content="light dark">
<link rel="canonical" href="${SITE_URL}${loc}/${canon}">${alternates}
<script>${THEME_BOOT}</script>
<link rel="stylesheet" href="${toRoot}assets/style.css?v=${assetVersion['style.css']}">
<script defer src="${toRoot}assets/site.js?v=${assetVersion['site.js']}"></script>
</head><body>
<header class="top"><div class="bar">
<a class="brand" href="${toLoc}index.html">vibe<span>-backend</span></a>
<button class="menu" aria-expanded="false" aria-controls="nav" aria-label="${t.menu}"><span></span><span></span></button>
<nav id="nav">${links}<a href="${REPO}">${t.nav.github}</a></nav>
<div class="tools"><div class="lang" role="group" aria-label="${t.switchTo}">${switcher}</div>
<button class="theme" type="button" aria-label="${t.theme}" title="${t.theme}">${ICON_THEME}</button></div>
</div></header>
<div class="${sidebar ? 'docs-wrap' : wide ? 'page wide' : 'page'}">${sidebar}<main>${body}</main></div>
<footer><div>${t.footer}<br><span class="maintainer">${esc(t.maintainedBy)}</span></div><a href="${REPO}">${t.source}</a></footer>
</body></html>`;
}

// Root-level pickers for every page path: localStorage choice > navigator.languages > en.
// Language mapping (product decision, kept as specified for #8): zh-Hant / zh-TW / zh-HK / zh-MO and a bare
// "zh" go to zh-hant; zh-Hans / zh-CN / zh-SG and everything else go to en. A stored choice always wins.
function picker(rel) {
  const depth = rel.split('/').length - 1;
  const up = '../'.repeat(depth);
  const links = locales.map((l) => `<a href="${up}${l}/${rel}" hreflang="${LOCALES[l].hreflang}">${LOCALES[l].name}</a>`).join(' · ');
  const alternates = locales.map((l) => `<link rel="alternate" hreflang="${LOCALES[l].hreflang}" href="${SITE_URL}${l}/${rel}">`).join('');
  const js = `(function(){var L=${JSON.stringify(locales)},p=null;try{p=localStorage.getItem('vb-lang')}catch(e){}
if(L.indexOf(p)<0){p='${DEFAULT_LOCALE}';var n=navigator.languages||[navigator.language||''];for(var i=0;i<n.length;i++){var x=String(n[i]).toLowerCase();
if(/^zh(-hant|-tw|-hk|-mo)?(\\b|-|$)/.test(x)&&!/^zh-(hans|cn|sg)/.test(x)){p='zh-hant';break}if(/^en\\b/.test(x)){break}}}
location.replace('${up}'+p+'/${rel}'+location.search+location.hash)})()`;
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>vibe-backend</title>${alternates}<link rel="alternate" hreflang="x-default" href="${SITE_URL}${rel === 'index.html' ? '' : rel}">
<script>${js}</script></head><body style="font-family:system-ui,sans-serif;padding:2rem">
<p>vibe-backend — ${links}</p></body></html>`;
}

// Sidebar: one "Spec" entry (-> latest; older versions via the version picker) + other doc groups.
function docsSidebar(loc, fromRel, current) {
  const t = LOCALES[loc].docs;
  const groups = {};
  for (const d of docs) if (d.group !== 'spec' || d.isLatest) (groups[d.group] ||= []).push(d);
  let s = `<aside class="sidebar"><a href="${relHref(fromRel, 'docs/index.html')}"${current === 'index' ? ' aria-current="page"' : ''}>${t.home}</a>`;
  for (const [g, items] of Object.entries(groups)) {
    s += `<h4>${esc(t[g])}</h4>` + items.map((d) => {
      const isCur = d.group === 'spec' ? current?.group === 'spec' : d === current;
      const href = d.group === 'spec' ? relHref(fromRel, LATEST_REL) : relHref(fromRel, d.rel);
      return `<a href="${href}"${isCur ? ' aria-current="page"' : ''}>${esc(docTitle(d, loc))}</a>`;
    }).join('');
  }
  return s + '</aside>';
}

// Spec version picker (segmented control) + latest/older banner.
function specChrome(loc, fromRel, d) {
  const t = LOCALES[loc].docs;
  const specs = docs.filter((x) => x.group === 'spec');
  const seg = specs.map((x) => x === d
    ? `<span class="cur" aria-current="page">v${esc(x.version)}${x.isLatest ? ` <em>${esc(t.latest)}</em>` : ''}</span>`
    : `<a href="${relHref(fromRel, x.isLatest ? LATEST_REL : x.rel)}">v${esc(x.version)}${x.isLatest ? ` <em>${esc(t.latest)}</em>` : ''}</a>`).join('');
  const picker = `<nav class="versions" aria-label="${esc(t.versions)}">${seg}</nav>`;
  const banner = d.isLatest ? '' : `<div class="notice old" role="note"><strong>${esc(t.olderVersion)}</strong> ${esc(t.olderText(d.version, LATEST))} <a href="${relHref(fromRel, LATEST_REL)}">${esc(t.goLatest(LATEST))}</a></div>`;
  return picker + banner;
}

// Locale-level redirect page (e.g. old docs/spec.html -> docs/spec/latest/).
// The canonical is absolute and points at the versioned URL; the redirect page itself is noindex.
function redirectPage(loc, fromRel, toRel, canonicalRel) {
  const href = relHref(fromRel, toRel);
  const canon = `${SITE_URL}${loc}/${canonicalRel.replace(/(^|\/)index\.html$/, '$1')}`;
  return `<!doctype html><html><head><meta charset="utf-8"><title>vibe-backend</title><meta name="robots" content="noindex"><meta http-equiv="refresh" content="0; url=${href}">`
    + `<link rel="canonical" href="${canon}"><script>location.replace(${JSON.stringify(href)}+location.hash)</script></head>`
    + `<body><p><a href="${href}">${href}</a></p></body></html>`;
}

// ---------- development activity (omitted when unavailable) ----------
const activity = await fetchActivity(process.env.GITHUB_TOKEN);
// Compact number for axis labels: 1200 -> "1.2k".
const compact = (n, locale) => new Intl.NumberFormat(locale, { notation: 'compact', maximumFractionDigits: 2 }).format(n);

// Inline SVG: stacked additions/deletions per day on a square-root scale (so one huge day doesn't
// flatten the rest), merged-PR counts as dots on a linear right-hand axis.
function activityChart(loc) {
  const L = LOCALES[loc]; const t = L.activity;
  const nf = new Intl.NumberFormat(L.numberLocale);
  const num = (n) => nf.format(Number.isSafeInteger(n) && n >= 0 ? n : 0);
  const [yy, mo] = activity.month.split('-').map(Number);
  const dayNums = activity.rows.map((r) => Number(r.day.slice(8)));
  const byDay = new Map(activity.rows.map((r) => [Number(r.day.slice(8)), r]));
  const todayN = Number(activity.today.slice(8));
  const n = dayNums.length;

  const W = 720, H = 300, ml = 52, mr = 34, mt = 34, mb = 36;
  const pw = W - ml - mr, ph = H - mt - mb;
  // Few days: fixed-width slots centred in the plot (no full-width slab); many days: fill the width.
  const slot = Math.min(pw / n, 72), bw = Math.min(40, Math.max(4, slot * 0.62));
  const x0 = ml + (pw - slot * n) / 2;
  const fullLabels = n <= 7;
  const maxLines = Math.max(1, ...activity.rows.map((r) => r.additions + r.deletions));
  const maxPrs = Math.max(1, ...activity.rows.map((r) => r.prs));
  // nice ceiling for lines (sqrt scale) and PRs (linear)
  const niceCeil = (v) => { const p = 10 ** Math.floor(Math.log10(v)); return Math.max(1, Math.ceil([1, 2, 2.5, 5, 10].map((m) => m * p).find((x) => x >= v))); };
  const topLines = niceCeil(maxLines), topPrs = Math.max(2, Math.ceil(maxPrs * 1.15));
  const yL = (v) => mt + ph - Math.sqrt(v / topLines) * ph;
  const yP = (v) => mt + ph - (v / topPrs) * ph;
  const r1 = (v) => Math.round(v * 10) / 10;

  // Integer-only ticks (small maxima would otherwise produce 0.1, 2.5, …).
  const ticks = [...new Set([0, 0.01, 0.1, 0.25, 0.5, 1].map((f) => topLines * f).filter(Number.isInteger))];
  let svg = '';
  for (const v of ticks) {
    const y = r1(yL(v));
    svg += `<line class="grid" x1="${ml}" x2="${W - mr}" y1="${y}" y2="${y}"/><text class="tick" x="${ml - 8}" y="${y}" text-anchor="end" dy="0.32em">${esc(compact(v, L.numberLocale))}</text>`;
  }
  for (let v = 0; v <= topPrs; v += Math.max(1, Math.ceil(topPrs / 4))) {
    svg += `<text class="tick prs-tick" x="${W - mr + 8}" y="${r1(yP(v))}" dy="0.32em">${v}</text>`;
  }
  let bars = '', dots = '', labels = '';
  dayNums.forEach((d, i) => {
    const cx = x0 + slot * (i + 0.5);
    const r = byDay.get(d);
    const keyLabel = fullLabels || i === 0 || d % 5 === 0 || d === todayN;
    labels += `<text class="day${keyLabel ? '' : ' minor'}${d === todayN ? ' today' : ''}" x="${r1(cx)}" y="${mt + ph + 6}" dy="1em" text-anchor="middle">${esc(fullLabels || i === 0 ? t.day(mo, d) : String(d))}</text>`;
    if (!r) { bars += `<rect class="empty" fill="#d2d2d7" x="${r1(cx - bw / 2)}" y="${mt + ph - 2}" width="${r1(bw)}" height="2" rx="1"/>`; return; }
    const total = r.additions + r.deletions;
    const label = esc(t.dayLabel(t.day(mo, d), num(r.additions), num(r.deletions), num(r.prs)));
    if (total > 0) {
      const top = yL(total), hTot = mt + ph - top;
      const hDel = hTot * (r.deletions / total);
      bars += `<g role="listitem" aria-label="${label}"><title>${label}</title>`
        + (hTot - hDel > 0 ? `<rect class="add" fill="#30d158" x="${r1(cx - bw / 2)}" y="${r1(top)}" width="${r1(bw)}" height="${r1(hTot - hDel)}" rx="2"/>` : '')
        + (hDel > 0 ? `<rect class="del" fill="#ff453a" x="${r1(cx - bw / 2)}" y="${r1(mt + ph - hDel)}" width="${r1(bw)}" height="${r1(hDel)}" rx="2"/>` : '')
        + (fullLabels
          ? `<text class="peak" x="${r1(cx)}" y="${r1(top - 8)}" text-anchor="middle"><tspan class="v-add" fill="#1f9d55">+${esc(num(r.additions))}</tspan> <tspan class="v-del" fill="#d93a3a">−${esc(num(r.deletions))}</tspan></text>`
          : `<text class="val${total === maxLines ? ' peak' : ''}" x="${r1(cx)}" y="${r1(top - 6)}" text-anchor="middle">${esc(compact(total, L.numberLocale))}</text>`)
        + '</g>';
    } else {
      bars += `<rect class="empty" fill="#d2d2d7" x="${r1(cx - bw / 2)}" y="${mt + ph - 2}" width="${r1(bw)}" height="2" rx="1"/>`;
    }
    if (r.prs > 0) dots += `<circle class="prs" fill="#ffffff" stroke="#0071e3" stroke-width="2.5" cx="${r1(cx)}" cy="${r1(yP(r.prs))}" r="4.5"><title>${label}</title></circle>`
      + (fullLabels ? `<text class="prs-val" x="${r1(cx + bw / 2 + 8)}" y="${r1(yP(r.prs))}" dy="0.32em">${esc(t.prsShort(num(r.prs)))}</text>` : '');
  });
  const busiestRow = activity.rows.reduce((b, r) => (r.additions + r.deletions > (b ? b.additions + b.deletions : 0) ? r : b), null);
  const busiest = busiestRow ? t.busiest(t.day(mo, Number(busiestRow.day.slice(8))), num(busiestRow.additions), num(busiestRow.deletions), num(busiestRow.prs)) : '';
  const monthName = t.monthName(yy, mo);
  const title = esc(t.chartTitle(monthName));
  const desc = esc(t.chartDesc(num(activity.totals.prs), num(activity.totals.additions), num(activity.totals.deletions), busiest));
  return `<svg class="chart" viewBox="0 0 ${W} ${H}" role="img" aria-labelledby="act-chart-title act-chart-desc" preserveAspectRatio="xMidYMid meet">`
    + `<title id="act-chart-title">${title}</title><desc id="act-chart-desc">${desc}</desc>`
    + `<g class="axis">${svg}<line class="base" x1="${ml}" x2="${W - mr}" y1="${mt + ph}" y2="${mt + ph}"/></g>`
    + `<g role="list">${bars}</g><g class="dots">${dots}</g><g class="days" aria-hidden="true">${labels}</g></svg>`;
}

function activitySection(loc) {
  if (!activity) return '';
  const t = LOCALES[loc].activity;
  const nf = new Intl.NumberFormat(LOCALES[loc].numberLocale);
  const num = (n) => (Number.isSafeInteger(n) && n >= 0 ? nf.format(n) : '0');
  const rows = activity.rows.map((r) => {
    const [, mm, dd] = r.day.split('-').map(Number);
    return `<tr${r.prs ? '' : ' class="zero"'}><th scope="row"><time datetime="${esc(r.day)}">${esc(t.day(mm, dd))}</time></th><td class="n">${num(r.prs)}</td>`
      + `<td class="n add">+${num(r.additions)}</td><td class="n del">−${num(r.deletions)}</td><td class="n">${num(r.additions + r.deletions)}</td></tr>`;
  }).join('');
  const [yy, mo] = activity.month.split('-').map(Number);
  const repos = activity.repos.map((r) => `<a href="https://github.com/${esc(r)}">${esc(r.split('/')[1])}</a>`).join(', ');
  const tot = activity.totals;
  return `<section class="activity" aria-labelledby="activity-title"><div class="section-head reveal"><p class="eyebrow">${esc(t.eyebrow)}</p><h2 id="activity-title">${esc(t.title(yy, mo))}</h2><p class="sub">${esc(t.since(t.day(mo, Number(activity.startDay.slice(8)))))}</p></div>`
    + `<div class="act-stats reveal"><div><b>${num(tot.prs)}</b><span>${esc(t.statPrs)}</span></div><div class="add"><b>+${num(tot.additions)}</b><span>${esc(t.statAdd)}</span></div><div class="del"><b>−${num(tot.deletions)}</b><span>${esc(t.statDel)}</span></div></div>`
    + `<figure class="chart-card reveal">${activityChart(loc)}`
    + `<figcaption><span class="lg add">${esc(t.legendAdd)}</span><span class="lg del">${esc(t.legendDel)}</span><span class="lg prs">${esc(t.legendPrs)}</span><small>${esc(t.scaleNote)}</small></figcaption></figure>`
    + `<details class="act-table reveal"><summary>${esc(t.showTable)}</summary><div class="table-wrap"><table class="activity-table"><thead><tr><th scope="col">${esc(t.date)}</th><th scope="col" class="n">${esc(t.prs)}</th><th scope="col" class="n">${esc(t.additions)}</th><th scope="col" class="n">${esc(t.deletions)}</th><th scope="col" class="n">${esc(t.total)}</th></tr></thead><tbody>${rows}</tbody></table></div></details>`
    + `<p class="note reveal">${esc(t.scope(activity.org, activity.repos.length))} ${repos}. ${esc(t.method(activity.generatedAt.slice(0, 16).replace('T', ' ') + ' UTC'))}</p></section>`;
}

// ---------- write ----------
const write = (rel, html) => { const f = path.join(out, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, html); };
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
fs.cpSync(path.join(src, 'assets'), path.join(out, 'assets'), { recursive: true });
fs.writeFileSync(path.join(out, '.nojekyll'), '');

const handPages = [['index.html', 'index', 'home'], ['features.html', 'features', 'features'], ['quick-start.html', 'quick-start', 'quickStart']];
const allRels = [...handPages.map(([f]) => f), 'docs/index.html', 'docs/spec.html', ...(LATEST ? [LATEST_REL] : []), ...docs.map((d) => d.rel)];
let count = 0;

for (const loc of locales) {
  const t = LOCALES[loc];
  for (const [file, key, active] of handPages) {
    let body = fs.readFileSync(path.join(src, 'pages', loc, file), 'utf8').replace('<!-- ACTIVITY -->', () => activitySection(loc));
    body = body.replace(/<pre data-tree([^>]*)>\n?([\s\S]*?)\n?<\/pre>/g, (_, attrs, c) => renderTree(c, attrs));
    body = body.replace(/<pre data-lang="([\w-]+)"([^>]*)>([\s\S]*?)<\/pre>/g, (_, l, attrs, c) =>
      `<pre${attrs}><code class="hljs language-${l}">${highlight(c.replace(/^\n/, '').replace(/\n$/, ''), l)}</code></pre>`);
    write(`${loc}/${file}`, layout({ loc, rel: file, title: t.pages[key], body, active, wide: key === 'index' }));
    count++;
  }

  const cards = docs.map((d) => `<a class="card" href="${relHref('docs/index.html', d.isLatest ? LATEST_REL : d.rel)}"><small>${esc(t.docs[d.group])}${d.isLatest ? ` · ${esc(t.docs.latest)}` : d.group === 'spec' ? ` · ${esc(t.docs.olderVersion)}` : ''}</small><h3>${esc(docTitle(d, loc))}</h3><p><code>${esc(d.variants[loc].source)}</code></p></a>`).join('');
  write(`${loc}/docs/index.html`, layout({
    loc, rel: 'docs/index.html', title: t.docs.title, active: 'docs', sidebar: docsSidebar(loc, 'docs/index.html', 'index'),
    body: `<h1>${t.docs.title}</h1><p class="lead">${t.docs.intro}</p>${docs.length ? `<div class="cards">${cards}</div>` : `<p>${t.docs.empty}</p>`}`,
  }));
  count++;

  const docPages = docs.flatMap((d) => (d.isLatest ? [[d, d.rel], [d, LATEST_REL]] : [[d, d.rel]]));
  for (const [d, rel] of docPages) {
    const v = d.variants[loc];
    const { html, toc } = render(v.md, v.source, rel);
    const notice = v.translated ? '' : `<div class="notice" role="note">${esc(t.docs.fallback[d.lang] || Object.values(t.docs.fallback)[0])}</div>`;
    const tocHtml = toc.length ? `<nav class="toc"><strong>${t.docs.onThisPage}</strong>${toc.map((x) => `<a class="d${x.depth}" href="#${x.id}">${esc(x.text)}</a>`).join('')}</nav>` : '';
    write(`${loc}/${rel}`, layout({
      loc, rel, canonicalRel: d.rel, title: docTitle(d, loc), active: 'docs', sidebar: docsSidebar(loc, rel, d),
      body: `${d.group === 'spec' ? specChrome(loc, rel, d) : ''}<p class="src">${t.docs.sourceLabel}: <a href="${REPO}/blob/main/${v.source}"><code>${v.source}</code></a></p>${notice}<article class="md"${v.translated ? '' : ` lang="${LOCALES[d.lang].htmlLang}"`}>${html}</article>${tocHtml}`,
    }));
    count++;
  }
  if (LATEST) write(`${loc}/docs/spec.html`, redirectPage(loc, 'docs/spec.html', LATEST_REL, docs.find((d) => d.isLatest).rel));
}
for (const rel of allRels) write(rel, picker(rel));

console.log(`Built ${count} pages in ${locales.length} locales (${docs.length} docs each) + ${allRels.length} language pickers into ${path.relative(root, out)}/`);
