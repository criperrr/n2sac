import React, { useCallback, useEffect, useRef, useState } from 'react';
import { AlertCircle, Check, Lock, MessageCircle, Send, Trash2, X } from 'lucide-react';
import {
  deleteConversation,
  fetchConversationMessages,
  NsacApiClientError,
  sendConversationMessage,
} from '../../api/nsacClient';
import type { ChatMessage, ChatRateLimitStatus } from '../../api/types';
import { useM3Theme } from '../../theme/ThemeContext';
import { ExpressiveModal, ModalIconButton } from '../expressive/ExpressiveModal';
import { MorphingShapeIndicator } from '../expressive/ShapeMorph';
import { ProfileAvatar } from '../profile/ProfileAvatar';

const MAX_CHARS = 500;
const POLL_INTERVAL_MS = 4000;

export interface ChatPeerTarget {
  accountId: number;
  label: string;
  avatarUrl: string | null;
  classCode: string;
}

function formatMessageTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleTimeString('pt-BR', { hour: '2-digit', minute: '2-digit' });
  } catch {
    return '';
  }
}

export const FriendChatModal: React.FC<{
  token: string;
  peer: ChatPeerTarget;
  onClose: () => void;
  onDeleted?: () => void;
}> = ({ token, peer, onClose, onDeleted }) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [isFriend, setIsFriend] = useState(true);
  const [rateLimit, setRateLimit] = useState<ChatRateLimitStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [clearing, setClearing] = useState(false);

  const listRef = useRef<HTMLDivElement>(null);
  const lastIdRef = useRef<number>(0);

  const scrollToBottom = useCallback((smooth = false) => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'auto' });
  }, []);

  const loadInitial = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const payload = await fetchConversationMessages(token, peer.accountId, { limit: 80 });
      setMessages(payload.messages);
      setIsFriend(payload.is_friend);
      setRateLimit(payload.rate_limit);
      lastIdRef.current = payload.messages[payload.messages.length - 1]?.id ?? 0;
      window.requestAnimationFrame(() => scrollToBottom(false));
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Não foi possível abrir esta conversa.');
    } finally {
      setLoading(false);
    }
  }, [token, peer.accountId, scrollToBottom]);

  useEffect(() => {
    void loadInitial();
  }, [loadInitial]);

  // Lightweight incremental polling while the chat dialog is open.
  useEffect(() => {
    const timer = window.setInterval(async () => {
      if (document.visibilityState !== 'visible') return;
      try {
        const payload = await fetchConversationMessages(token, peer.accountId, {
          afterId: lastIdRef.current || undefined,
          limit: 50,
        });
        setIsFriend(payload.is_friend);
        setRateLimit(payload.rate_limit);
        if (payload.messages.length > 0) {
          setMessages((prev) => {
            const seen = new Set(prev.map((m) => m.id));
            const next = [...prev];
            for (const m of payload.messages) {
              if (!seen.has(m.id)) next.push(m);
            }
            return next;
          });
          lastIdRef.current = payload.messages[payload.messages.length - 1]?.id ?? lastIdRef.current;
          window.requestAnimationFrame(() => scrollToBottom(true));
        }
      } catch {
        // Keep current messages on transient poll failure.
      }
    }, POLL_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [token, peer.accountId, scrollToBottom]);

  const charCount = [...draft].length;
  const trimmedCount = [...draft.trim()].length;
  const overLimit = charCount > MAX_CHARS;
  const quotaExhausted = rateLimit !== null && rateLimit.remaining <= 0;

  const handleSend = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!isFriend || sending || trimmedCount === 0 || overLimit) return;
    setSending(true);
    setError(null);
    try {
      const res = await sendConversationMessage(token, peer.accountId, draft);
      setMessages((prev) => (prev.some((m) => m.id === res.message.id) ? prev : [...prev, res.message]));
      lastIdRef.current = Math.max(lastIdRef.current, res.message.id);
      setRateLimit(res.rate_limit);
      setDraft('');
      window.requestAnimationFrame(() => scrollToBottom(true));
    } catch (err) {
      if (err instanceof NsacApiClientError) {
        setError(err.message);
      } else {
        setError('Não foi possível enviar a mensagem.');
      }
    } finally {
      setSending(false);
    }
  };

  const handleClear = async (close: () => void) => {
    if (!confirmClear) {
      setConfirmClear(true);
      return;
    }
    setClearing(true);
    try {
      await deleteConversation(token, peer.accountId);
      setMessages([]);
      lastIdRef.current = 0;
      setConfirmClear(false);
      onDeleted?.();
      if (!isFriend) close();
    } catch {
      setError('Não foi possível apagar a conversa agora.');
    } finally {
      setClearing(false);
    }
  };

  return (
    <ExpressiveModal
      ariaLabel={`Conversa com ${peer.label}`}
      onClose={onClose}
      maxWidth={540}
      zIndex={10090}
    >
      {(close) => (
        <div
          data-no-size-morph
          style={{
            display: 'flex',
            flexDirection: 'column',
            height: 'min(72dvh, 580px)',
            gap: 12,
          }}
        >
          {/* Header */}
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0 }}>
              <ProfileAvatar name={peer.label} image={peer.avatarUrl} size={42} />
              <div style={{ minWidth: 0 }}>
                <div
                  style={{
                    fontWeight: 800,
                    fontSize: '1rem',
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}
                >
                  {peer.label}
                </div>
                <div style={{ fontSize: '0.73rem', color: tokens.onSurfaceVariant }}>
                  {peer.classCode} • {isFriend ? 'Amigos' : 'Somente leitura'}
                </div>
              </div>
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexShrink: 0 }}>
              <ModalIconButton
                label={confirmClear ? 'Confirmar apagar conversa' : 'Apagar conversa'}
                onClick={() => void handleClear(close)}
              >
                {clearing ? (
                  <MorphingShapeIndicator size={16} label="Apagando" />
                ) : confirmClear ? (
                  <Check size={17} color={tokens.error} />
                ) : (
                  <Trash2 size={16} />
                )}
              </ModalIconButton>
              <ModalIconButton label="Fechar conversa" onClick={() => close()}>
                <X size={18} />
              </ModalIconButton>
            </div>
          </div>

          {confirmClear && (
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
                gap: 8,
                padding: '8px 12px',
                borderRadius: isMaterialYou ? 16 : 10,
                backgroundColor: tokens.errorContainer,
                color: tokens.onErrorContainer,
                fontSize: '0.78rem',
                fontWeight: 600,
              }}
            >
              <span>Toque no botão de confirmar acima para apagar seu histórico desta conversa.</span>
              <button
                type="button"
                onClick={() => setConfirmClear(false)}
                style={{
                  border: 'none',
                  background: 'transparent',
                  color: 'inherit',
                  fontWeight: 800,
                  cursor: 'pointer',
                  fontSize: '0.76rem',
                }}
              >
                Cancelar
              </button>
            </div>
          )}

          {/* Messages list */}
          <div
            ref={listRef}
            style={{
              flex: 1,
              minHeight: 0,
              overflowY: 'auto',
              display: 'flex',
              flexDirection: 'column',
              gap: 8,
              padding: '10px 4px',
              borderTop: `1px solid ${tokens.outlineVariant}`,
              borderBottom: `1px solid ${tokens.outlineVariant}`,
            }}
          >
            {loading ? (
              <div style={{ margin: 'auto', display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8 }}>
                <MorphingShapeIndicator size={28} label="Carregando conversa" />
                <span style={{ fontSize: '0.8rem', color: tokens.onSurfaceVariant }}>Carregando mensagens...</span>
              </div>
            ) : messages.length === 0 ? (
              <div
                style={{
                  margin: 'auto',
                  textAlign: 'center',
                  color: tokens.onSurfaceVariant,
                  padding: '24px 12px',
                  display: 'flex',
                  flexDirection: 'column',
                  alignItems: 'center',
                  gap: 8,
                }}
              >
                <MessageCircle size={24} />
                <div style={{ fontSize: '0.86rem', fontWeight: 600 }}>Nenhuma mensagem ainda</div>
                <div style={{ fontSize: '0.76rem' }}>
                  {isFriend
                    ? 'Envie uma mensagem de texto (até 500 caracteres).'
                    : 'Vocês não são mais amigos.'}
                </div>
              </div>
            ) : (
              messages.map((m) => (
                <div
                  key={m.id}
                  style={{
                    alignSelf: m.is_me ? 'flex-end' : 'flex-start',
                    maxWidth: '84%',
                    padding: '9px 13px',
                    borderRadius: isMaterialYou
                      ? m.is_me
                        ? '20px 20px 6px 20px'
                        : '20px 20px 20px 6px'
                      : 12,
                    backgroundColor: m.is_me ? tokens.primaryContainer : tokens.surfaceContainerHigh,
                    color: m.is_me ? tokens.onPrimaryContainer : tokens.onSurface,
                    fontSize: '0.88rem',
                    lineHeight: 1.45,
                    whiteSpace: 'pre-wrap',
                    overflowWrap: 'anywhere',
                  }}
                >
                  <div>{m.text}</div>
                  <div
                    style={{
                      fontSize: '0.66rem',
                      opacity: 0.72,
                      textAlign: 'right',
                      marginTop: 4,
                    }}
                  >
                    {formatMessageTime(m.created_at)}
                  </div>
                </div>
              ))
            )}
          </div>

          {error && (
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                fontSize: '0.78rem',
                fontWeight: 600,
                color: tokens.error,
              }}
            >
              <AlertCircle size={14} style={{ flexShrink: 0 }} />
              <span>{error}</span>
            </div>
          )}

          {/* Composer or Read-Only notice */}
          {isFriend ? (
            <form onSubmit={handleSend} style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
              <div style={{ display: 'flex', alignItems: 'flex-end', gap: 8 }}>
                <textarea
                  rows={2}
                  maxLength={MAX_CHARS + 20}
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.shiftKey) {
                      e.preventDefault();
                      void handleSend(e);
                    }
                  }}
                  placeholder="Escreva uma mensagem..."
                  style={{
                    flex: 1,
                    minHeight: 44,
                    maxHeight: 112,
                    padding: '10px 14px',
                    borderRadius: isMaterialYou ? 20 : 12,
                    border: `1px solid ${overLimit ? tokens.error : tokens.outlineVariant}`,
                    backgroundColor: tokens.surfaceContainer,
                    color: tokens.onSurface,
                    fontSize: '0.88rem',
                    fontFamily: 'inherit',
                    resize: 'none',
                    outline: 'none',
                    boxSizing: 'border-box',
                  }}
                />
                <button
                  type="submit"
                  disabled={sending || trimmedCount === 0 || overLimit || quotaExhausted}
                  aria-label="Enviar mensagem"
                  style={{
                    width: 44,
                    height: 44,
                    flexShrink: 0,
                    borderRadius: 999,
                    border: 'none',
                    backgroundColor:
                      sending || trimmedCount === 0 || overLimit || quotaExhausted
                        ? tokens.surfaceContainerHighest
                        : tokens.primary,
                    color:
                      sending || trimmedCount === 0 || overLimit || quotaExhausted
                        ? tokens.onSurfaceVariant
                        : tokens.onPrimary,
                    display: 'inline-flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    cursor:
                      sending || trimmedCount === 0 || overLimit || quotaExhausted
                        ? 'not-allowed'
                        : 'pointer',
                  }}
                >
                  {sending ? <MorphingShapeIndicator size={17} label="Enviando" /> : <Send size={17} />}
                </button>
              </div>
              <div
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                  fontSize: '0.7rem',
                  color: overLimit ? tokens.error : tokens.onSurfaceVariant,
                  padding: '0 4px',
                }}
              >
                <span>
                  {rateLimit
                    ? rateLimit.remaining > 0
                      ? `${rateLimit.remaining}/${rateLimit.limit} msgs disponíveis (3 min)`
                      : `Limite atingido — aguarde ${rateLimit.retry_after_secs}s`
                    : 'Até 10 mensagens a cada 3 minutos'}
                </span>
                <span style={{ fontVariantNumeric: 'tabular-nums', fontWeight: overLimit ? 700 : 500 }}>
                  {charCount}/{MAX_CHARS}
                </span>
              </div>
            </form>
          ) : (
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 8,
                padding: '10px 14px',
                borderRadius: isMaterialYou ? 18 : 10,
                backgroundColor: tokens.surfaceContainer,
                color: tokens.onSurfaceVariant,
                fontSize: '0.8rem',
              }}
            >
              <Lock size={15} style={{ flexShrink: 0 }} />
              <span>Vocês não são mais amigos — esta conversa está somente leitura.</span>
            </div>
          )}
        </div>
      )}
    </ExpressiveModal>
  );
};
