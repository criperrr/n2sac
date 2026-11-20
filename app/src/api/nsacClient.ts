import type {
  AccountProfileResponse,
  AllYearsResponse,
  ApiErrorEnvelope,
  ApiSuccessEnvelope,
  AtrasoRecord,
  BoletimApiQueryParams,
  CachedResource,
  ComparisonSummaryResponse,
  FichaAvaliacaoDetail,
  FichaAvaliacaoEntry,
  HomeInfo,
  LoginResponse,
  RankingPreferences,
  SaidaRecord,
} from './types';

const SESSION_STORAGE_KEY = 'nsac_expressive_session_v2';
const CACHE_PREFIX = 'nsac_expressive_cache_v2:';

export class NsacApiClientError extends Error {
  public readonly code: string;
  public readonly status: number;

  constructor(code: string, message: string, status: number) {
    super(message);
    this.name = 'NsacApiClientError';
    this.code = code;
    this.status = status;
  }
}

const REFRESH_LIMIT_KEY = 'n2sac_boletim_refresh_ts_v1';
const REFRESH_WINDOW_MS = 5 * 60 * 1000; // 5 minutes
const REFRESH_MAX_COUNT = 10; // 10 requests per 5-minute sliding window

/**
 * Frontend sliding-window rate limiter: max 10 manual refreshes per 5 minutes.
 * Throws NsacApiClientError(429) if exceeded, mirroring the backend limiter.
 */
export function consumeManualRefreshQuota(): { remaining: number } {
  const now = Date.now();
  let timestamps: number[] = [];
  try {
    const raw = localStorage.getItem(REFRESH_LIMIT_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        timestamps = parsed.filter((t): t is number => typeof t === 'number' && now - t < REFRESH_WINDOW_MS);
      }
    }
  } catch {
    timestamps = [];
  }

  if (timestamps.length >= REFRESH_MAX_COUNT) {
    const oldest = timestamps[0] ?? now;
    const retrySecs = Math.max(1, Math.ceil((REFRESH_WINDOW_MS - (now - oldest)) / 1000));
    throw new NsacApiClientError(
      'RATE_LIMITED',
      `Limite de 10 atualizações a cada 5 minutos atingido. Aguarde ${retrySecs}s.`,
      429
    );
  }

  timestamps.push(now);
  try {
    localStorage.setItem(REFRESH_LIMIT_KEY, JSON.stringify(timestamps));
  } catch {
    // ignore storage quota errors
  }

  return { remaining: Math.max(0, REFRESH_MAX_COUNT - timestamps.length) };
}

export function getApiBaseUrl(): string {
  const envUrl = import.meta.env.VITE_API_BASE_URL as string | undefined;
  if (envUrl && envUrl.trim().length > 0) {
    return envUrl.trim().replace(/\/+$/, '');
  }
  // When running inside Capacitor Android/iOS WebView (origin is http://localhost with no Vite proxy),
  // default to our permanent public HTTPS Tailscale Funnel endpoint.
  if (
    typeof window !== 'undefined' &&
    (window.location.origin === 'http://localhost' ||
      window.location.origin === 'https://localhost' ||
      window.location.protocol === 'capacitor:')
  ) {
    return 'https://notebook-server.tailddd6bf.ts.net';
  }
  return '';
}

