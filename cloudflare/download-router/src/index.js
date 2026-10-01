const DEFAULT_SECONDARY_COUNTRY_CODES = "CN";
const DEFAULT_SIGNED_URL_TTL_SECONDS = 3600;
const SERVED_PATH_PREFIXES = ["/codex/", "/claude/"];

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (!SERVED_PATH_PREFIXES.some((prefix) => url.pathname.startsWith(prefix))) {
      return new Response("Not found", { status: 404 });
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("Method not allowed", { status: 405, headers: { Allow: "GET, HEAD" } });
    }
    if (!env.GLOBAL_MIRROR_BASE_URL) {
      return new Response("Missing GLOBAL_MIRROR_BASE_URL", { status: 500 });
    }

    const objectKey = objectKeyForPath(url.pathname);
    const secondaryObjectKey = objectKey;
    const country = request.cf?.country || request.headers.get("CF-IPCountry") || "";
    const secondaryCountryCodes = new Set(
      (env.SECONDARY_COUNTRY_CODES || DEFAULT_SECONDARY_COUNTRY_CODES)
        .split(",")
        .map((code) => code.trim().toUpperCase())
        .filter(Boolean),
    );

    if (secondaryCountryCodes.has(country.toUpperCase()) && hasSecondaryS3Config(env)) {
      const signingOptions = {
        endpoint: env.SECONDARY_S3_ENDPOINT,
        bucket: env.SECONDARY_S3_BUCKET,
        key: secondaryObjectKey,
        region: env.SECONDARY_S3_REGION || "us-east-1",
        accessKeyId: env.SECONDARY_S3_ACCESS_KEY_ID,
        secretAccessKey: env.SECONDARY_S3_SECRET_ACCESS_KEY,
        expiresInSeconds: ttlSeconds(env.SECONDARY_S3_SIGNED_URL_TTL_SECONDS),
        responseHeaders: {},
      };
      const signedUrl = await presignS3Url(signingOptions);

      if (await secondaryIsReady(env, objectKey, signingOptions, signedUrl)) {
        return redirect(request.method === "HEAD"
          ? await presignS3Url({ ...signingOptions, method: "HEAD" })
          : signedUrl);
      }
    }

    return redirect(withObjectKeyAndSearch(env.GLOBAL_MIRROR_BASE_URL, objectKey, url.search).toString());
  },
};

async function secondaryIsReady(env, objectKey, signingOptions, signedUrl) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 3000);
  // Cloudflare may turn a cacheable HEAD into GET, invalidating its S3 signature.
  const options = { signal: controller.signal, redirect: "manual", cache: "no-store" };
  try {
    if (objectKey.endsWith("/latest.json")) {
      const [secondary, primary] = await Promise.all([
        fetch(signedUrl, options),
        fetch(withObjectKeyAndSearch(env.GLOBAL_MIRROR_BASE_URL, objectKey, ""), options),
      ]);
      if (!secondary.ok || !primary.ok) return false;
      const [secondaryManifest, primaryManifest] = await Promise.all([
        secondary.json(), primary.json(),
      ]);
      return Boolean(primaryManifest.version)
        && primaryManifest.version === secondaryManifest.version
        && JSON.stringify(platformEntries(primaryManifest)) === JSON.stringify(platformEntries(secondaryManifest));
    }

    // Probe headers only; large artifacts still download directly from storage.
    const headUrl = await presignS3Url({ ...signingOptions, method: "HEAD" });
    const response = await fetch(headUrl, { ...options, method: "HEAD" });
    return response.ok;
  } catch (error) {
    console.warn("Secondary mirror check failed; using R2", objectKey, error.name);
    return false;
  } finally {
    clearTimeout(timer);
    controller.abort();
  }
}

function platformEntries(manifest) {
  return Object.entries(manifest.platforms || {})
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([platform, entry]) => [platform, entry.file, entry.sha256, entry.size]);
}

function hasSecondaryS3Config(env) {
  return Boolean(
    env.SECONDARY_S3_ENDPOINT &&
      env.SECONDARY_S3_BUCKET &&
      env.SECONDARY_S3_ACCESS_KEY_ID &&
      env.SECONDARY_S3_SECRET_ACCESS_KEY,
  );
}

function ttlSeconds(value) {
  const parsed = Number.parseInt(value || DEFAULT_SIGNED_URL_TTL_SECONDS, 10);
  if (!Number.isFinite(parsed)) {
    return DEFAULT_SIGNED_URL_TTL_SECONDS;
  }
  return Math.min(Math.max(parsed, 1), 604800);
}

