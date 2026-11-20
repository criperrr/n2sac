import React, { useCallback, useEffect, useRef, useState } from 'react';
import ReactDOM from 'react-dom/client';
import { App as CapacitorApp } from '@capacitor/app';
import { Capacitor } from '@capacitor/core';
import App from './App';
import { ThemeProvider } from './theme/ThemeContext';
import { TelemetryErrorBoundary } from './components/TelemetryErrorBoundary';
import { TelemetryConsentModal } from './components/TelemetryConsentModal';
import { UpdateAvailableDialog } from './components/UpdateAvailableDialog';
import {
  getTelemetryConsent,
  setTelemetryConsent,
  startTelemetryEngine,
  type TelemetryConsentState,
} from './telemetry/telemetryEngine';
import {
  checkAllUpdates,
  initializeUpdateNotifications,
  syncBackgroundUpdateState,
  type NativeUpdateInfo,
} from './updater/autoUpdater';
import './index.css';

const TelemetryBootstrap: React.FC = () => {
  const [consent, setConsent] = useState<TelemetryConsentState>(() => getTelemetryConsent());
  const [nativeUpdate, setNativeUpdate] = useState<NativeUpdateInfo | null>(null);
  const updateCheckInFlight = useRef(false);
  const lastUpdateCheckAt = useRef(0);

  const checkForUpdates = useCallback(async () => {
    const now = Date.now();
    if (updateCheckInFlight.current || now - lastUpdateCheckAt.current < 3000) return;
    updateCheckInFlight.current = true;
    lastUpdateCheckAt.current = now;
    try {
      setNativeUpdate(await checkAllUpdates());
      await syncBackgroundUpdateState();
    } finally {
      updateCheckInFlight.current = false;
    }
  }, []);

  useEffect(() => {
    if (consent === 'granted') {
      startTelemetryEngine();
    }
  }, [consent]);

  useEffect(() => {
    const forceWebViewRepaint = () => {
      if (document.visibilityState === 'hidden') return;
      const root = document.documentElement;
      root.style.transform = 'translateZ(0)';
      window.requestAnimationFrame(() => {
        root.style.transform = '';
      });
    };
    document.addEventListener('visibilitychange', forceWebViewRepaint);
    window.addEventListener('focus', forceWebViewRepaint);
    window.addEventListener('pageshow', forceWebViewRepaint);
    return () => {
      document.removeEventListener('visibilitychange', forceWebViewRepaint);
      window.removeEventListener('focus', forceWebViewRepaint);
      window.removeEventListener('pageshow', forceWebViewRepaint);
    };
  }, []);

  useEffect(() => {
    void initializeUpdateNotifications();
    void checkForUpdates();
    let disposed = false;
    let appStateListener: { remove: () => Promise<void> } | undefined;

    const onResume = () => {
      if (document.visibilityState === 'visible') void checkForUpdates();
    };
    const onAppStateChange = ({ isActive }: { isActive: boolean }) => {
      if (isActive) void checkForUpdates();
    };

    document.addEventListener('visibilitychange', onResume);
    window.addEventListener('pageshow', onResume);
    window.addEventListener('focus', onResume);
    if (Capacitor.isNativePlatform()) {
      void CapacitorApp.addListener('appStateChange', onAppStateChange).then((handle) => {
        if (disposed) void handle.remove();
        else appStateListener = handle;
      });
    }

    return () => {
      disposed = true;
      document.removeEventListener('visibilitychange', onResume);
      window.removeEventListener('pageshow', onResume);
      window.removeEventListener('focus', onResume);
      if (appStateListener) void appStateListener.remove();
    };
  }, [checkForUpdates]);

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
        <UpdateAvailableDialog update={nativeUpdate} onDismiss={() => setNativeUpdate(null)} />
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
