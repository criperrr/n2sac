import React, { useState } from 'react';
import {
  GraduationCap,
  CalendarClock,
  Trophy,
  Bell,
  User,
  RefreshCw,
  LogOut,
  PanelLeftClose,
  PanelLeftOpen,
} from 'lucide-react';
import { M3_SPRINGS, useSpringValue } from '../theme/springPhysics';
import { useM3Theme } from '../theme/ThemeContext';
import { IconShapeContainer } from './expressive/ExpressivePrimitives';

const RAIL_COLLAPSED_KEY = 'nsac_rail_collapsed';

export type AppTabId = 'boletim' | 'ranking' | 'frequencia' | 'sentinela' | 'studio';

interface NavTabItem {
  id: AppTabId;
  label: string;
  shortLabel: string;
  icon: React.ReactNode;
}

export const NAV_TABS: NavTabItem[] = [
  {
    id: 'boletim',
    label: 'Boletim',
    shortLabel: 'Boletim',
    icon: <GraduationCap size={20} />,
  },
  {
    id: 'ranking',
    label: 'Ranking',
    shortLabel: 'Ranking',
    icon: <Trophy size={20} />,
  },
  {
    id: 'frequencia',
    label: 'Fichas & Frequência',
    shortLabel: 'Frequência',
    icon: <CalendarClock size={20} />,
  },
  {
    id: 'sentinela',
    label: 'Alertas & Conta',
    shortLabel: 'Alertas',
    icon: <Bell size={20} />,
  },
  {
    id: 'studio',
    label: 'Perfil',
    shortLabel: 'Perfil',
    icon: <User size={20} />,
  },
];

const ExpressiveNavItem: React.FC<{
  item: NavTabItem;
  selected: boolean;
  onClick: () => void;
  vertical?: boolean;
}> = ({ item, selected, onClick, vertical = true }) => {
  const { tokens } = useM3Theme();
  const [pressed, setPressed] = useState(false);

  const indicatorProgress = useSpringValue(selected ? 1 : 0, M3_SPRINGS.fastSpatial);
  const indicatorRadius = useSpringValue(pressed ? 8 : 16, M3_SPRINGS.lessSpringy);
  const scale = useSpringValue(pressed ? 0.93 : 1, M3_SPRINGS.springy);

  const targetWidth = vertical ? 46 : 188;
  const animatedIndicatorWidth = Math.max(0, targetWidth * indicatorProgress);

  return (
    <button
      type="button"
      role="tab"
      aria-selected={selected}
      aria-label={item.label}
      title={vertical ? undefined : item.label}
      onClick={onClick}
      onPointerDown={() => setPressed(true)}
      onPointerUp={() => setPressed(false)}
      onPointerLeave={() => setPressed(false)}
      style={{
        flex: vertical ? '1 1 0' : undefined,
        minWidth: 0,
        width: vertical ? undefined : '100%',
        display: 'flex',
        flexDirection: vertical ? 'column' : 'row',
        alignItems: 'center',
        justifyContent: vertical ? 'center' : 'flex-start',
        textAlign: 'left',
        gap: vertical ? 3 : 12,
        padding: vertical ? '6px 2px' : '2px 0',
        background: 'transparent',
        border: 'none',
        cursor: 'pointer',
        position: 'relative',
        transform: `scale(${scale})`,
        color: selected ? tokens.onSecondaryContainer : tokens.onSurfaceVariant,
        fontFamily: 'Outfit, sans-serif',
        userSelect: 'none',
      }}
    >
      {vertical ? (
        <>
          <div
            style={{
              position: 'relative',
              width: 48,
              height: 28,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
            }}
          >
            <div
              style={{
                position: 'absolute',
                width: `${animatedIndicatorWidth.toFixed(1)}px`,
                height: 28,
                borderRadius: `${indicatorRadius.toFixed(1)}px`,
                backgroundColor: tokens.secondaryContainer,
                opacity: Math.min(1, Math.max(0, indicatorProgress)),
              }}
            />
            <span
              style={{
                position: 'relative',
                zIndex: 1,
                display: 'flex',
                color: selected ? tokens.onSecondaryContainer : tokens.onSurfaceVariant,
              }}
            >
              {item.icon}
            </span>
          </div>
          <span
            style={{
              fontSize: '0.66rem',
              fontWeight: selected ? 700 : 500,
              color: selected ? tokens.onSurface : tokens.onSurfaceVariant,
              maxWidth: '100%',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              lineHeight: 1.1,
            }}
          >
            {item.shortLabel}
          </span>
        </>
      ) : (
        <div
          className="nsac-rail-item"
          style={{
            borderRadius: `${indicatorRadius.toFixed(1)}px`,
            backgroundColor: selected ? tokens.secondaryContainer : 'transparent',
            border: `1px solid ${selected ? tokens.primary : 'transparent'}`,
          }}
        >
          <span style={{ display: 'flex', flexShrink: 0 }}>{item.icon}</span>
          <span className="nsac-rail-fade" style={{ fontSize: '0.92rem', fontWeight: selected ? 700 : 500 }}>
            {item.label}
          </span>
        </div>
      )}
    </button>
  );
};

