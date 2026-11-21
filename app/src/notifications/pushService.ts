import { Capacitor } from '@capacitor/core';
import { LocalNotifications } from '@capacitor/local-notifications';
import { PushNotifications } from '@capacitor/push-notifications';
import { updateAccountFcmToken } from '../api/nsacClient';

export type SocialPushKind =
  | 'chat_message'
  | 'follow'
  | 'follow_request'
  | 'follow_accepted'
  | 'friendship';

export interface SocialPushEventDetail {
  kind: SocialPushKind;
  actorId: number | null;
  actorLabel: string;
  title: string;
  body: string;
}

const SOCIAL_CHANNELS = [
  {
    id: 'n2sac-social-messages',
    name: 'Mensagens de amigos',
    description: 'Avisos de novas mensagens recebidas no chat do n²sac',
    importance: 4 as const,
    sound: 'default',
    vibration: true,
  },
  {
    id: 'n2sac-social-follows',
    name: 'Novos seguidores',
    description: 'Avisos quando alguém segue você ou envia solicitação',
    importance: 4 as const,
    sound: 'default',
    vibration: true,
  },
  {
    id: 'n2sac-social-friends',
    name: 'Amizades conquistadas',
    description: 'Avisos quando uma nova amizade mútua é formada no n²sac',
    importance: 4 as const,
    sound: 'default',
    vibration: true,
  },
  {
    id: 'n2sac-grades',
    name: 'Notas do NSAC',
    description: 'Avisos instantâneos quando novas notas são lançadas',
    importance: 4 as const,
    sound: 'default',
    vibration: true,
  },
];

let listenersBound = false;
let channelsCreated = false;
let latestAuthToken: string | null = null;
let pendingResolve: ((token: string) => void) | null = null;
let pendingReject: ((err: Error) => void) | null = null;

function channelForKind(kind?: string): string {
  switch (kind) {
    case 'chat_message':
      return 'n2sac-social-messages';
    case 'friendship':
      return 'n2sac-social-friends';
    case 'follow':
    case 'follow_request':
    case 'follow_accepted':
      return 'n2sac-social-follows';
    default:
      return 'n2sac-grades';
  }
}

function isSocialKind(kind: unknown): kind is SocialPushKind {
  return (
    kind === 'chat_message' ||
    kind === 'follow' ||
    kind === 'follow_request' ||
    kind === 'follow_accepted' ||
    kind === 'friendship'
  );
}

function parseSocialDetail(
  data: Record<string, unknown> | undefined,
  title?: string,
  body?: string
): SocialPushEventDetail | null {
  const kind = data?.kind;
  if (!isSocialKind(kind)) return null;
  const rawId = data?.actor_id ?? data?.actorId;
  const parsedId = typeof rawId === 'number' ? rawId : Number.parseInt(String(rawId ?? ''), 10);
  const actorId = Number.isFinite(parsedId) && parsedId > 0 ? parsedId : null;
  const actorLabel =
    typeof data?.actor_label === 'string' && data.actor_label.trim()
      ? data.actor_label.trim()
      : title?.trim() || 'Colega';
  return {
    kind,
    actorId,
    actorLabel,
    title: title?.trim() || 'n²sac',
    body: body?.trim() || '',
  };
}

export async function ensureNotificationChannels(): Promise<void> {
  if (channelsCreated || !Capacitor.isNativePlatform()) return;
  channelsCreated = true;
  for (const ch of SOCIAL_CHANNELS) {
    try {
      await LocalNotifications.createChannel(ch);
    } catch {
      // ignore if channel already exists or unsupported
    }
    try {
      await PushNotifications.createChannel(ch);
    } catch {
      // ignore if unsupported
    }
  }
}

