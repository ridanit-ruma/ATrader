import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { Health, OverviewRow } from "./types";

/** Live updates from `/api/stream`: fills and orders refresh their account, equity ticks patch the overview. */
export function useLiveStream(enabled: boolean) {
  const qc = useQueryClient();
  useEffect(() => {
    if (!enabled) return;
    const es = new EventSource("/api/stream");
    // Fill events carry no account id, so refresh every account's queries; there are only a few.
    const refresh = () => {
      qc.invalidateQueries({ queryKey: ["overview"] });
      qc.invalidateQueries({ queryKey: ["account"] });
    };
    es.addEventListener("fill", refresh);
    es.addEventListener("order", refresh);
    es.addEventListener("equity", (e) => {
      const tick = JSON.parse((e as MessageEvent).data) as { account: string; equity_krw: number; at: string };
      qc.setQueryData<OverviewRow[]>(["overview"], (rows) =>
        rows?.map((r) => (r.id === tick.account ? { ...r, summary: { ...r.summary, equity_krw: tick.equity_krw, as_of: tick.at } } : r)),
      );
    });
    es.addEventListener("health", (e) => qc.setQueryData<Health>(["health"], JSON.parse((e as MessageEvent).data)));
    return () => es.close();
  }, [enabled, qc]);
}
