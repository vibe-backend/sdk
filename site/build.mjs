// Static site builder for the vibe-backend product site.
// Docs are generated at build time from ../docs/spec-v1.5.md and ../docs/best-practices/*.md.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Marked } from 'marked';
import hljs from 'highlight.js';
import sanitizeHtml from 'sanitize-html';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const out = path.join(here, 'dist');
const src = path.join(here, 'src');
const REPO = 'https://github.com/vibe-backend/sdk';

const slugify = (s) => s.toLowerCase().trim()
  .replace(/<[^>]+>/g, '').replace(/[`*_~]/g, '')
  .replace(/[^\p{L}\p{N}\s-]/gu, '').replace(/\s+/g, '-') || 'section';
const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function render(md, sourcePath) {
  const toc = [];
  const seen = new Map();
  const marked = new Marked({
    gfm: true,
    renderer: {
      code({ text, lang }) {
        const l = (lang || '').split(/\s/)[0];
        const html = highlight(text, l);
        return `<pre><code class="hljs${l ? ' language-' + esc(l) : ''}">${html}</code></pre>\n`;
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
        // Rewrite links between docs (*.md) to generated pages.
        const h = rewriteLink(href || '', sourcePath);
        return `<a href="${esc(h)}"${title ? ` title="${esc(title)}"` : ''}>${this.parser.parseInline(tokens)}</a>`;
      },
    },
  });
  const html = sanitize(marked.parse(md));
  return { html, toc };
}


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

// Map links between repo files: md files with a generated page -> that page; other relative paths -> GitHub blob URL.
function rewriteLink(href, sourcePath) {
  if (!href || href.startsWith('#') || /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('//')) return href;
  const [p, frag = ''] = href.split(/(?=#)/);
  const repoPath = p.startsWith('/')
    ? path.posix.normalize(p).replace(/^\/+/, '') // repo-root-relative, e.g. /README.md
    : path.posix.normalize(path.posix.join(path.posix.dirname(sourcePath), p));
  if (repoPath.startsWith('..')) return href;
  const page = pageBySource.get(repoPath);
  if (page) return `${page}.html${frag}`;
  return `${REPO}/blob/main/${repoPath}${frag}`;
}

function titleOf(md, fallback) {
  const m = /^#\s+(.+)$/m.exec(md);
  return m ? m[1].replace(/[`*_]/g, '').trim() : fallback;
}

// ---- collect docs ----
const docs = [];
const specPath = path.join(root, 'docs', 'spec-v1.5.md');
if (fs.existsSync(specPath)) {
  const md = fs.readFileSync(specPath, 'utf8');
  docs.push({ slug: 'spec', group: 'Specification', title: 'Spec v1.5', fullTitle: titleOf(md, 'Spec v1.5'), md, source: 'docs/spec-v1.5.md' });
}
const bpDir = path.join(root, 'docs', 'best-practices');
if (fs.existsSync(bpDir)) {
  const files = fs.readdirSync(bpDir).filter((f) => f.endsWith('.md'));
  // README.md is the best-practices index page and is listed first.
  files.sort((a, b) => (b.toLowerCase() === 'readme.md') - (a.toLowerCase() === 'readme.md') || a.localeCompare(b));
  for (const f of files) {
    const md = fs.readFileSync(path.join(bpDir, f), 'utf8');
    const isIndex = f.toLowerCase() === 'readme.md';
    const slug = isIndex ? 'best-practices' : f.replace(/\.md$/, '').toLowerCase().replace(/[^a-z0-9-]+/g, '-');
    const t = isIndex ? 'Overview' : titleOf(md, slug);
    docs.push({ slug, group: 'Best practices', title: t, fullTitle: titleOf(md, t), md, source: `docs/best-practices/${f}` });
  }
}
// Ensure unique slugs (reserved: index) and build source -> page map for link rewriting.
const used = new Set(['index']);
for (const d of docs) { let s = d.slug, i = 2; while (used.has(s)) s = `${d.slug}-${i++}`; d.slug = s; used.add(s); }
const pageBySource = new Map(docs.map((d) => [d.source, d.slug]));

