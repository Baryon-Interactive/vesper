import {
  dbOne,
  dbRun,
  Env,
  json,
  now,
  randomToken,
  Row,
  sha256Hex,
  text,
} from "./common";

export async function sessionUser(
  request: Request,
  env: Env,
): Promise<Row | null> {
  const authorization = request.headers.get("authorization") ?? "";
  const bearer = authorization.startsWith("Bearer ")
    ? authorization.slice(7)
    : null;
  const cookie = request.headers
    .get("cookie")
    ?.match(/vesper_session=([^;]+)/)?.[1];
  const token = bearer ?? cookie;
  if (!token) return null;
  return dbOne(
    env.DB,
    "SELECT u.* FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token_hash=? AND s.expires_at>?",
    await sha256Hex(token),
    now(),
  );
}

export async function issueSession(
  env: Env,
  userId: string,
): Promise<Response> {
  const token = randomToken();
  await dbRun(
    env.DB,
    "INSERT INTO sessions(token_hash,user_id,expires_at,created_at) VALUES(?,?,?,?)",
    await sha256Hex(token),
    userId,
    now() + 30 * 86400,
    now(),
  );
  return json({ authenticated: true }, 200, {
    "set-cookie": `vesper_session=${token}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${30 * 86400}`,
  });
}

export async function ensureUser(
  env: Env,
  fields: {
    id?: string;
    github_id?: string;
    email?: string;
    display_name?: string;
  },
): Promise<string> {
  const existing = fields.github_id
    ? await dbOne(
        env.DB,
        "SELECT id FROM users WHERE github_id=?",
        fields.github_id,
      )
    : fields.email
      ? await dbOne(env.DB, "SELECT id FROM users WHERE email=?", fields.email)
      : null;
  if (existing) return String(existing.id);
  const id = fields.id ?? crypto.randomUUID();
  await dbRun(
    env.DB,
    "INSERT INTO users(id,github_id,email,display_name,created_at) VALUES(?,?,?,?,?)",
    id,
    fields.github_id ?? null,
    fields.email ?? null,
    fields.display_name ?? null,
    now(),
  );
  return id;
}

async function deliverEmail(
  env: Env,
  email: string,
  code: string,
): Promise<void> {
  if (env.DEV_MODE === "true") {
    console.log(`[dev email] ${email}: ${code}`);
    return;
  }
  if (!env.EMAIL_PROVIDER_URL)
    throw new Error("email provider is not configured");
  const response = await fetch(env.EMAIL_PROVIDER_URL, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(env.EMAIL_PROVIDER_TOKEN
        ? { authorization: `Bearer ${env.EMAIL_PROVIDER_TOKEN}` }
        : {}),
    },
    body: JSON.stringify({ to: email, code }),
  });
  if (!response.ok) {
    throw new Error(`email provider returned ${response.status}`);
  }
}

export async function handleAuth(
  request: Request,
  env: Env,
  url: URL,
  body: string,
): Promise<Response | undefined> {
  if (url.pathname === "/auth/github/start") {
    const state = randomToken(24);
    const redirect = new URL("https://github.com/login/oauth/authorize");
    redirect.searchParams.set("client_id", env.GITHUB_CLIENT_ID ?? "");
    redirect.searchParams.set(
      "redirect_uri",
      env.GITHUB_REDIRECT_URI ??
        new URL("/auth/github/callback", url).toString(),
    );
    redirect.searchParams.set("state", state);
    return new Response(null, {
      status: 302,
      headers: {
        location: redirect.toString(),
        "set-cookie": `oauth_state=${state}; HttpOnly; Secure; SameSite=Lax; Path=/auth/github; Max-Age=600`,
      },
    });
  }

  if (url.pathname === "/auth/github/callback") {
    const state = url.searchParams.get("state");
    const cookie = request.headers
      .get("cookie")
      ?.match(/oauth_state=([^;]+)/)?.[1];
    if (!state || !cookie || state !== cookie) {
      return text("invalid oauth state", 400);
    }
    if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) {
      return text("github oauth is not configured", 503);
    }
    const tokenResponse = await fetch(
      "https://github.com/login/oauth/access_token",
      {
        method: "POST",
        headers: {
          accept: "application/json",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          client_id: env.GITHUB_CLIENT_ID,
          client_secret: env.GITHUB_CLIENT_SECRET,
          code: url.searchParams.get("code"),
          redirect_uri: env.GITHUB_REDIRECT_URI,
        }),
      },
    );
    const token = (await tokenResponse.json()) as Row;
    const userResponse = await fetch("https://api.github.com/user", {
      headers: {
        authorization: `Bearer ${token.access_token}`,
        "user-agent": "vesper-gateway",
      },
    });
    const github = (await userResponse.json()) as Row;
    const userId = await ensureUser(env, {
      github_id: String(github.id),
      email: typeof github.email === "string" ? github.email : undefined,
      display_name: String(github.login ?? ""),
    });
    const response = await issueSession(env, userId);
    response.headers.append("set-cookie", "oauth_state=; Max-Age=0; Path=/;");
    return response;
  }

  if (url.pathname === "/auth/email/start" && request.method === "POST") {
    const email = String(JSON.parse(body).email ?? "")
      .trim()
      .toLowerCase();
    if (!/^\S+@\S+\.\S+$/.test(email)) {
      return json({ error: "invalid email" }, 400);
    }
    const code = String(
      (crypto.getRandomValues(new Uint32Array(1))[0] % 900000) + 100000,
    );
    try {
      await deliverEmail(env, email, code);
    } catch {
      return json({ error: "email delivery is not configured" }, 503);
    }
    await dbRun(
      env.DB,
      "INSERT INTO email_codes(email,code_hash,attempts,expires_at,created_at) " +
        "VALUES(?,?,?,?,?) ON CONFLICT(email) DO UPDATE SET " +
        "code_hash=excluded.code_hash,attempts=0,expires_at=excluded.expires_at,created_at=excluded.created_at",
      email,
      await sha256Hex(code),
      0,
      now() + 600,
      now(),
    );
    return json({
      sent: true,
      ...(env.DEV_MODE === "true" ? { dev_code: code } : {}),
    });
  }

  if (url.pathname === "/auth/email/verify" && request.method === "POST") {
    const value = JSON.parse(body) as Row;
    const email = String(value.email ?? "")
      .trim()
      .toLowerCase();
    const code = String(value.code ?? "");
    const row = await dbOne(
      env.DB,
      "SELECT * FROM email_codes WHERE email=?",
      email,
    );
    const valid =
      row &&
      Number(row.expires_at) >= now() &&
      Number(row.attempts) < 5 &&
      (await sha256Hex(code)) === row.code_hash;
    if (!valid) {
      if (row) {
        await dbRun(
          env.DB,
          "UPDATE email_codes SET attempts=attempts+1 WHERE email=?",
          email,
        );
        if (Number(row.attempts) + 1 >= 5) {
          await dbRun(
            env.DB,
            "UPDATE email_codes SET expires_at=? WHERE email=?",
            now() - 1,
            email,
          );
        }
      }
      return json({ error: "invalid or expired code" }, 401);
    }
    await dbRun(env.DB, "DELETE FROM email_codes WHERE email=?", email);
    const userId = await ensureUser(env, { email, display_name: email });
    return issueSession(env, userId);
  }

  return undefined;
}