function redirect(location) {
  return new Response(null, {
    status: 302,
    headers: {
      Location: location,
      "Cache-Control": "private, no-store",
    },
  });
}

function withObjectKeyAndSearch(baseUrl, objectKey, search) {
  const target = new URL(baseUrl);
  const basePath = target.pathname === "/" ? "" : target.pathname.replace(/\/$/, "");
  const cleanObjectKey = objectKey.replace(/^\/+/, "");
  target.pathname = `${basePath}/${cleanObjectKey}`;
  target.search = search;
  return target;
}

function objectKeyForPath(pathname) {
  return pathname.replace(/^\/+/, "");
}

async function presignS3Url(options) {
  const endpointUrl = new URL(options.endpoint);
  const now = new Date();
  const amzDate = formatAmzDate(now);
  const dateStamp = amzDate.slice(0, 8);
  const credentialScope = `${dateStamp}/${options.region}/s3/aws4_request`;
  const signedHeaders = "host";
  const canonicalUri = canonicalS3Uri(endpointUrl, options.bucket, options.key);

  const queryParams = [
    ["X-Amz-Algorithm", "AWS4-HMAC-SHA256"],
    ["X-Amz-Credential", `${options.accessKeyId}/${credentialScope}`],
    ["X-Amz-Date", amzDate],
    ["X-Amz-Expires", String(options.expiresInSeconds)],
    ["X-Amz-SignedHeaders", signedHeaders],
    ...Object.entries(options.responseHeaders || {}),
  ];
  const canonicalQuery = canonicalQueryString(queryParams);
  const canonicalHeaders = `host:${endpointUrl.host}\n`;
  const canonicalRequest = [
    options.method || "GET",
    canonicalUri,
    canonicalQuery,
    canonicalHeaders,
    signedHeaders,
    "UNSIGNED-PAYLOAD",
  ].join("\n");
  const canonicalRequestHash = await sha256Hex(canonicalRequest);
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    amzDate,
    credentialScope,
    canonicalRequestHash,
  ].join("\n");
  const signingKey = await signingKeyBytes(options.secretAccessKey, dateStamp, options.region, "s3");
  const signature = toHex(await hmac(signingKey, stringToSign));

  endpointUrl.pathname = canonicalUri;
  endpointUrl.search = `${canonicalQuery}&X-Amz-Signature=${signature}`;
  return endpointUrl.toString();
}

function canonicalS3Uri(endpointUrl, bucket, key) {
  const basePath = endpointUrl.pathname === "/" ? "" : endpointUrl.pathname.replace(/\/$/, "");
  const encodedKey = key.split("/").map(encodeRfc3986).join("/");
  return `${basePath}/${encodeRfc3986(bucket)}/${encodedKey}`;
}

function canonicalQueryString(params) {
  return params
    .map(([key, value]) => [encodeRfc3986(key), encodeRfc3986(value)])
    .sort(([leftKey, leftValue], [rightKey, rightValue]) => {
      if (leftKey === rightKey) {
        return leftValue < rightValue ? -1 : leftValue > rightValue ? 1 : 0;
      }
      return leftKey < rightKey ? -1 : 1;
    })
    .map(([key, value]) => `${key}=${value}`)
    .join("&");
}

function encodeRfc3986(value) {
  return encodeURIComponent(value).replace(/[!'()*]/g, (char) =>
    `%${char.charCodeAt(0).toString(16).toUpperCase()}`,
  );
}

function formatAmzDate(date) {
  return date.toISOString().replace(/[:-]|\.\d{3}/g, "");
}

async function signingKeyBytes(secretAccessKey, dateStamp, region, service) {
  const dateKey = await hmac(utf8(`AWS4${secretAccessKey}`), dateStamp);
  const regionKey = await hmac(dateKey, region);
  const serviceKey = await hmac(regionKey, service);
  return hmac(serviceKey, "aws4_request");
}

async function sha256Hex(value) {
  const digest = await crypto.subtle.digest("SHA-256", utf8(value));
  return toHex(new Uint8Array(digest));
}

async function hmac(keyBytes, value) {
  const cryptoKey = await crypto.subtle.importKey(
    "raw",
    keyBytes,
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign("HMAC", cryptoKey, utf8(value));
  return new Uint8Array(signature);
}

function utf8(value) {
  return new TextEncoder().encode(value);
}

function toHex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
