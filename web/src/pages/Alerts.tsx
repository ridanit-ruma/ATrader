import { useQueries, useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { fmtNum, fmtTime } from "@/format";
import { Section, DataTable, PageHeader } from "@/components/common";
import { TableCell, TableRow } from "@/components/ui/table";

export function Alerts() {
  const overview = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  const accounts = overview.data ?? [];
  const alerts = useQueries({
    queries: accounts.map((a) => ({ queryKey: ["account", a.id, "alerts"], queryFn: () => api.alerts(a.id) })),
  });
  return (
    <div className="grid gap-4">
      <PageHeader title="알림" description="에이전트가 걸어 둔 알림" />
      {accounts.map((a, i) => {
        const rows = alerts[i]?.data ?? [];
        return (
          <Section key={a.id} title={`${a.summary.name} (${a.id})`}>
            <DataTable head={["종류", "대상", "기준", "메모", "상태", "마지막 발동", "만든 시각"]} empty={!rows.length}>
              {rows.map((r) => (
                <TableRow key={r.id} className={r.active ? "" : "text-muted-foreground"}>
                  <TableCell>{r.kind}</TableCell>
                  <TableCell className="text-right tabular-nums">{r.instrument ?? r.venue ?? "—"}</TableCell>
                  <TableCell className="text-right tabular-nums">
                    {fmtNum(r.threshold)}
                    {r.window_minutes ? ` / ${r.window_minutes}분` : ""}
                  </TableCell>
                  <TableCell className="max-w-xs text-right text-xs whitespace-normal">{r.note}</TableCell>
                  <TableCell className="text-right tabular-nums">{r.active ? (r.once ? "대기 (1회)" : "대기") : "꺼짐"}</TableCell>
                  <TableCell className="text-right text-xs tabular-nums">{fmtTime(r.last_fired_at)}</TableCell>
                  <TableCell className="text-right text-xs tabular-nums">{fmtTime(r.created_at)}</TableCell>
                </TableRow>
              ))}
            </DataTable>
          </Section>
        );
      })}
    </div>
  );
}
