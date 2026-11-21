import React, { useEffect, useRef, useState } from 'react';
import { useM3Theme } from '../../theme/ThemeContext';

/**
 * Material 3 Expressive CircularWavyProgressIndicator
 * (Jetpack Compose `CircularWavyProgressIndicator` / `WavyProgressIndicatorDefaults`):
 * - Active arc: sinusoidal wave along the circle `r(θ) = R + A * sin(N * θ - phase)`
 * - Track arc: smooth circular arc separated by a small gap on both ends
 * - Supports determinate (`progress` 0..1, e.g. pull-to-refresh) and indeterminate (lively spinning + rippling).
 */
export const CircularWavyProgressIndicator: React.FC<{
  size?: number;
  /** 0..1 for determinate pull progress; omit for indeterminate refreshing animation. */
  progress?: number;
  color?: string;
  trackColor?: string;
  label?: string;
}> = ({ size = 40, progress, color, trackColor, label = 'Atualizando' }) => {
  const { tokens } = useM3Theme();
  const activePathRef = useRef<SVGPathElement>(null);
  const trackPathRef = useRef<SVGPathElement>(null);

  const isDeterminate = typeof progress === 'number';
  const progressRef = useRef(progress ?? 0);
  progressRef.current = progress ?? 0;

  useEffect(() => {
    const activeEl = activePathRef.current;
    const trackEl = trackPathRef.current;
    if (!activeEl || !trackEl) return;

    const cx = 50;
    const cy = 50;
    const baseR = 35;
    const maxAmp = 3.9;
    const waveCount = 10;
    const gapRad = (20 * Math.PI) / 180;

    const buildWavyArc = (startAngle: number, sweepAngle: number, amp: number, phase: number): string => {
      const clampedSweep = Math.max(0.08, Math.min(Math.PI * 2 - 0.05, sweepAngle));
      const steps = Math.max(24, Math.round((clampedSweep / (Math.PI * 2)) * 140));
      let d = '';
      for (let i = 0; i <= steps; i++) {
        const u = i / steps;
        const theta = startAngle + u * clampedSweep;
        const endTaper = Math.min(1, u * 8, (1 - u) * 8);
        const r = baseR + amp * endTaper * Math.sin(waveCount * theta - phase);
        const x = cx + r * Math.cos(theta);
        const y = cy + r * Math.sin(theta);
        d += i === 0 ? `M ${x.toFixed(2)} ${y.toFixed(2)}` : ` L ${x.toFixed(2)} ${y.toFixed(2)}`;
      }
      return d;
    };

    const buildSmoothArc = (startAngle: number, sweepAngle: number): string => {
      if (sweepAngle <= 0.08) return '';
      const clamped = Math.min(Math.PI * 2 - 0.05, sweepAngle);
      const x1 = cx + baseR * Math.cos(startAngle);
      const y1 = cy + baseR * Math.sin(startAngle);
      const endAngle = startAngle + clamped;
      const x2 = cx + baseR * Math.cos(endAngle);
      const y2 = cy + baseR * Math.sin(endAngle);
      const largeArc = clamped > Math.PI ? 1 : 0;
      return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${baseR} ${baseR} 0 ${largeArc} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`;
    };

    let raf = 0;
    const startTime = performance.now();

    const tick = (now: number) => {
      const t = (now - startTime) / 1000;
      let startAngle: number;
      let sweepAngle: number;
      let amp: number;
      let phase: number;

      if (isDeterminate) {
        const p = Math.max(0, Math.min(1, progressRef.current));
        startAngle = -Math.PI / 2 + p * Math.PI * 1.6 + t * 1.4;
        sweepAngle = (0.14 + p * 0.64) * Math.PI * 2;
        amp = maxAmp * Math.min(1, p * 1.2);
        phase = t * 7.5 + p * 5;
      } else {
        // Lively M3 Expressive indeterminate spin + breathing arc + fast travelling wave.
        const cycle = (Math.sin(t * 3.1) + 1) * 0.5;
        startAngle = -Math.PI / 2 + t * 4.1;
        sweepAngle = (0.28 + cycle * 0.48) * Math.PI * 2;
        amp = maxAmp;
        phase = t * 9.8;
      }

      activeEl.setAttribute('d', buildWavyArc(startAngle, sweepAngle, amp, phase));

      const trackStart = startAngle + sweepAngle + gapRad;
      const trackSweep = Math.PI * 2 - sweepAngle - gapRad * 2;
      trackEl.setAttribute('d', buildSmoothArc(trackStart, trackSweep));

      raf = requestAnimationFrame(tick);
    };

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [isDeterminate]);

  const activeStroke = color ?? tokens.primary;
  const inactiveStroke = trackColor ?? `${tokens.primary}38`;

  return (
    <span
      role="progressbar"
      aria-label={label}
      style={{
        width: size,
        height: size,
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        flexShrink: 0,
      }}
    >
      <svg viewBox="0 0 100 100" width={size} height={size} aria-hidden="true">
        <path
          ref={trackPathRef}
          fill="none"
          stroke={inactiveStroke}
          strokeWidth={8.5}
          strokeLinecap="round"
        />
        <path
          ref={activePathRef}
          fill="none"
          stroke={activeStroke}
          strokeWidth={8.5}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </span>
  );
};

const TRIGGER_PX = 64;
const REFRESH_HOLD_PX = 64;
const MAX_TENSION_PX = 96;

/**
 * MIUI-style sticky rubber-band Pull-to-Refresh container for mobile:
 * pulling down from the top pushes the page content down with asymptotic tension,
 * revealing the M3 Expressive wavy indicator in the empty space above the page.
 */
export const MobilePullToRefreshContainer: React.FC<{
  refreshing: boolean;
  onRefresh: () => void;
  children: React.ReactNode;
}> = ({ refreshing, onRefresh, children }) => {
  const [pullPx, setPullPx] = useState(0);
  const [dragging, setDragging] = useState(false);

  const pullingRef = useRef(false);
  const startYRef = useRef(0);
  const startXRef = useRef(0);
  const lockedRef = useRef<'none' | 'pull' | 'ignore'>('none');
  const pullPxRef = useRef(0);
  const refreshingRef = useRef(refreshing);
  refreshingRef.current = refreshing;

  useEffect(() => {
    const onTouchStart = (e: TouchEvent) => {
      if (window.innerWidth >= 920 || refreshingRef.current || e.touches.length !== 1) return;
      if (window.scrollY > 2) return;
      if (document.querySelector('[role="dialog"]')) return;

      const touch = e.touches[0]!;
      pullingRef.current = true;
      lockedRef.current = 'none';
      startXRef.current = touch.clientX;
      startYRef.current = touch.clientY;
      pullPxRef.current = 0;
    };

    const onTouchMove = (e: TouchEvent) => {
      if (!pullingRef.current || e.touches.length !== 1) return;
      const touch = e.touches[0]!;
      const dy = touch.clientY - startYRef.current;
      const dx = touch.clientX - startXRef.current;

      if (lockedRef.current === 'none') {
        if (Math.abs(dy) < 10 && Math.abs(dx) < 10) return;
        if (dy > 0 && dy > Math.abs(dx) * 1.25 && window.scrollY <= 2) {
          lockedRef.current = 'pull';
          setDragging(true);
        } else {
          lockedRef.current = 'ignore';
          pullingRef.current = false;
          return;
        }
      }

      if (lockedRef.current === 'pull') {
        if (dy <= 0) {
          pullPxRef.current = 0;
          setPullPx(0);
          return;
        }
        // Asymptotic rubber-band tension: smooth resistance that caps at MAX_TENSION_PX
        const tension = MAX_TENSION_PX * (1 - Math.exp(-dy / 165));
        pullPxRef.current = tension;
        setPullPx(tension);
      }
    };

    const finishPull = () => {
      if (!pullingRef.current) return;
      pullingRef.current = false;
      setDragging(false);
      const triggered = lockedRef.current === 'pull' && pullPxRef.current >= TRIGGER_PX;
      lockedRef.current = 'none';
      pullPxRef.current = 0;
      setPullPx(0);
      if (triggered && !refreshingRef.current) {
        onRefresh();
      }
    };

    window.addEventListener('touchstart', onTouchStart, { passive: true });
    window.addEventListener('touchmove', onTouchMove, { passive: true });
    window.addEventListener('touchend', finishPull, { passive: true });
    window.addEventListener('touchcancel', finishPull, { passive: true });

    return () => {
      window.removeEventListener('touchstart', onTouchStart);
      window.removeEventListener('touchmove', onTouchMove);
      window.removeEventListener('touchend', finishPull);
      window.removeEventListener('touchcancel', finishPull);
    };
  }, [onRefresh]);

  const offset = refreshing ? REFRESH_HOLD_PX : pullPx;
  const progress = Math.min(1, offset / TRIGGER_PX);
  const showIndicator = offset > 4;

  return (
    <div style={{ position: 'relative', width: '100%' }}>
      {/* Revealed empty gap above the pushed-down page */}
      <div
        className="mobile-only-btn"
        aria-hidden={!showIndicator}
        style={{
          height: offset,
          overflow: 'hidden',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          transition: dragging ? 'none' : 'height 320ms cubic-bezier(0.05, 0.7, 0.1, 1)',
        }}
      >
        <div
          style={{
            opacity: refreshing ? 1 : Math.min(1, progress * 1.25),
            transform: `scale(${refreshing ? 1 : 0.65 + progress * 0.35})`,
            transition: dragging ? 'none' : 'transform 260ms cubic-bezier(0.05, 0.7, 0.1, 1), opacity 200ms ease',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
          }}
        >
          {(showIndicator || refreshing) && (
            <CircularWavyProgressIndicator
              size={38}
              progress={refreshing ? undefined : progress}
              label="Atualizando notas"
            />
          )}
        </div>
      </div>

      {children}
    </div>
  );
};
