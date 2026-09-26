import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router";
import { api } from "@/api";
import { fmtKrw, fmtNum, fmtPct, fmtTime } from "@/format";
import { EquityChart, PnlBars } from "@/components/charts";
import { Choice, DataTable, ErrorText, Metric, Muted, PageHeader, Section, Signed } from "@/components/common";
import { TableCell, TableRow } from "@/components/ui/table";

const RANGES = [
  ["1d", "1일"],
  ["1w", "1주"],
  ["1m", "1개월"],
  ["all", "전체"],
] as const;

export function Account() {
  const id = useParams().id!;
  const [range, setRange] = useState<(typeof RANGES)[number][0]>("1m");
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
    <div className="grid gap-4">
      <PageHeader title={d.summary.name} description={`ID ${id}`} />
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <Metric
          label="평가금액 (원)"
          value={fmtKrw(d.summary.equity_krw)}
          trend={perf?.return_pct}
          trendLabel={fmtPct(perf?.return_pct)}
          footer={`현금 ${d.summary.cash.map((c) => `${c.currency} ${fmtNum(c.balance)}`).join(" · ")}`}
        />
        <Metric
          label="실현 손익 (원)"
          value={<Signed value={perf?.realized_pnl_krw}>{fmtKrw(perf?.realized_pnl_krw)}</Signed>}
          footer={`수수료·세금 ${fmtKrw(perf?.fees_krw)}원`}
        />
        <Metric
          label="최대 낙폭"
          value={fmtPct(perf ? -Number(perf.max_drawdown_pct) : null)}
          footer={`샤프 ${perf?.sharpe == null ? "—" : perf.sharpe.toFixed(2)}`}
        />
        <Metric
          label="거래"
          value={perf?.trades ?? "—"}
          footer={`승률 ${perf?.win_rate_pct == null ? "—" : `${Number(perf.win_rate_pct).toFixed(1)}%`}`}
        />
      </div>

      <Section
        title="평가금액 추이"
        right={
          <Choice value={range} options={RANGES} onChange={setRange} />
        }
      >
        {equity.data?.length ? <EquityChart points={equity.data} /> : <Muted>아직 기록이 없습니다.</Muted>}
      </Section>

      <Section title="일별 손익">
        {pnl.data?.daily.length ? <PnlBars daily={pnl.data.daily} /> : <Muted>아직 기록이 없습니다.</Muted>}
      </Section>

      <Section title="보유 종목">
        <DataTable head={["종목", "수량", "평균단가", "현재가", "평가금액", "평가손익", "수익률", "비중"]} empty={!d.positions.length}>
          {d.positions.map((p) => (
            <TableRow key={p.instrument}>
              <TableCell>
                <Link to={inst(p.instrument)} className="underline">
                  {p.name}
                </Link>
                <div className="text-xs text-muted-foreground">{p.instrument}</div>
              </TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(p.qty)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(p.avg_cost)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(p.price)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(p.market_value)}</TableCell>
              <TableCell className="text-right tabular-nums">
                <Signed value={p.unrealized_pnl}>{fmtNum(p.unrealized_pnl)}</Signed>
              </TableCell>
              <TableCell className="text-right tabular-nums">
                <Signed value={p.unrealized_pct}>{fmtPct(p.unrealized_pct)}</Signed>
              </TableCell>
              <TableCell className="text-right tabular-nums">{fmtPct(p.weight_pct).replace("+", "")}</TableCell>
            </TableRow>
          ))}
        </DataTable>
      </Section>

      <Section title="미체결 주문">
        <DataTable head={["종목", "구분", "유형", "수량", "지정가", "체결", "사유", "주문 시각"]} empty={!d.open_orders.length}>
          {d.open_orders.map((o) => (
            <TableRow key={o.id}>
              <TableCell>
                <Link to={inst(o.instrument)} className="underline">
                  {o.instrument}
                </Link>
              </TableCell>
              <TableCell className="text-right tabular-nums">{o.side === "buy" ? "매수" : "매도"}</TableCell>
              <TableCell className="text-right tabular-nums">{o.kind}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(o.qty ?? o.notional)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(o.limit_price)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(o.filled_qty)}</TableCell>
              <TableCell className="text-right text-xs whitespace-normal">{o.reason}</TableCell>
              <TableCell className="text-right text-xs tabular-nums">{fmtTime(o.created_at)}</TableCell>
            </TableRow>
          ))}
        </DataTable>
      </Section>

      <Section title="체결 내역">
        <DataTable head={["시각", "종목", "구분", "수량", "가격", "수수료·세금", "실현손익", "에이전트의 판단"]} empty={!fills.data?.length}>
          {fills.data?.map((f, i) => (
            <TableRow key={`${f.order_id}-${i}`}>
              <TableCell className="text-xs">{fmtTime(f.at)}</TableCell>
              <TableCell className="text-right tabular-nums">
                <Link to={inst(f.instrument)} className="underline">
                  {f.instrument}
                </Link>
              </TableCell>
              <TableCell className="text-right tabular-nums">{f.side === "buy" ? "매수" : "매도"}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(f.qty)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(f.price)}</TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(Number(f.fee) + Number(f.tax))}</TableCell>
              <TableCell className="text-right tabular-nums">
                <Signed value={f.realized_pnl}>{fmtNum(f.realized_pnl)}</Signed>
              </TableCell>
              <TableCell className="max-w-md text-right text-xs whitespace-normal">{f.reason}</TableCell>
            </TableRow>
          ))}
        </DataTable>
      </Section>

      <Section title="종목별 실현 손익 (거래 통화)">
        <DataTable head={["종목", "실현손익", "수수료·세금"]} empty={!pnl.data?.by_symbol.length}>
          {pnl.data?.by_symbol.map((r) => (
            <TableRow key={r.instrument}>
              <TableCell>{r.instrument}</TableCell>
              <TableCell className="text-right tabular-nums">
                <Signed value={r.realized}>{fmtNum(r.realized)}</Signed>
              </TableCell>
              <TableCell className="text-right tabular-nums">{fmtNum(r.fees)}</TableCell>
            </TableRow>
          ))}
        </DataTable>
      </Section>
    </div>
  );
}

