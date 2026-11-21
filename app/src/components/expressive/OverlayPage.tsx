import React, { useCallback, useEffect, useLayoutEffect, useRef } from 'react';
import { usePageScrollLock } from '../../theme/scrollLock';
import { createPortal } from 'react-dom';
import { useM3Theme } from '../../theme/ThemeContext';
import { useBackLayer } from '../../navigation/backStack';
import { DURATION, EASING, animateSharedAxisX, prefersReducedMotion } from '../../theme/motion';

/** Shared axis X timing (MDC: long1 + emphasized). */
const ENTER = { duration: DURATION.long1, easing: EASING.emphasizedDecelerate };
const EXIT = { duration: DURATION.medium2, easing: EASING.emphasizedAccelerate };

/**
 * Full-screen page pushed above the current screen with the M3 shared axis X (forward) pattern:
 * it slides in 30dp from the end with a fade through, while the page underneath slides 30dp to
 * the start and fades out. Closing plays the backward pattern. The page underneath stays
 * mounted, so the predictive back gesture can reveal it.
 *
 * Covers only the content area: the desktop rail and the mobile bottom navigation stay visible.
 * `children` receives `close()`, which plays the backward transition before calling `onClose`.
 */
export const OverlayPage: React.FC<{
  ariaLabel: string;
  onClose: () => void;
  children: (close: () => void) => React.ReactNode;
}> = ({ ariaLabel, onClose, children }) => {
  const { tokens } = useM3Theme();
  const closingRef = useRef(false);
  const surfaceRef = useRef<HTMLDivElement>(null);
  const underlayAnimations = useRef<Animation[]>([]);

  const underlay = () => document.querySelector<HTMLElement>('.nsac-page-content');

  const revealUnderlay = () => {
    underlayAnimations.current.forEach((a) => a.cancel());
    underlayAnimations.current = [];
  };

  /** Pins the page underneath in its "covered" end state (slid to the start, invisible). */
  const coverUnderlay = (animate: boolean) => {
    const el = underlay();
    if (!el) return;
    revealUnderlay();
    const timing = animate && !prefersReducedMotion() ? ENTER : { duration: 1, easing: EASING.linear };
    underlayAnimations.current = animateSharedAxisX(el, 'leave', true, timing, 'forwards');
  };

  useLayoutEffect(() => {
    const surface = surfaceRef.current;
    if (surface && !prefersReducedMotion()) animateSharedAxisX(surface, 'enter', true, ENTER, 'backwards');
    coverUnderlay(true);
    return revealUnderlay;
    // Runs once per mount: the pattern belongs to this page's lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const close = useCallback(() => {
    if (closingRef.current) return;
    closingRef.current = true;
    const surface = surfaceRef.current;
    const el = underlay();
    if (!surface || prefersReducedMotion()) {
      revealUnderlay();
      onClose();
      return;
    }
    // Backward shared axis X: this page slides to the end and fades out, the page below returns.
    surface.style.pointerEvents = 'none';
    const [out] = animateSharedAxisX(surface, 'leave', false, EXIT, 'forwards');
    revealUnderlay();
    if (el) underlayAnimations.current = animateSharedAxisX(el, 'enter', false, EXIT, 'forwards');
    out!.finished.then(() => {
      revealUnderlay();
      onClose();
    });
  }, [onClose]);

  useBackLayer(
    true,
    (source) => {
      if (source === 'gesture') {
        closingRef.current = true;
        revealUnderlay();
        onClose();
      } else {
        close();
      }
    },
    {
      kind: 'page',
      surfaceRef,
      // The predictive back preview must reveal the real page underneath.
      onGestureStart: revealUnderlay,
      onGestureCancel: () => coverUnderlay(false),
    }
  );

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [close]);
  usePageScrollLock();

  return createPortal(
    <div
      ref={surfaceRef}
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      className="nsac-overlay-page"
      style={{ backgroundColor: tokens.background, color: tokens.onBackground }}
    >
      <div className="nsac-overlay-page-content">{children(close)}</div>
    </div>,
    document.body
  );
};
