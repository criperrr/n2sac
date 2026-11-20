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
import './index.css';

const TelemetryBootstrap: React.FC = () => {
  const [consent, setConsent] = useState<TelemetryConsentState>(() => getTelemetryConsent());

  useEffect(() => {
    if (consent === 'granted') {
      startTelemetryEngine();
    }
  }, [consent]);

  if (consent !== 'undecided') return null;

  return (
    <TelemetryConsentModal
      onDecide={(granted) => {
        const next: TelemetryConsentState = granted ? 'granted' : 'denied';
        setTelemetryConsent(next);
        setConsent(next);
      }}
    />
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
