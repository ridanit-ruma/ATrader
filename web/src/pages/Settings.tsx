import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Cash } from "@/api";
import { setColorScheme, useColorScheme } from "@/colors";
import type { ColorScheme } from "@/format";
import { fmtTime } from "@/format";
import { DataTable, ErrorText, Muted, Section, PageHeader } from "@/components/common";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { TableCell, TableRow } from "@/components/ui/table";

const CURRENCIES = ["KRW", "USD", "USDT"] as const;

function Field({ id, label, ...props }: { id: string; label: string } & React.ComponentProps<typeof Input>) {
  return (
    <div className="grid gap-2">
      <Label htmlFor={id}>{label}</Label>
      <Input id={id} {...props} />
    </div>
  );
}

function CashInputs({ prefix, cash, onChange }: { prefix: string; cash: Cash; onChange: (c: Cash) => void }) {
  return (
    <div className="grid grid-cols-3 gap-2">
      {CURRENCIES.map((c) => (
        <Field key={c} id={`${prefix}-${c}`} label={`${c} 현금`} inputMode="decimal" value={cash[c] ?? ""} onChange={(e) => onChange({ ...cash, [c]: e.target.value })} />
      ))}
    </div>
  );
}

const nonEmpty = (cash: Cash): Cash => Object.fromEntries(Object.entries(cash).filter(([, v]) => v.trim() !== "").map(([k, v]) => [k, v.trim()]));

function Done({ show, children }: { show: boolean; children: string }) {
  return show ? <p className="text-sm text-green-600 dark:text-green-400">{children}</p> : null;
}

function CreateAccount() {
  const qc = useQueryClient();
  const [f, setF] = useState({ id: "", name: "", agent: "" });
  const [cash, setCash] = useState<Cash>({ KRW: "10000000" });
  const m = useMutation({
    mutationFn: () => api.createAccount(f.id.trim(), f.name.trim(), f.agent.trim(), nonEmpty(cash)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["overview"] });
      setF({ id: "", name: "", agent: "" });
    },
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    m.mutate();
  };
  return (
    <Section title="계좌 만들기">
      <form onSubmit={submit} className="grid gap-4">
        <div className="grid gap-2 md:grid-cols-3">
          <Field id="new-id" label="id (a-z, 0-9, _, -)" value={f.id} onChange={(e) => setF({ ...f, id: e.target.value })} required />
          <Field id="new-name" label="이름" value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} required />
          <Field id="new-agent" label="Attacca 에이전트 id (선택)" value={f.agent} onChange={(e) => setF({ ...f, agent: e.target.value })} />
        </div>
        <CashInputs prefix="new" cash={cash} onChange={setCash} />
        <ErrorText error={m.error} />
        <Done show={m.isSuccess}>만들었습니다.</Done>
        <Button type="submit" className="w-fit" disabled={m.isPending}>
          만들기
        </Button>
      </form>
    </Section>
  );
}

