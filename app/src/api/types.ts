export type RecoveryStatusCode = 'SAT' | 'INS' | 'NC' | 'NAC';

export type RecoveryMessage =
  | 'Satisfatório'
  | 'Insatisfatório'
  | 'Não Compareceu'
  | 'Não Aconteceu'
  | 'Não aconteceu';

export interface PersonalBiInformation {
  grade: number | null;
  absences: number;
  recovery: boolean;
  recovered?: boolean;
  recoveryCode?: RecoveryStatusCode;
  recoveryMessage?: RecoveryMessage;
}

export interface ClassBiInformation {
  averageGrade: number | null;
}

export interface UnifiedBimesterData {
  bimester: number;
  personal: PersonalBiInformation;
  class: ClassBiInformation;
}

export interface ResultData {
  grade: number | null;
  totalAbsences: number;
}

export interface FullGrades {
  subjectName: string;
  results: ResultData;
  bimesters: UnifiedBimesterData[];
}

export interface BimesterData {
  userAverage: number;
  classAverage: number;
  totalAbsences: number;
}

export interface YearInfo {
  title: string;
  year: number;
  status: string;
  grades: FullGrades[];
  bimestersMetrics: BimesterData[];
}

export interface AllYearsResponse {
  warning: boolean | string;
  userCurrentYear: number;
  data: YearInfo[];
}

export interface HomeLinkItem {
  title: string;
  description: string;
  url: string;
}

export interface HomeInfo {
  portalDate: string | null;
  availableRoutes: string[];
  infoLinks: HomeLinkItem[];
}

export interface AtrasoRecord {
  date: string;
  time: string | null;
  subject: string | null;
  reason: string | null;
  rawColumns: string[];
}

export interface SaidaRecord {
  date: string;
  time: string | null;
  reason: string | null;
  authorizedBy: string | null;
  rawColumns: string[];
}

export interface FichaAvaliacaoEntry {
  schoolYear: number | null;
  bimester: number;
  lastViewed: string | null;
  statusText: string;
  available: boolean;
  detailId: number | null;
  detailUrl: string | null;
}

export interface FichaAvaliacaoDetail {
  bimester: number;
  title: string | null;
  tables: string[][][];
  notes: string[];
}

export interface LoginResponse {
  account_id: number;
  nsac_email: string;
  class_code: string | null;
  token: string;
  expires_at: string;
}

export interface AccountProfileResponse {
  account_id: number;
  nsac_email: string;
  class_code: string | null;
  has_fcm_token: boolean;
  discord_id: string | null;
  active: boolean;
  created_at: string;
  updated_at: string;
}

export interface ApiSuccessEnvelope<T> {
  success: true;
  data: T;
  next_poll_hint_secs?: number;
}

export interface ApiErrorItem {
  code: string;
  message: string;
}

export interface ApiErrorEnvelope {
  success: false;
  errors: ApiErrorItem[];
}

export interface CachedResource<T> {
  data: T;
  etag: string | null;
  cachedAt: number;
  nextPollHintSecs: number;
  lastStatus: 200 | 304;
}

export interface BoletimApiQueryParams {
  schoolYear?: number;
  targetBimester?: number;
  subjectNameContains?: string;
  gradeGte?: number;
  gradeLt?: number;
  classAverageGte?: number;
  classAverageLt?: number;
  deltaGte?: number;
  deltaLt?: number;
  absencesGte?: number;
  absencesLte?: number;
  totalAbsencesGte?: number;
  totalAbsencesLte?: number;
  isRecovery?: boolean;
  recoveryCode?: string;
  postedOnly?: boolean;
}

export interface RankingPreferences {
  account_id: number;
  opted_in: boolean;
  hide_name: boolean;
  display_name: string | null;
  has_decided: boolean;
  updated_at: string;
}

export interface LeaderboardEntry {
  rank: number;
  account_id: number;
  is_me: boolean;
  display_label: string;
  is_anonymous: boolean;
  class_code: string;
  personal_average: number;
  class_average: number;
  delta_vs_class: number;
  posted_bimesters_count: number;
  subjects_count: number;
  total_absences: number;
}

export interface SubjectComparisonRow {
  subject_name: string;
  my_average: number | null;
  peer_average: number | null;
  class_average: number | null;
  delta_me_vs_peer: number | null;
  my_absences: number;
  peer_absences: number;
}

export interface PeerComparisonDetail {
  peer_entry: LeaderboardEntry;
  my_wins: number;
  peer_wins: number;
  ties: number;
  subjects: SubjectComparisonRow[];
}

export interface ComparisonSummaryResponse {
  scope: 'class' | 'course' | 'school';
  school_year: number;
  bimester: number | null;
  subject_filter: string | null;
  total_participants: number;
  my_preferences: RankingPreferences;
  my_rank: number | null;
  my_percentile: number | null;
  my_entry: LeaderboardEntry | null;
  leaderboard: LeaderboardEntry[];
  peer_comparison: PeerComparisonDetail | null;
}
