import React, { useCallback, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { X } from 'lucide-react';
import { useM3Theme } from '../../theme/ThemeContext';
import { IconShapeContainer } from './ExpressivePrimitives';

const EXIT_ANIMATION_MS = 220;

/**
 * Centered portal dialog sharing the exact look & motion of the subject details popup
 * (`m3-modal-*` enter/exit animations, blurred scrim, Escape + scrim dismissal, body scroll lock).
 *
 * `children` receives `close(next?)`: it plays the exit animation, then runs `next`
 * (or `onClose` when omitted), so in-dialog actions never cut the animation short.
 */
export const ExpressiveModal: React.FC<{
  ariaLabel: string;
  onClose: () => void;
  maxWidth?: number;
  zIndex?: number;
  dismissible?: boolean;
  children: (close: (next?: () => void) => void) => React.ReactNode;
}> = ({ ariaLabel, onClose, maxWidth = 720, zIndex = 9999, dismissible = true, children }) => {
  const { tokens } = useM3Theme();
  const [isClosing, setIsClosing] = useState(false);
  const closingRef = useRef(false);

  const close = useCallback(
    (next?: () => void) => {
      if (closingRef.current) return;
      closingRef.current = true;
      setIsClosing(true);
      window.setTimeout(() => (next ?? onClose)(), EXIT_ANIMATION_MS);
    },
    [onClose]
  );

  useEffect(() => {
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && dismissible) close();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      document.body.style.overflow = prevOverflow;
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [close, dismissible]);

  return createPortal(
    <div
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      onClick={dismissible ? () => close() : undefined}
      className={isClosing ? 'm3-modal-backdrop-exit' : 'm3-modal-backdrop'}
      style={{
        position: 'fixed',
        inset: 0,
        zIndex,
        backgroundColor: 'rgba(0, 0, 0, 0.68)',
        backdropFilter: 'blur(8px)',
        WebkitBackdropFilter: 'blur(8px)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 16,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className={isClosing ? 'm3-modal-dialog-exit' : 'm3-modal-dialog-enter'}
        style={{
          width: '100%',
          maxWidth,
          maxHeight: '88dvh',
          overflowY: 'auto',
          overflowX: 'hidden',
          boxSizing: 'border-box',
          borderRadius: 32,
          padding: '22px 24px 28px',
          backgroundColor: tokens.surfaceContainerLow,
          color: tokens.onSurface,
          border: `1px solid ${tokens.outlineVariant}`,
          boxShadow: '0 28px 72px rgba(0, 0, 0, 0.55)',
        }}
      >
        {children(close)}
      </div>
    </div>,
    document.body
  );
};

/** Round tonal icon button used in dialog/page headers (close, expand, back). */
export const ModalIconButton: React.FC<{
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}> = ({ label, onClick, children }) => {
  const { tokens } = useM3Theme();
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      style={{
        width: 38,
        height: 38,
        borderRadius: 999,
        border: `1px solid ${tokens.outlineVariant}`,
        backgroundColor: tokens.surfaceContainerHigh,
        color: tokens.onSurface,
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        cursor: 'pointer',
        flexShrink: 0,
      }}
    >
      {children}
    </button>
  );
};

/** Header row: shaped icon + eyebrow + title on the left, actions (close by default) on the right. */
export const ModalHeader: React.FC<{
  icon: React.ReactNode;
  eyebrow: React.ReactNode;
  title: React.ReactNode;
  onClose?: () => void;
  actions?: React.ReactNode;
}> = ({ icon, eyebrow, title, onClose, actions }) => {
  const { tokens } = useM3Theme();
  return (
    <div style={{ display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between', gap: 12 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, minWidth: 0 }}>
        <IconShapeContainer size={46} active>
          {icon}
        </IconShapeContainer>
        <div style={{ minWidth: 0 }}>
          <div
            style={{
              fontSize: '0.76rem',
              fontWeight: 700,
              color: tokens.primary,
              textTransform: 'uppercase',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {eyebrow}
          </div>
          <h2
            style={{
              margin: '4px 0 0',
              fontFamily: 'Outfit, sans-serif',
              fontSize: '1.45rem',
              fontWeight: 800,
              overflowWrap: 'anywhere',
            }}
          >
            {title}
          </h2>
        </div>
      </div>
      <div style={{ display: 'flex', gap: 8, flexShrink: 0 }}>
        {actions}
        {onClose && (
          <ModalIconButton label="Fechar" onClick={onClose}>
            <X size={18} />
          </ModalIconButton>
        )}
      </div>
    </div>
  );
};

/**
 * Summary metric tile from the subject details popup (label / big value / caption),
 * shared so every dialog renders its KPIs with the same shape and type scale.
 */
export const MetricTile: React.FC<{
  label: React.ReactNode;
  value: React.ReactNode;
  caption?: React.ReactNode;
  background: string;
  color: string;
  borderColor?: string;
  labelColor?: string;
  captionColor?: string;
  compactValue?: boolean;
}> = ({ label, value, caption, background, color, borderColor, labelColor, captionColor, compactValue = false }) => (
  <div
    style={{
      padding: '14px 16px',
      borderRadius: 22,
      backgroundColor: background,
      color,
      border: borderColor ? `1px solid ${borderColor}` : undefined,
      minWidth: 0,
    }}
  >
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 5,
        fontSize: '0.7rem',
        fontWeight: 700,
        textTransform: 'uppercase',
        color: labelColor,
      }}
    >
      {label}
    </div>
    <div
      style={{
        fontFamily: 'Outfit, sans-serif',
        fontSize: compactValue ? '1.02rem' : '1.75rem',
        fontWeight: 800,
        marginTop: compactValue ? 6 : 4,
      }}
    >
      {value}
    </div>
    {caption !== undefined && (
      <div style={{ fontSize: '0.74rem', marginTop: 2, opacity: captionColor ? 1 : 0.85, color: captionColor }}>
        {caption}
      </div>
    )}
  </div>
);
