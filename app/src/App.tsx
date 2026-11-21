import { ProfileViewerProvider } from './components/profile/ProfileViewer';
import { initializePushNotifications } from './notifications/pushService';
import React, { lazy, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import {
  checkServerHealth,
  fetchAccountProfile,
  fetchAtrasos,
  fetchBoletim,
  fetchFichaList,
  fetchHomeInfo,
  fetchRankingPreferences,
  fetchSaidas,
  loadCachedResource,
  loadStoredSession,
  logoutNsacSession,
  NsacApiClientError,
  updateRankingPreferences,
} from './api/nsacClient';
import type {
  AccountProfileResponse,
  AllYearsResponse,
  AtrasoRecord,
  FichaAvaliacaoEntry,
  HomeInfo,
  LoginResponse,
  RankingPreferences,
  SaidaRecord,
} from './api/types';
import {
  ExpressiveNavigationShell,
  type AppTabId,
} from './components/ExpressiveNavigation';
import { RankingConsentModal } from './components/RankingConsentModal';
import { MorphingLoadingState } from './components/expressive/ShapeMorph';
import { useM3Theme } from './theme/ThemeContext';
import { useBackLayer } from './navigation/backStack';
import { useTabSwipeGesture } from './navigation/useTabSwipeGesture';
import { canRunScreenTransition, runScreenTransition, type ScreenTransition } from './theme/viewTransition';

const BoletimScreen = lazy(() =>
  import('./screens/BoletimScreen').then((m) => ({ default: m.BoletimScreen }))
);
const RankingScreen = lazy(() =>
  import('./screens/RankingScreen').then((m) => ({ default: m.RankingScreen }))
);
const FrequenciaScreen = lazy(() =>
  import('./screens/FrequenciaScreen').then((m) => ({ default: m.FrequenciaScreen }))
);
const SentinelaScreen = lazy(() =>
  import('./screens/SentinelaScreen').then((m) => ({ default: m.SentinelaScreen }))
);
const ProfileScreen = lazy(() =>
  import('./screens/ProfileScreen').then((m) => ({ default: m.ProfileScreen }))
);
const AdaptiveStudioScreen = lazy(() =>
  import('./screens/AdaptiveStudioScreen').then((m) => ({ default: m.AdaptiveStudioScreen }))
);
const LoginScreen = lazy(() =>
  import('./screens/LoginScreen').then((m) => ({ default: m.LoginScreen }))
);

const MANUAL_REFRESH_COOLDOWN_SECS = 60;

export const App: React.FC = () => {
  const { applyCourseClassCode } = useM3Theme();
  const [session, setSession] = useState<LoginResponse | null>(() => loadStoredSession());
  const [activeTab, setActiveTab] = useState<AppTabId>('boletim');
  const tabSurfaceRef = useRef<HTMLDivElement>(null);
  /**
   * How the current page arrived, so it animates exactly once:
   * - `vt`: a View Transition already animated it (no CSS entry animation, no card stagger);
   * - `css`: fallback fade-through entry animation (no View Transitions / first load);
   * - `none`: predictive back already previewed the swap.
   */
  const [pageEntry, setPageEntry] = useState<'vt' | 'css' | 'swipe-next' | 'swipe-prev' | 'none'>('css');
  // Top-level destinations: back returns to the start destination (Boletim), previewing the page.
  // Gesture: the outgoing page already previewed out, so animate Boletim in cleanly via CSS.
  useBackLayer(
    activeTab !== 'boletim',
    (source) => {
      if (source === 'gesture') {
        setPageEntry('css');
        setActiveTab('boletim');
      } else {
        selectTab('boletim');
      }
    },
    { kind: 'tab', surfaceRef: tabSurfaceRef }
  );

  // M3 transition patterns: top-level destinations fade through on tap, slide horizontally on
  // swipe, and Settings (opened from the top bar gear) moves along the shared X axis.
  const navigateTab = useCallback(
    (tab: AppTabId, overrideKind?: ScreenTransition) => {
      if (tab === activeTab) return;
      const kind: ScreenTransition =
        overrideKind ??
        (tab === 'settings' ? 'axis-x-forward' : activeTab === 'settings' ? 'axis-x-backward' : 'fade-through');
      const entry = canRunScreenTransition() ? 'vt' : 'css';
      runScreenTransition(kind, () => {
        setPageEntry(entry);
        setActiveTab(tab);
      });
    },
    [activeTab]
  );

  const selectTab = useCallback((tab: AppTabId) => navigateTab(tab), [navigateTab]);

  const handleSwipeTab = useCallback(
    (tab: AppTabId, direction: 'next' | 'prev') => {
      if (tab === activeTab) return;
      // Direct CSS directional slide (no View Transition snapshot stall, zero touch blocking).
      setPageEntry(direction === 'next' ? 'swipe-next' : 'swipe-prev');
      setActiveTab(tab);
    },
    [activeTab]
  );

  useTabSwipeGesture(tabSurfaceRef, activeTab, handleSwipeTab, Boolean(session));
  const [serverHealthy, setServerHealthy] = useState<boolean | null>(null);

  // Instant 0ms Stale-While-Revalidate Local Cache Initialization
  const [boletim, setBoletim] = useState<AllYearsResponse | null>(() => {
    const s = loadStoredSession();
    if (!s) return null;
    return loadCachedResource<AllYearsResponse>('boletim', s.account_id)?.data ?? null;
  });
  const [boletimEtag, setBoletimEtag] = useState<string | null>(() => {
    const s = loadStoredSession();
    if (!s) return null;
    return loadCachedResource<AllYearsResponse>('boletim', s.account_id)?.etag ?? null;
  });
  const [lastStatus, setLastStatus] = useState<200 | 304 | null>(() => {
    const s = loadStoredSession();
    if (!s) return null;
    return loadCachedResource<AllYearsResponse>('boletim', s.account_id)?.lastStatus ?? null;
  });
  const [cachedAt, setCachedAt] = useState<number | null>(() => {
    const s = loadStoredSession();
    if (!s) return null;
    return loadCachedResource<AllYearsResponse>('boletim', s.account_id)?.cachedAt ?? null;
  });
  const [nextPollHintSecs, setNextPollHintSecs] = useState<number>(() => {
    const s = loadStoredSession();
    if (!s) return 300;
    return loadCachedResource<AllYearsResponse>('boletim', s.account_id)?.nextPollHintSecs ?? 300;
  });
  const nextPollHintSecsRef = useRef(nextPollHintSecs);
  nextPollHintSecsRef.current = nextPollHintSecs;
  const [nextPollAt, setNextPollAt] = useState<number | null>(() => {
    const s = loadStoredSession();
    if (!s) return null;
    const hint = loadCachedResource<AllYearsResponse>('boletim', s.account_id)?.nextPollHintSecs ?? 300;
    return Date.now() + hint * 1000;
  });
  const remainingPollMsRef = useRef<number>(nextPollHintSecs * 1000);
  const prevVisibleRef = useRef<boolean>(
    typeof document !== 'undefined' ? document.visibilityState === 'visible' : true
  );

  const [accountProfile, setAccountProfile] = useState<AccountProfileResponse | null>(null);
  const [rankingPrefs, setRankingPrefs] = useState<RankingPreferences | null>(null);
  const [savingRankingConsent, setSavingRankingConsent] = useState(false);
  const [fichas, setFichas] = useState<FichaAvaliacaoEntry[]>(() => {
    const s = loadStoredSession();
    if (!s) return [];
    return loadCachedResource<FichaAvaliacaoEntry[]>('ficha_list', s.account_id)?.data ?? [];
  });
  const [atrasos, setAtrasos] = useState<AtrasoRecord[]>(() => {
    const s = loadStoredSession();
    if (!s) return [];
    return loadCachedResource<AtrasoRecord[]>('atrasos', s.account_id)?.data ?? [];
  });
  const [saidas, setSaidas] = useState<SaidaRecord[]>(() => {
    const s = loadStoredSession();
    if (!s) return [];
    return loadCachedResource<SaidaRecord[]>('saidas', s.account_id)?.data ?? [];
  });
  const [homeInfo, setHomeInfo] = useState<HomeInfo | null>(() => {
    const s = loadStoredSession();
    if (!s) return null;
    return loadCachedResource<HomeInfo>('home', s.account_id)?.data ?? null;
  });

  const [isRefreshing, setIsRefreshing] = useState(false);
  const [refreshCooldownUntil, setRefreshCooldownUntil] = useState<number | null>(null);
  const [isPageVisible, setIsPageVisible] = useState(
    typeof document !== 'undefined' ? document.visibilityState === 'visible' : true
  );

  const syncInFlightRef = useRef(false);

  // Check backend health
  useEffect(() => {
    checkServerHealth().then(setServerHealthy);
  }, []);

  // Track page foreground/background visibility (`document.visibilityState`)
  useEffect(() => {
    const onVisChange = () => {
      setIsPageVisible(document.visibilityState === 'visible');
    };
    document.addEventListener('visibilitychange', onVisChange);
    return () => document.removeEventListener('visibilitychange', onVisChange);
  }, []);

  // Single-shot timeout for manual refresh anti-spam cooldown (zero 1s App re-renders)
  useEffect(() => {
    if (refreshCooldownUntil === null) return;
    const delayMs = Math.max(0, refreshCooldownUntil - Date.now());
    const timer = window.setTimeout(() => {
      setRefreshCooldownUntil(null);
    }, delayMs);
    return () => window.clearTimeout(timer);
  }, [refreshCooldownUntil]);

  useEffect(() => {
    if (!session?.token) return;
    void initializePushNotifications(session.token);
  }, [session?.token]);

  const handleLogout = useCallback(async () => {
    if (session) {
      await logoutNsacSession(session.token);
    }
    // Leaving the app level: backward shared axis Z back to the login screen.
    runScreenTransition('axis-z-backward', () => {
      setSession(null);
      setBoletim(null);
      setAccountProfile(null);
      setNextPollAt(null);
    });
  }, [session]);

  const synchronizeAllData = useCallback(
    async (forceBackendRefresh = false) => {
      if (!session || syncInFlightRef.current) return;
      syncInFlightRef.current = true;
      setIsRefreshing(true);

      try {
        const [boletimRes, profileRes, rankingPrefsRes] = await Promise.all([
          fetchBoletim(session.token, session.account_id, forceBackendRefresh),
          fetchAccountProfile(session.token).catch(() => null),
          fetchRankingPreferences(session.token).catch(() => null),
        ]);

        if (rankingPrefsRes) {
          setRankingPrefs(rankingPrefsRes);
        }

        setBoletim(boletimRes.data);
        setBoletimEtag(boletimRes.etag);
        setLastStatus(boletimRes.lastStatus);
        setCachedAt(boletimRes.cachedAt);
        setNextPollHintSecs(boletimRes.nextPollHintSecs);
        const pollDelayMs = Math.max(1000, boletimRes.nextPollHintSecs * 1000);
        remainingPollMsRef.current = pollDelayMs;
        setNextPollAt(Date.now() + pollDelayMs);

        if (profileRes) {
          setAccountProfile(profileRes);
          if (profileRes.class_code) {
            applyCourseClassCode(profileRes.class_code);
          }
        }

        // Background-hydrate auxiliary NSAC routes with ETag support
        Promise.allSettled([
          fetchFichaList(session.token, session.account_id, forceBackendRefresh).then((r) =>
            setFichas(r.data)
          ),
          fetchAtrasos(session.token, session.account_id, forceBackendRefresh).then((r) =>
            setAtrasos(r.data)
          ),
          fetchSaidas(session.token, session.account_id, forceBackendRefresh).then((r) =>
            setSaidas(r.data)
          ),
          fetchHomeInfo(session.token, session.account_id, forceBackendRefresh).then((r) =>
            setHomeInfo(r.data)
          ),
        ]);
      } catch (err) {
        if (err instanceof NsacApiClientError && err.status === 401) {
          await handleLogout();
        } else {
          const fallbackDelayMs = Math.max(1000, nextPollHintSecsRef.current * 1000);
          remainingPollMsRef.current = fallbackDelayMs;
          setNextPollAt(Date.now() + fallbackDelayMs);
        }
      } finally {
        setIsRefreshing(false);
        syncInFlightRef.current = false;
      }
    },
    [session, applyCourseClassCode, handleLogout]
  );

  // Initial Stale-While-Revalidate check on login or startup
  useEffect(() => {
    if (!session) return;
    // Hydrate from local cache in 0ms first
    const cachedBoletim = loadCachedResource<AllYearsResponse>('boletim', session.account_id);
    if (cachedBoletim) {
      setBoletim(cachedBoletim.data);
      setBoletimEtag(cachedBoletim.etag);
      setLastStatus(cachedBoletim.lastStatus);
      setCachedAt(cachedBoletim.cachedAt);
      setNextPollHintSecs(cachedBoletim.nextPollHintSecs);
      const pollDelayMs = Math.max(1000, cachedBoletim.nextPollHintSecs * 1000);
      remainingPollMsRef.current = pollDelayMs;
      setNextPollAt(Date.now() + pollDelayMs);
    }
    if (session.class_code) {
      applyCourseClassCode(session.class_code);
    }
    synchronizeAllData(false);
  }, [session, synchronizeAllData, applyCourseClassCode]);

  // Smart Sentinel single-shot scheduler (zero 1s App re-renders; pauses when tab is backgrounded)
  useEffect(() => {
    const wasVisible = prevVisibleRef.current;
    prevVisibleRef.current = isPageVisible;

    if (!session) return;

    if (!isPageVisible) {
      if (nextPollAt !== null) {
        remainingPollMsRef.current = Math.max(1000, nextPollAt - Date.now());
      }
      return;
    }

    if (!wasVisible) {
      const resumedTarget = Date.now() + Math.max(1000, remainingPollMsRef.current);
      setNextPollAt(resumedTarget);
      return;
    }

    if (nextPollAt === null) return;
    const delayMs = Math.max(1000, nextPollAt - Date.now());
    const timer = window.setTimeout(() => {
      const nextMs = Math.max(1000, nextPollHintSecs * 1000);
      remainingPollMsRef.current = nextMs;
      setNextPollAt(Date.now() + nextMs);
      void synchronizeAllData(false);
    }, delayMs);
    return () => window.clearTimeout(timer);
  }, [session, isPageVisible, nextPollAt, nextPollHintSecs, synchronizeAllData]);

  const handleManualRefresh = useCallback(
    (forceBackendRefresh = false) => {
      if (refreshCooldownUntil !== null && forceBackendRefresh) return;
      if (forceBackendRefresh) {
        setRefreshCooldownUntil(Date.now() + MANUAL_REFRESH_COOLDOWN_SECS * 1000);
      }
      void synchronizeAllData(forceBackendRefresh);
    },
    [refreshCooldownUntil, synchronizeAllData]
  );

  if (!session) {
    return (
      <Suspense
        fallback={
          <div style={{ minHeight: '100dvh', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
            <MorphingLoadingState label="Carregando..." />
          </div>
        }
      >
        <LoginScreen
          onLoginSuccess={(newSession) => {
            const entry = canRunScreenTransition() ? 'vt' : 'css';
            runScreenTransition('axis-z-forward', () => {
              setPageEntry(entry);
              setSession(newSession);
            });
          }}
          serverHealthy={serverHealthy}
        />
      </Suspense>
    );
  }

  const effectiveClassCode =
    accountProfile?.class_code ??
    session.class_code ??
    boletim?.data?.[boletim.data.length - 1]?.title ??
    null;

  return (
    <ProfileViewerProvider token={session.token}>
      <ExpressiveNavigationShell
        activeTab={activeTab}
        onSelectTab={selectTab}
        classCode={effectiveClassCode}
        nsacEmail={session.nsac_email}
        refreshCooldownUntil={refreshCooldownUntil}
        isRefreshing={isRefreshing}
        lastHttpStatus={lastStatus}
        onManualRefresh={() => handleManualRefresh(false)}
        onLogout={handleLogout}
      >
        <div
          key={activeTab}
          ref={tabSurfaceRef}
          className={
            pageEntry === 'vt'
              ? 'nsac-page-vt'
              : pageEntry === 'css'
                ? 'm3-page-transition'
                : pageEntry === 'swipe-next'
                  ? 'm3-page-swipe-next'
                  : pageEntry === 'swipe-prev'
                    ? 'm3-page-swipe-prev'
                    : undefined
          }
        >
          <Suspense fallback={<MorphingLoadingState label="Carregando tela..." />}>
            {activeTab === 'boletim' && (
              <BoletimScreen boletim={boletim} loading={isRefreshing && !boletim} />
            )}
            {activeTab === 'ranking' && (
              <RankingScreen
                token={session.token}
                accountId={session.account_id}
                classCode={effectiveClassCode}
                preferences={rankingPrefs}
                onPreferencesUpdated={setRankingPrefs}
              />
            )}
            {activeTab === 'frequencia' && (
              <FrequenciaScreen
                token={session.token}
                accountId={session.account_id}
                fichas={fichas}
                atrasos={atrasos}
                saidas={saidas}
                homeInfo={homeInfo}
              />
            )}
            {activeTab === 'sentinela' && (
              <SentinelaScreen
                token={session.token}
                accountProfile={accountProfile}
                onProfileUpdated={setAccountProfile}
                etag={boletimEtag}
                lastStatus={lastStatus}
                cachedAt={cachedAt}
                nextPollHintSecs={nextPollHintSecs}
                nextPollAt={nextPollAt}
                isPageVisible={isPageVisible}
                refreshCooldownUntil={refreshCooldownUntil}
                isRefreshing={isRefreshing}
                onManualRefresh={handleManualRefresh}
              />
            )}
            {activeTab === 'profile' && (
              <ProfileScreen
                token={session.token}
                accountId={session.account_id}
                accountProfile={accountProfile}
                detectedClassCode={effectiveClassCode}
                onLogout={handleLogout}
              />
            )}
            {activeTab === 'settings' && <AdaptiveStudioScreen detectedClassCode={effectiveClassCode} />}
          </Suspense>
        </div>
        <RankingConsentModal
          open={Boolean(rankingPrefs && !rankingPrefs.has_decided)}
          saving={savingRankingConsent}
          onDecide={async (optedIn, hideName) => {
            setSavingRankingConsent(true);
            try {
              const updated = await updateRankingPreferences(session.token, {
                opted_in: optedIn,
                hide_name: hideName,
              });
              setRankingPrefs(updated);
            } finally {
              setSavingRankingConsent(false);
            }
          }}
        />
      </ExpressiveNavigationShell>
    </ProfileViewerProvider>
  );
};

export default App;
