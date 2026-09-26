import { Link } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { fmtKrw, fmtNum, fmtPct } from "@/format";
import { ErrorText, Metric, Muted, PageHeader, Signed, Stat } from "@/components/common";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

export function Overview() {
  const q = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  if (q.error) return <ErrorText error={q.error} />;
  if (!q.data) return null;
  if (!q.data.length) {
    return (
      <Muted>
        계좌가 없습니다.{" "}
        <Link to="/settings" className="underline">
          설정
        </Link>
        에서 만드세요.
      </Muted>
    );
  }
  const total = q.data.reduce((s, r) => s + Number(r.summary.equity_krw), 0);
  const known = q.data.filter((r) => r.day_pnl_krw !== null);
  const dayPnl = known.length ? known.reduce((s, r) => s + Number(r.day_pnl_krw), 0) : null;
  const dayPct = dayPnl === null || total === dayPnl ? null : (dayPnl / (total - dayPnl)) * 100;
  return (
    <div className="grid gap-4">
      <PageHeader title="개요" description="모든 계좌의 현재 상태" />
      <div className="grid gap-4 sm:grid-cols-3">
        <Metric label="전체 평가금액 (원)" value={fmtKrw(total)} trend={dayPct} trendLabel={fmtPct(dayPct)} footer="오늘 00시(KST) 대비" />
        <Metric label="오늘 손익 (원)" value={<Signed value={dayPnl}>{fmtKrw(dayPnl)}</Signed>} footer={`${known.length}개 계좌 기준`} />
        <Metric label="계좌" value={q.data.length} />
      </div>
      <div className="grid gap-4 md:grid-cols-2">
        {q.data.map((r) => (
          <Link key={r.id} to={`/accounts/${encodeURIComponent(r.id)}`} className="rounded-xl transition hover:ring-2 hover:ring-ring/40">
            <Card className="h-full">
              <CardHeader>
                <CardTitle>{r.summary.name}</CardTitle>
              </CardHeader>
              <CardContent className="grid gap-3">
                <div className="grid grid-cols-3 gap-2">
                  <Stat label="평가금액">{fmtKrw(r.summary.equity_krw)}</Stat>
                  <Stat label="오늘 손익">
                    <Signed value={r.day_pnl_krw}>{fmtKrw(r.day_pnl_krw)}</Signed>
                  </Stat>
                  <Stat label="누적 수익률">
                    <Signed value={r.total_return_pct}>{fmtPct(r.total_return_pct)}</Signed>
                  </Stat>
                </div>
                <div className="flex flex-wrap gap-3 text-xs text-muted-foreground">
                  {r.summary.cash.map((c) => (
                    <span key={c.currency}>
                      {c.currency} {fmtNum(c.balance)}
                    </span>
                  ))}
                  <span>보유 종목 평가 {fmtKrw(r.summary.positions_value_krw)}원</span>
                </div>
              </CardContent>
            </Card>
          </Link>
        ))}
      </div>
    </div>
  );
}
