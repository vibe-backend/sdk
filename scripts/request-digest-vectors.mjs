// Reference implementation and test vectors for the request digest
// (spec v1.8 section 7.2b, decision 140, issue #156).
// Tested with Node 20 (node:crypto, Buffer base64url). No dependencies.
// Usage: node scripts/request-digest-vectors.mjs
import { createHash } from "node:crypto";

const LIMITS = { path: 2048, query: 4096, queryItems: 64, header: 1024, body: 1024 * 1024 };
const UNRESERVED = /^[A-Za-z0-9\-._~]$/;
// RFC 9110 token, restricted to upper case (no lower-case letters allowed).
const METHOD = /^[A-Z0-9!#$%&'*+\-.^_`|~]+$/;
const NONCE = /^[A-Za-z0-9_-]{22}$/;

class Invalid extends Error {}
const fail = (why) => { throw new Invalid(why); };

// Percent-normalise one path segment or query component. No Unicode normalisation:
// bytes are compared as sent.
function normComponent(raw, inQuery) {
  const bytes = [];
  for (let i = 0; i < raw.length; i++) {
    const c = raw[i];
    const code = c.charCodeAt(0);
    if (c === "%") {
      const hex = raw.slice(i + 1, i + 3);
      if (!/^[0-9A-Fa-f]{2}$/.test(hex)) fail("bad percent-encoding");
      bytes.push(parseInt(hex, 16));
      i += 2;
    } else if (inQuery && c === "+") {
      fail("raw + in query");
    } else if (code < 0x21 || code > 0x7e) {
      fail("raw non-ASCII, control or space");
    } else {
      bytes.push(code);
    }
  }
  return bytes
    .map((b) => {
      const ch = String.fromCharCode(b);
      return UNRESERVED.test(ch) ? ch : "%" + b.toString(16).toUpperCase().padStart(2, "0");
    })
    .join("");
}

function canonPath(path) {
  if (path.length > LIMITS.path) fail("path too long");
  if (!path.startsWith("/")) fail("path must start with /");
  const segs = path.slice(1).split("/").map((s) => normComponent(s, false));
  // Checked after decoding, so %2E and %2e are caught too.
  for (const s of segs) if (s === "." || s === "..") fail("dot segment");
  return "/" + segs.join("/");
}

function canonQuery(query) {
  if (query.length > LIMITS.query) fail("query too long");
  if (query === "") return "";
  const items = query.split("&").filter((p) => p !== "");
  if (items.length > LIMITS.queryItems) fail("too many query items");
  const pairs = items.map((p) => {
    const i = p.indexOf("=");
    const name = i < 0 ? p : p.slice(0, i);
    const value = i < 0 ? "" : p.slice(i + 1);
    return [normComponent(name, true), normComponent(value, true)];
  });
  // ASCII only after normalisation, so code-unit order equals byte order.
  const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
  pairs.sort((a, b) => cmp(a[0], b[0]) || cmp(a[1], b[1]));
  return pairs.map(([n, v]) => `${n}=${v}`).join("&");
}

// headers: array of [name, value] pairs exactly as received (duplicates visible).
function header(headers, name) {
  const found = headers.filter(([k]) => k.toLowerCase() === name);
  if (found.length > 1) fail("duplicate " + name);
  if (found.length === 0) return undefined;
  const v = found[0][1];
  if (v.length > LIMITS.header) fail(name + " too long");
  if (!/^[\x20-\x7e\t]*$/.test(v)) fail("non-ASCII or control in " + name);
  return v.replace(/[ \t]+/g, " ").replace(/^ | $/g, ""); // trim only space and tab
}

export function canonicalString({ method, target, headers = [], body = Buffer.alloc(0) }) {
  if (!METHOD.test(method)) fail("method is not an upper-case token");
  if (body.length > LIMITS.body) fail("body too large (413)");
  if (header(headers, "content-encoding") !== undefined) fail("content-encoding present");
  const nonce = header(headers, "x-vibe-request-nonce");
  if (nonce === undefined || !NONCE.test(nonce)) fail("nonce missing or not 22 base64url chars");
  if (Buffer.from(nonce, "base64url").toString("base64url") !== nonce) fail("nonce not canonical base64url");
  const q = target.indexOf("?");
  const path = q < 0 ? target : target.slice(0, q);
  const query = q < 0 ? "" : target.slice(q + 1);
  return [
    "vibe-request-digest-v1",
    "nonce:" + nonce,
    method,
    canonPath(path),
    canonQuery(query),
    "content-type:" + (header(headers, "content-type") ?? "").toLowerCase(),
    "if-match:" + (header(headers, "if-match") ?? ""),
    createHash("sha256").update(body).digest("hex"),
  ].join("\n");
}

export function requestDigest(req) {
  return "rd1." + createHash("sha256").update(Buffer.from(canonicalString(req), "utf8")).digest("base64url");
}

const N1 = "AAECAwQFBgcICQoLDA0ODw"; // bytes 00..0f
const N2 = "8PHy8_T19vf4-fr7_P3-_w"; // bytes f0..ff
const json = (s) => Buffer.from(s, "utf8");
const reg = { method: "POST", target: "/v1/t/acme-7f3k/register",
  headers: [["Content-Type", "application/json"], ["X-Vibe-Request-Nonce", N1]],
  body: json('{"email":"a@example.com","password":"correct horse battery staple"}') };

export const valid = [
  { name: "V1 POST JSON", ...reg },
  { name: "V2 GET query", method: "GET",
    target: "/v1/t/acme-7f3k/products?sort=price&tag=b%20c&tag=a&q=caf%C3%A9&x=%7e&p=1%2B1",
    headers: [["x-vibe-request-nonce", N2]] },
  { name: "V3 PATCH If-Match", method: "PATCH", target: "/v1/t/acme-7f3k/items/%61bc",
    headers: [["CONTENT-TYPE", "Application/Merge-Patch+JSON"], ["If-Match", ' "v42" '], ["X-Vibe-Request-Nonce", N1]],
    body: json('{"name":"新名"}') },
  { name: "V4 = V1 with one body byte changed", ...reg,
    body: json('{"email":"a@example.com","password":"correct horse battery staplf"}') },
  { name: "V5 = V1 with another nonce", ...reg, headers: [["Content-Type", "application/json"], ["X-Vibe-Request-Nonce", N2]] },
  { name: "V6 %2F, empty segment, trailing slash, ?x", method: "GET", target: "/v1/t/acme-7f3k/a%2fb//c/?x",
    headers: [["X-Vibe-Request-Nonce", N1]] },
  { name: "V7 same as V6 but ?x=", method: "GET", target: "/v1/t/acme-7f3k/a%2Fb//c/?x=",
    headers: [["X-Vibe-Request-Nonce", N1]] },
  { name: "V8 NFC body (é = C3 A9)", method: "POST", target: "/v1/t/acme-7f3k/register",
    headers: [["Content-Type", "application/json"], ["X-Vibe-Request-Nonce", N1]], body: json('{"n":"\u00e9"}') },
  { name: "V9 NFD body (e + U+0301), differs from V8", method: "POST", target: "/v1/t/acme-7f3k/register",
    headers: [["Content-Type", "application/json"], ["X-Vibe-Request-Nonce", N1]], body: json('{"n":"e\u0301"}') },
];

export const invalid = [
  { name: "I1 raw + in query", ...reg, target: "/v1/t/acme-7f3k/register?q=a+b" },
  { name: "I2 lower-case method", ...reg, method: "post" },
  { name: "I3 duplicate Content-Type", ...reg, headers: [...reg.headers, ["content-type", "application/json"]] },
  { name: "I4 non-ASCII in If-Match", ...reg, headers: [...reg.headers, ["If-Match", '"v\u00e9"']] },
  { name: "I5 nonce 21 chars", ...reg, headers: [["Content-Type", "application/json"], ["X-Vibe-Request-Nonce", N1.slice(0, 21)]] },
  { name: "I6 nonce 23 chars", ...reg, headers: [["Content-Type", "application/json"], ["X-Vibe-Request-Nonce", N1 + "A"]] },
  { name: "I7 nonce missing", ...reg, headers: [["Content-Type", "application/json"]] },
  { name: "I8 dot segment %2E%2E", ...reg, target: "/v1/t/acme-7f3k/%2E%2E/register" },
  { name: "I9 dot segment .", ...reg, target: "/v1/t/acme-7f3k/./register" },
  { name: "I10 bad percent-encoding", ...reg, target: "/v1/t/acme-7f3k/reg%2Gister" },
  { name: "I11 truncated percent-encoding", ...reg, target: "/v1/t/acme-7f3k/register?q=%4" },
  { name: "I12 Content-Encoding present", ...reg, headers: [...reg.headers, ["Content-Encoding", "gzip"]] },
  { name: "I13 CR in If-Match", ...reg, headers: [...reg.headers, ["If-Match", '"v1"\r']] },
];

if (import.meta.url === `file://${process.argv[1]}`) {
  for (const v of valid) {
    console.log(`## ${v.name}\ncanonical:\n${canonicalString(v)}\ndigest: ${requestDigest(v)}\n`);
  }
  let bad = 0;
  for (const v of invalid) {
    try { requestDigest(v); console.log(`!! ${v.name}: accepted (BUG)`); bad++; }
    catch (e) { if (!(e instanceof Invalid)) throw e; console.log(`INVALID ${v.name}: ${e.message}`); }
  }
  process.exit(bad ? 1 : 0);
}
