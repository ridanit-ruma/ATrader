import { useState } from "react";
import { useParams, useSearchParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { fmtNum, fmtTime } from "../format";
import { CandleChart } from "../components/charts";
import { ErrorText, Section, Stat } from "../components/ui";

const INTERVALS = ["1m", "5m", "15m", "1h", "1d", "1w"];

export function Instrument() {
  const id = useParams().id!;
  const account = useSearchParams()[0].get("account") ?? undefined;
  const [interval, setInterval] = useState("1d");
  const q = useQuery({
    queryKey: ["account", account ?? "", "chart", id, interval],
    queryFn: () => api.chart(id, interval, account),
    refetchInterval: 30_000,
  });
  const quote = q.data?.quote;
  return (
    <div className="space-y-4">
      <div className="card grid grid-cols-2 gap-4 md:grid-cols-5">
        <Stat label={id}>{fmtNum(quote?.mid ?? quote?.last_trade_price)}</Stat>
        <Stat label="매수 호가">{fmtNum(quote?.bid)}</Stat>
        <Stat label="매도 호가">{fmtNum(quote?.ask)}</Stat>
        <Stat label="시장 충격 (bp)">{quote ? quote.impact_offset_bps.toFixed(1) : "—"}</Stat>
        <Stat label="기준 시각">{quote?.stale ? "지연됨" : fmtTime(quote?.as_of)}</Stat>
      </div>
      <Section
        title={account ? `차트 · ${account} 계좌의 매매 표시` : "차트"}
        right={
          <div className="flex gap-1 text-sm">
            {INTERVALS.map((i) => (
              <button key={i} className={`btn-quiet ${interval === i ? "bg-zinc-200 dark:bg-zinc-800" : ""}`} onClick={() => setInterval(i)}>
                {i}
              </button>
            ))}
          </div>
        }
      >
        <ErrorText error={q.error} />
        {q.data && <CandleChart candles={q.data.candles} markers={q.data.markers} />}
      </Section>
    </div>
  );
}
