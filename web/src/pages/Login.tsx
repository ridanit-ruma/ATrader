import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "../api";
import { ErrorText } from "../components/ui";

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

  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [k]: e.target.value });

  return (
    <main className="mx-auto mt-24 max-w-sm px-4">
      <h1 className="mb-6 text-2xl font-bold">ATrader</h1>
      <form onSubmit={submit} className="card space-y-3">
        <input className="input" placeholder="사용자 이름" autoComplete="username" value={form.username} onChange={set("username")} required />
        <input className="input" type="password" placeholder="비밀번호" autoComplete="current-password" value={form.password} onChange={set("password")} required />
        <input className="input" placeholder="인증 코드 또는 복구 코드" autoComplete="one-time-code" value={form.code} onChange={set("code")} />
        <ErrorText error={error} />
        <button className="btn w-full" disabled={busy}>
          로그인
        </button>
      </form>
    </main>
  );
}
