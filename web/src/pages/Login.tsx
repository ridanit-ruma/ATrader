import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "@/api";
import { ErrorText } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export function Login() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [form, setForm] = useState({ username: "", password: "", code: "" });
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const r = await api.login(form.username, form.password, form.code.trim());
      qc.clear();
      navigate(r.mfa_pending ? "/enrol" : "/", { replace: true });
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const field = (k: keyof typeof form, label: string, props: React.ComponentProps<"input">) => (
    <div className="grid gap-2">
      <Label htmlFor={k}>{label}</Label>
      <Input id={k} value={form[k]} onChange={(e) => setForm({ ...form, [k]: e.target.value })} {...props} />
    </div>
  );

  return (
    <main className="mx-auto mt-24 max-w-sm px-4">
      <Card>
        <CardHeader>
          <CardTitle className="text-xl">ATrader</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="grid gap-4">
            {field("username", "사용자 이름", { autoComplete: "username", required: true })}
            {field("password", "비밀번호", { type: "password", autoComplete: "current-password", required: true })}
            {field("code", "인증 코드 또는 복구 코드", { autoComplete: "one-time-code", inputMode: "numeric" })}
            <ErrorText error={error} />
            <Button type="submit" disabled={busy}>
              로그인
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
