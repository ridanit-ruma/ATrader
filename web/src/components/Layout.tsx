import { useEffect } from "react";
import { NavLink, Outlet, useNavigate } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError } from "@/api";
import { useLiveStream } from "@/stream";
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";

const link = ({ isActive }: { isActive: boolean }) => buttonVariants({ variant: isActive ? "secondary" : "ghost", size: "sm" });

export function Layout() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const me = useQuery({ queryKey: ["me"], queryFn: api.me, retry: false });
  const ready = me.isSuccess && !me.data.mfa_pending;
  const health = useQuery({ queryKey: ["health"], queryFn: api.health, enabled: ready });
  useLiveStream(ready);

  useEffect(() => {
    if (me.error instanceof ApiError && me.error.status === 401) navigate("/login", { replace: true });
    if (me.data?.mfa_pending) navigate("/enrol", { replace: true });
  }, [me.error, me.data, navigate]);

  if (!ready) return null;

  const logout = async () => {
    await api.logout().catch(() => {});
    qc.clear();
    navigate("/login", { replace: true });
  };

  return (
    <div className="mx-auto max-w-6xl px-4 pb-12">
      <header className="sticky top-0 z-10 -mx-4 mb-6 flex h-14 items-center gap-1 border-b bg-background/95 px-4 backdrop-blur">
        <span className="mr-4 font-heading font-semibold">ATrader</span>
        <NavLink to="/" end className={link}>
          개요
        </NavLink>
        <NavLink to="/alerts" className={link}>
          알림
        </NavLink>
        <NavLink to="/settings" className={link}>
          설정
        </NavLink>
        <span className="ml-auto flex items-center gap-2">
          <Badge variant={health.data?.zyris_connected ? "default" : "outline"}>{health.data?.zyris_connected ? "에이전트 연결됨" : "에이전트 미연결"}</Badge>
          <Button variant="outline" size="sm" onClick={logout}>
            로그아웃
          </Button>
        </span>
      </header>
      <Outlet />
    </div>
  );
}
