import { Capacitor } from '@capacitor/core';
import { PushNotifications } from '@capacitor/push-notifications';
import { updateAccountFcmToken } from '../api/nsacClient';

let listenersBound = false;
let latestAuthToken: string | null = null;
let pendingResolve: ((token: string) => void) | null = null;
let pendingReject: ((err: Error) => void) | null = null;

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

  void PushNotifications.addListener('pushNotificationReceived', (notification) => {
    console.info('[n2sac-push] Foreground push received:', notification.title);
  });
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
