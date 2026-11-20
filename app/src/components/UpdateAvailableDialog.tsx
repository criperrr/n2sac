import React, { useState } from 'react';
import { Download, Sparkles, Zap } from 'lucide-react';
import { useM3Theme } from '../theme/ThemeContext';
import { formatVersionLabel } from '../appVersion';
import { applyOtaUpdate, type NativeUpdateInfo } from '../updater/autoUpdater';
import { EnhancedMorphButton } from './expressive/ExpressivePrimitives';
import { ExpressiveModal, ModalHeader } from './expressive/ExpressiveModal';

/** Themed "update available" popup shown on app open/resume (OTA reload or new APK download). */
export const UpdateAvailableDialog: React.FC<{
  update: NativeUpdateInfo;
  onDismiss: () => void;
}> = ({ update, onDismiss }) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const [applying, setApplying] = useState(false);
  const isOta = update.source === 'server-ota';
  const notes = update.notes.trim();

  return (
    <ExpressiveModal ariaLabel="Atualização disponível" onClose={onDismiss} maxWidth={440} zIndex={10000}>
      {(close) => (
        <>
          <ModalHeader
            icon={<Sparkles size={21} />}
            eyebrow={isOta ? 'Atualização rápida' : 'Nova versão do app'}
            title={formatVersionLabel(update.version)}
            onClose={() => close()}
          />

          <div
            style={{
              display: 'flex',
              alignItems: 'flex-start',
              gap: 10,
              marginTop: 18,
              padding: '12px 14px',
              borderRadius: isMaterialYou ? 20 : 12,
              backgroundColor: tokens.surfaceContainer,
              border: `1px solid ${tokens.outlineVariant}`,
            }}
          >
            {isOta ? (
              <Zap size={18} color={tokens.primary} style={{ flexShrink: 0, marginTop: 1 }} />
            ) : (
              <Download size={18} color={tokens.primary} style={{ flexShrink: 0, marginTop: 1 }} />
            )}
            <div style={{ fontSize: '0.84rem', lineHeight: 1.5, color: tokens.onSurfaceVariant }}>
              {isOta
                ? 'Leva só alguns segundos: o app recarrega já atualizado, sem reinstalar.'
                : 'Baixe e instale o novo APK para receber esta versão.'}
              {notes && !isOta && (
                <div style={{ marginTop: 6, color: tokens.onSurface, whiteSpace: 'pre-line' }}>{notes}</div>
              )}
            </div>
          </div>

          <div style={{ display: 'flex', justifyContent: 'flex-end', alignItems: 'center', gap: 10, marginTop: 20, flexWrap: 'wrap' }}>
            <EnhancedMorphButton variant="outlined" size="sm" onClick={() => close()} disabled={applying}>
              Depois
            </EnhancedMorphButton>
            {isOta ? (
              <EnhancedMorphButton
                size="sm"
                icon={<Zap size={15} />}
                disabled={applying}
                onClick={() => {
                  setApplying(true);
                  void applyOtaUpdate(update.buildId, update.version).catch(() => setApplying(false));
                }}
              >
                {applying ? 'Atualizando…' : 'Atualizar agora'}
              </EnhancedMorphButton>
            ) : (
              <a
                href={update.downloadUrl || update.releaseUrl}
                target="_blank"
                rel="noopener noreferrer"
                onClick={() => close()}
                style={{
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 6,
                  padding: '8px 16px',
                  borderRadius: isMaterialYou ? 999 : 10,
                  backgroundColor: tokens.primary,
                  color: tokens.onPrimary,
                  textDecoration: 'none',
                  fontWeight: 700,
                  fontSize: '0.86rem',
                  whiteSpace: 'nowrap',
                }}
              >
                <Download size={15} />
                Baixar APK
              </a>
            )}
          </div>
        </>
      )}
    </ExpressiveModal>
  );
};
