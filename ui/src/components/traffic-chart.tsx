import { useEffect, useMemo, useRef, useState } from "react";
import { runtimeTraffic, type TrafficSample } from "../api";

/** 最近 1 小时窗口；2s 采样约 1800 点，按时间戳裁剪。Author: kejiqing */
const WINDOW_MS = 60 * 60 * 1000;
const INTERVAL_MS = 2_000;
const WIDTH = 640;
const HEIGHT = 168;
const PAD_L = 54;
const PAD_R = 14;
const PAD_T = 10;
const PAD_B = 26;

type Point = { t: number; up: number; down: number };

function formatRate(bytesPerSec: number): string {
  if (bytesPerSec < 1024) {
    return `${Math.round(bytesPerSec)} B/s`;
  }
  if (bytesPerSec < 1024 * 1024) {
    const kb = bytesPerSec / 1024;
    return `${kb >= 10 ? kb.toFixed(0) : kb.toFixed(1)} KB/s`;
  }
  const mb = bytesPerSec / (1024 * 1024);
  return `${mb >= 10 ? mb.toFixed(1) : mb.toFixed(2)} MB/s`;
}

/** 纵轴刻度取整到 1/2/5×10ⁿ，避免挤成一排怪数字。Author: kejiqing */
function niceMax(raw: number): number {
  if (raw <= 0) {
    return 1024;
  }
  const exp = Math.floor(Math.log10(raw));
  const base = 10 ** exp;
  const n = raw / base;
  if (n <= 1) {
    return base;
  }
  if (n <= 2) {
    return 2 * base;
  }
  if (n <= 5) {
    return 5 * base;
  }
  return 10 * base;
}

