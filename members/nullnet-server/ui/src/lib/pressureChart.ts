export const PRESSURE_RANGE_MS = 60_000;

interface TimedWindow {
  age_ms: number;
  duration_ms: number;
}

export function recentPressureWindows<T extends TimedWindow>(windows: T[]) {
  return windows
    .filter(window => window.age_ms <= PRESSURE_RANGE_MS)
    .toSorted((left, right) => right.age_ms - left.age_ms);
}

function coordinate(value: number) {
  return Number(value.toFixed(3));
}

export function timedLinePoints<T extends TimedWindow>(
  windows: T[],
  value: (window: T) => number,
  width: number,
  height: number,
  max: number,
) {
  const points = [`0,${height}`];
  let cursor = 0;
  for (const window of windows) {
    const endAge = Math.min(PRESSURE_RANGE_MS, Math.max(0, window.age_ms));
    const startAge = Math.min(PRESSURE_RANGE_MS, endAge + window.duration_ms);
    const endX = coordinate((1 - endAge / PRESSURE_RANGE_MS) * width);
    const startX = coordinate(Math.max(cursor, (1 - startAge / PRESSURE_RANGE_MS) * width));
    const y = coordinate(height - (value(window) / max) * height);
    if (startX > cursor) points.push(`${startX},${height}`);
    points.push(`${endX},${y}`);
    cursor = Math.max(cursor, endX);
  }
  if (cursor < width) points.push(`${width},${height}`);
  return points.join(' ');
}