export function loadStoredSession(): LoginResponse | null {
  try {
    const raw = localStorage.getItem(SESSION_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as LoginResponse;
    if (!parsed.token || !parsed.nsac_email) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function saveStoredSession(session: LoginResponse | null): void {
  try {
    if (!session) {
      localStorage.removeItem(SESSION_STORAGE_KEY);
    } else {
      localStorage.setItem(SESSION_STORAGE_KEY, JSON.stringify(session));
    }
  } catch {
    // ignore storage errors
  }
}

export function loadCachedResource<T>(routeKey: string, accountId: number): CachedResource<T> | null {
  try {
    const raw = localStorage.getItem(`${CACHE_PREFIX}${accountId}:${routeKey}`);
    if (!raw) return null;
    return JSON.parse(raw) as CachedResource<T>;
  } catch {
    return null;
  }
}

export function saveCachedResource<T>(
  routeKey: string,
  accountId: number,
  resource: CachedResource<T>
): void {
  try {
    localStorage.setItem(`${CACHE_PREFIX}${accountId}:${routeKey}`, JSON.stringify(resource));
  } catch {
    // ignore quota errors
  }
}

export function clearAccountCache(accountId: number): void {
  try {
    const prefix = `${CACHE_PREFIX}${accountId}:`;
    const keysToRemove: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && k.startsWith(prefix)) {
        keysToRemove.push(k);
      }
    }
    keysToRemove.forEach((k) => localStorage.removeItem(k));
  } catch {
    // ignore
  }
}

async function parseErrorResponse(res: Response): Promise<NsacApiClientError> {
  try {
    const body = (await res.json()) as ApiErrorEnvelope;
    const first = body.errors?.[0];
    if (first) {
      return new NsacApiClientError(first.code, first.message, res.status);
    }
  } catch {
    // Fallback if non-JSON error
  }
  return new NsacApiClientError(
    `HTTP_${res.status}`,
    res.status === 502 || res.status === 504
      ? 'O servidor do NSAC ou a API local está temporariamente indisponível.'
      : `Erro de comunicação HTTP (${res.status}).`,
    res.status
  );
}

/**
 * Zero-Trust Login (`POST /api/v1/auth/login`)
 * Sends ONLY institutional email and password. Class code, school years, and subjects
 * are derived 100% by the Rust backend directly from the authenticated NSAC session.
 */
export async function loginNsacZeroTrust(
  nsacEmail: string,
  nsacPassword: string
): Promise<LoginResponse> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      nsac_email: nsacEmail.trim(),
      nsac_password: nsacPassword,
    }),
  });

  if (!res.ok) {
    throw await parseErrorResponse(res);
  }

  const envelope = (await res.json()) as ApiSuccessEnvelope<LoginResponse>;
  saveStoredSession(envelope.data);
  return envelope.data;
}

export async function logoutNsacSession(token: string): Promise<void> {
  const base = getApiBaseUrl();
  try {
    await fetch(`${base}/api/v1/auth/logout`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
      },
    });
  } finally {
    saveStoredSession(null);
  }
}

/**
 * ETag-aware + Anti-Spam `next_poll_hint_secs` fetcher for all `/api/v1/nsac/*` routes.
 * Sends `If-None-Match` when a cached ETag exists and seamlessly handles `304 Not Modified`
 * with zero JSON re-parsing overhead.
 */
export async function fetchWithEtagCache<T>(options: {
  path: string;
  routeKey: string;
  token: string;
  accountId: number;
  forceRefresh?: boolean;
}): Promise<CachedResource<T>> {
  const base = getApiBaseUrl();
  const existing = loadCachedResource<T>(options.routeKey, options.accountId);

  const headers: Record<string, string> = {
    Authorization: `Bearer ${options.token}`,
  };

  if (!options.forceRefresh && existing?.etag) {
    headers['If-None-Match'] = existing.etag;
  }

  const url = options.forceRefresh
    ? `${base}${options.path}${options.path.includes('?') ? '&' : '?'}refresh=true`
    : `${base}${options.path}`;

  const res = await fetch(url, {
    method: 'GET',
    headers,
  });

  if (res.status === 304 && existing) {
    const updated: CachedResource<T> = {
      ...existing,
      etag: res.headers.get('etag') ?? existing.etag,
      cachedAt: Date.now(),
      lastStatus: 304,
    };
    saveCachedResource(options.routeKey, options.accountId, updated);
    return updated;
  }

  if (!res.ok) {
    throw await parseErrorResponse(res);
  }

  const etag = res.headers.get('etag');
  const envelope = (await res.json()) as ApiSuccessEnvelope<T>;
  const resource: CachedResource<T> = {
    data: envelope.data,
    etag: etag ?? existing?.etag ?? null,
    cachedAt: Date.now(),
    nextPollHintSecs: envelope.next_poll_hint_secs ?? existing?.nextPollHintSecs ?? 300,
    lastStatus: 200,
  };
  saveCachedResource(options.routeKey, options.accountId, resource);
  return resource;
}

