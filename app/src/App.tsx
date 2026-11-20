import React, { useCallback, useEffect, useRef, useState } from 'react';
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
import { AdaptiveStudioScreen } from './screens/AdaptiveStudioScreen';
import { BoletimScreen } from './screens/BoletimScreen';
import { FrequenciaScreen } from './screens/FrequenciaScreen';
import { LoginScreen } from './screens/LoginScreen';
import { RankingScreen } from './screens/RankingScreen';
import { SentinelaScreen } from './screens/SentinelaScreen';
import { useM3Theme } from './theme/ThemeContext';

const MANUAL_REFRESH_COOLDOWN_SECS = 60;

export const App: React.FC = () => {
  const { applyCourseClassCode } = useM3Theme();
  const [session, setSession] = useState<LoginResponse | null>(() => loadStoredSession());
  const [activeTab, setActiveTab] = useState<AppTabId>('boletim');
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
  const [nextPollCountdown, setNextPollCountdown] = useState<number>(300);

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
  const [refreshCooldown, setRefreshCooldown] = useState(0);
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

  // Manual refresh 60s anti-spam cooldown ticker
  useEffect(() => {
    if (refreshCooldown <= 0) return;
    const timer = window.setInterval(() => {
      setRefreshCooldown((prev) => Math.max(0, prev - 1));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [refreshCooldown]);

  const handleLogout = useCallback(async () => {
    if (session) {
      await logoutNsacSession(session.token);
    }
    setSession(null);
    setBoletim(null);
    setAccountProfile(null);
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
        setNextPollCountdown(boletimRes.nextPollHintSecs);

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
      setNextPollCountdown(cachedBoletim.nextPollHintSecs);
    }
    if (session.class_code) {
      applyCourseClassCode(session.class_code);
    }
    synchronizeAllData(false);
  }, [session, synchronizeAllData, applyCourseClassCode]);

  // Smart Sentinel Countdown that pauses when app/tab is in background (`!isPageVisible`)
  useEffect(() => {
    if (!session || !isPageVisible) return;
    const timer = window.setInterval(() => {
      setNextPollCountdown((prev) => {
        if (prev <= 1) {
          synchronizeAllData(false);
          return nextPollHintSecs;
        }
        return prev - 1;
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, [session, isPageVisible, nextPollHintSecs, synchronizeAllData]);

  const handleManualRefresh = (forceBackendRefresh = false) => {
    if (refreshCooldown > 0 && forceBackendRefresh) return;
    if (forceBackendRefresh) {
      setRefreshCooldown(MANUAL_REFRESH_COOLDOWN_SECS);
    }
    synchronizeAllData(forceBackendRefresh);
  };

  if (!session) {
    return (
      <LoginScreen
        onLoginSuccess={(newSession) => setSession(newSession)}
        serverHealthy={serverHealthy}
      />
    );
  }

  const effectiveClassCode =
    accountProfile?.class_code ??
    session.class_code ??
    boletim?.data?.[boletim.data.length - 1]?.title ??
    null;

  return (
    <ExpressiveNavigationShell
      activeTab={activeTab}
      onSelectTab={setActiveTab}
      classCode={effectiveClassCode}
      nsacEmail={session.nsac_email}
      nextPollCountdown={nextPollCountdown}
      refreshCooldown={refreshCooldown}
      isRefreshing={isRefreshing}
      lastHttpStatus={lastStatus}
      onManualRefresh={() => handleManualRefresh(false)}
      onLogout={handleLogout}
    >
      <div key={activeTab} className="m3-page-transition">
        {activeTab === 'boletim' && (
          <BoletimScreen boletim={boletim} loading={isRefreshing && !boletim} />
        )}
        {activeTab === 'ranking' && (
          <RankingScreen
            token={session.token}
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
            nextPollCountdown={nextPollCountdown}
            isPageVisible={isPageVisible}
            refreshCooldown={refreshCooldown}
            isRefreshing={isRefreshing}
            onManualRefresh={handleManualRefresh}
          />
        )}
        {activeTab === 'studio' && (
          <AdaptiveStudioScreen detectedClassCode={effectiveClassCode} />
        )}
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
  );
};

export default App;