export const ExpressiveNavigationShell: React.FC<{
  activeTab: AppTabId;
  onSelectTab: (tab: AppTabId) => void;
  classCode: string | null;
  nsacEmail: string;
  nextPollCountdown: number;
  refreshCooldown: number;
  isRefreshing: boolean;
  lastHttpStatus?: 200 | 304 | null;
  onManualRefresh: () => void;
  onLogout: () => void;
  children: React.ReactNode;
}> = ({
  activeTab,
  onSelectTab,
  classCode,
  nsacEmail,
  refreshCooldown,
  isRefreshing,
  onManualRefresh,
  onLogout,
  children,
}) => {
  const { tokens } = useM3Theme();
  const [railCollapsed, setRailCollapsed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(RAIL_COLLAPSED_KEY) === '1';
    } catch {
      return false;
    }
  });
  const toggleRail = () => {
    setRailCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(RAIL_COLLAPSED_KEY, next ? '1' : '0');
      } catch {
        // storage unavailable: keep in-memory state only
      }
      return next;
    });
  };
  const refreshDisabled = refreshCooldown > 0 || isRefreshing;

  return (
    <div className="nsac-shell-layout" style={{ backgroundColor: tokens.background, color: tokens.onBackground }}>
      {/* Desktop / Tablet Navigation Rail (collapsible) */}
      <aside
        className={`nsac-desktop-rail${railCollapsed ? ' is-collapsed' : ''}`}
        style={{
          backgroundColor: tokens.surfaceContainerLow,
          borderRight: `1px solid ${tokens.outlineVariant}`,
        }}
      >
        <div className="nsac-rail-header">
          <div className="nsac-rail-fade nsac-rail-brand">
            <IconShapeContainer size={46} active={isRefreshing}>
              <GraduationCap size={22} />
            </IconShapeContainer>
            <div style={{ minWidth: 0 }}>
              <div
                style={{
                  fontFamily: 'Outfit, sans-serif',
                  fontWeight: 800,
                  fontSize: '1.25rem',
                  letterSpacing: '-0.02em',
                  color: tokens.primary,
                }}
              >
                n²sac
              </div>
              <div style={{ fontSize: '0.72rem', color: tokens.onSurfaceVariant }}>CTI • UNESP Bauru</div>
            </div>
          </div>
          <button
            type="button"
            onClick={toggleRail}
            aria-label={railCollapsed ? 'Expandir barra lateral' : 'Recolher barra lateral'}
            aria-expanded={!railCollapsed}
            title={railCollapsed ? 'Expandir barra lateral' : 'Recolher barra lateral'}
            style={{
              width: 38,
              height: 38,
              flexShrink: 0,
              borderRadius: 999,
              border: `1px solid ${tokens.outlineVariant}`,
              backgroundColor: tokens.surfaceContainerHigh,
              color: tokens.onSurface,
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              cursor: 'pointer',
            }}
          >
            {railCollapsed ? <PanelLeftOpen size={18} /> : <PanelLeftClose size={18} />}
          </button>
        </div>

        {/* Student Account Badge */}
        <div
          className="nsac-rail-badge"
          title={railCollapsed ? `${classCode ?? 'CTI'} • ${nsacEmail}` : undefined}
          style={{
            backgroundColor: tokens.surfaceContainerHigh,
            border: `1px solid ${tokens.outlineVariant}`,
          }}
        >
          <div className="nsac-rail-badge-row">
            <span
              className="nsac-rail-fade"
              style={{ fontSize: '0.72rem', fontWeight: 700, color: tokens.primary, textTransform: 'uppercase' }}
            >
              Sua Turma
            </span>
            <span
              style={{
                padding: '2px 8px',
                borderRadius: 999,
                fontSize: '0.72rem',
                fontWeight: 800,
                whiteSpace: 'nowrap',
                backgroundColor: tokens.primaryContainer,
                color: tokens.onPrimaryContainer,
              }}
            >
              {classCode ?? 'CTI'}
            </span>
          </div>
          <div className="nsac-rail-fade-block" style={{ fontSize: '0.8rem', fontWeight: 600 }} title={nsacEmail}>
            {nsacEmail}
          </div>
        </div>

        <nav style={{ display: 'flex', flexDirection: 'column', gap: 4, flex: 1 }}>
          {NAV_TABS.map((item) => (
            <ExpressiveNavItem
              key={item.id}
              item={item}
              selected={activeTab === item.id}
              onClick={() => onSelectTab(item.id)}
              vertical={false}
            />
          ))}
        </nav>

        {/* Actions in Rail Footer */}
        <div
          className="nsac-rail-footer"
          style={{
            backgroundColor: tokens.surfaceContainer,
            border: `1px solid ${tokens.outlineVariant}`,
          }}
        >
          <button
            type="button"
            className="nsac-rail-refresh"
            disabled={refreshDisabled}
            onClick={onManualRefresh}
            title="Atualizar notas"
            style={{
              height: 36,
              borderRadius: 999,
              border: `1px solid ${tokens.outlineVariant}`,
              backgroundColor: tokens.primaryContainer,
              color: tokens.onPrimaryContainer,
              fontWeight: 700,
              fontSize: '0.76rem',
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              cursor: refreshDisabled ? 'not-allowed' : 'pointer',
              opacity: refreshDisabled ? 0.5 : 1,
            }}
          >
            <RefreshCw size={14} className={isRefreshing ? 'spin-icon' : ''} style={{ flexShrink: 0 }} />
            <span className="nsac-rail-fade">{refreshCooldown > 0 ? `${refreshCooldown}s` : 'Atualizar'}</span>
          </button>
          <button
            type="button"
            onClick={onLogout}
            title="Sair da conta"
            aria-label="Sair da conta"
            style={{
              width: 36,
              height: 36,
              flexShrink: 0,
              borderRadius: 999,
              border: `1px solid ${tokens.outlineVariant}`,
              backgroundColor: tokens.surfaceContainerHighest,
              color: tokens.onSurface,
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              cursor: 'pointer',
            }}
          >
            <LogOut size={15} />
          </button>
        </div>
      </aside>

      {/* Main Content Area */}
      <div className="nsac-main-viewport">
        {/* Mobile Top Bar (hidden on Desktop via CSS) */}
        <header
          className="nsac-top-bar"
          style={{
            backgroundColor: `${tokens.surface}99`,
            borderBottom: `1px solid ${tokens.outlineVariant}`,
          }}
        >
          <div
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 10,
              padding: '5px 14px 5px 8px',
              borderRadius: 999,
              backgroundColor: tokens.surfaceContainerHigh,
              border: `1px solid ${tokens.outlineVariant}`,
            }}
          >
            <IconShapeContainer size={32} active={isRefreshing}>
              <GraduationCap size={16} />
            </IconShapeContainer>
            <span
              style={{
                fontFamily: 'Outfit, sans-serif',
                fontWeight: 800,
                fontSize: '1.02rem',
                color: tokens.primary,
              }}
            >
              n²sac
            </span>
            {classCode && (
              <span
                style={{
                  padding: '2px 8px',
                  borderRadius: 999,
                  fontSize: '0.73rem',
                  fontWeight: 800,
                  backgroundColor: tokens.primaryContainer,
                  color: tokens.onPrimaryContainer,
                }}
              >
                {classCode}
              </span>
            )}
          </div>

          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <button
              type="button"
              disabled={refreshCooldown > 0 || isRefreshing}
              onClick={onManualRefresh}
              title="Atualizar notas"
              style={{
                height: 36,
                padding: '0 14px',
                borderRadius: 999,
                border: `1px solid ${tokens.outlineVariant}`,
                backgroundColor: tokens.primaryContainer,
                color: tokens.onPrimaryContainer,
                fontWeight: 700,
                fontSize: '0.76rem',
                display: 'inline-flex',
                alignItems: 'center',
                gap: 6,
                cursor: refreshCooldown > 0 || isRefreshing ? 'not-allowed' : 'pointer',
                opacity: refreshCooldown > 0 || isRefreshing ? 0.5 : 1,
              }}
            >
              <RefreshCw size={14} className={isRefreshing ? 'spin-icon' : ''} />
              <span>{refreshCooldown > 0 ? `${refreshCooldown}s` : 'Atualizar'}</span>
            </button>

            <button
              type="button"
              onClick={onLogout}
              title="Sair da conta"
              className="mobile-only-btn"
              style={{
                width: 36,
                height: 36,
                borderRadius: 999,
                border: `1px solid ${tokens.outlineVariant}`,
                backgroundColor: tokens.surfaceContainerHigh,
                color: tokens.onSurface,
                display: 'inline-flex',
                alignItems: 'center',
                justifyContent: 'center',
                cursor: 'pointer',
              }}
            >
              <LogOut size={15} />
            </button>
          </div>
        </header>

        <main className="nsac-page-content">{children}</main>

        {/* Mobile Floating Pill Bottom Navigation Bar */}
        <nav
          className="nsac-floating-bottom-pill"
          style={{
            backgroundColor: `${tokens.surfaceContainerHigh}A8`,
            border: `1px solid ${tokens.outlineVariant}`,
          }}
        >
          {NAV_TABS.map((item) => (
            <ExpressiveNavItem
              key={item.id}
              item={item}
              selected={activeTab === item.id}
              onClick={() => onSelectTab(item.id)}
              vertical={true}
            />
          ))}
        </nav>
      </div>
    </div>
  );
};