// ---- layout ----
const nav = [
  ['index.html', 'Home'], ['features.html', 'Features'], ['quick-start.html', 'Quick start'], ['docs/index.html', 'Docs'],
];
function layout({ title, body, depth = 0, active, sidebar = '' }) {
  const pre = depth ? '../'.repeat(depth) : '';
  const links = nav.map(([h, t]) => `<a href="${pre}${h}"${active === t ? ' class="active"' : ''}>${t}</a>`).join('');
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(title)} · vibe-backend</title>
<meta name="description" content="vibe-backend: a Markdown-driven serverless backend SDK.">
<link rel="stylesheet" href="${pre}assets/style.css"><link rel="stylesheet" href="${pre}assets/hljs.css">
</head><body>
<header class="top"><a class="brand" href="${pre}index.html">vibe<span>-backend</span></a><nav>${links}<a href="${REPO}">GitHub</a></nav></header>
<div class="${sidebar ? 'docs-wrap' : 'wrap'}">${sidebar}<main>${body}</main></div>
<footer>vibe-backend SDK · dual licensed MIT / Apache-2.0 · <a href="${REPO}">Source on GitHub</a></footer>
</body></html>`;
}

function docsSidebar(current) {
  const groups = {};
  for (const d of docs) (groups[d.group] ||= []).push(d);
  let s = '<aside class="sidebar"><a href="index.html" class="' + (current === 'index' ? 'cur' : '') + '">Docs home</a>';
  for (const [g, items] of Object.entries(groups)) {
    s += `<h4>${esc(g)}</h4>` + items.map((d) => `<a href="${d.slug}.html"${d.slug === current ? ' class="cur"' : ''}>${esc(d.title)}</a>`).join('');
  }
  return s + '</aside>';
}

// ---- write ----
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(path.join(out, 'docs'), { recursive: true });
fs.cpSync(path.join(src, 'assets'), path.join(out, 'assets'), { recursive: true });
fs.copyFileSync(path.join(here, 'node_modules/highlight.js/styles/github-dark.css'), path.join(out, 'assets/hljs.css'));
fs.writeFileSync(path.join(out, '.nojekyll'), '');

for (const [file, title, active] of [
  ['index.html', 'Markdown-driven serverless backend', 'Home'],
  ['features.html', 'Features', 'Features'],
  ['quick-start.html', 'Quick start', 'Quick start'],
]) {
  let body = fs.readFileSync(path.join(src, 'pages', file), 'utf8');
  // Allow ```code``` fences in hand-written pages to be highlighted too.
  body = body.replace(/<pre data-lang="(\w+)">([\s\S]*?)<\/pre>/g, (_, l, c) =>
    `<pre><code class="hljs">${highlight(c.replace(/^\n/, ''), l)}</code></pre>`);
  fs.writeFileSync(path.join(out, file), layout({ title, body, active }));
}

const cards = docs.map((d) => `<a class="card" href="${d.slug}.html"><small>${esc(d.group)}</small><h3>${esc(d.title)}</h3><p><code>${esc(d.source)}</code></p></a>`).join('');
fs.writeFileSync(path.join(out, 'docs/index.html'), layout({
  title: 'Docs', depth: 1, active: 'Docs', sidebar: docsSidebar('index'),
  body: `<h1>Documentation</h1><p>These pages are generated at build time from the Markdown files in <code>docs/</code> of the SDK repository. The specification is currently authored in Cantonese (Traditional Chinese) and is rendered as written.</p>${docs.length ? `<div class="cards">${cards}</div>` : '<p>No documents found.</p>'}`,
}));

for (const d of docs) {
  const { html, toc } = render(d.md, d.source);
  const tocHtml = toc.length ? `<nav class="toc"><strong>On this page</strong>${toc.map((t) => `<a class="d${t.depth}" href="#${t.id}">${esc(t.text)}</a>`).join('')}</nav>` : '';
  fs.writeFileSync(path.join(out, 'docs', d.slug + '.html'), layout({
    title: d.title, depth: 1, active: 'Docs', sidebar: docsSidebar(d.slug),
    body: `<p class="src">Source: <a href="${REPO}/blob/main/${d.source}"><code>${d.source}</code></a></p><article class="md">${html}</article>${tocHtml}`,
  }));
}

console.log(`Built ${3 + 1 + docs.length} pages (${docs.length} docs) into ${path.relative(root, out)}/`);
