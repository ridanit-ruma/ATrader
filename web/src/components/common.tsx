import type { ReactNode } from "react";
import { useColorScheme } from "@/colors";
import { tone, type Num } from "@/format";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { TrendingDown, TrendingUp } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";

/** A signed figure coloured by the user's up/down convention. */
export function Signed({ value, children }: { value: Num; children: ReactNode }) {
  const scheme = useColorScheme();
  return <span className={tone(value, scheme)}>{children}</span>;
}

export function Stat({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="text-lg font-semibold tabular-nums">{children}</div>
    </div>
  );
}

export function Section({ title, children, right }: { title: string; children: ReactNode; right?: ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        {right && <CardAction>{right}</CardAction>}
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

export function DataTable({ head, children, empty }: { head: ReactNode[]; children: ReactNode; empty?: boolean }) {
  return (
    <>
      <Table>
        <TableHeader>
          <TableRow>
            {head.map((h, i) => (
              <TableHead key={i} className={i === 0 ? "" : "text-right"}>
                {h}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>{children}</TableBody>
      </Table>
      {empty && <p className="py-4 text-center text-sm text-muted-foreground">없음</p>}
    </>
  );
}

/** A row of mutually exclusive choices (ranges, intervals). */
export function Choice<T extends string>({ value, options, onChange }: { value: T; options: readonly (readonly [T, string])[]; onChange: (v: T) => void }) {
  return (
    <Tabs value={value} onValueChange={(v) => onChange(v as T)}>
      <TabsList>
        {options.map(([k, label]) => (
          <TabsTrigger key={k} value={k}>
            {label}
          </TabsTrigger>
        ))}
      </TabsList>
    </Tabs>
  );
}

export function ErrorText({ error }: { error: unknown }) {
  if (!error) return null;
  return (
    <Alert variant="destructive">
      <AlertDescription>{error instanceof Error ? error.message : String(error)}</AlertDescription>
    </Alert>
  );
}

export function Muted({ children }: { children: ReactNode }) {
  return <p className="text-sm text-muted-foreground">{children}</p>;
}

export function PageHeader({ title, description, children }: { title: string; description?: ReactNode; children?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-2">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {children}
    </div>
  );
}

/** One headline number, shadcn "section card" style: label, value, optional trend badge and footnote. */
export function Metric({ label, value, trend, trendLabel, footer }: { label: string; value: ReactNode; trend?: Num; trendLabel?: string; footer?: ReactNode }) {
  const scheme = useColorScheme();
  const n = trend === null || trend === undefined ? null : Number(trend);
  return (
    <Card className="@container/card">
      <CardHeader>
        <CardDescription>{label}</CardDescription>
        <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">{value}</CardTitle>
        {n !== null && trendLabel && (
          <CardAction>
            <Badge variant="outline" className={tone(n, scheme)}>
              {n >= 0 ? <TrendingUp /> : <TrendingDown />}
              {trendLabel}
            </Badge>
          </CardAction>
        )}
      </CardHeader>
      {footer && <CardFooter className="text-sm text-muted-foreground">{footer}</CardFooter>}
    </Card>
  );
}