function ResetAccount() {
  const qc = useQueryClient();
  const overview = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  const [id, setId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState("");
  const [cash, setCash] = useState<Cash>({ KRW: "10000000" });
  const m = useMutation({
    mutationFn: () => api.resetAccount(id!, nonEmpty(cash)),
    onSuccess: () => {
      qc.invalidateQueries();
      setConfirm("");
    },
  });
  const items = (overview.data ?? []).map((a) => ({ value: a.id, label: `${a.summary.name} (${a.id})` }));
  return (
    <Section title="계좌 초기화">
      <div className="grid gap-4">
        <Muted>보유 종목과 미체결 주문을 모두 지우고 입력한 현금으로 다시 시작합니다. 이전 기록은 남습니다.</Muted>
        <Select items={items} value={id} onValueChange={(v) => setId(v as string | null)}>
          <SelectTrigger className="w-64">
            <SelectValue placeholder="계좌 선택" />
          </SelectTrigger>
          <SelectContent>
            {items.map((i) => (
              <SelectItem key={i.value} value={i.value}>
                {i.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <CashInputs prefix="reset" cash={cash} onChange={setCash} />
        <Field id="reset-confirm" label="확인을 위해 계좌 id를 입력하세요" value={confirm} onChange={(e) => setConfirm(e.target.value)} />
        <ErrorText error={m.error} />
        <Done show={m.isSuccess}>초기화했습니다.</Done>
        <Button variant="destructive" className="w-fit" disabled={!id || confirm !== id || m.isPending} onClick={() => m.mutate()}>
          초기화
        </Button>
      </div>
    </Section>
  );
}

function Password() {
  const [f, setF] = useState({ current: "", next: "", again: "" });
  const m = useMutation({ mutationFn: () => api.password(f.current, f.next), onSuccess: () => setF({ current: "", next: "", again: "" }) });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (f.next === f.again) m.mutate();
  };
  return (
    <Section title="비밀번호 변경">
      <form onSubmit={submit} className="grid max-w-sm gap-4">
        <Field id="pw-current" label="현재 비밀번호" type="password" autoComplete="current-password" value={f.current} onChange={(e) => setF({ ...f, current: e.target.value })} required />
        <Field id="pw-new" label="새 비밀번호 (12자 이상)" type="password" autoComplete="new-password" minLength={12} value={f.next} onChange={(e) => setF({ ...f, next: e.target.value })} required />
        <Field id="pw-again" label="새 비밀번호 확인" type="password" autoComplete="new-password" value={f.again} onChange={(e) => setF({ ...f, again: e.target.value })} required />
        {f.again && f.next !== f.again && <ErrorText error="새 비밀번호가 서로 다릅니다." />}
        <ErrorText error={m.error} />
        <Done show={m.isSuccess}>바꿨습니다. 다른 기기의 세션은 모두 로그아웃되었습니다.</Done>
        <Button type="submit" className="w-fit" disabled={m.isPending}>
          변경
        </Button>
      </form>
    </Section>
  );
}

function Sessions() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["sessions"], queryFn: api.sessions });
  const revoke = useMutation({ mutationFn: api.revoke, onSuccess: () => qc.invalidateQueries({ queryKey: ["sessions"] }) });
  return (
    <Section title="로그인 세션">
      <DataTable head={["기기", "IP", "로그인", "마지막 사용", ""]} empty={!q.data?.length}>
        {q.data?.map((s) => (
          <TableRow key={s.id}>
            <TableCell className="max-w-xs truncate text-xs" title={s.user_agent}>
              {s.user_agent || "—"}
            </TableCell>
            <TableCell className="text-right">{s.ip}</TableCell>
            <TableCell className="text-right text-xs">{fmtTime(s.created_at)}</TableCell>
            <TableCell className="text-right text-xs">{fmtTime(s.last_seen)}</TableCell>
            <TableCell className="text-right">
              {s.current ? (
                <Badge variant="secondary">현재 세션</Badge>
              ) : (
                <Button variant="outline" size="sm" onClick={() => revoke.mutate(s.id)}>
                  종료
                </Button>
              )}
            </TableCell>
          </TableRow>
        ))}
      </DataTable>
    </Section>
  );
}

const SCHEMES = [
  { value: "red-up", label: "상승 빨강 · 하락 파랑" },
  { value: "green-up", label: "상승 초록 · 하락 빨강" },
];

function Status() {
  const health = useQuery({ queryKey: ["health"], queryFn: api.health });
  const scheme = useColorScheme();
  return (
    <Section title="상태와 표시">
      <div className="grid gap-4 text-sm">
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant={health.data?.zyris_connected ? "default" : "outline"}>Attacca {health.data?.zyris_connected ? "연결됨" : "연결 안 됨"}</Badge>
          {health.data?.feeds.map((f) => (
            <Badge key={f.venue} variant="secondary">
              {f.venue} · 구독 {f.subscribed}종목
            </Badge>
          ))}
        </div>
        <div className="grid gap-2">
          <Label>색상</Label>
          <Select items={SCHEMES} value={scheme} onValueChange={(v) => v && setColorScheme(v as ColorScheme)}>
            <SelectTrigger className="w-64">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SCHEMES.map((s) => (
                <SelectItem key={s.value} value={s.value}>
                  {s.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <Muted>
          2단계 인증을 초기화하려면 서버에서 <code>atrader user reset-2fa &lt;이름&gt;</code>을 실행하세요.
        </Muted>
      </div>
    </Section>
  );
}

function Audit() {
  const q = useQuery({ queryKey: ["audit"], queryFn: api.audit });
  return (
    <Section title="감사 기록">
      <DataTable head={["시각", "동작", "내용", "IP"]} empty={!q.data?.length}>
        {q.data?.map((r, i) => (
          <TableRow key={i}>
            <TableCell className="text-xs">{fmtTime(r.at)}</TableCell>
            <TableCell className="text-right">{r.action}</TableCell>
            <TableCell className="text-right">{r.detail}</TableCell>
            <TableCell className="text-right">{r.ip}</TableCell>
          </TableRow>
        ))}
      </DataTable>
    </Section>
  );
}

export function Settings() {
  return (
    <div className="grid gap-4">
      <PageHeader title="설정" description="계좌, 보안, 표시 설정" />
      <CreateAccount />
      <ResetAccount />
      <Status />
      <Password />
      <Sessions />
      <Audit />
    </div>
  );
}
