import type * as T from "./types";

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

async function call<R>(method: string, path: string, body?: unknown): Promise<R> {
  const headers: Record<string, string> = {};
  if (method !== "GET") headers["X-Requested-With"] = "atrader";
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(path, {
    method,
    headers,
    credentials: "same-origin",
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(res.status, data.error ?? "error", data.message ?? res.statusText);
  return data as R;
}

const get = <R>(path: string) => call<R>("GET", path);
const post = <R>(path: string, body?: unknown) => call<R>("POST", path, body ?? {});
const enc = encodeURIComponent;

export type Cash = Record<string, string>;

export const api = {
  login: (username: string, password: string, code: string) =>
    post<{ mfa_pending: boolean }>("/api/auth/login", { username, password, code: code || undefined }),
  logout: () => post("/api/auth/logout"),
  me: () => get<T.Me>("/api/auth/me"),
  totpSetup: () => post<{ secret: string; otpauth_url: string }>("/api/auth/totp/setup"),
  totpEnable: (code: string) => post<{ recovery_codes: string[] }>("/api/auth/totp/enable", { code }),
  password: (current: string, next: string) => post("/api/auth/password", { current, new: next }),
  sessions: () => get<T.SessionRow[]>("/api/auth/sessions"),
  revoke: (id: string) => call("DELETE", `/api/auth/sessions/${enc(id)}`),
  audit: () => get<T.AuditRow[]>("/api/audit"),
  overview: () => get<T.OverviewRow[]>("/api/overview"),
  createAccount: (id: string, name: string, agent_id: string, cash: Cash) =>
    post("/api/accounts", { id, name, agent_id: agent_id || undefined, cash }),
  resetAccount: (id: string, cash: Cash) => post(`/api/accounts/${enc(id)}/reset`, { cash }),
  account: (id: string) => get<T.AccountDetail>(`/api/accounts/${enc(id)}`),
  equity: (id: string, range: string) => get<T.EquityPoint[]>(`/api/accounts/${enc(id)}/equity?range=${range}`),
  fills: (id: string) => get<T.FillView[]>(`/api/accounts/${enc(id)}/fills?limit=200`),
  orders: (id: string) => get<T.OrderView[]>(`/api/accounts/${enc(id)}/orders?limit=200`),
  pnl: (id: string) => get<T.Pnl>(`/api/accounts/${enc(id)}/pnl`),
  alerts: (id: string) => get<T.AlertView[]>(`/api/accounts/${enc(id)}/alerts`),
  chart: (id: string, interval: string, account?: string) =>
    get<T.Chart>(`/api/instruments/${enc(id)}/chart?interval=${interval}&limit=300${account ? `&account=${enc(account)}` : ""}`),
  health: () => get<T.Health>("/api/health"),
  agents: () => get<T.Agent[]>("/api/agents"),
  setAgent: (id: string, agent_id: string | null) => call("PUT", `/api/accounts/${enc(id)}/agent`, { agent_id }),
  keys: () => get<T.KeySetting[]>("/api/settings/keys"),
  saveKeys: (values: Record<string, string>) => call<{ restarting: boolean }>("PUT", "/api/settings/keys", values),
  zyris: () => get<T.ZyrisStatus>("/api/zyris"),
  zyrisEnroll: () => post<T.Enrollment>("/api/zyris/enroll"),
};

/** Wait for the server to go down and come back after it restarts to apply settings. Gives up
 * waiting for the drop after 20 s (it may have restarted between two polls). */
export async function waitForRestart(): Promise<void> {
  const up = async () => {
    try {
      await api.me();
      return true;
    } catch (e) {
      return e instanceof ApiError && e.status < 500;
    }
  };
  let wentDown = false;
  for (let i = 0; i < 90; i++) {
    await new Promise((r) => setTimeout(r, 1000));
    const ok = await up();
    if (!ok) wentDown = true;
    else if (wentDown || i >= 20) return;
  }
}
