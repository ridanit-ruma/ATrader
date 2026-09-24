import type { ReactNode } from "react";
import { useColorScheme } from "../colors";
import { tone, type Num } from "../format";

/** A signed figure coloured by the user's up/down convention. */
export function Signed({ value, children }: { value: Num; children: ReactNode }) {
  const scheme = useColorScheme();
  return <span className={tone(value, scheme)}>{children}</span>;
}

export function Stat({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <div className="text-xs text-zinc-500">{label}</div>
      <div className="text-lg font-semibold tabular-nums">{children}</div>
    </div>
  );
}

export function Section({ title, children, right }: { title: string; children: ReactNode; right?: ReactNode }) {
  return (
    <section className="card">
      <div className="mb-3 flex items-center justify-between gap-2">
        <h2 className="font-semibold">{title}</h2>
        {right}
      </div>
      {children}
    </section>
  );
}

export function Table({ head, children, empty }: { head: ReactNode[]; children: ReactNode; empty?: boolean }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead className="text-xs text-zinc-500">
          <tr>
            {head.map((h, i) => (
              <th key={i} className={`px-2 py-1 font-normal ${i === 0 ? "text-left" : "text-right"}`}>
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800">{children}</tbody>
      </table>
      {empty && <p className="py-4 text-center text-sm text-zinc-500">없음</p>}
    </div>
  );
}

export function ErrorText({ error }: { error: unknown }) {
  if (!error) return null;
  return <p className="text-sm text-red-600">{error instanceof Error ? error.message : String(error)}</p>;
}
