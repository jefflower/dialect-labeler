/**
 * Brand logo for the login page — square outline + inset animated
 * "speech waveform" bars. SVG only (no images, no fonts) so it stays
 * crisp at any DPI and contributes ~0 KB to the bundle.
 *
 * Ported from the design prototype in `AI 工作台登录页面设计.zip` →
 * `logo.jsx`. The bars heights / durations come straight from the
 * mock, which was tuned by eye to feel "alive but not jittery".
 */

interface LogoMarkProps {
  size?: number;
  animate?: boolean;
}

export function LogoMark({ size = 32, animate = true }: LogoMarkProps) {
  // Bar heights at rest (fraction of inner space). Six bars, asymmetric
  // — the eye reads symmetric bars as static.
  const bars = [0.35, 0.65, 0.92, 0.55, 0.78, 0.32];
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      style={{ display: "block" }}
    >
      <rect
        x="1.5"
        y="1.5"
        width="29"
        height="29"
        rx="6"
        stroke="var(--accent)"
        strokeWidth="1.6"
      />
      <g>
        {bars.map((h, i) => {
          const barH = h * 16;
          const x = 6 + i * 3.4;
          const y = 16 - barH / 2;
          return (
            <rect
              key={i}
              x={x}
              y={y}
              width="2"
              height={barH}
              rx="1"
              fill="var(--accent)"
            >
              {animate && (
                <>
                  <animate
                    attributeName="height"
                    values={`${barH};${barH * 0.4};${barH * 1.1};${barH}`}
                    dur={`${1.4 + i * 0.18}s`}
                    repeatCount="indefinite"
                  />
                  <animate
                    attributeName="y"
                    values={`${y};${16 - (barH * 0.4) / 2};${16 - (barH * 1.1) / 2};${y}`}
                    dur={`${1.4 + i * 0.18}s`}
                    repeatCount="indefinite"
                  />
                </>
              )}
            </rect>
          );
        })}
      </g>
    </svg>
  );
}

export function Logo({ size = 32 }: { size?: number }) {
  return (
    <div className="login-logo-row">
      <LogoMark size={size} />
      <div className="login-logo-text">
        <div className="login-logo-cn">方言工坊</div>
        <div className="login-logo-en">Dialect Workshop</div>
      </div>
    </div>
  );
}
