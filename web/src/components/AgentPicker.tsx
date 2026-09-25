import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

const NONE = "__none";

/** Pick an Attacca agent for an account; free text when Attacca is not connected. `""` is none. */
export function AgentPicker({ value, onChange, id }: { value: string; onChange: (agentId: string) => void; id?: string }) {
  const agents = useQuery({ queryKey: ["agents"], queryFn: api.agents, retry: false, staleTime: 60_000 });
  if (agents.isError) {
    return (
      <div className="grid gap-1">
        <Input id={id} placeholder="Attacca 에이전트 id" value={value} onChange={(e) => onChange(e.target.value)} />
        <p className="text-xs text-muted-foreground">Attacca에 연결되지 않아 목록을 불러올 수 없습니다. id를 직접 입력하세요.</p>
      </div>
    );
  }
  const list = agents.data ?? [];
  const items = [{ value: NONE, label: "에이전트 없음 (사람이 관리)" }, ...list.map((a) => ({ value: a.id, label: a.name }))];
  // An id that is no longer in the list still shows, so saving does not silently drop it.
  if (value && !list.some((a) => a.id === value)) items.push({ value, label: value });
  return (
    <Select items={items} value={value || NONE} onValueChange={(v) => onChange(v === NONE ? "" : ((v as string) ?? ""))}>
      <SelectTrigger id={id} className="w-full">
        <SelectValue placeholder={agents.isLoading ? "불러오는 중…" : "에이전트 선택"} />
      </SelectTrigger>
      <SelectContent>
        {items.map((i) => (
          <SelectItem key={i.value} value={i.value}>
            {i.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