export function buildBoletimFilterQueryString(params: BoletimApiQueryParams): string {
  const sp = new URLSearchParams();
  if (params.schoolYear !== undefined) sp.set('schoolYear[eq]', String(params.schoolYear));
  if (params.targetBimester !== undefined) sp.set('targetBimester[eq]', String(params.targetBimester));
  if (params.subjectNameContains && params.subjectNameContains.trim()) {
    sp.set('subjectName[contains]', params.subjectNameContains.trim());
  }
  if (params.gradeGte !== undefined) sp.set('grade[gte]', String(params.gradeGte));
  if (params.gradeLt !== undefined) sp.set('grade[lt]', String(params.gradeLt));
  if (params.classAverageGte !== undefined) sp.set('classAverage[gte]', String(params.classAverageGte));
  if (params.classAverageLt !== undefined) sp.set('classAverage[lt]', String(params.classAverageLt));
  if (params.deltaGte !== undefined) sp.set('delta[gte]', String(params.deltaGte));
  if (params.deltaLt !== undefined) sp.set('delta[lt]', String(params.deltaLt));
  if (params.absencesGte !== undefined) sp.set('absences[gte]', String(params.absencesGte));
  if (params.absencesLte !== undefined) sp.set('absences[lte]', String(params.absencesLte));
  if (params.totalAbsencesGte !== undefined) {
    sp.set('totalAbsences[gte]', String(params.totalAbsencesGte));
  }
  if (params.totalAbsencesLte !== undefined) {
    sp.set('totalAbsences[lte]', String(params.totalAbsencesLte));
  }
  if (params.isRecovery !== undefined) sp.set('isRecovery[eq]', String(params.isRecovery));
  if (params.recoveryCode && params.recoveryCode.trim()) {
    sp.set('recoveryCode[eq]', params.recoveryCode.trim());
  }
  if (params.postedOnly !== undefined) sp.set('postedOnly[eq]', String(params.postedOnly));
  return sp.toString();
}

export async function fetchBoletim(
  token: string,
  accountId: number,
  forceRefresh = false,
  filterParams?: BoletimApiQueryParams
): Promise<CachedResource<AllYearsResponse>> {
  if (forceRefresh) {
    consumeManualRefreshQuota();
  }
  const qs = filterParams ? buildBoletimFilterQueryString(filterParams) : '';
  const refreshParam = forceRefresh ? (qs ? '&refresh=true' : 'refresh=true') : '';
  const fullQs = `${qs}${refreshParam}`;
  const path = fullQs ? `/api/v1/nsac/boletim?${fullQs}` : '/api/v1/nsac/boletim';
  const routeKey = qs ? `boletim:${qs}` : 'boletim';
  return fetchWithEtagCache<AllYearsResponse>({
    path,
    routeKey,
    token,
    accountId,
    forceRefresh,
  });
}

export async function fetchHomeInfo(
  token: string,
  accountId: number,
  forceRefresh = false
): Promise<CachedResource<HomeInfo>> {
  return fetchWithEtagCache<HomeInfo>({
    path: '/api/v1/nsac/home',
    routeKey: 'home',
    token,
    accountId,
    forceRefresh,
  });
}

export async function fetchAtrasos(
  token: string,
  accountId: number,
  forceRefresh = false
): Promise<CachedResource<AtrasoRecord[]>> {
  return fetchWithEtagCache<AtrasoRecord[]>({
    path: '/api/v1/nsac/atrasos',
    routeKey: 'atrasos',
    token,
    accountId,
    forceRefresh,
  });
}

export async function fetchSaidas(
  token: string,
  accountId: number,
  forceRefresh = false
): Promise<CachedResource<SaidaRecord[]>> {
  return fetchWithEtagCache<SaidaRecord[]>({
    path: '/api/v1/nsac/saidas',
    routeKey: 'saidas',
    token,
    accountId,
    forceRefresh,
  });
}

export async function fetchFichaList(
  token: string,
  accountId: number,
  forceRefresh = false
): Promise<CachedResource<FichaAvaliacaoEntry[]>> {
  return fetchWithEtagCache<FichaAvaliacaoEntry[]>({
    path: '/api/v1/nsac/ficha-avaliacao',
    routeKey: 'ficha_list',
    token,
    accountId,
    forceRefresh,
  });
}

export async function fetchFichaDetail(
  token: string,
  accountId: number,
  bimesterId: number,
  forceRefresh = false
): Promise<CachedResource<FichaAvaliacaoDetail>> {
  return fetchWithEtagCache<FichaAvaliacaoDetail>({
    path: `/api/v1/nsac/ficha-avaliacao/${bimesterId}`,
    routeKey: `ficha_detail_${bimesterId}`,
    token,
    accountId,
    forceRefresh,
  });
}

export async function fetchAccountProfile(token: string): Promise<AccountProfileResponse> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw await parseErrorResponse(res);
  const env = (await res.json()) as ApiSuccessEnvelope<AccountProfileResponse>;
  return env.data;
}

