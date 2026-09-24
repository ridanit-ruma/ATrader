import { Link } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { fmtKrw, fmtNum, fmtPct } from "../format";
import { ErrorText, Signed, Stat } from "../components/ui";

export function Overview() {
  const q = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  if (q.error) return <ErrorText error={q.error} />;
  if (!q.data) return null;
  if (!q.data.length) {
    return (
      <p className="card text-sm">
        계좌가 없습니다. <Link to="/settings" className="underline">설정</Link>에서 만드세요.
      </p>
    );
  }
  const total = q.data.reduce((s, r) => s + Number(r.summary.equity_krw), 0);
  return (
    <div className="space-y-4">
      <div className="card">
        <Stat label="전체 평가금액 (원)">{fmtKrw(total)}</Stat>
      </div>
      <div className="grid gap-4 md:grid-cols-2">
        {q.data.map((r) => (
          <Link key={r.id} to={`/accounts/${encodeURIComponent(r.id)}`} className="card block hover:border-zinc-400">
            <div className="mb-3 flex items-baseline justify-between gap-2">
              <span className="font-semibold">{r.summary.name}</span>
              <span className="text-xs text-zinc-500">
                {r.id} · {r.agent_id ? `에이전트 ${r.agent_id}` : "에이전트 없음"}
              </span>
            </div>
            <div className="grid grid-cols-3 gap-2">
              <Stat label="평가금액">{fmtKrw(r.summary.equity_krw)}</Stat>
              <Stat label="오늘 손익">
                <Signed value={r.day_pnl_krw}>{fmtKrw(r.day_pnl_krw)}</Signed>
              </Stat>
              <Stat label="누적 수익률">
                <Signed value={r.total_return_pct}>{fmtPct(r.total_return_pct)}</Signed>
              </Stat>
            </div>
            <div className="mt-3 flex flex-wrap gap-3 text-xs text-zinc-500">
              {r.summary.cash.map((c) => (
                <span key={c.currency}>
                  {c.currency} {fmtNum(c.balance)}
                </span>
              ))}
              <span>보유 종목 평가 {fmtKrw(r.summary.positions_value_krw)}원</span>
            </div>
          </Link>
        ))}
      </div>
    </div>
  );
}
