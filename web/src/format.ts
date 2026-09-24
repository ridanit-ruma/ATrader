export type Num = number | string | null | undefined;
export type ColorScheme = "red-up" | "green-up";

const toNum = (x: Num): number | null => (x === null || x === undefined || x === "" ? null : Number(x));

const krw = new Intl.NumberFormat("ko-KR", { maximumFractionDigits: 0 });
const plain = new Intl.NumberFormat("ko-KR", { maximumFractionDigits: 8 });

export function fmtKrw(x: Num): string {
  const n = toNum(x);
  return n === null ? "—" : krw.format(Math.round(n));
}

export function fmtNum(x: Num): string {
  const n = toNum(x);
  return n === null ? "—" : plain.format(n);
}

export function fmtPct(x: Num): string {
  const n = toNum(x);
  return n === null ? "—" : `${n > 0 ? "+" : ""}${n.toFixed(2)}%`;
}

export function fmtTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  return new Date(iso).toLocaleString("ko-KR", { timeZone: "Asia/Seoul", hour12: false });
}

/** Text colour class for a signed figure. */
export function tone(x: Num, scheme: ColorScheme): string {
  const n = toNum(x);
  if (!n) return "";
  if (scheme === "green-up") return n > 0 ? "text-up-green" : "text-down-red";
  return n > 0 ? "text-up-red" : "text-down-blue";
}

/** Chart colours for up and down moves. */
export function chartColors(scheme: ColorScheme): { up: string; down: string } {
  return scheme === "green-up" ? { up: "#16a34a", down: "#dc2626" } : { up: "#e11d48", down: "#2563eb" };
}
