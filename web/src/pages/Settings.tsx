import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Cash } from "../api";
import { setColorScheme, useColorScheme } from "../colors";
import { fmtTime } from "../format";
import { ErrorText, Section, Table } from "../components/ui";

const CURRENCIES = ["KRW", "USD", "USDT"] as const;

function CashInputs({ cash, onChange }: { cash: Cash; onChange: (c: Cash) => void }) {
  return (
    <div className="grid grid-cols-3 gap-2">
      {CURRENCIES.map((c) => (
        <label key={c} className="text-xs text-zinc-500">
          {c}
          <input className="input" inputMode="decimal" value={cash[c] ?? ""} onChange={(e) => onChange({ ...cash, [c]: e.target.value })} />
        </label>
      ))}
    </div>
  );
}

const nonEmpty = (cash: Cash): Cash => Object.fromEntries(Object.entries(cash).filter(([, v]) => v.trim() !== "").map(([k, v]) => [k, v.trim()]));

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
      <form onSubmit={submit} className="space-y-2">
        <div className="grid gap-2 md:grid-cols-3">
          <input className="input" placeholder="id (a-z, 0-9, _, -)" value={f.id} onChange={(e) => setF({ ...f, id: e.target.value })} required />
          <input className="input" placeholder="이름" value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} required />
          <input className="input" placeholder="Attacca 에이전트 id (선택)" value={f.agent} onChange={(e) => setF({ ...f, agent: e.target.value })} />
        </div>
        <CashInputs cash={cash} onChange={setCash} />
        <ErrorText error={m.error} />
        {m.isSuccess && <p className="text-sm text-green-600">만들었습니다.</p>}
        <button className="btn" disabled={m.isPending}>
          만들기
        </button>
      </form>
    </Section>
  );
}

function ResetAccount() {
  const qc = useQueryClient();
  const overview = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  const [id, setId] = useState("");
  const [confirm, setConfirm] = useState("");
  const [cash, setCash] = useState<Cash>({ KRW: "10000000" });
  const m = useMutation({
    mutationFn: () => api.resetAccount(id, nonEmpty(cash)),
    onSuccess: () => {
      qc.invalidateQueries();
      setConfirm("");
    },
  });
  return (
    <Section title="계좌 초기화">
      <p className="mb-2 text-sm text-zinc-500">보유 종목과 미체결 주문을 모두 지우고 입력한 현금으로 다시 시작합니다. 이전 기록은 남습니다.</p>
      <div className="space-y-2">
        <select className="input" value={id} onChange={(e) => setId(e.target.value)}>
          <option value="">계좌 선택</option>
          {overview.data?.map((a) => (
            <option key={a.id} value={a.id}>
              {a.summary.name} ({a.id})
            </option>
          ))}
        </select>
        <CashInputs cash={cash} onChange={setCash} />
        <input className="input" placeholder="확인을 위해 계좌 id를 입력하세요" value={confirm} onChange={(e) => setConfirm(e.target.value)} />
        <ErrorText error={m.error} />
        {m.isSuccess && <p className="text-sm text-green-600">초기화했습니다.</p>}
        <button className="btn" disabled={!id || confirm !== id || m.isPending} onClick={() => m.mutate()}>
          초기화
        </button>
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
      <form onSubmit={submit} className="space-y-2">
        <input className="input" type="password" autoComplete="current-password" placeholder="현재 비밀번호" value={f.current} onChange={(e) => setF({ ...f, current: e.target.value })} required />
        <input className="input" type="password" autoComplete="new-password" placeholder="새 비밀번호 (12자 이상)" minLength={12} value={f.next} onChange={(e) => setF({ ...f, next: e.target.value })} required />
        <input className="input" type="password" autoComplete="new-password" placeholder="새 비밀번호 확인" value={f.again} onChange={(e) => setF({ ...f, again: e.target.value })} required />
        {f.again && f.next !== f.again && <p className="text-sm text-red-600">새 비밀번호가 서로 다릅니다.</p>}
        <ErrorText error={m.error} />
        {m.isSuccess && <p className="text-sm text-green-600">바꿨습니다. 다른 기기의 세션은 모두 로그아웃되었습니다.</p>}
        <button className="btn" disabled={m.isPending}>
          변경
        </button>
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
      <Table head={["기기", "IP", "로그인", "마지막 사용", ""]} empty={!q.data?.length}>
        {q.data?.map((s) => (
          <tr key={s.id}>
            <td className="max-w-xs truncate px-2 py-1 text-xs" title={s.user_agent}>
              {s.user_agent || "—"}
            </td>
            <td className="num px-2">{s.ip}</td>
            <td className="num px-2 text-xs">{fmtTime(s.created_at)}</td>
            <td className="num px-2 text-xs">{fmtTime(s.last_seen)}</td>
            <td className="num px-2">
              {s.current ? (
                <span className="text-xs text-zinc-500">현재 세션</span>
              ) : (
                <button className="btn-quiet text-xs" onClick={() => revoke.mutate(s.id)}>
                  종료
                </button>
              )}
            </td>
          </tr>
        ))}
      </Table>
    </Section>
  );
}

function Status() {
  const health = useQuery({ queryKey: ["health"], queryFn: api.health });
  const scheme = useColorScheme();
  return (
    <Section title="상태와 표시">
      <div className="space-y-3 text-sm">
        <p>Attacca 에이전트 연결: {health.data?.zyris_connected ? "연결됨" : "연결 안 됨"}</p>
        <div className="flex flex-wrap gap-3">
          {health.data?.feeds.map((f) => (
            <span key={f.venue} className="btn-quiet">
              {f.venue} · 구독 {f.subscribed}종목
            </span>
          ))}
        </div>
        <label className="flex items-center gap-2">
          색상
          <select className="input w-auto" value={scheme} onChange={(e) => setColorScheme(e.target.value as typeof scheme)}>
            <option value="red-up">상승 빨강 · 하락 파랑</option>
            <option value="green-up">상승 초록 · 하락 빨강</option>
          </select>
        </label>
        <p className="text-zinc-500">2단계 인증을 초기화하려면 서버에서 <code>atrader user reset-2fa &lt;이름&gt;</code>을 실행하세요.</p>
      </div>
    </Section>
  );
}

function Audit() {
  const q = useQuery({ queryKey: ["audit"], queryFn: api.audit });
  return (
    <Section title="감사 기록">
      <Table head={["시각", "동작", "내용", "IP"]} empty={!q.data?.length}>
        {q.data?.map((r, i) => (
          <tr key={i}>
            <td className="px-2 py-1 text-xs whitespace-nowrap">{fmtTime(r.at)}</td>
            <td className="num px-2">{r.action}</td>
            <td className="num px-2">{r.detail}</td>
            <td className="num px-2">{r.ip}</td>
          </tr>
        ))}
      </Table>
    </Section>
  );
}

export function Settings() {
  return (
    <div className="space-y-4">
      <CreateAccount />
      <ResetAccount />
      <Status />
      <Password />
      <Sessions />
      <Audit />
    </div>
  );
}
