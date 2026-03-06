interface Env {
  REFRESH_TOKEN: string;
  GITHUB_TOKEN: string;
  GITHUB_OWNER: string;
  GITHUB_REPO: string;
  GITHUB_WORKFLOW: string;
  GITHUB_REF?: string;
  THROTTLE_SECONDS?: string;
}

const PROVIDER_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/;

function json(data: unknown, init?: ResponseInit): Response {
  const headers = new Headers(init?.headers);
  headers.set("content-type", "application/json; charset=utf-8");
  headers.set("cache-control", "no-store");
  return new Response(JSON.stringify(data, null, 2), { ...init, headers });
}

function requireBearer(request: Request, expected: string | undefined): Response | undefined {
  if (!expected) {
    return json({ success: false, error: "server misconfigured" }, { status: 500 });
  }

  const raw = request.headers.get("authorization") ?? "";
  const token = raw.startsWith("Bearer ") ? raw.slice("Bearer ".length) : "";
  if (!token) {
    return json({ success: false, error: "missing bearer token" }, { status: 401 });
  }
  if (token !== expected) {
    return json({ success: false, error: "invalid bearer token" }, { status: 401 });
  }
  return undefined;
}

async function throttle(provider: string, seconds: number): Promise<Response | undefined> {
  if (!Number.isFinite(seconds) || seconds <= 0) {
    return undefined;
  }

  const cacheKey = new Request(
    `https://acm-refresh-dispatch.invalid/throttle/${encodeURIComponent(provider)}`,
    { method: "GET" },
  );

  const cached = await caches.default.match(cacheKey);
  if (cached) {
    return json(
      { success: false, error: "throttled", retry_after_seconds: seconds },
      { status: 429 },
    );
  }

  await caches.default.put(cacheKey, new Response("1"), { expirationTtl: seconds });
  return undefined;
}

async function dispatchGithubWorkflow(env: Env, provider: string): Promise<Response> {
  const owner = env.GITHUB_OWNER;
  const repo = env.GITHUB_REPO;
  const workflow = env.GITHUB_WORKFLOW;
  const ref = env.GITHUB_REF || "main";

  if (!owner || !repo || !workflow) {
    return json(
      { success: false, error: "missing GITHUB_* vars" },
      { status: 500 },
    );
  }
  if (!env.GITHUB_TOKEN) {
    return json(
      { success: false, error: "missing GITHUB_TOKEN secret" },
      { status: 500 },
    );
  }

  const url = `https://api.github.com/repos/${owner}/${repo}/actions/workflows/${workflow}/dispatches`;
  const resp = await fetch(url, {
    method: "POST",
    headers: {
      authorization: `Bearer ${env.GITHUB_TOKEN}`,
      accept: "application/vnd.github+json",
      "user-agent": "acm-refresh-dispatch",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      ref,
      inputs: { provider },
    }),
  });

  if (resp.status === 204) {
    return json({ success: true, provider, workflow, ref });
  }

  const body = (await resp.text()).slice(0, 1000);
  return json(
    { success: false, provider, status: resp.status, body },
    { status: 502 },
  );
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const match = url.pathname.match(/^\/api\/([^/]+)\/refresh$/);
    if (!match) {
      return new Response("Not Found", { status: 404 });
    }

    if (request.method !== "POST") {
      return new Response("Method Not Allowed", { status: 405 });
    }

    const provider = decodeURIComponent(match[1]);
    if (provider !== "all" && !PROVIDER_RE.test(provider)) {
      return json({ success: false, error: "invalid provider" }, { status: 400 });
    }

    const authErr = requireBearer(request, env.REFRESH_TOKEN);
    if (authErr) {
      return authErr;
    }

    const throttleSeconds = Number.parseInt(env.THROTTLE_SECONDS || "30", 10);
    const throttled = await throttle(provider, throttleSeconds);
    if (throttled) {
      return throttled;
    }

    return dispatchGithubWorkflow(env, provider);
  },
};

