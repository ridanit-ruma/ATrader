import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { api } from "@/api";
import { ErrorText } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

const NONE = "__none";

/** Which conversation in Attacca's "ATrader" project receives this account's alerts. */
export function AlertSessionPicker({ account, current }: { account: string; current: string | null }) {
  const qc = useQueryClient();
  const sessions = useQuery({ queryKey: ["attacca-sessions"], queryFn: api.attaccaSessions, retry: false, staleTime: 30_000 });
  const save = useMutation({
    mutationFn: (session: string | null) => api.setAlertSession(account, session),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["overview"] }),
  });
  if (sessions.isError) {
    return <p className="text-xs text-muted-foreground">Attacca에 연결되지 않아 대화 목록을 불러올 수 없습니다.</p>;
  }
  const list = sessions.data?.sessions ?? [];
  const items = [{ value: NONE, label: "선택 안 함 (알림 보내지 않음)" }, ...list.map((x) => ({ value: x.id, label: x.title || "제목 없는 대화" }))];
  if (current && !list.some((x) => x.id === current)) items.push({ value: current, label: "ATrader 프로젝트 밖의 대화" });
  return (
    <div className="grid gap-1">
      <div className="flex items-center gap-2">
        <Select items={items} value={current ?? NONE} onValueChange={(v) => save.mutate(v === NONE || !v ? null : (v as string))}>
          <SelectTrigger className="w-64" aria-label="알림 받을 대화">
            <SelectValue placeholder={sessions.isLoading ? "불러오는 중…" : "알림 받을 대화"} />
          </SelectTrigger>
          <SelectContent>
            {items.map((i) => (
              <SelectItem key={i.value} value={i.value}>
                {i.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button variant="ghost" size="icon" aria-label="대화 목록 새로고침" onClick={() => sessions.refetch()}>
          <RefreshCw className={sessions.isFetching ? "animate-spin" : ""} />
        </Button>
      </div>
      <ErrorText error={save.error} />
    </div>
  );
}