function formatClock(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

function plotX(t: number, windowStart: number): number {
  const innerW = WIDTH - PAD_L - PAD_R;
  const ratio = Math.min(1, Math.max(0, (t - windowStart) / WINDOW_MS));
  return PAD_L + ratio * innerW;
}

function plotY(value: number, max: number): number {
  const innerH = HEIGHT - PAD_T - PAD_B;
  return PAD_T + innerH - (max <= 0 ? 0 : (value / max) * innerH);
}

function buildPath(points: Point[], key: "up" | "down", windowStart: number, max: number): string {
  if (points.length === 0) {
    return "";
  }
  return points
    .map((point, index) => {
      const x = plotX(point.t, windowStart);
      const y = plotY(point[key], max);
      return `${index === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
}

function buildArea(points: Point[], key: "up" | "down", windowStart: number, max: number): string {
  const line = buildPath(points, key, windowStart, max);
  if (!line || points.length === 0) {
    return "";
  }
  const baseY = HEIGHT - PAD_B;
  const endX = plotX(points[points.length - 1].t, windowStart);
  const startX = plotX(points[0].t, windowStart);
  return `${line} L${endX.toFixed(1)},${baseY} L${startX.toFixed(1)},${baseY} Z`;
}

/** 状态页流量图：最近 1 小时 + 横纵轴。Author: kejiqing */
export function TrafficChart(props: { running: boolean }) {
  const { running } = props;
  const [points, setPoints] = useState<Point[]>([]);
  const [latest, setLatest] = useState<TrafficSample>({ up: 0, down: 0, running: false });
  const [now, setNow] = useState(() => Date.now());
  const busy = useRef(false);

  useEffect(() => {
    if (!running) {
      setPoints([]);
      setLatest({ up: 0, down: 0, running: false });
      return;
    }

    let cancelled = false;

    async function tick() {
      if (busy.current || cancelled) {
        return;
      }
      busy.current = true;
      try {
        const sample = await runtimeTraffic();
        if (cancelled) {
          return;
        }
        const t = Date.now();
        setNow(t);
        setLatest(sample);
        setPoints((prev) => {
          const cutoff = t - WINDOW_MS;
          const next = prev.filter((item) => item.t >= cutoff);
          next.push({ t, up: sample.up, down: sample.down });
          return next;
        });
      } catch {
        // 采样失败时保持上一帧，避免图表抖动。
      } finally {
        busy.current = false;
      }
    }

    void tick();
    const id = window.setInterval(() => void tick(), INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [running]);

  const chart = useMemo(() => {
    const windowStart = now - WINDOW_MS;
    const windowEnd = now;
    const visible = points.filter((item) => item.t >= windowStart);
    const peak = Math.max(0, ...visible.map((item) => Math.max(item.up, item.down)));
    const max = niceMax(peak);
    const yTicks = [0, 0.5, 1].map((ratio) => ({
      value: max * ratio,
      y: plotY(max * ratio, max),
      label: formatRate(max * ratio),
    }));
    const xTicks = [0, 0.25, 0.5, 0.75, 1].map((ratio) => {
      const t = windowStart + ratio * WINDOW_MS;
      return {
        t,
        x: plotX(t, windowStart),
        label: formatClock(t),
      };
    });
    return {
      windowStart,
      windowEnd,
      max,
      yTicks,
      xTicks,
      upLine: buildPath(visible, "up", windowStart, max),
      downLine: buildPath(visible, "down", windowStart, max),
      upArea: buildArea(visible, "up", windowStart, max),
      downArea: buildArea(visible, "down", windowStart, max),
      hasData: visible.length > 0,
    };
  }, [points, now]);

  const mode = running ? "running" : "stopped";
  const baseY = HEIGHT - PAD_B;
  const plotRight = WIDTH - PAD_R;

  return (
    <div className={`traffic-chart traffic-chart--${mode}`}>
      <div className="traffic-chart__head">
        <strong>流量</strong>
        <span className={`traffic-chart__badge traffic-chart__badge--${mode}`}>
          {running ? "最近 1 小时" : "已停止"}
        </span>
        <span className="traffic-chart__rates">
          <span className="traffic-chart__up">↑ {formatRate(latest.up)}</span>
          <span className="traffic-chart__down">↓ {formatRate(latest.down)}</span>
        </span>
      </div>
      <svg
        className="traffic-chart__svg"
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        preserveAspectRatio="xMidYMid meet"
        role="img"
        aria-label={running ? "最近一小时上下行流量" : "内核已停止，流量为零"}
      >
        {chart.yTicks.map((tick) => (
          <g key={`y-${tick.value}`}>
            <line
              className="traffic-chart__grid"
              x1={PAD_L}
              y1={tick.y}
              x2={plotRight}
              y2={tick.y}
            />
            <text className="traffic-chart__axis-label traffic-chart__axis-label--y" x={PAD_L - 6} y={tick.y + 3}>
              {tick.label}
            </text>
          </g>
        ))}
        {chart.xTicks.map((tick) => (
          <g key={`x-${tick.t}`}>
            <line
              className="traffic-chart__tick"
              x1={tick.x}
              y1={baseY}
              x2={tick.x}
              y2={baseY + 4}
            />
            <text className="traffic-chart__axis-label traffic-chart__axis-label--x" x={tick.x} y={HEIGHT - 6}>
              {tick.label}
            </text>
          </g>
        ))}
        <line className="traffic-chart__axis" x1={PAD_L} y1={PAD_T} x2={PAD_L} y2={baseY} />
        <line className="traffic-chart__axis" x1={PAD_L} y1={baseY} x2={plotRight} y2={baseY} />
        {running && chart.hasData ? (
          <>
            <path className="traffic-chart__area traffic-chart__area--down" d={chart.downArea} />
            <path className="traffic-chart__area traffic-chart__area--up" d={chart.upArea} />
            <path className="traffic-chart__line traffic-chart__line--down" d={chart.downLine} />
            <path className="traffic-chart__line traffic-chart__line--up" d={chart.upLine} />
          </>
        ) : (
          <path
            className="traffic-chart__line traffic-chart__line--idle"
            d={`M${PAD_L},${baseY} L${plotRight},${baseY}`}
          />
        )}
      </svg>
      {!running && <p className="traffic-chart__hint">启动内核后开始采样上下行</p>}
    </div>
  );
}
