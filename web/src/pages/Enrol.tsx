import { useEffect, useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import QRCode from "qrcode";
import { api, ApiError } from "../api";
import { ErrorText } from "../components/ui";

export function Enrol() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [setup, setSetup] = useState<{ secret: string; qr: string } | null>(null);
  const [code, setCode] = useState("");
  const [codes, setCodes] = useState<string[] | null>(null);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    let live = true;
    api
      .totpSetup()
      .then(async (s) => live && setSetup({ secret: s.secret, qr: await QRCode.toDataURL(s.otpauth_url) }))
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) navigate("/login", { replace: true });
        else if (e instanceof ApiError && e.code === "already_enrolled") navigate("/", { replace: true });
        else setError(e);
      });
    return () => {
      live = false;
    };
  }, [navigate]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      setCodes((await api.totpEnable(code.trim())).recovery_codes);
    } catch (err) {
      setError(err);
    }
  };

  if (codes) {
    return (
      <main className="mx-auto mt-16 max-w-md space-y-4 px-4">
        <h1 className="text-xl font-bold">복구 코드</h1>
        <p className="text-sm">인증 앱을 잃어버렸을 때 로그인에 쓰는 일회용 코드입니다. 지금 한 번만 보여 드리니 안전한 곳에 적어 두세요.</p>
        <pre className="card grid grid-cols-2 gap-2 font-mono">{codes.map((c) => <span key={c}>{c}</span>)}</pre>
        <button
          className="btn w-full"
          onClick={() => {
            qc.clear();
            navigate("/", { replace: true });
          }}
        >
          저장했습니다
        </button>
      </main>
    );
  }

  return (
    <main className="mx-auto mt-16 max-w-md space-y-4 px-4">
      <h1 className="text-xl font-bold">2단계 인증 등록</h1>
      <p className="text-sm">인증 앱(Google Authenticator, 1Password 등)으로 QR 코드를 스캔한 뒤 앱에 표시된 6자리 코드를 입력하세요.</p>
      {setup && (
        <div className="card flex flex-col items-center gap-2">
          <img src={setup.qr} alt="TOTP QR 코드" className="h-48 w-48" />
          <code className="text-xs break-all">{setup.secret}</code>
        </div>
      )}
      <form onSubmit={submit} className="flex gap-2">
        <input className="input" inputMode="numeric" autoComplete="one-time-code" placeholder="123456" value={code} onChange={(e) => setCode(e.target.value)} required />
        <button className="btn" disabled={!setup}>
          등록
        </button>
      </form>
      <ErrorText error={error} />
    </main>
  );
}
