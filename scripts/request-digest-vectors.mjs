// Reference implementation and test vectors for the request digest (decision 140, issue #156).
// Usage: node scripts/request-digest-vectors.mjs
import { createHash } from "node:crypto";

const UNRESERVED = /[A-Za-z0-9\-._~]/;

// Normalise one path segment or query component (bytes in, string out).
function normComponent(raw) {
  const bytes = [];
  for (let i = 0; i < raw.length; i++) {
    const c = raw[i];
    if (c === "%") {
      const hex = raw.slice(i + 1, i + 3);
      if (!/^[0-9A-Fa-f]{2}$/.test(hex)) throw new Error("invalid percent-encoding");
      bytes.push(parseInt(hex, 16));
      i += 2;
    } else {
      const code = c.charCodeAt(0);
      if (code < 0x21 || code > 0x7e) throw new Error("raw non-ASCII or control byte");
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
  if (!path.startsWith("/")) throw new Error("path must start with /");
  const segs = path.slice(1).split("/").map(normComponent);
  for (const s of segs) if (s === "." || s === ".." || s === "%2E" || s === "%2E%2E") throw new Error("dot segment");
  return "/" + segs.join("/");
}

function canonQuery(query) {
  if (query === "") return "";
  const pairs = query.split("&").filter((p) => p !== "").map((p) => {
    const i = p.indexOf("=");
    const name = i < 0 ? p : p.slice(0, i);
    const value = i < 0 ? "" : p.slice(i + 1);
    return [normComponent(name), normComponent(value)];
  });
  const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0); // ASCII only after normalisation, so code-unit order = byte order
  pairs.sort((a, b) => cmp(a[0], b[0]) || cmp(a[1], b[1]));
  return pairs.map(([n, v]) => `${n}=${v}`).join("&");
}

function canonHeader(v) {
  return v === undefined ? "" : v.replace(/[ \t]+/g, " ").trim();
}

export function canonicalString({ method, target, headers = {}, body = Buffer.alloc(0) }) {
  const q = target.indexOf("?");
  const path = q < 0 ? target : target.slice(0, q);
  const query = q < 0 ? "" : target.slice(q + 1);
  const h = Object.fromEntries(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), v]));
  return [
    "vibe-request-digest-v1",
    method.toUpperCase(),
    canonPath(path),
    canonQuery(query),
    "content-type:" + canonHeader(h["content-type"]).toLowerCase(),
    "if-match:" + canonHeader(h["if-match"]),
    createHash("sha256").update(body).digest("hex"),
  ].join("\n");
}

export function requestDigest(req) {
  const d = createHash("sha256").update(Buffer.from(canonicalString(req), "utf8")).digest("base64url");
  return "rd1." + d;
}

const vectors = [
  { name: "V1 POST JSON", method: "POST", target: "/v1/t/acme-7f3k/register",
    headers: { "Content-Type": "application/json" },
    body: Buffer.from('{"email":"a@example.com","password":"correct horse battery staple"}', "utf8") },
  { name: "V2 GET query", method: "get", target: "/v1/t/acme-7f3k/products?sort=price&tag=b%20c&tag=a&q=caf%C3%A9&x=%7e",
    headers: {}, body: Buffer.alloc(0) },
  { name: "V3 PATCH If-Match, encoded path", method: "PATCH", target: "/v1/t/acme-7f3k/items/%61bc",
    headers: { "CONTENT-TYPE": "Application/Merge-Patch+JSON", "If-Match": ' "v42" ' },
    body: Buffer.from('{"name":"新名"}', "utf8") },
  { name: "V4 same as V1, body one byte different", method: "POST", target: "/v1/t/acme-7f3k/register",
    headers: { "content-type": "application/json" },
    body: Buffer.from('{"email":"a@example.com","password":"correct horse battery staplf"}', "utf8") },
];

for (const v of vectors) {
  console.log(`## ${v.name}`);
  console.log("canonical:\n" + canonicalString(v));
  console.log("digest: " + requestDigest(v) + "\n");
}
