import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Send } from "lucide-react";
import { api } from "@/api";
import { ErrorText } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

const CADENCES = [
  { value: "off", label: "브리핑 끄기" },
  { value: "edges", label: "장 시작·마감만" },
  { value: "1h", label: "장 시작·마감 + 1시간마다" },
  { value: "2h", label: "장 시작·마감 + 2시간마다" },
  { value: "4h", label: "장 시작·마감 + 4시간마다" },
];

/** How often this account's conversation gets a market briefing, and a button to send one now. */
export function BriefingControl({ account, current }: { account: string; current: string | null }) {
  const qc = useQueryClient();
  const save = useMutation({
    mutationFn: (b: string) => api.setBriefing(account, b),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["overview"] }),
  });
  const send = useMutation({ mutationFn: () => api.sendBriefing(account) });
  return (
    <div className="grid gap-1">
      <div className="flex items-center gap-2">
        <Select items={CADENCES} value={current ?? "2h"} onValueChange={(v) => v && save.mutate(v as string)}>
          <SelectTrigger className="w-64" aria-label="정기 브리핑">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {CADENCES.map((c) => (
              <SelectItem key={c.value} value={c.value}>
                {c.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button variant="outline" size="sm" disabled={send.isPending} onClick={() => send.mutate()}>
          <Send /> {send.isSuccess ? "보냄" : "지금 보내기"}
        </Button>
      </div>
      <ErrorText error={save.error ?? send.error} />
    </div>
  );
}
