/**
 * Audio-waveform background strip along the bottom of the login page.
 *
 * Bars heights are derived from a sum of sines so the silhouette
 * reads as "speech-like" rather than uniform/random. Memoised so we
 * compute it once per mount — no need to recompute on every render.
 *
 * The wrapper is `aria-hidden` because this is purely decorative;
 * screen readers shouldn't enumerate 72 spans.
 */

import { useMemo } from "react";

export function Waveform({ bars = 72 }: { bars?: number }) {
  const heights = useMemo(() => {
    const arr: number[] = [];
    for (let i = 0; i < bars; i++) {
      // Composite of three sines + a floor — keeps each bar non-zero
      // (avoids visual "dead spots") while still varied.
      const v =
        0.32 +
        0.22 * Math.sin(i * 0.32) +
        0.18 * Math.sin(i * 0.71 + 0.7) +
        0.12 * Math.sin(i * 1.4 + 1.3);
      arr.push(Math.max(0.08, Math.min(1, v)));
    }
    return arr;
  }, [bars]);

  return (
    <div className="login-waveform-wrap" aria-hidden="true">
      <div className="login-waveform-row">
        {heights.map((h, i) => (
          <span
            key={i}
            className="login-waveform-bar"
            style={{
              height: `${h * 100}%`,
              animationDelay: `${(i * 0.06) % 1.6}s`,
              opacity: 0.18 + h * 0.5,
            }}
          />
        ))}
      </div>
    </div>
  );
}