function ensureListeners() {
  if (listenersBound) return;
  listenersBound = true;

  void PushNotifications.addListener('registration', async (token) => {
    const fcmToken = token.value?.trim();
    if (!fcmToken) {
      pendingReject?.(new Error('Token FCM vazio retornado pelo Android.'));
      pendingResolve = null;
      pendingReject = null;
      return;
    }

    try {
      if (latestAuthToken) {
        await updateAccountFcmToken(latestAuthToken, fcmToken);
      }
      pendingResolve?.(fcmToken);
    } catch (err) {
      pendingReject?.(err instanceof Error ? err : new Error(String(err)));
    } finally {
      pendingResolve = null;
      pendingReject = null;
    }
  });

  void PushNotifications.addListener('registrationError', (err) => {
    const message =
      typeof err === 'object' && err && 'error' in err
        ? String((err as { error: unknown }).error)
        : 'Falha ao registrar no Firebase Cloud Messaging.';
    pendingReject?.(new Error(message));
    pendingResolve = null;
    pendingReject = null;
  });

  void PushNotifications.addListener('pushNotificationReceived', async (notification) => {
    const data = notification.data as Record<string, unknown> | undefined;
    const title = notification.title || 'n²sac';
    const body = notification.body || '';
    const detail = parseSocialDetail(data, title, body);

    if (detail) {
      window.dispatchEvent(new CustomEvent<SocialPushEventDetail>('n2sac:social-push', { detail }));
    }

    // Android suppresses system banners for FCM pushes while the app is in foreground;
    // schedule an immediate LocalNotification on the matching high-importance channel.
    if (Capacitor.isNativePlatform() && (title || body)) {
      try {
        await ensureNotificationChannels();
        const id = Math.floor(Date.now() % 2_000_000_000);
        await LocalNotifications.schedule({
          notifications: [
            {
              id,
              title,
              body,
              channelId: channelForKind(typeof data?.kind === 'string' ? data.kind : undefined),
              extra: data ?? {},
              schedule: { at: new Date(Date.now() + 50) },
            },
          ],
        });
      } catch {
        // Fallback: in-app toast already dispatched above.
      }
    }
  });

  void PushNotifications.addListener('pushNotificationActionPerformed', (action) => {
    const data = action.notification.data as Record<string, unknown> | undefined;
    const detail = parseSocialDetail(data, action.notification.title, action.notification.body);
    if (detail) {
      window.dispatchEvent(new CustomEvent<SocialPushEventDetail>('n2sac:social-push-open', { detail }));
    }
  });

  void LocalNotifications.addListener('localNotificationActionPerformed', (action) => {
    const data = action.notification.extra as Record<string, unknown> | undefined;
    const detail = parseSocialDetail(data, action.notification.title, action.notification.body);
    if (detail) {
      window.dispatchEvent(new CustomEvent<SocialPushEventDetail>('n2sac:social-push-open', { detail }));
    }
  });
}

/**
 * Initializes Android notification channels, binds foreground/tap listeners, and silently
 * refreshes the FCM token on boot whenever the user has already granted push permission.
 */
export async function initializePushNotifications(authToken: string): Promise<void> {
  latestAuthToken = authToken;
  if (!Capacitor.isNativePlatform()) return;

  ensureListeners();
  await ensureNotificationChannels();

  try {
    const perm = await PushNotifications.checkPermissions();
    if (perm.receive === 'granted') {
      await PushNotifications.register();
    }
  } catch {
    // Non-fatal if Play Services is temporarily unavailable.
  }
}

/**
 * Requests Android 13+ POST_NOTIFICATIONS permission, registers with Firebase FCM,
 * and uploads the real device FCM token to `PUT /api/v1/account/fcm-token`.
 */
export async function registerNativeFcmPush(authToken: string): Promise<string> {
  latestAuthToken = authToken;

  if (!Capacitor.isNativePlatform()) {
    throw new Error('Notificações push nativas FCM só funcionam no aplicativo Android.');
  }

  ensureListeners();
  await ensureNotificationChannels();

  let perm = await PushNotifications.checkPermissions();
  if (perm.receive === 'prompt' || perm.receive === 'prompt-with-rationale') {
    perm = await PushNotifications.requestPermissions();
  }
  if (perm.receive !== 'granted') {
    throw new Error('Permissão de notificações negada no Android. Ative nas configurações do app.');
  }

  return new Promise<string>((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      pendingResolve = null;
      pendingReject = null;
      reject(new Error('Tempo esgotado aguardando token FCM do Google Play Services.'));
    }, 15_000);

    pendingResolve = (tok) => {
      window.clearTimeout(timeout);
      resolve(tok);
    };
    pendingReject = (err) => {
      window.clearTimeout(timeout);
      reject(err);
    };

    void PushNotifications.register();
  });
}
