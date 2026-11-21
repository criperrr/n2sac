import React, { useState } from 'react';
import {
  Bell,
  BellOff,
  MessageSquare,
  CheckCircle2,
  Sparkles,
  Hash,
} from 'lucide-react';
import {
  deleteAccountFcmToken,
  NsacApiClientError,
  updateAccountDiscordLink,
  updateAccountFcmToken,
} from '../api/nsacClient';
import { Capacitor } from '@capacitor/core';
import { registerNativeFcmPush } from '../notifications/pushService';
import type { AccountProfileResponse } from '../api/types';
import { M3_SPRINGS, useSpringValue } from '../theme/springPhysics';
import { useM3Theme } from '../theme/ThemeContext';
import {
  AnimatedNumber,
  EnhancedMorphButton,
  ExpressiveSegmentedCard,
  IconShapeContainer,
} from '../components/expressive/ExpressivePrimitives';
import { MorphingShapeIndicator } from '../components/expressive/ShapeMorph';
import { useMinimumBusy } from '../theme/motion';
import { PollCountdownLabel } from '../components/PollCountdown';

export const SentinelaScreen: React.FC<{
  token: string;
  accountProfile: AccountProfileResponse | null;
  onProfileUpdated: (profile: AccountProfileResponse) => void;
  etag: string | null;
  lastStatus: 200 | 304 | null;
  cachedAt: number | null;
  nextPollHintSecs: number;
  nextPollAt: number | null;
  isPageVisible: boolean;
  refreshCooldownUntil: number | null;
  isRefreshing: boolean;
  onManualRefresh: (forceBackendRefresh: boolean) => void;
}> = ({
  token,
  accountProfile,
  onProfileUpdated,
  cachedAt,
  nextPollHintSecs,
  nextPollAt,
  isPageVisible,
  isRefreshing,
}) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const [fcmInput, setFcmInput] = useState('');
  const [discordInput, setDiscordInput] = useState(accountProfile?.discord_id ?? '');
  const [savingFcm, setSavingFcm] = useState(false);
  const showSavingFcm = useMinimumBusy(savingFcm);
  const [savingDiscord, setSavingDiscord] = useState(false);
  const [statusBanner, setStatusBanner] = useState<{
    type: 'success' | 'error';
    message: string;
  } | null>(null);

  const activityState =
    nextPollHintSecs <= 60
      ? {
          mode: 'Verificação Intensa',
          certainty: 94,
          description:
            'Lançamento recente de notas detectado na sua turma. As verificações estão mais frequentes.',
          bg: tokens.gradeWarningContainer,
          fg: tokens.onGradeWarningContainer,
        }
      : nextPollHintSecs <= 180
        ? {
            mode: 'Período de Lançamento',
            certainty: 76,
            description:
              'Período típico de publicação de notas no CTI.',
            bg: tokens.primaryContainer,
            fg: tokens.onPrimaryContainer,
          }
        : {
            mode: 'Monitoramento Automático',
            certainty: 34,
            description:
              'Monitoramento automático ativo para avisar quando saírem novas notas na sua turma.',
            bg: tokens.gradeSuccessContainer,
            fg: tokens.onGradeSuccessContainer,
          };

  const animatedCertainty = useSpringValue(activityState.certainty, M3_SPRINGS.numberGauge);

  const handleRegisterFcm = async (customToken?: string) => {
    setSavingFcm(true);
    setStatusBanner(null);
    try {
      let targetToken = customToken ?? fcmInput.trim();
      if (!targetToken && Capacitor.isNativePlatform()) {
        targetToken = await registerNativeFcmPush(token);
      }
      if (!targetToken) {
        throw new Error('Abra o aplicativo Android para ativar notificações push nativas (FCM).');
      }
      const updated = await updateAccountFcmToken(token, targetToken);
      onProfileUpdated(updated);
      setFcmInput(targetToken);
      setStatusBanner({
        type: 'success',
        message: 'Notificações nativas (Firebase FCM) ativadas para a sua turma!',
      });
    } catch (err) {
      setStatusBanner({
        type: 'error',
        message:
          err instanceof Error
            ? err.message
            : 'Não foi possível ativar as notificações.',
      });
    } finally {
      setSavingFcm(false);
    }
  };

  const handleDeleteFcm = async () => {
    setSavingFcm(true);
    setStatusBanner(null);
    try {
      const res = await deleteAccountFcmToken(token);
      if (res.profile) {
        onProfileUpdated(res.profile);
      } else if (accountProfile) {
        onProfileUpdated({ ...accountProfile, has_fcm_token: false });
      }
      setFcmInput('');
      setStatusBanner({
        type: 'success',
        message: 'Notificações desativadas neste dispositivo.',
      });
    } catch (err) {
      setStatusBanner({
        type: 'error',
        message: err instanceof Error ? err.message : 'Erro ao desativar notificações.',
      });
    } finally {
      setSavingFcm(false);
    }
  };

  const handleLinkDiscord = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!discordInput.trim()) return;
    setSavingDiscord(true);
    setStatusBanner(null);
    try {
      const updated = await updateAccountDiscordLink(token, discordInput.trim());
      onProfileUpdated(updated);
      setStatusBanner({
        type: 'success',
        message: `Conta do Discord (${updated.discord_id}) vinculada com sucesso!`,
      });
    } catch (err) {
      setStatusBanner({
        type: 'error',
        message:
          err instanceof NsacApiClientError
            ? err.message
            : 'Não foi possível vincular o Discord.',
      });
    } finally {
      setSavingDiscord(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: isMaterialYou ? 18 : 14 }}>
      {/* 1. MONITORING HERO CARD */}
      <section
        style={{
          borderRadius: isMaterialYou ? 32 : 12,
          padding: isMaterialYou ? '24px 22px' : '18px 20px',
          backgroundColor: tokens.surfaceContainerLow,
          border: `1px solid ${tokens.outlineVariant}`,
        }}
      >
        <div
          style={{
            display: 'flex',
            flexWrap: 'wrap',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 12,
            marginBottom: 18,
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', gap: 14 }}>
            <IconShapeContainer size={isMaterialYou ? 52 : 42} active={isRefreshing}>
              <Bell size={isMaterialYou ? 25 : 20} />
            </IconShapeContainer>
            <div>
              <div style={{ fontSize: '0.73rem', fontWeight: 700, color: tokens.primary, textTransform: 'uppercase' }}>
                Turma {accountProfile?.class_code ?? 'CTI'}
              </div>
              <h1 style={{ margin: 0, fontSize: isMaterialYou ? '1.55rem' : '1.4rem', fontWeight: 800, letterSpacing: '-0.03em' }}>
                Alertas & Conta
              </h1>
            </div>
          </div>

          <span
            style={{
              padding: isMaterialYou ? '6px 14px' : '4px 10px',
              borderRadius: isMaterialYou ? 999 : 6,
              fontSize: '0.74rem',
              fontWeight: 700,
              backgroundColor: activityState.bg,
              color: activityState.fg,
            }}
          >
            {activityState.mode}
          </span>
        </div>

        <p style={{ margin: '0 0 18px', fontSize: '0.88rem', color: tokens.onSurfaceVariant, lineHeight: 1.5 }}>
          {activityState.description}
        </p>

        <div
          style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))',
            gap: 12,
          }}
        >
          <div
            style={{
              padding: '16px 18px',
              borderRadius: 24,
              backgroundColor: tokens.surfaceContainerHigh,
              border: `1px solid ${tokens.outlineVariant}`,
            }}
          >
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.76rem', fontWeight: 700 }}>
              <span>PROBABILIDADE DE LANÇAMENTO</span>
              <span style={{ color: tokens.primary }}>
                <AnimatedNumber value={activityState.certainty} decimals={0} suffix="%" />
              </span>
            </div>
            <div
              style={{
                height: 12,
                borderRadius: 999,
                backgroundColor: tokens.surfaceContainerLowest,
                marginTop: 10,
                overflow: 'hidden',
              }}
            >
              <div
                style={{
                  width: `${animatedCertainty.toFixed(1)}%`,
                  height: '100%',
                  borderRadius: 999,
                  backgroundColor: tokens.primary,
                }}
              />
            </div>
            <div style={{ fontSize: '0.74rem', color: tokens.onSurfaceVariant, marginTop: 8 }}>
              Estimativa baseada no histórico de lançamentos da turma
            </div>
          </div>

          <div
            style={{
              padding: '16px 18px',
              borderRadius: 24,
              backgroundColor: tokens.surfaceContainerHigh,
              border: `1px solid ${tokens.outlineVariant}`,
              display: 'flex',
              flexDirection: 'column',
              justifyContent: 'space-between',
            }}
          >
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.76rem', fontWeight: 700 }}>
              <span>PRÓXIMA VERIFICAÇÃO</span>
              <span style={{ color: tokens.gradeSuccess }}>
                <PollCountdownLabel
                  nextPollAt={nextPollAt}
                  paused={!isPageVisible}
                  fallbackSecs={nextPollHintSecs}
                />
              </span>
            </div>
            <div style={{ fontSize: '0.78rem', color: tokens.onSurfaceVariant, marginTop: 10 }}>
              {cachedAt
                ? `Última atualização às ${new Date(cachedAt).toLocaleTimeString('pt-BR')}`
                : 'Sincronizado com o portal'}
            </div>
          </div>
        </div>
      </section>

      {statusBanner && (
        <div
          style={{
            padding: '14px 18px',
            borderRadius: 22,
            backgroundColor:
              statusBanner.type === 'success'
                ? tokens.gradeSuccessContainer
                : tokens.errorContainer,
            color:
              statusBanner.type === 'success'
                ? tokens.onGradeSuccessContainer
                : tokens.onErrorContainer,
            fontWeight: 600,
            fontSize: '0.86rem',
            display: 'flex',
            alignItems: 'center',
            gap: 10,
          }}
        >
          <CheckCircle2 size={18} />
          <span>{statusBanner.message}</span>
        </div>
      )}

      {/* 2. PUSH NOTIFICATIONS & DISCORD LINKING */}
      <section style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
        <ExpressiveSegmentedCard index={0} total={2} style={{ padding: '18px 20px' }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 14 }}>
              <IconShapeContainer
                size={44}
                containerColor={
                  accountProfile?.has_fcm_token
                    ? tokens.gradeSuccessContainer
                    : tokens.surfaceContainerHighest
                }
                contentColor={
                  accountProfile?.has_fcm_token
                    ? tokens.onGradeSuccessContainer
                    : tokens.onSurfaceVariant
                }
              >
                {accountProfile?.has_fcm_token ? <Bell size={19} /> : <BellOff size={19} />}
              </IconShapeContainer>
              <div>
                <div style={{ fontFamily: 'Outfit, sans-serif', fontWeight: 800, fontSize: '1.02rem' }}>
                  Notificações no Dispositivo
                </div>
                <div style={{ fontSize: '0.8rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                  Receba um aviso assim que sair nota nova na turma{' '}
                  <strong>{accountProfile?.class_code ?? 'CTI'}</strong>.
                </div>
              </div>
            </div>

            <span
              style={{
                padding: '4px 11px',
                borderRadius: 999,
                fontSize: '0.74rem',
                fontWeight: 800,
                backgroundColor: accountProfile?.has_fcm_token
                  ? tokens.gradeSuccessContainer
                  : tokens.surfaceContainerHighest,
                color: accountProfile?.has_fcm_token
                  ? tokens.onGradeSuccessContainer
                  : tokens.onSurfaceVariant,
              }}
            >
              {accountProfile?.has_fcm_token ? 'Ativo' : 'Desativado'}
            </span>
          </div>

          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
            <EnhancedMorphButton
              size="md"
              variant="filled"
              disabled={showSavingFcm}
              icon={showSavingFcm ? <MorphingShapeIndicator size={16} label="Salvando" /> : <Sparkles size={15} />}
              onClick={() => handleRegisterFcm()}
            >
              {showSavingFcm
                ? 'Salvando...'
                : accountProfile?.has_fcm_token
                  ? 'Atualizar Notificações'
                  : 'Ativar Notificações'}
            </EnhancedMorphButton>

            {accountProfile?.has_fcm_token && (
              <EnhancedMorphButton
                size="md"
                variant="error"
                disabled={savingFcm}
                icon={<BellOff size={15} />}
                onClick={handleDeleteFcm}
              >
                Desativar
              </EnhancedMorphButton>
            )}
          </div>
        </ExpressiveSegmentedCard>

        <ExpressiveSegmentedCard index={1} total={2} style={{ padding: '18px 20px' }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 14 }}>
              <IconShapeContainer size={44}>
                <MessageSquare size={19} />
              </IconShapeContainer>
              <div>
                <div style={{ fontFamily: 'Outfit, sans-serif', fontWeight: 800, fontSize: '1.02rem' }}>
                  Vincular com o Discord
                </div>
                <div style={{ fontSize: '0.8rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                  Consulte seu boletim pelo bot e receba avisos de notas novas por mensagem direta.
                </div>
              </div>
            </div>

            {accountProfile?.discord_id && (
              <span
                style={{
                  padding: '4px 11px',
                  borderRadius: 999,
                  fontSize: '0.74rem',
                  fontWeight: 800,
                  backgroundColor: tokens.primaryContainer,
                  color: tokens.onPrimaryContainer,
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 4,
                }}
              >
                <Hash size={12} />
                {accountProfile.discord_id}
              </span>
            )}
          </div>

          <form onSubmit={handleLinkDiscord} style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
            <input
              type="text"
              placeholder="Seu ID do Discord (ex: 348921849128491008)"
              value={discordInput}
              onChange={(e) => setDiscordInput(e.target.value)}
              style={{
                flex: '1 1 240px',
                height: 42,
                padding: '0 14px',
                borderRadius: 14,
                border: `1px solid ${tokens.outlineVariant}`,
                backgroundColor: tokens.surfaceContainerLowest,
                color: tokens.onSurface,
                fontFamily: 'Outfit, sans-serif',
                fontSize: '0.86rem',
              }}
            />
            <EnhancedMorphButton
              type="submit"
              size="md"
              variant="tonal"
              disabled={savingDiscord || !discordInput.trim()}
            >
              {savingDiscord ? 'Vinculando...' : 'Vincular Discord'}
            </EnhancedMorphButton>
          </form>

          <div
            style={{
              marginTop: 12,
              paddingTop: 12,
              borderTop: `1px solid ${tokens.outlineVariant}`,
              display: 'flex',
              flexWrap: 'wrap',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 10,
            }}
          >
            <span style={{ fontSize: '0.78rem', color: tokens.onSurfaceVariant }}>
              Quer usar a <strong>Gemilona</strong> no servidor da sua turma?
            </span>
            <a
              href="https://discord.com/oauth2/authorize?client_id=1391245623965126808"
              target="_blank"
              rel="noopener noreferrer"
              style={{
                display: 'inline-flex',
                alignItems: 'center',
                gap: 6,
                padding: '7px 14px',
                borderRadius: 999,
                backgroundColor: tokens.primaryContainer,
                color: tokens.onPrimaryContainer,
                fontSize: '0.78rem',
                fontWeight: 700,
                textDecoration: 'none',
              }}
            >
              Adicionar Gemilona ao Servidor ↗
            </a>
          </div>
        </ExpressiveSegmentedCard>
      </section>
    </div>
  );
};
