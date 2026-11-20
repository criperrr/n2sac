import React, { useEffect, useState } from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { ThemeProvider } from './theme/ThemeContext';
import { TelemetryErrorBoundary } from './components/TelemetryErrorBoundary';
import { TelemetryConsentModal } from './components/TelemetryConsentModal';
import {
  getTelemetryConsent,
  setTelemetryConsent,
  startTelemetryEngine,
  type TelemetryConsentState,
} from './telemetry/telemetryEngine';
import {
  checkGitHubNativeRelease,
  checkSilentServerOta,
  type NativeUpdateInfo,
} from './updater/autoUpdater';
import './index.css';

const TelemetryBootstrap: React.FC = () => {
  const [consent, setConsent] = useState<TelemetryConsentState>(() => getTelemetryConsent());
  const [nativeUpdate, setNativeUpdate] = useState<NativeUpdateInfo | null>(null);

  useEffect(() => {
    if (consent === 'granted') {
      startTelemetryEngine();
    }
  }, [consent]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const reloaded = await checkSilentServerOta();
      if (reloaded || cancelled) return;
      const ghUpdate = await checkGitHubNativeRelease();
      if (!cancelled && ghUpdate) {
        setNativeUpdate(ghUpdate);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <>
      {consent === 'undecided' && (
        <TelemetryConsentModal
          onDecide={(granted) => {
            const next: TelemetryConsentState = granted ? 'granted' : 'denied';
            setTelemetryConsent(next);
            setConsent(next);
          }}
        />
      )}
      {nativeUpdate && (
        <div
          style={{
            position: 'fixed',
            bottom: 84,
            left: '50%',
            transform: 'translateX(-50%)',
            zIndex: 9998,
            display: 'flex',
            alignItems: 'center',
            gap: 10,
            padding: '10px 14px',
            borderRadius: 999,
            backgroundColor: '#111827',
            color: '#F9FAFB',
            border: '1px solid rgba(255,255,255,0.16)',
            boxShadow: '0 12px 32px rgba(0,0,0,0.45)',
            fontSize: '0.8rem',
            fontWeight: 600,
            maxWidth: '92vw',
          }}
        >
          <span>Nova versão {nativeUpdate.version} disponível</span>
          <a
            href={nativeUpdate.downloadUrl}
            target="_blank"
            rel="noopener noreferrer"
            style={{
              padding: '5px 11px',
              borderRadius: 999,
              backgroundColor: '#10A37F',
              color: '#FFFFFF',
              textDecoration: 'none',
              fontWeight: 700,
              whiteSpace: 'nowrap',
            }}
          >
            Atualizar
          </a>
          <button
            type="button"
            onClick={() => setNativeUpdate(null)}
            style={{
              background: 'none',
              border: 'none',
              color: '#9CA3AF',
              cursor: 'pointer',
              fontSize: '0.9rem',
              padding: '0 4px',
            }}
            aria-label="Fechar aviso"
          >
            ✕
          </button>
        </div>
      )}
    </>
  );
};

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <TelemetryErrorBoundary>
      <ThemeProvider>
        <App />
        <TelemetryBootstrap />
      </ThemeProvider>
    </TelemetryErrorBoundary>
  </React.StrictMode>
);
