import { useQueries, useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { fmtNum, fmtTime } from "../format";
import { Section, Table } from "../components/ui";

export function Alerts() {
  const overview = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  const accounts = overview.data ?? [];
  const alerts = useQueries({
    queries: accounts.map((a) => ({ queryKey: ["account", a.id, "alerts"], queryFn: () => api.alerts(a.id) })),
  });
  return (
    <div className="space-y-4">
      {accounts.map((a, i) => {
        const rows = alerts[i]?.data ?? [];
        return (
          <Section key={a.id} title={`${a.summary.name} (${a.id})`}>
            <Table head={["종류", "대상", "기준", "메모", "상태", "마지막 발동", "만든 시각"]} empty={!rows.length}>
              {rows.map((r) => (
                <tr key={r.id} className={r.active ? "" : "text-zinc-400"}>
                  <td className="px-2 py-1">{r.kind}</td>
                  <td className="num px-2">{r.instrument ?? r.venue ?? "—"}</td>
                  <td className="num px-2">
                    {fmtNum(r.threshold)}
                    {r.window_minutes ? ` / ${r.window_minutes}분` : ""}
                  </td>
                  <td className="max-w-xs px-2 text-right text-xs">{r.note}</td>
                  <td className="num px-2">{r.active ? (r.once ? "대기 (1회)" : "대기") : "꺼짐"}</td>
                  <td className="num px-2 text-xs">{fmtTime(r.last_fired_at)}</td>
                  <td className="num px-2 text-xs">{fmtTime(r.created_at)}</td>
                </tr>
              ))}
            </Table>
          </Section>
        );
      })}
    </div>
  );
}
