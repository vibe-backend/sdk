// Build-time development activity, fetched from the GitHub API with GITHUB_TOKEN (read-only),
// Asia/Hong_Kong day boundaries:
//  - totals for the current month: merged PRs, lines added, lines deleted;
//  - a contribution heatmap: merged PRs per day (by merged_at) across every org repo, last 52-53 weeks.
// Returns null — and the section is omitted — when the token is missing or any request fails.
const ORG = 'vibe-backend';
const API = 'https://api.github.com';
const TZ_OFFSET_H = 8; // Asia/Hong_Kong, no DST
const MAX_PAGES = 10;

const isCount = (n) => Number.isSafeInteger(n) && n >= 0;

const MAX_RETRIES = 3;
const MAX_WAIT_S = 90;

async function gh(url, token) {
  let res;
  for (let attempt = 0; ; attempt++) {
    res = await fetch(url, {
      headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'vibe-backend-site-build' },
    });
    // Primary / secondary rate limits: wait (bounded) and retry, otherwise give up and fall back.
    const limited = res.status === 429 || (res.status === 403 && (res.headers.get('retry-after') || res.headers.get('x-ratelimit-remaining') === '0'));
    if (!limited || attempt >= MAX_RETRIES) break;
    const retryAfter = Number(res.headers.get('retry-after'));
    const reset = Number(res.headers.get('x-ratelimit-reset'));
    const wait = retryAfter > 0 ? retryAfter : reset > 0 ? reset - Date.now() / 1000 + 1 : 2 ** attempt * 5;
    if (wait > MAX_WAIT_S) break;
    console.warn(`[activity] rate limited; retrying in ${Math.ceil(wait)}s`);
    await new Promise((r) => setTimeout(r, Math.max(1, wait) * 1000));
  }
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
  const next = /<([^>]+)>;\s*rel="next"/.exec(res.headers.get('link') || '');
  return { data: await res.json(), next: next ? next[1] : null };
}

async function paginate(url, token, pick) {
  const items = [];
  for (let page = 0; url; page++) {
    if (page >= MAX_PAGES) throw new Error(`too many pages for ${url}`);
    const { data, next } = await gh(url, token);
    items.push(...pick(data));
    url = next;
  }
  return items;
}

// "YYYY-MM-DD" of an instant in Hong Kong time.
const hkDay = (ms) => new Date(ms + TZ_OFFSET_H * 3600e3).toISOString().slice(0, 10);

const isoZ = (ms) => new Date(ms).toISOString().replace(/\.\d+Z$/, 'Z');
const addDays = (day, n) => new Date(Date.parse(`${day}T00:00:00Z`) + n * 86400e3).toISOString().slice(0, 10);
// UTC instant of 00:00 Hong Kong time on a "YYYY-MM-DD" day.
const hkStart = (day) => Date.parse(`${day}T00:00:00Z`) - TZ_OFFSET_H * 3600e3;

async function searchMergedPrs(token, fromMs, toMs) {
  const q = encodeURIComponent(`org:${ORG} is:public is:pr is:merged merged:${isoZ(fromMs)}..${isoZ(toMs - 1000)}`);
  let total = 0;
  const items = await paginate(`${API}/search/issues?q=${q}&per_page=100`, token, (d) => {
    if (d.incomplete_results) throw new Error('search returned incomplete results');
    total = d.total_count;
    return d.items;
  });
  if (total > 1000) throw new Error(`search window over the 1000-result cap (${total})`);
  return items;
}

// Heatmap grid: weeks start on Sunday (GitHub layout); 52 full weeks back plus the current week.
export async function fetchHeatmap(token, today) {
  const dow = new Date(`${today}T00:00:00Z`).getUTCDay();
  const start = addDays(today, -dow - 52 * 7);
  const counts = new Map();
  // Month-sized search windows keep every query far below the search API's 1000-result cap.
  for (let from = start; from <= today;) {
    const [fy, fm] = from.split('-').map(Number);
    const next = new Date(Date.UTC(fy, fm, 1)).toISOString().slice(0, 10);
    const to = next > addDays(today, 1) ? addDays(today, 1) : next;
    const hits = await searchMergedPrs(token, hkStart(from), hkStart(to));
    for (const hit of hits) {
      const at = Date.parse(hit.pull_request?.merged_at || '');
      if (!Number.isFinite(at)) throw new Error(`missing merged_at for ${hit.html_url}`);
      const day = hkDay(at);
      if (day >= start && day <= today) counts.set(day, (counts.get(day) || 0) + 1);
    }
    from = to;
  }
  const days = [];
  for (let d = start; d <= today; d = addDays(d, 1)) days.push({ day: d, count: counts.get(d) || 0 });
  const total = days.reduce((t, d) => t + d.count, 0);
  return { start, end: today, days, total, max: Math.max(0, ...days.map((d) => d.count)) };
}

export async function fetchActivity(token, now = new Date()) {
  if (!token) { console.warn('[activity] GITHUB_TOKEN not set; omitting activity section.'); return null; }
  try {
    const today = hkDay(now.getTime());
    const [y, m] = today.split('-').map(Number);
    const startUtc = Date.UTC(y, m - 1, 1) - TZ_OFFSET_H * 3600e3;

    const repoList = await paginate(`${API}/orgs/${ORG}/repos?type=all&per_page=100`, token, (d) => d);
    const repos = repoList.filter((r) => !r.private).map((r) => r.full_name).sort();
    if (!repos.length) throw new Error('no repositories visible');

    const hits = await searchMergedPrs(token, startUtc, hkStart(addDays(today, 1)));
    const totals = { prs: 0, additions: 0, deletions: 0 };
    let first = null;
    for (const hit of hits) {
      const { data: pr } = await gh(hit.pull_request.url, token);
      if (!pr.merged_at || !isCount(pr.additions) || !isCount(pr.deletions)) throw new Error(`unexpected PR data for ${hit.html_url}`);
      const day = hkDay(Date.parse(pr.merged_at));
      if (day.slice(0, 7) !== today.slice(0, 7)) continue;
      totals.prs += 1; totals.additions += pr.additions; totals.deletions += pr.deletions;
      if (!first || day < first) first = day;
    }
    if (![totals.prs, totals.additions, totals.deletions].every(isCount)) throw new Error('totals overflow');
    const heatmap = await fetchHeatmap(token, today);
    console.log(`[activity] generated: ${totals.prs} merged PRs, +${totals.additions}/-${totals.deletions} in ${today.slice(0, 7)}; heatmap ${heatmap.total} merged PRs ${heatmap.start}..${heatmap.end} across ${repos.length} repos (${repos.join(', ')}).`);
    return { org: ORG, repos, month: today.slice(0, 7), today, startDay: first || today, totals, heatmap, generatedAt: now.toISOString() };
  } catch (err) {
    console.warn(`[activity] omitting activity section: ${err.message}`);
    return null;
  }
}
