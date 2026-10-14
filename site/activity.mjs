// Build-time development activity: merged PRs and lines changed per day for the current month
// (Asia/Hong_Kong day boundaries), fetched from the GitHub API with GITHUB_TOKEN (read-only).
// Returns null — and the section is omitted — when the token is missing or any request fails.
const ORG = 'vibe-backend';
const API = 'https://api.github.com';
const TZ_OFFSET_H = 8; // Asia/Hong_Kong, no DST
const MAX_PAGES = 10;

const isCount = (n) => Number.isSafeInteger(n) && n >= 0;

async function gh(url, token) {
  const res = await fetch(url, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'vibe-backend-site-build' },
  });
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

export async function fetchActivity(token, now = new Date()) {
  if (!token) { console.warn('[activity] GITHUB_TOKEN not set; omitting activity section.'); return null; }
  try {
    const today = hkDay(now.getTime());
    const [y, m] = today.split('-').map(Number);
    const startUtc = Date.UTC(y, m - 1, 1) - TZ_OFFSET_H * 3600e3;
    const since = new Date(startUtc).toISOString().replace(/\.\d+Z$/, 'Z');

    const repoList = await paginate(`${API}/orgs/${ORG}/repos?type=all&per_page=100`, token, (d) => d);
    const repos = repoList.map((r) => r.full_name).sort();
    if (!repos.length) throw new Error('no repositories visible');

    const q = encodeURIComponent(`org:${ORG} is:pr is:merged merged:>=${since}`);
    const hits = await paginate(`${API}/search/issues?q=${q}&per_page=100`, token, (d) => {
      if (d.incomplete_results) throw new Error('search returned incomplete results');
      return d.items;
    });

    const mergedTimes = [];
    const days = new Map();
    for (let d = 1; ; d++) {
      const key = `${y}-${String(m).padStart(2, '0')}-${String(d).padStart(2, '0')}`;
      if (key > today || new Date(`${key}T00:00:00Z`).getUTCMonth() !== m - 1) break;
      days.set(key, { prs: 0, additions: 0, deletions: 0 });
    }
    for (const hit of hits) {
      const { data: pr } = await gh(hit.pull_request.url, token);
      if (!pr.merged_at || !isCount(pr.additions) || !isCount(pr.deletions)) throw new Error(`unexpected PR data for ${hit.html_url}`);
      mergedTimes.push(Date.parse(pr.merged_at));
      const bucket = days.get(hkDay(Date.parse(pr.merged_at)));
      if (!bucket) continue; // merged outside this month (search is inclusive by instant)
      bucket.prs += 1; bucket.additions += pr.additions; bucket.deletions += pr.deletions;
    }
    // Skip leading zero days: start at the first day (HKT) with a merged PR this month.
    const inMonth = mergedTimes.filter((ms) => days.has(hkDay(ms)));
    const originDay = hkDay(inMonth.length ? Math.min(...inMonth) : now.getTime());
    const startDay = [...days.keys()][0] > originDay ? [...days.keys()][0] : originDay;
    const rows = [...days].filter(([day]) => day >= startDay).map(([day, v]) => ({ day, ...v }));
    const totals = rows.reduce((t, r) => ({ prs: t.prs + r.prs, additions: t.additions + r.additions, deletions: t.deletions + r.deletions }), { prs: 0, additions: 0, deletions: 0 });
    if (![totals.prs, totals.additions, totals.deletions].every(isCount)) throw new Error('totals overflow');
    console.log(`[activity] generated: ${totals.prs} merged PRs, +${totals.additions}/-${totals.deletions} across ${repos.length} repos (${repos.join(', ')}), ${rows.length} days of ${today.slice(0, 7)}.`);
    return { org: ORG, repos, month: today.slice(0, 7), today, startDay: rows[0].day, rows, totals, generatedAt: now.toISOString() };
  } catch (err) {
    console.warn(`[activity] omitting activity section: ${err.message}`);
    return null;
  }
}
