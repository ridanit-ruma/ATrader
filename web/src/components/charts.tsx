import { useEffect, useRef } from "react";
import {
  AreaSeries,
  CandlestickSeries,
  ColorType,
  HistogramSeries,
  createChart,
  createSeriesMarkers,
  type IChartApi,
  type Time,
  type UTCTimestamp,
} from "lightweight-charts";
import { useColorScheme } from "../colors";
import { chartColors } from "../format";
import type { CandleView, Chart, EquityPoint, Pnl } from "../types";

const ts = (iso: string) => Math.floor(new Date(iso).getTime() / 1000) as UTCTimestamp;

function dark() {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Mount a chart into a div, rebuild it when `deps` change. */
function useChart(build: (chart: IChartApi) => void, deps: unknown[]) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!ref.current) return;
    const d = dark();
    const chart = createChart(ref.current, {
      autoSize: true,
      layout: { background: { type: ColorType.Solid, color: "transparent" }, textColor: d ? "#a1a1aa" : "#52525b" },
      grid: { vertLines: { color: d ? "#27272a" : "#f4f4f5" }, horzLines: { color: d ? "#27272a" : "#f4f4f5" } },
      timeScale: { timeVisible: true },
      localization: { locale: "ko-KR" },
    });
    build(chart);
    chart.timeScale().fitContent();
    return () => chart.remove();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return ref;
}

export function EquityChart({ points }: { points: EquityPoint[] }) {
  const ref = useChart(
    (chart) => {
      const s = chart.addSeries(AreaSeries, { lineColor: "#6366f1", topColor: "#6366f155", bottomColor: "#6366f105", lineWidth: 2 });
      s.setData(dedupe(points.map((p) => ({ time: ts(p.at), value: Number(p.equity_krw) }))));
    },
    [points],
  );
  return <div ref={ref} className="h-64 w-full" />;
}

export function PnlBars({ daily }: { daily: Pnl["daily"] }) {
  const scheme = useColorScheme();
  const ref = useChart(
    (chart) => {
      const { up, down } = chartColors(scheme);
      const s = chart.addSeries(HistogramSeries, {});
      s.setData(daily.map((d) => ({ time: d.date as Time, value: Number(d.pnl_krw), color: Number(d.pnl_krw) >= 0 ? up : down })));
    },
    [daily, scheme],
  );
  return <div ref={ref} className="h-40 w-full" />;
}

export function CandleChart({ candles, markers }: { candles: CandleView[]; markers: Chart["markers"] }) {
  const scheme = useColorScheme();
  const ref = useChart(
    (chart) => {
      const { up, down } = chartColors(scheme);
      const s = chart.addSeries(CandlestickSeries, { upColor: up, downColor: down, borderVisible: false, wickUpColor: up, wickDownColor: down });
      s.setData(
        dedupe(candles.map((c) => ({ time: ts(c.start), open: Number(c.open), high: Number(c.high), low: Number(c.low), close: Number(c.close) }))),
      );
      // Markers snap to the candle that contains the fill.
      const starts = candles.map((c) => ts(c.start));
      const snap = (t: UTCTimestamp) => {
        let best = starts[0];
        for (const st of starts) if (st <= t) best = st;
        return best;
      };
      if (starts.length) {
        createSeriesMarkers(
          s,
          markers
            .map((m) => ({ ...m, t: snap(ts(m.at)) }))
            .sort((a, b) => a.t - b.t)
            .map((m) =>
              m.side === "buy"
                ? { time: m.t, position: "belowBar" as const, color: up, shape: "arrowUp" as const, text: "매수" }
                : { time: m.t, position: "aboveBar" as const, color: down, shape: "arrowDown" as const, text: "매도" },
            ),
        );
      }
    },
    [candles, markers, scheme],
  );
  return <div ref={ref} className="h-96 w-full" />;
}

/** lightweight-charts wants strictly increasing times. */
function dedupe<P extends { time: UTCTimestamp }>(points: P[]): P[] {
  const sorted = [...points].sort((a, b) => a.time - b.time);
  return sorted.filter((p, i) => i === sorted.length - 1 || sorted[i + 1].time !== p.time);
}
