import { useState } from "react";
import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { fmtKrw, fmtNum, fmtPct, fmtTime } from "../format";
import { EquityChart, PnlBars } from "../components/charts";
import { ErrorText, Section, Signed, Stat, Table } from "../components/ui";

const RANGES = [
  ["1d", "1일"],
  ["1w", "1주"],
  ["1m", "1개월"],
  ["all", "전체"],
] as const;

export function Account() {
  const id = useParams().id!;
  const [range, setRange] = useState<string>("1m");
  const detail = useQuery({ queryKey: ["account", id], queryFn: () => api.account(id) });
  const equity = useQuery({ queryKey: ["account", id, "equity", range], queryFn: () => api.equity(id, range) });
  const pnl = useQuery({ queryKey: ["account", id, "pnl"], queryFn: () => api.pnl(id) });
  const fills = useQuery({ queryKey: ["account", id, "fills"], queryFn: () => api.fills(id) });

  if (detail.error) return <ErrorText error={detail.error} />;
  const d = detail.data;
  if (!d) return null;
  const perf = d.performance_all;
  const inst = (i: string) => `/instruments/${encodeURIComponent(i)}?account=${encodeURIComponent(id)}`;

  return (
    <div className="space-y-4">
      <div className="card grid grid-cols-2 gap-4 md:grid-cols-4">
        <Stat label={`${d.summary.name} 평가금액`}>{fmtKrw(d.summary.equity_krw)}</Stat>
        <Stat label="누적 수익률">
          <Signed value={perf?.return_pct}>{fmtPct(perf?.return_pct)}</Signed>
        </Stat>
        <Stat label="실현 손익">
          <Signed value={perf?.realized_pnl_krw}>{fmtKrw(perf?.realized_pnl_krw)}</Signed>
        </Stat>
        <Stat label="최대 낙폭">{fmtPct(perf ? -Number(perf.max_drawdown_pct) : null)}</Stat>
        <Stat label="거래 수">{perf?.trades ?? "—"}</Stat>
        <Stat label="승률">{perf?.win_rate_pct == null ? "—" : `${Number(perf.win_rate_pct).toFixed(1)}%`}</Stat>
        <Stat label="샤프">{perf?.sharpe == null ? "—" : perf.sharpe.toFixed(2)}</Stat>
        <Stat label="수수료·세금">{fmtKrw(perf?.fees_krw)}</Stat>
      </div>

      <Section
        title="평가금액 추이"
        right={
          <div className="flex gap-1 text-sm">
            {RANGES.map(([k, label]) => (
              <button key={k} className={`btn-quiet ${range === k ? "bg-zinc-200 dark:bg-zinc-800" : ""}`} onClick={() => setRange(k)}>
                {label}
              </button>
            ))}
          </div>
        }
      >
        {equity.data?.length ? <EquityChart points={equity.data} /> : <p className="text-sm text-zinc-500">아직 기록이 없습니다.</p>}
      </Section>

      <Section title="일별 손익">
        {pnl.data?.daily.length ? <PnlBars daily={pnl.data.daily} /> : <p className="text-sm text-zinc-500">아직 기록이 없습니다.</p>}
      </Section>

      <Section title="보유 종목">
        <Table head={["종목", "수량", "평균단가", "현재가", "평가금액", "평가손익", "수익률", "비중"]} empty={!d.positions.length}>
          {d.positions.map((p) => (
            <tr key={p.instrument}>
              <td className="px-2 py-1">
                <Link to={inst(p.instrument)} className="underline">
                  {p.name}
                </Link>
                <div className="text-xs text-zinc-500">{p.instrument}</div>
              </td>
              <td className="num px-2">{fmtNum(p.qty)}</td>
              <td className="num px-2">{fmtNum(p.avg_cost)}</td>
              <td className="num px-2">{fmtNum(p.price)}</td>
              <td className="num px-2">{fmtNum(p.market_value)}</td>
              <td className="num px-2">
                <Signed value={p.unrealized_pnl}>{fmtNum(p.unrealized_pnl)}</Signed>
              </td>
              <td className="num px-2">
                <Signed value={p.unrealized_pct}>{fmtPct(p.unrealized_pct)}</Signed>
              </td>
              <td className="num px-2">{fmtPct(p.weight_pct).replace("+", "")}</td>
            </tr>
          ))}
        </Table>
      </Section>

      <Section title="미체결 주문">
        <Table head={["종목", "구분", "유형", "수량", "지정가", "체결", "사유", "주문 시각"]} empty={!d.open_orders.length}>
          {d.open_orders.map((o) => (
            <tr key={o.id}>
              <td className="px-2 py-1">
                <Link to={inst(o.instrument)} className="underline">
                  {o.instrument}
                </Link>
              </td>
              <td className="num px-2">{o.side === "buy" ? "매수" : "매도"}</td>
              <td className="num px-2">{o.kind}</td>
              <td className="num px-2">{fmtNum(o.qty ?? o.notional)}</td>
              <td className="num px-2">{fmtNum(o.limit_price)}</td>
              <td className="num px-2">{fmtNum(o.filled_qty)}</td>
              <td className="px-2 text-right text-xs">{o.reason}</td>
              <td className="num px-2 text-xs">{fmtTime(o.created_at)}</td>
            </tr>
          ))}
        </Table>
      </Section>

      <Section title="체결 내역">
        <Table head={["시각", "종목", "구분", "수량", "가격", "수수료·세금", "실현손익", "에이전트의 판단"]} empty={!fills.data?.length}>
          {fills.data?.map((f, i) => (
            <tr key={`${f.order_id}-${i}`}>
              <td className="px-2 py-1 text-xs whitespace-nowrap">{fmtTime(f.at)}</td>
              <td className="num px-2">
                <Link to={inst(f.instrument)} className="underline">
                  {f.instrument}
                </Link>
              </td>
              <td className="num px-2">{f.side === "buy" ? "매수" : "매도"}</td>
              <td className="num px-2">{fmtNum(f.qty)}</td>
              <td className="num px-2">{fmtNum(f.price)}</td>
              <td className="num px-2">{fmtNum(Number(f.fee) + Number(f.tax))}</td>
              <td className="num px-2">
                <Signed value={f.realized_pnl}>{fmtNum(f.realized_pnl)}</Signed>
              </td>
              <td className="max-w-md px-2 text-right text-xs">{f.reason}</td>
            </tr>
          ))}
        </Table>
      </Section>

      <Section title="종목별 실현 손익 (거래 통화)">
        <Table head={["종목", "실현손익", "수수료·세금"]} empty={!pnl.data?.by_symbol.length}>
          {pnl.data?.by_symbol.map((r) => (
            <tr key={r.instrument}>
              <td className="px-2 py-1">{r.instrument}</td>
              <td className="num px-2">
                <Signed value={r.realized}>{fmtNum(r.realized)}</Signed>
              </td>
              <td className="num px-2">{fmtNum(r.fees)}</td>
            </tr>
          ))}
        </Table>
      </Section>
    </div>
  );
}
