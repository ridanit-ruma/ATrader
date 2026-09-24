import { useEffect } from "react";
import { NavLink, Outlet, useNavigate } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError } from "../api";
import { useLiveStream } from "../stream";

const link = ({ isActive }: { isActive: boolean }) =>
  `rounded px-3 py-1 ${isActive ? "bg-zinc-200 dark:bg-zinc-800" : "hover:bg-zinc-100 dark:hover:bg-zinc-900"}`;

export function Layout() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const me = useQuery({ queryKey: ["me"], queryFn: api.me, retry: false });
  const health = useQuery({ queryKey: ["health"], queryFn: api.health, enabled: me.isSuccess && !me.data.mfa_pending });
  useLiveStream(me.isSuccess && !me.data.mfa_pending);

  useEffect(() => {
    if (me.error instanceof ApiError && me.error.status === 401) navigate("/login", { replace: true });
    if (me.data?.mfa_pending) navigate("/enrol", { replace: true });
  }, [me.error, me.data, navigate]);

  if (!me.data || me.data.mfa_pending) return null;

  const logout = async () => {
    await api.logout().catch(() => {});
    qc.clear();
    navigate("/login", { replace: true });
  };

  return (
    <div className="mx-auto max-w-6xl px-4 pb-12">
      <header className="flex flex-wrap items-center gap-2 py-4">
        <span className="mr-4 font-bold">ATrader</span>
        <NavLink to="/" end className={link}>
          개요
        </NavLink>
        <NavLink to="/alerts" className={link}>
          알림
        </NavLink>
        <NavLink to="/settings" className={link}>
          설정
        </NavLink>
        <span className="ml-auto flex items-center gap-3 text-sm text-zinc-500">
          <span title="Attacca 연결">{health.data?.zyris_connected ? "● 에이전트 연결됨" : "○ 에이전트 미연결"}</span>
          <button className="btn-quiet" onClick={logout}>
            로그아웃
          </button>
        </span>
      </header>
      <Outlet />
    </div>
  );
}
