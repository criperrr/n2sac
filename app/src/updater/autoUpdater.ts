import { getApiBaseUrl } from '../api/nsacClient';

export const CURRENT_APP_VERSION = '2.0.0';
const GITHUB_RELEASES_API =
  'https://api.github.com/repos/criperrr/ultimate-gemilona/releases/latest';
const LAST_BUILD_ID_KEY = 'n2sac_ota_build_id_v1';

export interface NativeUpdateInfo {
  version: string;
  downloadUrl: string;
  releaseUrl: string;
  notes: string;
}

function compareSemver(a: string, b: string): number {
  const pa = a.replace(/^v/i, '').split('.').map((n) => parseInt(n, 10) || 0);
  const pb = b.replace(/^v/i, '').split('.').map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

/**
 * Checks our own server's `/version.json` for silent frontend OTA updates.
 * If a newer `buildId` is detected, clears browser/WebView caches and silently reloads.
 */
export async function checkSilentServerOta(): Promise<boolean> {
  try {
    const base = getApiBaseUrl();
    const res = await fetch(`${base}/version.json?t=${Date.now()}`, {
      cache: 'no-store',
    });
    if (!res.ok) return false;
    const data = (await res.json()) as { version?: string; buildId?: string };
    if (!data.buildId) return false;

    const prevBuildId = localStorage.getItem(LAST_BUILD_ID_KEY);
    if (!prevBuildId) {
      localStorage.setItem(LAST_BUILD_ID_KEY, data.buildId);
      return false;
    }

    if (prevBuildId !== data.buildId) {
      localStorage.setItem(LAST_BUILD_ID_KEY, data.buildId);
      if ('caches' in window) {
        const keys = await caches.keys();
        await Promise.all(keys.map((k) => caches.delete(k)));
      }
      // Silent reload to apply new web assets
      window.location.reload();
      return true;
    }
  } catch {
    // ignore offline / unreachable server
  }
  return false;
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
      downloadUrl: apkAsset?.browser_download_url ?? data.html_url ?? '',
      releaseUrl: data.html_url ?? '',
      notes: (data.body ?? '').slice(0, 200),
    };
  } catch {
    return null;
  }
}
