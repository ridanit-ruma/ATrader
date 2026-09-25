// Mirrors src/web/mod.rs and src/tools/dto.rs. Decimals arrive as JSON numbers.
export type Dec = number;

export interface Me {
  username: string;
  totp_enabled: boolean;
  mfa_pending: boolean;
}

export interface CashView {
  currency: string;
  balance: Dec;
  available: Dec;
}

export interface AccountSummary {
  id: string;
  name: string;
  cash: CashView[];
  positions_value_krw: Dec;
  equity_krw: Dec;
  usd_krw: Dec;
  as_of: string;
}

export interface OverviewRow {
  id: string;
  agent_id: string | null;
  generation: number;
  summary: AccountSummary;
  day_pnl_krw: Dec | null;
  total_return_pct: Dec | null;
}

export interface PositionView {
  instrument: string;
  name: string;
  currency: string;
  qty: Dec;
  avg_cost: Dec;
  price: Dec | null;
  market_value: Dec;
  unrealized_pnl: Dec;
  unrealized_pct: Dec;
  weight_pct: Dec;
}

export interface OrderView {
  id: number;
  account: string;
  instrument: string;
  side: "buy" | "sell";
  kind: string;
  qty: Dec | null;
  notional: Dec | null;
  limit_price: Dec | null;
  tif: string;
  status: string;
  filled_qty: Dec;
  avg_price: Dec | null;
  reason: string;
  created_at: string;
}

export interface FillView {
  order_id: number;
  instrument: string;
  side: "buy" | "sell";
  qty: Dec;
  price: Dec;
  notional: Dec;
  fee: Dec;
  tax: Dec;
  realized_pnl: Dec | null;
  liquidity: string;
  at: string;
  reason?: string | null;
}

export interface PerformanceView {
  period: string;
  start_equity_krw: Dec;
  end_equity_krw: Dec;
  return_pct: Dec;
  max_drawdown_pct: Dec;
  volatility_pct: number | null;
  sharpe: number | null;
  trades: number;
  sells: number;
  win_rate_pct: Dec | null;
  realized_pnl_krw: Dec;
  fees_krw: Dec;
  turnover: Dec | null;
}

export interface AccountDetail {
  agent_id: string | null;
  generation: number;
  summary: AccountSummary;
  positions: PositionView[];
  open_orders: OrderView[];
  performance_all: PerformanceView | null;
}

export interface EquityPoint {
  at: string;
  equity_krw: Dec;
}

export interface Pnl {
  daily: { date: string; pnl_krw: Dec }[];
  by_symbol: { instrument: string; realized: Dec; fees: Dec }[];
}

export interface AlertView {
  id: number;
  kind: string;
  instrument: string | null;
  venue: string | null;
  threshold: Dec | null;
  window_minutes: number | null;
  note: string;
  once: boolean;
  active: boolean;
  created_at: string;
  last_fired_at: string | null;
}

export interface CandleView {
  start: string;
  open: Dec;
  high: Dec;
  low: Dec;
  close: Dec;
  volume: Dec;
  value: Dec;
}

export interface Quote {
  id: string;
  currency: string;
  bid: Dec | null;
  ask: Dec | null;
  mid: Dec | null;
  real_bid: Dec | null;
  real_ask: Dec | null;
  last_trade_price: Dec | null;
  last_trade_at: string | null;
  impact_offset_bps: number;
  as_of: string | null;
  stale: boolean;
  error: string | null;
}

export interface Chart {
  candles: CandleView[];
  markers: { at: string; side: string; price: Dec; qty: Dec }[];
  quote: Quote | null;
}

export interface Health {
  feeds: { venue: string; subscribed: number }[];
  zyris_connected: boolean;
}

export interface SessionRow {
  id: string;
  ip: string;
  user_agent: string;
  created_at: string;
  last_seen: string;
  current: boolean;
}

export interface AuditRow {
  at: string;
  action: string;
  detail: string;
  ip: string;
}

export interface KeySetting {
  name: string;
  secret: boolean;
  configured: boolean;
  source: "env" | "file" | "dashboard" | null;
  value: string | null;
}

export interface Enrollment {
  status: "idle" | "pending" | "granted" | "denied" | "expired" | "error";
  user_code: string | null;
  verification_uri: string | null;
  expires_at: string | null;
  message: string | null;
}

export interface ZyrisStatus {
  connected: boolean;
  enrolled: boolean;
  source: "env" | "file" | "dashboard" | null;
  enrollment: Enrollment;
}
