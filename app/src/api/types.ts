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
  /** Show my profile photo in the ranking (never while `hide_name`). */
  show_avatar: boolean;
  /** Let other ranking participants open my profile (never while `hide_name`). */
  public_profile: boolean;
  /** Findable in student search even without showing the name in the ranking. */
  discoverable: boolean;
  /** Never exposed: out of the ranking, search and profiles. */
  invisible: boolean;
  has_decided: boolean;
  updated_at: string;
}

export interface LeaderboardEntry {
  rank: number;
  account_id: number;
  is_me: boolean;
  display_label: string;
  is_anonymous: boolean;
  /** Versioned `/api/v1/profiles/{id}/avatar?v=…` path (needs the bearer token). */
  avatar_url: string | null;
  /** The requester may open this student's profile. */
  has_public_profile: boolean;
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

export type FollowEdgeStatus = 'none' | 'pending' | 'accepted';

export interface RelationshipStatus {
  outgoing: FollowEdgeStatus;
  incoming: FollowEdgeStatus;
  is_friend: boolean;
  blocked_by_me: boolean;
  blocked_me: boolean;
}

/** `GET /api/v1/students/search?q=`: a findable student (never includes the e-mail). */
export interface StudentSearchResult {
  account_id: number;
  display_label: string;
  class_code: string;
  avatar_url: string | null;
  has_public_profile: boolean;
  requires_follow_approval?: boolean;
  followers_count?: number;
  following_count?: number;
  relationship?: RelationshipStatus;
}

/** `GET /api/v1/profiles/{account_id}`: a classmate's public profile. */
export interface PublicProfile {
  account_id: number;
  is_me: boolean;
  display_label: string;
  class_code: string;
  bio: string | null;
  avatar_url: string | null;
  banner_url: string | null;
  followers_count?: number;
  following_count?: number;
  relationship?: RelationshipStatus;
}

export interface SocialUserSummary {
  account_id: number;
  display_label: string;
  class_code: string;
  avatar_url: string | null;
  has_public_profile: boolean;
  is_friend: boolean;
  outgoing: FollowEdgeStatus;
  incoming: FollowEdgeStatus;
  since: string | null;
}

export interface ConversationSummary {
  peer: SocialUserSummary;
  is_friend: boolean;
  last_message_id: number | null;
  last_message_preview: string | null;
  last_message_sender_id: number | null;
  last_message_at: string | null;
  unread_count: number;
}

export interface SocialOverview {
  friends_count: number;
  followers_count: number;
  following_count: number;
  pending_requests_count: number;
  unread_messages_count: number;
  friends: SocialUserSummary[];
  followers: SocialUserSummary[];
  following: SocialUserSummary[];
  incoming_requests: SocialUserSummary[];
  outgoing_requests: SocialUserSummary[];
  blocked: SocialUserSummary[];
  conversations: ConversationSummary[];
}

export interface FollowMutationResult {
  relationship: RelationshipStatus;
  became_friends: boolean;
  status: FollowEdgeStatus;
  peer_account_id: number;
  peer_label: string;
  peer_avatar_url: string | null;
  peer_class_code: string;
}

export interface ChatMessage {
  id: number;
  sender_id: number;
  is_me: boolean;
  text: string;
  created_at: string;
}

export interface ChatRateLimitStatus {
  limit: number;
  window_secs: number;
  remaining: number;
  retry_after_secs: number;
}

export interface ConversationMessagesPayload {
  peer: SocialUserSummary;
  is_friend: boolean;
  blocked: boolean;
  messages: ChatMessage[];
  rate_limit: ChatRateLimitStatus;
}

export interface SendMessagePayload {
  message: ChatMessage;
  rate_limit: ChatRateLimitStatus;
}
