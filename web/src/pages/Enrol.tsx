import { useEffect, useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import QRCode from "qrcode";
import { api, ApiError } from "@/api";
import { ErrorText } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";

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

  return (
    <main className="mx-auto mt-16 max-w-md px-4">
      {codes ? (
        <Card>
          <CardHeader>
            <CardTitle>복구 코드</CardTitle>
            <CardDescription>인증 앱을 잃어버렸을 때 로그인에 쓰는 일회용 코드입니다. 지금 한 번만 보여 드리니 안전한 곳에 적어 두세요.</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-2 gap-2 rounded-md bg-muted p-4 font-mono text-sm">
              {codes.map((c) => (
                <span key={c}>{c}</span>
              ))}
            </div>
          </CardContent>
          <CardFooter>
            <Button
              className="w-full"
              onClick={() => {
                qc.clear();
                navigate("/", { replace: true });
              }}
            >
              저장했습니다
            </Button>
          </CardFooter>
        </Card>
      ) : (
        <Card>
          <CardHeader>
            <CardTitle>2단계 인증 등록</CardTitle>
            <CardDescription>인증 앱(Google Authenticator, 1Password 등)으로 QR 코드를 스캔한 뒤 앱에 표시된 6자리 코드를 입력하세요.</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            {setup && (
              <div className="flex flex-col items-center gap-2">
                <img src={setup.qr} alt="TOTP QR 코드" className="size-48 rounded-md bg-white p-2" />
                <code className="text-xs break-all text-muted-foreground">{setup.secret}</code>
              </div>
            )}
            <form onSubmit={submit} className="flex gap-2">
              <Input inputMode="numeric" autoComplete="one-time-code" placeholder="123456" value={code} onChange={(e) => setCode(e.target.value)} required />
              <Button type="submit" disabled={!setup}>
                등록
              </Button>
            </form>
            <ErrorText error={error} />
          </CardContent>
        </Card>
      )}
    </main>
  );
}
