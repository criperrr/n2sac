import React, { useEffect, useMemo, useRef, useState } from 'react';
import { prefersReducedMotion } from '../../theme/motion';
import { useM3Theme } from '../../theme/ThemeContext';

/**
 * Loading veil in the spirit of Circle to Search: a translucent haze colored by the active M3
 * theme tokens drifting over the area, a soft highlight sweeping across, and fine film grain.
 * When `active` turns false the veil fades out while its motion decelerates to a stop, revealing
 * what is underneath.
 *
 * Every moving part is a `transform`/`opacity` animation (compositor only), so the veil costs
 * nothing on the main thread and doesn't fight scrolling.
 */

function hexAlpha(hex: string, alpha: number): string {
  const clean = hex.trim().replace('#', '');
  const full =
    clean.length === 3
      ? clean
          .split('')
          .map((c) => c + c)
          .join('')
      : clean;
  if (full.length !== 6) return hex;
  const r = parseInt(full.slice(0, 2), 16);
  const g = parseInt(full.slice(2, 4), 16);
  const b = parseInt(full.slice(4, 6), 16);
  if (Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b)) return hex;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

let grainUrl: string | null = null;
/** Monochrome noise tile, generated once per session (cheaper than an SVG turbulence filter). */
function grainTile(): string {
  if (grainUrl) return grainUrl;
  const size = 128;
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = size;
  const ctx = canvas.getContext('2d');
  if (!ctx) return '';
  const img = ctx.createImageData(size, size);
  for (let i = 0; i < img.data.length; i += 4) {
    const v = Math.random() * 255;
    img.data[i] = img.data[i + 1] = img.data[i + 2] = v;
    img.data[i + 3] = Math.random() < 0.55 ? 38 : 0;
  }
  ctx.putImageData(img, 0, 0);
  grainUrl = canvas.toDataURL('image/png');
  return grainUrl;
}

const EXIT_MS = 1100;
const SURFACE_EXIT_MS = 560;

export const ShimmerVeil: React.FC<{
  active: boolean;
  /** Corner radius of the covered area (the veil clips to it). */
  radius?: number | string;
  /** Lighter variant for small areas (text skeletons): no grain, subtler haze. */
  compact?: boolean;
  /** Large dialog/card surface variant: smaller GPU overhang, no mix-blend readback, crisp exit. */
  surface?: boolean;
  /** Fires whenever the veil mounts or finishes fading out and unmounts. */
  onMountedChange?: (mounted: boolean) => void;
}> = ({ active, radius = 0, compact = false, surface = false, onMountedChange }) => {
  const { tokens, effectiveIsDark } = useM3Theme();
  const rootRef = useRef<HTMLDivElement>(null);
  const [mounted, setMounted] = useState(active);
  const onMountedChangeRef = useRef(onMountedChange);
  onMountedChangeRef.current = onMountedChange;

  const themePalette = useMemo(() => {
    const baseAlpha = compact ? (effectiveIsDark ? 0.14 : 0.18) : surface ? (effectiveIsDark ? 0.22 : 0.24) : effectiveIsDark ? 0.28 : 0.3;
    const boost = effectiveIsDark ? 1 : 0.92;
    const haze = [
      `radial-gradient(38% 42% at 28% 34%, ${hexAlpha(tokens.primary, 0.54 * boost)}, transparent 70%)`,
      `radial-gradient(34% 40% at 70% 30%, ${hexAlpha(tokens.tertiary, 0.48 * boost)}, transparent 72%)`,
      `radial-gradient(40% 44% at 62% 72%, ${hexAlpha(tokens.primaryContainer, 0.58 * boost)}, transparent 70%)`,
      `radial-gradient(30% 36% at 30% 76%, ${hexAlpha(tokens.secondary, 0.44 * boost)}, transparent 72%)`,
      `radial-gradient(26% 30% at 84% 62%, ${hexAlpha(tokens.tertiaryContainer, 0.46 * boost)}, transparent 72%)`,
    ].join(', ');
    const sweepColor = effectiveIsDark ? hexAlpha(tokens.primary, 0.3) : hexAlpha('#FFFFFF', 0.48);
    const sweep = `linear-gradient(100deg, transparent 38%, ${sweepColor} 50%, transparent 62%)`;
    return {
      base: hexAlpha(tokens.primaryContainer, baseAlpha),
      haze,
      sweep,
    };
  }, [tokens, effectiveIsDark, compact, surface]);

  useEffect(() => {
    onMountedChangeRef.current?.(mounted);
  }, [mounted]);

  useEffect(() => {
    if (active) {
      setMounted(true);
      const root = rootRef.current;
      if (root) {
        root.getAnimations({ subtree: true }).forEach((a) => a.updatePlaybackRate(1));
      }
      return;
    }
    const root = rootRef.current;
    if (!root || prefersReducedMotion()) {
      setMounted(false);
      return;
    }
    let cancelled = false;
    let startRaf = 0;
    let fade: Animation | null = null;
    let frame = 0;

    // Wait one frame after the revealed content commits/paints so the fadeout always starts cleanly
    // at opacity: 1 instead of being eaten by the content's mount frame.
    startRaf = requestAnimationFrame(() => {
      if (cancelled) return;
      const exitMs = surface ? SURFACE_EXIT_MS : EXIT_MS;
      fade = root.animate([{ opacity: 1 }, { opacity: 0 }], {
        duration: exitMs,
        easing: surface ? 'cubic-bezier(0.25, 0.1, 0.25, 1)' : 'cubic-bezier(0.05, 0.7, 0.1, 1)',
        fill: 'forwards',
      });
      if (!surface) {
        const loops = root.getAnimations({ subtree: true }).filter((a) => a !== fade);
        const start = performance.now();
        const brake = (now: number) => {
          const t = Math.min(1, (now - start) / exitMs);
          const rate = Math.max(0.02, (1 - t) ** 2);
          loops.forEach((a) => a.updatePlaybackRate(rate));
          if (t < 1) frame = requestAnimationFrame(brake);
        };
        frame = requestAnimationFrame(brake);
      }
      fade.onfinish = () => {
        if (!cancelled) setMounted(false);
      };
    });
    return () => {
      cancelled = true;
      cancelAnimationFrame(startRaf);
      cancelAnimationFrame(frame);
      fade?.cancel();
    };
  }, [active, surface]);

  if (!mounted) return null;
  const reduced = prefersReducedMotion();
  return (
    <div
      ref={rootRef}
      aria-hidden="true"
      className={`n2sac-veil${compact ? ' n2sac-veil--compact' : ''}${surface ? ' n2sac-veil--surface' : ''}${reduced ? ' n2sac-veil--static' : ''}`}
      style={{ borderRadius: radius, background: themePalette.base }}
    >
      <div className="n2sac-veil__haze" style={{ background: themePalette.haze }} />
      <div className="n2sac-veil__sweep" style={{ background: themePalette.sweep }} />
      {!compact && !surface && <div className="n2sac-veil__grain" style={{ backgroundImage: `url(${grainTile()})` }} />}
    </div>
  );
};
