import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
import test from "node:test";
import worker from "../cloudflare/download-router/src/index.js";

const env = {
  GLOBAL_MIRROR_BASE_URL: "https://primary.example",
  SECONDARY_COUNTRY_CODES: "CN",
  SECONDARY_S3_ENDPOINT: "https://secondary.example",
  SECONDARY_S3_BUCKET: "mirror",
  SECONDARY_S3_REGION: "us-east-1",
  SECONDARY_S3_ACCESS_KEY_ID: "test-key",
  SECONDARY_S3_SECRET_ACCESS_KEY: "test-secret",
};
const manifest = {
  version: "v2",
  platforms: { windows: { file: "tool.zip", sha256: "checksum", size: 123 } },
};
function request(path, country = "CN", method = "GET") {
  const req = new Request(`https://install.example${path}`, { method });
  Object.defineProperty(req, "cf", { value: { country } });
  return req;
}
function signature(url, method) {
  const u = new URL(url);
  const date = u.searchParams.get("X-Amz-Date");
  const scope = u.searchParams.get("X-Amz-Credential").split("/").slice(1).join("/");
  const expected = u.searchParams.get("X-Amz-Signature");
  u.searchParams.delete("X-Amz-Signature");
  const canonical = [method, u.pathname, u.searchParams.toString(), `host:${u.host}\n`, "host", "UNSIGNED-PAYLOAD"].join("\n");
  const sha = createHash("sha256").update(canonical).digest("hex");
  let key = Buffer.from("AWS4test-secret");
  for (const value of [date.slice(0, 8), "us-east-1", "s3", "aws4_request"]) {
    key = createHmac("sha256", key).update(value).digest();
  }
  return { expected, actual: createHmac("sha256", key).update(["AWS4-HMAC-SHA256", date, scope, sha].join("\n")).digest("hex") };
}
test("global requests redirect without contacting the secondary", async (t) => {
  t.mock.method(globalThis, "fetch", () => { throw new Error("unexpected probe"); });
  const response = await worker.fetch(request("/codex/latest.json?run=123", "US"), env);
  assert.equal(response.status, 302);
  assert.equal(response.headers.get("Location"), "https://primary.example/codex/latest.json?run=123");
});

test("healthy artifacts use a HEAD probe and a valid GET signature", async (t) => {
  t.mock.method(globalThis, "fetch", async (url, options) => {
    assert.equal(options.method, "HEAD");
    const signed = signature(url, "HEAD");
    assert.equal(signed.actual, signed.expected);
    return new Response(null, { status: 200 });
  });
  const response = await worker.fetch(request("/codex/v2/windows/tool.zip"), env);
  const location = response.headers.get("Location");
  assert.equal(new URL(location).host, "secondary.example");
  const signed = signature(location, "GET");
  assert.equal(signed.actual, signed.expected);
  assert.equal(response.headers.get("Cache-Control"), "private, no-store");
});

test("HEAD clients receive a HEAD signature", async (t) => {
  t.mock.method(globalThis, "fetch", async () => new Response(null, { status: 200 }));
  const response = await worker.fetch(request("/codex/v2/windows/tool.zip", "CN", "HEAD"), env);
  const signed = signature(response.headers.get("Location"), "HEAD");
  assert.equal(signed.actual, signed.expected);
});

for (const status of [404, 403, 500]) {
  test(`secondary HTTP ${status} falls back to R2`, async (t) => {
    t.mock.method(globalThis, "fetch", async () => new Response(null, { status }));
    const response = await worker.fetch(request("/claude/v2/windows/claude.exe"), env);
    assert.equal(response.headers.get("Location"), "https://primary.example/claude/v2/windows/claude.exe");
  });
}

test("secondary network failure falls back to R2", async (t) => {
  t.mock.method(globalThis, "fetch", async () => { throw new TypeError("unavailable"); });
  const response = await worker.fetch(request("/codex/install.sh"), env);
  assert.equal(new URL(response.headers.get("Location")).host, "primary.example");
});

for (const [label, secondary] of [
  ["stale version", { ...manifest, version: "v1" }],
  ["changed checksum", { ...manifest, platforms: { windows: { ...manifest.platforms.windows, sha256: "old" } } }],
]) {
  test(`${label} latest.json falls back to R2`, async (t) => {
    t.mock.method(globalThis, "fetch", async (url) => Response.json(new URL(url).host === "secondary.example" ? secondary : manifest));
    const response = await worker.fetch(request("/codex/latest.json"), env);
    assert.equal(response.headers.get("Location"), "https://primary.example/codex/latest.json");
  });
}

test("matching manifests retain the domestic source", async (t) => {
  t.mock.method(globalThis, "fetch", async () => Response.json(manifest));
  const response = await worker.fetch(request("/codex/latest.json"), env);
  assert.equal(new URL(response.headers.get("Location")).host, "secondary.example");
});

test("a hung secondary probe times out and falls back", async (t) => {
  t.mock.method(globalThis, "fetch", (_url, options) => new Promise((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
  }));
  const response = await worker.fetch(request("/codex/install.sh"), env);
  assert.equal(new URL(response.headers.get("Location")).host, "primary.example");
});

test("unsupported paths and methods are rejected", async () => {
  assert.equal((await worker.fetch(request("/other/latest.json"), env)).status, 404);
  assert.equal((await worker.fetch(request("/codex/latest.json", "CN", "POST"), env)).status, 405);
});
