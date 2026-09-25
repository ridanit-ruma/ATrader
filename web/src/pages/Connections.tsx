import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, ExternalLink } from "lucide-react";
import { api, waitForRestart } from "@/api";
import type { KeySetting } from "@/types";
import { ErrorText, Muted, Section } from "@/components/common";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

function Restarting({ show }: { show: boolean }) {
  return show ? <Muted>저장했습니다. 서버를 다시 시작하는 중입니다…</Muted> : null;
}

/** Connect to Attacca by approving a device code there; the server restarts once approved. */
export function AttaccaConnection() {
  const qc = useQueryClient();
  const [restarting, setRestarting] = useState(false);
  const [copied, setCopied] = useState(false);
  const status = useQuery({
    queryKey: ["zyris"],
    queryFn: api.zyris,
    refetchInterval: (q) => (q.state.data?.enrollment.status === "pending" ? 2000 : false),
  });
  const start = useMutation({ mutationFn: api.zyrisEnroll, onSuccess: () => qc.invalidateQueries({ queryKey: ["zyris"] }) });
  const s = status.data;
  const e = s?.enrollment;

  const granted = e?.status === "granted";
  useEffect(() => {
    if (!granted) return;
    setRestarting(true);
    waitForRestart().then(() => {
      setRestarting(false);
      qc.invalidateQueries();
    });
  }, [granted, qc]);

  const copy = async (text: string) => {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  const fixed = s?.source === "env" || s?.source === "file";
  return (
    <Section
      title="Attacca 연결"
      right={<Badge variant={s?.connected ? "default" : "outline"}>{s?.connected ? "연결됨" : s?.enrolled ? "등록됨 · 연결 안 됨" : "연결 안 됨"}</Badge>}
    >
      <div className="grid gap-4">
        {e?.status === "pending" && e.user_code ? (
          <div className="grid gap-3">
            <Muted>아래 코드를 복사해 Attacca에 입력하고 승인하세요. 승인되면 자동으로 연결됩니다.</Muted>
            <div className="flex flex-wrap items-center gap-2">
              <code className="rounded-md bg-muted px-4 py-2 font-mono text-2xl font-semibold tracking-widest">{e.user_code}</code>
              <Button variant="outline" size="sm" onClick={() => copy(e.user_code!)}>
                <Copy /> {copied ? "복사됨" : "복사"}
              </Button>
              {e.verification_uri && (
                <a href={e.verification_uri} target="_blank" rel="noreferrer" className="inline-flex">
                  <Button variant="outline" size="sm">
                    <ExternalLink /> Attacca 열기
                  </Button>
                </a>
              )}
            </div>
          </div>
        ) : (
          <>
            {e?.status === "expired" && <Muted>코드가 만료되었습니다. 다시 받아 주세요.</Muted>}
            {e?.status === "denied" && <Muted>Attacca에서 거절되었습니다.</Muted>}
            {e?.status === "error" && <ErrorText error={e.message ?? "등록에 실패했습니다."} />}
            {fixed ? (
              <Muted>서버 설정 파일에서 지정된 credential을 사용 중입니다. 바꾸려면 서버에서 변경하세요.</Muted>
            ) : (
              <Button className="w-fit" disabled={start.isPending || restarting} onClick={() => start.mutate()}>
                {s?.enrolled ? "다시 연결하기 (새 코드 받기)" : "연결 코드 받기"}
              </Button>
            )}
          </>
        )}
        <ErrorText error={start.error} />
        <Restarting show={restarting} />
      </div>
    </Section>
  );
}

const LABELS: Record<string, string> = {
  KIS_APP_KEY: "KIS app key",
  KIS_APP_SECRET: "KIS app secret",
  KIS_ENV: "KIS 환경",
  DART_API_KEY: "OpenDART API key",
  EDGAR_USER_AGENT: "SEC EDGAR User-Agent (이름과 이메일)",
};

const HINTS: Record<string, string> = {
  KIS_APP_KEY: "한국·미국 주식 시세. 한국투자증권 KIS Developers에서 발급",
  DART_API_KEY: "한국 기업 공시·재무. opendart.fss.or.kr에서 무료 발급",
  EDGAR_USER_AGENT: "미국 기업 공시·재무. 예: ATrader you@example.com",
};

const KIS_ENVS = [
  { value: "real", label: "실전" },
  { value: "mock", label: "모의투자" },
];

function sourceBadge(k: KeySetting) {
  if (!k.configured) return <Badge variant="outline">미설정</Badge>;
  return <Badge variant="secondary">{k.source === "dashboard" ? "설정됨" : "서버 설정"}</Badge>;
}

/** Data provider keys. Secrets are write-only: the field stays empty and a blank field keeps the value. */
export function DataKeys() {
  const qc = useQueryClient();
  const keys = useQuery({ queryKey: ["keys"], queryFn: api.keys });
  const [values, setValues] = useState<Record<string, string>>({});
  const [restarting, setRestarting] = useState(false);
  const save = useMutation({
    mutationFn: (v: Record<string, string>) => api.saveKeys(v),
    onSuccess: async () => {
      setValues({});
      setRestarting(true);
      await waitForRestart();
      setRestarting(false);
      qc.invalidateQueries();
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const changed = Object.fromEntries(Object.entries(values).filter(([, v]) => v.trim() !== ""));
    if (Object.keys(changed).length) save.mutate(changed);
  };

  return (
    <Section title="데이터 API 키">
      <form onSubmit={submit} className="grid gap-4">
        <Muted>키는 서버에만 저장되고 다시 표시되지 않습니다. 비워 둔 칸은 기존 값을 유지합니다. 저장하면 서버가 다시 시작됩니다.</Muted>
        {keys.data?.map((k) => {
          const fixed = k.source === "env" || k.source === "file";
          return (
            <div key={k.name} className="grid gap-2">
              <div className="flex items-center gap-2">
                <Label htmlFor={k.name}>{LABELS[k.name] ?? k.name}</Label>
                {sourceBadge(k)}
              </div>
              {k.name === "KIS_ENV" ? (
                <Select
                  items={KIS_ENVS}
                  value={values.KIS_ENV ?? k.value ?? "real"}
                  onValueChange={(v) => setValues({ ...values, KIS_ENV: v as string })}
                  disabled={fixed}
                >
                  <SelectTrigger className="w-40">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {KIS_ENVS.map((o) => (
                      <SelectItem key={o.value} value={o.value}>
                        {o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              ) : (
                <Input
                  id={k.name}
                  type={k.secret ? "password" : "text"}
                  autoComplete="off"
                  disabled={fixed}
                  placeholder={fixed ? "서버 설정에서 지정됨" : k.secret && k.configured ? "변경하려면 새 값을 입력" : (k.value ?? "")}
                  value={values[k.name] ?? ""}
                  onChange={(e) => setValues({ ...values, [k.name]: e.target.value })}
                />
              )}
              {HINTS[k.name] && <p className="text-xs text-muted-foreground">{HINTS[k.name]}</p>}
            </div>
          );
        })}
        <ErrorText error={save.error} />
        <Restarting show={restarting} />
        <Button type="submit" className="w-fit" disabled={save.isPending || restarting}>
          저장
        </Button>
      </form>
    </Section>
  );
}
