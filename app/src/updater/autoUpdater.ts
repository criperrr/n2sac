import { Capacitor } from '@capacitor/core';
import { BackgroundRunner } from '@capacitor/background-runner';
import { LocalNotifications } from '@capacitor/local-notifications';
import { getApiBaseUrl } from '../api/nsacClient';
import { APP_VERSION } from '../appVersion';

export const CURRENT_APP_VERSION = APP_VERSION;
const GITHUB_RELEASES_API =
  'https://api.github.com/repos/criperrr/ultimate-gemilona/releases/latest';
const LAST_BUILD_ID_KEY = 'n2sac_ota_build_id_v1';
// v2 key: versions stored under v1 belong to the pre-release 2.0.x numbering and must not
// outrank the restarted 1.x line.
const INSTALLED_OTA_VERSION_KEY = 'n2sac_ota_version_v2';
const BACKGROUND_RUNNER_LABEL = 'com.cti.n2sac.update-check';

export async function syncBackgroundUpdateState(): Promise<void> {
  if (!Capacitor.isNativePlatform()) return;
  try {
    await BackgroundRunner.dispatchEvent({
      label: BACKGROUND_RUNNER_LABEL,
      event: 'syncUpdateState',
      details: {
        buildId: localStorage.getItem(LAST_BUILD_ID_KEY) ?? '',
        otaVersion: localStorage.getItem(INSTALLED_OTA_VERSION_KEY) ?? '',
        nativeVersion: CURRENT_APP_VERSION,
      },
    });
  } catch {
    // Runner state is refreshed on the next app resume if background work is unavailable.
  }
}

export function getEffectiveAppVersion(): string {
  try {
    const otaVer = localStorage.getItem(INSTALLED_OTA_VERSION_KEY);
    if (otaVer && compareSemver(otaVer, CURRENT_APP_VERSION) > 0) {
      return otaVer;
    }
  } catch {
    // ignore
  }
  return CURRENT_APP_VERSION;
}

export async function initializeUpdateNotifications(): Promise<void> {
  if (!Capacitor.isNativePlatform()) return;

  try {
    let localPermission = await LocalNotifications.checkPermissions();
    if (localPermission.display === 'prompt') {
      localPermission = await LocalNotifications.requestPermissions();
    }
    if (localPermission.display === 'granted') {
      await LocalNotifications.createChannel({
        id: 'n2sac-updates',
        name: 'Atualizações do n²sac',
        description: 'Avisos de novas versões do aplicativo',
        importance: 3,
      });
    }

    let runnerPermission = await BackgroundRunner.checkPermissions();
    if (runnerPermission.notifications === 'prompt') {
      runnerPermission = await BackgroundRunner.requestPermissions({ apis: ['notifications'] });
    }

    await syncBackgroundUpdateState();
  } catch {
    // Notifications and background work are optional when unsupported or denied.
  }
}

export interface NativeUpdateInfo {
  version: string;
  source: 'server-ota' | 'github-release';
  downloadUrl: string;
  releaseUrl: string;
  buildId?: string;
  notes: string;
}

export function compareSemver(a: string, b: string): number {
  const pa = a.replace(/^v/i, '').split('.').map((n) => parseInt(n, 10) || 0);
  const pb = b.replace(/^v/i, '').split('.').map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

export async function applyOtaUpdate(buildId?: string, version?: string): Promise<void> {
  try {
    if (buildId) localStorage.setItem(LAST_BUILD_ID_KEY, buildId);
    if (version) localStorage.setItem(INSTALLED_OTA_VERSION_KEY, version);
  } catch {
    // ignore
  }
  await syncBackgroundUpdateState();
  if ('caches' in window) {
    const keys = await caches.keys();
    await Promise.all(keys.map((k) => caches.delete(k)));
  }
  window.location.reload();
}

/**
 * Checks our own server's `/version.json` for OTA updates.
 */
export async function checkServerOtaUpdate(): Promise<NativeUpdateInfo | null> {
  try {
    const base = getApiBaseUrl();
    const res = await fetch(`${base}/version.json?t=${Date.now()}`, {
      cache: 'no-store',
    });
    if (!res.ok) return null;
    const data = (await res.json()) as { version?: string; buildId?: string };
    const remoteVersion = (data.version ?? '').trim();
    const remoteBuildId = (data.buildId ?? '').trim();
    const prevBuildId = localStorage.getItem(LAST_BUILD_ID_KEY);

    // If this exact buildId was already applied/seen, sync its version and do not prompt again!
    if (remoteBuildId.length > 0 && prevBuildId === remoteBuildId) {
      if (remoteVersion.length > 0) {
        try {
          localStorage.setItem(INSTALLED_OTA_VERSION_KEY, remoteVersion);
        } catch {
          // ignore
        }
      }
      return null;
    }

    const hasNewerVersion =
      remoteVersion.length > 0 && compareSemver(remoteVersion, getEffectiveAppVersion()) > 0;
    const hasNewerBuildId =
      remoteBuildId.length > 0 && prevBuildId !== null && prevBuildId !== remoteBuildId;

    if (hasNewerVersion || hasNewerBuildId) {
      return {
        version: remoteVersion || 'OTA',
        source: 'server-ota',
        downloadUrl: `${base}/`,
        releaseUrl: `${base}/`,
        buildId: remoteBuildId || undefined,
        notes: 'Atualização rápida disponível no servidor n²sac.',
      };
    }

    if (remoteBuildId && !prevBuildId) {
      localStorage.setItem(LAST_BUILD_ID_KEY, remoteBuildId);
    }
  } catch {
    // ignore offline / unreachable server
  }
  return null;
}

/**
 * Checks GitHub Releases for a newer native APK version (> CURRENT_APP_VERSION).
 */
export async function checkGitHubNativeRelease(): Promise<NativeUpdateInfo | null> {
  try {
    const res = await fetch(GITHUB_RELEASES_API, {
      headers: { Accept: 'application/vnd.github+json' },
      cache: 'no-store',
    });
    if (!res.ok) return null;
    const data = (await res.json()) as {
      tag_name?: string;
      html_url?: string;
      body?: string;
      assets?: Array<{ name?: string; browser_download_url?: string }>;
    };
    const latestTag = (data.tag_name ?? '').trim();
    if (!latestTag || compareSemver(latestTag, CURRENT_APP_VERSION) <= 0) {
      return null;
    }
    const apkAsset = data.assets?.find((a) => a.name?.toLowerCase().endsWith('.apk'));
    return {
      version: latestTag,
      source: 'github-release',
      downloadUrl: apkAsset?.browser_download_url ?? data.html_url ?? '',
      releaseUrl: data.html_url ?? '',
      notes: (data.body ?? '').slice(0, 200),
    };
  } catch {
    return null;
  }
}

/**
 * Checks both GitHub Releases (native APK) and Server OTA (`/version.json`).
 */
export async function checkAllUpdates(): Promise<NativeUpdateInfo | null> {
  const [gh, ota] = await Promise.all([checkGitHubNativeRelease(), checkServerOtaUpdate()]);
  return gh ?? ota;
}
