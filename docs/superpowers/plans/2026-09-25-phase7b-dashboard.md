# Phase 7b: React dashboard — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** The single-page dashboard from spec §10, served by `atrader` from `web/dist`, on top of the Phase 7a API.

**Architecture:** Vite + React + TypeScript in `web/`. TanStack Query owns server state; one SSE hook
invalidates queries and patches live equity. `lightweight-charts` draws the equity curve, daily PnL
bars and candles. Tailwind v4 through its Vite plugin. No component library, no state library.

**Tech stack:** react 19, react-router 7 (library mode, `createBrowserRouter`), @tanstack/react-query 5,
lightweight-charts 5, tailwindcss 4 + @tailwindcss/vite, qrcode (TOTP enrolment QR as a data URL,
allowed by the CSP's `img-src data:`), vitest for pure helpers.

## Global Constraints

- Every request goes through `web/src/api.ts`: `fetch` with `credentials: "same-origin"`, JSON, and
  `X-Requested-With: atrader` on non-GET. A 401 sends the user to `/login`; a 403 `mfa_required`
  sends them to `/enrol`.
- Money arrives as JSON numbers or strings (arbitrary-precision decimals). Display only; never do
  arithmetic on money in the browser except differences for colouring. Format with
  `Intl.NumberFormat("ko-KR")`.
- **Colours:** up is red and down is blue by default (Korean convention); Settings switches to
  green-up/red-down. Stored per browser in `localStorage` (`atrader.colors`), wrapped in try/catch.
  One helper `tone(x)` returns the class for a signed number; every signed figure uses it.
- UI copy is Korean (the user is Korean; spec §2 exception 1). Code, comments and identifiers are English.
- Light and dark themes via `prefers-color-scheme`.
- CSP is `default-src 'self'`: no inline scripts, no external fonts or CDNs. Vite's build output
  complies; `style-src 'unsafe-inline'` covers lightweight-charts' inline styles.
- `web/dist` is not committed. `cargo build` embeds whatever is there (`allow_missing`); the
  README documents `npm ci && npm run build` in `web/` first. Phase 8's Nix build does the same.
- Dev: `npm run dev` proxies `/api` to `http://127.0.0.1:8750`.

## Scope trimmed from spec §10 (API has no data for these; add with the API)

- Instrument page: order book and shadow-vs-real price (no endpoint yet).
- Alerts page: per-fire delivery history (the API lists alerts with their last state only).
- Settings: archiving accounts (no archive concept in the store).

## File Structure

```
web/
  package.json, vite.config.ts, tsconfig.json, index.html
  src/main.tsx          router + QueryClient
  src/api.ts            fetch wrapper, typed endpoints
  src/types.ts          response types mirroring src/web/mod.rs and tools/dto.rs
  src/format.ts         money/pct/date formatting, tone(); format.test.ts
  src/colors.ts         colour preference (localStorage)
  src/stream.ts         useLiveStream(): EventSource('/api/stream') → query invalidation
  src/components/       Layout, Table, Stat, EquityChart, PnlBars, CandleChart
  src/pages/            Login, Enrol, Overview, Account, Instrument, Alerts, Settings
```

### Task 1: Scaffold, API client, auth flow

- [ ] Create `web/` with Vite (react-ts), add the dependencies above, Tailwind plugin, the dev proxy
  and `vitest`. Add `web/node_modules` and `web/dist` to `.gitignore`.
- [ ] `format.test.ts` (RED first): `fmtKrw(1234567.4) === "1,234,567"`, `fmtPct(-1.234) === "-1.23%"`,
  `tone(1)`/`tone(-1)`/`tone(0)` give up/down/neutral classes and swap when the preference is
  green-up.
- [ ] `api.ts`, `types.ts`, `format.ts`, `colors.ts`.
- [ ] Pages: `Login` (username, password, code; shows the server's message on 401/429),
  `Enrol` (setup → QR + secret → code → shows the ten recovery codes once with a confirm button),
  `Layout` (nav: 개요, 알림, 설정; logout). Router guard: `GET /api/auth/me`; 401 → `/login`,
  `mfa_pending` → `/enrol`.
- [ ] `npm test` and `npm run build` pass. Commit: "Scaffold the dashboard with login and TOTP enrolment".

### Task 2: Overview and account pages, live stream

- [ ] `Overview`: a card per account (name, agent, equity, day PnL, total return) plus allocation by
  currency from `summary.cash` and positions; links to the account page.
- [ ] `Account`: equity chart with 1D/1W/1M/All (`/equity?range`), daily PnL bars (`/pnl.daily`),
  positions table (qty, avg, price, value, ± unrealised), open orders, fills with the agent's
  `reason`, realised PnL by symbol, performance stats (`performance_all`).
- [ ] `stream.ts`: on `fill`/`order` invalidate the account's queries; on `equity` update the
  overview cache in place; on `health` update a header indicator. Reconnect is EventSource's own.
- [ ] Build passes. Commit: "Add overview and account pages with a live stream".

### Task 3: Instrument, alerts and settings

- [ ] `Instrument` (`/instrument/:id?account=`): candles with interval picker and this account's
  buy/sell markers (`createSeriesMarkers`), latest quote.
- [ ] `Alerts`: per account, `list_alerts` output.
- [ ] `Settings`: create account (id, name, agent id, cash per currency), reset account (confirm by
  typing the id), change password, sessions with revoke, audit log, feed and zyris status, colour
  preference.
- [ ] Build passes. Commit: "Add instrument, alerts and settings pages".

### Task 4: Serve and smoke

- [ ] `npm run build`, `cargo build`, run `serve --no-zyris` with a user created, and drive the
  real flow in a browser (Playwright): login → enrol → overview → account → settings create/reset.
  Fix what breaks.
- [ ] README: build steps for the dashboard. Commit: "Document building the dashboard".