export async function updateAccountFcmToken(
  token: string,
  fcmToken: string
): Promise<AccountProfileResponse> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/fcm-token`, {
    method: 'PUT',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ fcm_token: fcmToken.trim() }),
  });
  if (!res.ok) throw await parseErrorResponse(res);
  const env = (await res.json()) as ApiSuccessEnvelope<AccountProfileResponse>;
  return env.data;
}

export async function deleteAccountFcmToken(
  token: string
): Promise<{ cleared: boolean; profile?: AccountProfileResponse }> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/fcm-token`, {
    method: 'DELETE',
    headers: {
      Authorization: `Bearer ${token}`,
    },
  });
  if (res.ok) {
    const env = (await res.json()) as ApiSuccessEnvelope<AccountProfileResponse>;
    return { cleared: true, profile: env.data };
  }
  // If server only exposes PUT /account/fcm-token, clear local device registration gracefully
  return { cleared: true };
}

export interface UserProfileData {
  account_id: number;
  first_name: string;
  display_name: string | null;
  bio: string | null;
  avatar_data_url: string | null;
  banner_data_url: string | null;
}

export async function fetchUserProfile(token: string): Promise<UserProfileData> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/profile`, {
    method: 'GET',
    headers: { Authorization: `Bearer ${token}` },
  });
  const body = (await res.json()) as { success?: boolean; data?: UserProfileData; error?: { code?: string; message?: string } };
  if (!res.ok || !body?.success || !body.data) {
    throw new NsacApiClientError(
      body?.error?.code ?? 'PROFILE_ERROR',
      body?.error?.message ?? 'Não foi possível carregar o perfil.',
      res.status
    );
  }
  return body.data;
}

export async function updateUserProfile(
  token: string,
  payload: {
    display_name?: string | null;
    bio?: string | null;
    avatar_data_url?: string | null;
    banner_data_url?: string | null;
  }
): Promise<UserProfileData> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/profile`, {
    method: 'PUT',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(payload),
  });
  const body = (await res.json()) as { success?: boolean; data?: UserProfileData; error?: { code?: string; message?: string } };
  if (!res.ok || !body?.success || !body.data) {
    throw new NsacApiClientError(
      body?.error?.code ?? 'PROFILE_ERROR',
      body?.error?.message ?? 'Não foi possível salvar o perfil.',
      res.status
    );
  }
  return body.data;
}

export async function updateAccountDiscordLink(
  token: string,
  discordId: string
): Promise<AccountProfileResponse> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/discord-link`, {
    method: 'PUT',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ discord_id: discordId.trim() }),
  });
  if (!res.ok) throw await parseErrorResponse(res);
  const env = (await res.json()) as ApiSuccessEnvelope<AccountProfileResponse>;
  return env.data;
}

export async function checkServerHealth(): Promise<boolean> {
  const base = getApiBaseUrl();
  try {
    const res = await fetch(`${base}/api/v1/health`);
    return res.ok;
  } catch {
    return false;
  }
}

export async function fetchRankingPreferences(token: string): Promise<RankingPreferences> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/ranking-preferences`, {
    method: 'GET',
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw await parseErrorResponse(res);
  const env = (await res.json()) as ApiSuccessEnvelope<RankingPreferences>;
  return env.data;
}

export async function updateRankingPreferences(
  token: string,
  payload: { opted_in: boolean; hide_name: boolean; display_name?: string | null }
): Promise<RankingPreferences> {
  const base = getApiBaseUrl();
  const res = await fetch(`${base}/api/v1/account/ranking-preferences`, {
    method: 'PUT',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(payload),
  });
  if (!res.ok) throw await parseErrorResponse(res);
  const env = (await res.json()) as ApiSuccessEnvelope<RankingPreferences>;
  return env.data;
}

export async function fetchCompareLeaderboard(
  token: string,
  params: {
    scope?: 'class' | 'course' | 'school';
    year?: number;
    bimester?: number | null;
    subject?: string;
    peerId?: number | null;
  } = {}
): Promise<ComparisonSummaryResponse> {
  const base = getApiBaseUrl();
  const sp = new URLSearchParams();
  if (params.scope) sp.set('scope', params.scope);
  if (params.year) sp.set('year', String(params.year));
  if (params.bimester) sp.set('bimester', String(params.bimester));
  if (params.subject && params.subject.trim()) sp.set('subject', params.subject.trim());
  if (params.peerId) sp.set('peer_id', String(params.peerId));
  const qs = sp.toString();
  const res = await fetch(`${base}/api/v1/compare${qs ? `?${qs}` : ''}`, {
    method: 'GET',
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw await parseErrorResponse(res);
  const env = (await res.json()) as ApiSuccessEnvelope<ComparisonSummaryResponse>;
  return env.data;
}
