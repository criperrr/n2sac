import { getApiBaseUrl } from '../api/nsacClient';
import { APP_VERSION } from '../appVersion';

const CONSENT_STORAGE_KEY = 'n2sac_telemetry_consent_v1';
const FINGERPRINT_KEY = 'n2sac_device_fp_v1';

export type TelemetryConsentState = 'undecided' | 'granted' | 'denied';

export function getTelemetryConsent(): TelemetryConsentState {
  try {
    const raw = localStorage.getItem(CONSENT_STORAGE_KEY);
    if (raw === 'granted' || raw === 'denied') return raw;
  } catch {
    // ignore
  }
  return 'undecided';
}

export function setTelemetryConsent(value: 'granted' | 'denied'): void {
  try {
    localStorage.setItem(CONSENT_STORAGE_KEY, value);
  } catch {
    // ignore
  }
  if (value === 'granted') {
    startTelemetryEngine();
  } else {
    stopTelemetryEngine();
  }
}

function getOrCreateDeviceFingerprint(): string {
  try {
    const existing = localStorage.getItem(FINGERPRINT_KEY);
    if (existing) return existing;
    const generated =
      typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
        ? crypto.randomUUID()
        : `dev-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    localStorage.setItem(FINGERPRINT_KEY, generated);
    return generated;
  } catch {
    return 'ephemeral-device';
  }
}

function detectPlatform(): string {
  if (typeof window === 'undefined') return 'unknown';
  const ua = navigator.userAgent || '';
  if (/Android/i.test(ua)) return 'android';
  if (/iPhone|iPad|iPod/i.test(ua)) return 'ios';
  return 'web';
}

export function collectHardwareProfile(): Record<string, unknown> {
  if (typeof window === 'undefined') return {};
  let gpuRenderer = 'unknown';
  let gpuVendor = 'unknown';
  try {
    const canvas = document.createElement('canvas');
    const gl = canvas.getContext('webgl') || canvas.getContext('experimental-webgl');
    if (gl && 'getExtension' in gl) {
      const dbg = (gl as WebGLRenderingContext).getExtension('WEBGL_debug_renderer_info');
      if (dbg) {
        gpuRenderer = String((gl as WebGLRenderingContext).getParameter(dbg.UNMASKED_RENDERER_WEBGL) ?? 'unknown');
        gpuVendor = String((gl as WebGLRenderingContext).getParameter(dbg.UNMASKED_VENDOR_WEBGL) ?? 'unknown');
      }
    }
  } catch {
    // ignore
  }

  const nav = navigator as Navigator & {
    deviceMemory?: number;
    connection?: { effectiveType?: string; rtt?: number; downlink?: number };
  };

  return {
    cpu_cores: nav.hardwareConcurrency ?? null,
    device_memory_gb: nav.deviceMemory ?? null,
    gpu_renderer: gpuRenderer,
    gpu_vendor: gpuVendor,
    screen_width: window.screen?.width ?? null,
    screen_height: window.screen?.height ?? null,
    dpr: window.devicePixelRatio ?? 1,
    viewport_width: window.innerWidth,
    viewport_height: window.innerHeight,
    network_type: nav.connection?.effectiveType ?? null,
    network_rtt_ms: nav.connection?.rtt ?? null,
    network_downlink_mbps: nav.connection?.downlink ?? null,
    user_agent: nav.userAgent.slice(0, 220),
  };
}

let rafId: number | null = null;
let flushTimer: number | null = null;
let lastFrameTs = 0;
let frameDurations: number[] = [];
let jankFrames = 0;
let severeJankFrames = 0;
let longTaskCount = 0;
let maxLongTaskMs = 0;
let longTaskObserver: PerformanceObserver | null = null;
let globalErrorHandlersAttached = false;

function sampleFrame(ts: number) {
  if (lastFrameTs > 0) {
    const dt = ts - lastFrameTs;
    if (dt > 0 && dt < 1000) {
      frameDurations.push(dt);
      if (frameDurations.length > 600) frameDurations.shift();
      if (dt > 18) jankFrames++;
      if (dt > 34) severeJankFrames++;
    }
  }
  lastFrameTs = ts;
  rafId = window.requestAnimationFrame(sampleFrame);
}

function collectRuntimeMetrics(): Record<string, unknown> {
  const count = frameDurations.length;
  const avgDt = count > 0 ? frameDurations.reduce((a, b) => a + b, 0) / count : 16.67;
  const maxDt = count > 0 ? Math.max(...frameDurations) : 16.67;
  const fpsAvg = Math.min(120, Math.round((1000 / avgDt) * 10) / 10);
  const fpsMin = Math.max(1, Math.round((1000 / maxDt) * 10) / 10);

  const perfMem = (performance as Performance & {
    memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number };
  }).memory;

  const metrics: Record<string, unknown> = {
    fps_avg: fpsAvg,
    fps_min: fpsMin,
    frame_samples: count,
    jank_frames: jankFrames,
    severe_jank_frames: severeJankFrames,
    long_tasks: longTaskCount,
    max_long_task_ms: Math.round(maxLongTaskMs * 10) / 10,
    dom_nodes: typeof document !== 'undefined' ? document.getElementsByTagName('*').length : 0,
    js_heap_used_mb: perfMem ? Math.round((perfMem.usedJSHeapSize / 1048576) * 10) / 10 : null,
    js_heap_total_mb: perfMem ? Math.round((perfMem.totalJSHeapSize / 1048576) * 10) / 10 : null,
    route: typeof window !== 'undefined' ? window.location.pathname : '/',
  };

  // Reset window counters after snapshot
  frameDurations = [];
  jankFrames = 0;
  severeJankFrames = 0;
  longTaskCount = 0;
  maxLongTaskMs = 0;

  return metrics;
}

async function sendTelemetryPayload(
  eventType: 'performance' | 'crash',
  crash?: Record<string, unknown>
): Promise<void> {
  if (getTelemetryConsent() !== 'granted') return;
  try {
    const base = getApiBaseUrl();
    const url = `${base}/api/v1/telemetry`;
    await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      keepalive: true,
      body: JSON.stringify({
        event_type: eventType,
        app_version: APP_VERSION,
        platform: detectPlatform(),
        device_fingerprint: getOrCreateDeviceFingerprint(),
        hardware: collectHardwareProfile(),
        metrics: collectRuntimeMetrics(),
        crash: crash ?? null,
      }),
    });
  } catch {
    // never interrupt user experience if telemetry server is unreachable
  }
}

export function reportUiCrash(error: Error | unknown, componentStack?: string): void {
  const errObj = error instanceof Error ? error : new Error(String(error));
  void sendTelemetryPayload('crash', {
    name: errObj.name || 'Error',
    message: errObj.message || 'Unknown UI crash',
    stack: (errObj.stack || '').slice(0, 4000),
    component_stack: (componentStack || '').slice(0, 2000),
    route: typeof window !== 'undefined' ? window.location.pathname : '/',
    timestamp: new Date().toISOString(),
  });
}

export function startTelemetryEngine(): void {
  if (typeof window === 'undefined') return;
  if (getTelemetryConsent() !== 'granted') return;

  if (rafId === null) {
    lastFrameTs = 0;
    rafId = window.requestAnimationFrame(sampleFrame);
  }

  if (!longTaskObserver && typeof PerformanceObserver !== 'undefined') {
    try {
      longTaskObserver = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          longTaskCount++;
          if (entry.duration > maxLongTaskMs) maxLongTaskMs = entry.duration;
        }
      });
      longTaskObserver.observe({ entryTypes: ['longtask'] });
    } catch {
      // browser may not support longtask
    }
  }

  if (!globalErrorHandlersAttached) {
    globalErrorHandlersAttached = true;
    window.addEventListener('error', (event) => {
      reportUiCrash(event.error ?? new Error(event.message || 'Uncaught window error'));
    });
    window.addEventListener('unhandledrejection', (event) => {
      reportUiCrash(event.reason ?? new Error('Unhandled promise rejection'));
    });
  }

  if (flushTimer === null) {
    // Initial sample after 15s, then every 60s
    window.setTimeout(() => void sendTelemetryPayload('performance'), 15_000);
    flushTimer = window.setInterval(() => void sendTelemetryPayload('performance'), 60_000);
  }
}

export function stopTelemetryEngine(): void {
  if (rafId !== null) {
    window.cancelAnimationFrame(rafId);
    rafId = null;
  }
  if (flushTimer !== null) {
    window.clearInterval(flushTimer);
    flushTimer = null;
  }
  if (longTaskObserver) {
    longTaskObserver.disconnect();
    longTaskObserver = null;
  }
}
