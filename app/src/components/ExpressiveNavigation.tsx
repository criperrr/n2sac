import React, { useState } from 'react';
import {
  GraduationCap,
  CalendarClock,
  Trophy,
  Bell,
  Palette,
  RefreshCw,
  LogOut,
} from 'lucide-react';
import { M3_SPRINGS, useSpringValue } from '../theme/springPhysics';
import { useM3Theme } from '../theme/ThemeContext';
import { IconShapeContainer } from './expressive/ExpressivePrimitives';

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
    label: 'Aparência',
    shortLabel: 'Aparência',
    icon: <Palette size={20} />,
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
        gap: vertical ? 3 : 12,
        padding: vertical ? '6px 2px' : '12px 16px',
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
          style={{
            position: 'relative',
            width: '100%',
            display: 'flex',
            alignItems: 'center',
            gap: 12,
            padding: '10px 14px',
            borderRadius: `${indicatorRadius.toFixed(1)}px`,
            backgroundColor: selected ? tokens.secondaryContainer : 'transparent',
            border: `1px solid ${selected ? tokens.primary : 'transparent'}`,
            transition: 'background-color 220ms ease',
          }}
        >
          <span style={{ display: 'flex' }}>{item.icon}</span>
          <span style={{ fontSize: '0.92rem', fontWeight: selected ? 700 : 500 }}>
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

  return (
    <div className="nsac-shell-layout" style={{ backgroundColor: tokens.background, color: tokens.onBackground }}>
      {/* Desktop / Tablet Navigation Rail */}
      <aside
        className="nsac-desktop-rail"
        style={{
          backgroundColor: tokens.surfaceContainerLow,
          borderRight: `1px solid ${tokens.outlineVariant}`,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 12, padding: '8px 10px 20px' }}>
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
            <div style={{ fontSize: '0.72rem', color: tokens.onSurfaceVariant }}>
              CTI • UNESP Bauru
            </div>
          </div>
        </div>

        {/* Student Account Badge */}
        <div
          style={{
            padding: '12px 14px',
            borderRadius: 20,
            backgroundColor: tokens.surfaceContainerHigh,
            border: `1px solid ${tokens.outlineVariant}`,
            marginBottom: 16,
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
            <span style={{ fontSize: '0.72rem', fontWeight: 700, color: tokens.primary, textTransform: 'uppercase' }}>
              Sua Turma
            </span>
            <span
              style={{
                padding: '2px 8px',
                borderRadius: 999,
                fontSize: '0.72rem',
                fontWeight: 800,
                backgroundColor: tokens.primaryContainer,
                color: tokens.onPrimaryContainer,
              }}
            >
              {classCode ?? 'CTI'}
            </span>
          </div>
          <div
            style={{
              fontSize: '0.8rem',
              fontWeight: 600,
              marginTop: 6,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
            title={nsacEmail}
          >
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
          style={{
            padding: 12,
            borderRadius: 22,
            backgroundColor: tokens.surfaceContainer,
            border: `1px solid ${tokens.outlineVariant}`,
            display: 'flex',
            gap: 6,
          }}
        >
          <button
            type="button"
            disabled={refreshCooldown > 0 || isRefreshing}
            onClick={onManualRefresh}
            style={{
              flex: 1,
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
              gap: 6,
              cursor: refreshCooldown > 0 || isRefreshing ? 'not-allowed' : 'pointer',
              opacity: refreshCooldown > 0 || isRefreshing ? 0.5 : 1,
            }}
          >
            <RefreshCw size={14} className={isRefreshing ? 'spin-icon' : ''} />
            {refreshCooldown > 0 ? `${refreshCooldown}s` : 'Atualizar'}
          </button>
          <button
            type="button"
            onClick={onLogout}
            title="Sair da conta"
            style={{
              width: 36,
              height: 36,
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
