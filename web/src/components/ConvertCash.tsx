import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowRight } from "lucide-react";
import { api } from "@/api";
import { fmtNum } from "@/format";
import type { AccountSummary } from "@/types";
import { ErrorText, Muted, Section } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

const CURRENCIES = ["KRW", "USD", "USDT"].map((c) => ({ value: c, label: c }));
/** Mirrors the server's conversion spread; the preview is only a preview. */
const SPREAD = 0.001;

function CurrencySelect({ id, value, onChange }: { id: string; value: string; onChange: (v: string) => void }) {
  return (
    <Select items={CURRENCIES} value={value} onValueChange={(v) => v && onChange(v as string)}>
      <SelectTrigger id={id} className="w-28">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {CURRENCIES.map((c) => (
          <SelectItem key={c.value} value={c.value}>
            {c.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/** Exchange this account's cash between KRW, USD and USDT at the live rate less the spread. */
export function ConvertCash({ account, summary }: { account: string; summary: AccountSummary }) {
  const qc = useQueryClient();
  const [from, setFrom] = useState("KRW");
  const [to, setTo] = useState("USD");
  const [amount, setAmount] = useState("");
  const rate = Number(summary.usd_krw);
  const krwPer = (c: string) => (c === "KRW" ? 1 : rate);
  const n = Number(amount);
  const preview = from !== to && n > 0 ? ((n * krwPer(from)) / krwPer(to)) * (1 - SPREAD) : null;
  const available = summary.cash.find((c) => c.currency === from)?.available;
  const m = useMutation({
    mutationFn: () => api.convert(account, from, to, amount.trim()),
    onSuccess: () => {
      setAmount("");
      qc.invalidateQueries({ queryKey: ["account", account] });
      qc.invalidateQueries({ queryKey: ["overview"] });
    },
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    m.mutate();
  };
  return (
    <Section title="환전">
      <form onSubmit={submit} className="grid gap-4">
        <div className="flex flex-wrap items-end gap-2">
          <div className="grid gap-2">
            <Label htmlFor="fx-from">보낼 통화</Label>
            <CurrencySelect id="fx-from" value={from} onChange={setFrom} />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="fx-amount">금액</Label>
            <div className="flex gap-1">
              <Input id="fx-amount" className="w-40" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} required />
              <Button type="button" variant="outline" disabled={!available} onClick={() => setAmount(String(available ?? ""))}>
                전액
              </Button>
            </div>
          </div>
          <ArrowRight className="mb-2 text-muted-foreground" />
          <div className="grid gap-2">
            <Label htmlFor="fx-to">받을 통화</Label>
            <CurrencySelect id="fx-to" value={to} onChange={setTo} />
          </div>
          <Button type="submit" disabled={m.isPending || from === to}>
            환전
          </Button>
        </div>
        <Muted>
          USD/KRW {fmtNum(rate)} (USDT는 USD와 같게 봅니다) · 스프레드 0.1%
          {preview !== null && ` · 약 ${fmtNum(Math.floor(preview * 100) / 100)} ${to} 받음`}
          {available !== undefined && ` · 사용 가능 ${fmtNum(available)} ${from}`}
        </Muted>
        <ErrorText error={m.error} />
        {m.data && (
          <p className="text-sm text-green-600 dark:text-green-400">
            {fmtNum(m.data.debit)} {m.data.from} → {fmtNum(m.data.credit)} {m.data.to} 환전했습니다.
          </p>
        )}
      </form>
    </Section>
  );
}
