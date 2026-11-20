import React, { useEffect, useRef, useState } from 'react';
import { Camera, Check, Settings, User, Image as ImageIcon } from 'lucide-react';
import {
  fetchUserProfile,
  updateUserProfile,
  type UserProfileData,
} from '../api/nsacClient';
import type { AccountProfileResponse } from '../api/types';
import { useM3Theme } from '../theme/ThemeContext';
import { AdaptiveStudioScreen } from './AdaptiveStudioScreen';
import { EnhancedMorphButton, ExpressiveSegmentedCard } from '../components/expressive/ExpressivePrimitives';

export function extractFirstNameFromEmail(email?: string | null): string {
  if (!email) return 'Aluno';
  const local = email.split('@')[0]?.trim() ?? '';
  const first = local.split(/[._-]/)[0]?.trim() ?? '';
  if (!first) return 'Aluno';
  return first.charAt(0).toUpperCase() + first.slice(1).toLowerCase();
}

function compressImageToDataUrl(
  file: File,
  maxWidth: number,
  maxHeight: number,
  quality = 0.82
): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error('Falha ao ler imagem'));
    reader.onload = () => {
      const img = new Image();
      img.onerror = () => reject(new Error('Imagem inválida'));
      img.onload = () => {
        const ratio = Math.min(1, maxWidth / img.width, maxHeight / img.height);
        const w = Math.max(1, Math.round(img.width * ratio));
        const h = Math.max(1, Math.round(img.height * ratio));
        const canvas = document.createElement('canvas');
        canvas.width = w;
        canvas.height = h;
        const ctx = canvas.getContext('2d');
        if (!ctx) {
          reject(new Error('Canvas indisponível'));
          return;
        }
        ctx.drawImage(img, 0, 0, w, h);
        resolve(canvas.toDataURL('image/jpeg', quality));
      };
      img.src = String(reader.result);
    };
    reader.readAsDataURL(file);
  });
}

export const ProfileScreen: React.FC<{
  token: string;
  accountProfile: AccountProfileResponse | null;
  detectedClassCode: string | null;
}> = ({ token, accountProfile, detectedClassCode }) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const [subTab, setSubTab] = useState<'profile' | 'settings'>('profile');
  const [profile, setProfile] = useState<UserProfileData | null>(null);
  const [displayName, setDisplayName] = useState('');
  const [bio, setBio] = useState('');
  const [avatarUrl, setAvatarUrl] = useState<string | null>(null);
  const [bannerUrl, setBannerUrl] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);

  const avatarInputRef = useRef<HTMLInputElement>(null);
  const bannerInputRef = useRef<HTMLInputElement>(null);

  const fallbackFirstName = extractFirstNameFromEmail(accountProfile?.nsac_email);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const data = await fetchUserProfile(token);
        if (cancelled) return;
        setProfile(data);
        setDisplayName(data.display_name ?? '');
        setBio(data.bio ?? '');
        setAvatarUrl(data.avatar_data_url);
        setBannerUrl(data.banner_data_url);
      } catch {
        // fallback gracefully if server is updating
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [token]);

  const handlePickAvatar = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const dataUrl = await compressImageToDataUrl(file, 256, 256, 0.82);
      setAvatarUrl(dataUrl);
      setStatusMsg(null);
    } catch {
      setStatusMsg('Não foi possível processar a foto de perfil.');
    }
  };

  const handlePickBanner = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const dataUrl = await compressImageToDataUrl(file, 1080, 360, 0.8);
      setBannerUrl(dataUrl);
      setStatusMsg(null);
    } catch {
      setStatusMsg('Não foi possível processar o banner.');
    }
  };

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setStatusMsg(null);
    try {
      const updated = await updateUserProfile(token, {
        display_name: displayName.trim() || null,
        bio: bio.trim() || null,
        avatar_data_url: avatarUrl,
        banner_data_url: bannerUrl,
      });
      setProfile(updated);
      setStatusMsg('Perfil salvo com sucesso!');
    } catch {
      setStatusMsg('Erro ao salvar perfil. Tente novamente.');
    } finally {
      setSaving(false);
    }
  };

  const shownName =
    displayName.trim() || profile?.first_name || fallbackFirstName;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
      {/* Top Intuitive Switcher: Perfil | Configurações */}
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: '1fr 1fr',
          gap: 8,
          padding: 6,
          borderRadius: isMaterialYou ? 999 : 14,
          backgroundColor: tokens.surfaceContainerLow,
          border: `1px solid ${tokens.outlineVariant}`,
        }}
      >
        <button
          type="button"
          onClick={() => setSubTab('profile')}
          style={{
            display: 'inline-flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: 8,
            padding: '10px 14px',
            borderRadius: isMaterialYou ? 999 : 10,
            border: 'none',
            backgroundColor: subTab === 'profile' ? tokens.primary : 'transparent',
            color: subTab === 'profile' ? tokens.onPrimary : tokens.onSurfaceVariant,
            fontWeight: 700,
            fontSize: '0.86rem',
            cursor: 'pointer',
          }}
        >
          <User size={16} />
          <span>Meu Perfil</span>
        </button>

        <button
          type="button"
          onClick={() => setSubTab('settings')}
          style={{
            display: 'inline-flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: 8,
            padding: '10px 14px',
            borderRadius: isMaterialYou ? 999 : 10,
            border: 'none',
            backgroundColor: subTab === 'settings' ? tokens.primary : 'transparent',
            color: subTab === 'settings' ? tokens.onPrimary : tokens.onSurfaceVariant,
            fontWeight: 700,
            fontSize: '0.86rem',
            cursor: 'pointer',
          }}
        >
          <Settings size={16} />
          <span>Configurações</span>
        </button>
      </div>

      {subTab === 'settings' ? (
        <AdaptiveStudioScreen detectedClassCode={detectedClassCode} />
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          {/* Hidden file inputs */}
          <input
            ref={avatarInputRef}
            type="file"
            accept="image/*"
            style={{ display: 'none' }}
            onChange={(e) => void handlePickAvatar(e)}
          />
          <input
            ref={bannerInputRef}
            type="file"
            accept="image/*"
            style={{ display: 'none' }}
            onChange={(e) => void handlePickBanner(e)}
          />

          {/* Banner + Avatar Hero Card */}
          <div
            style={{
              borderRadius: isMaterialYou ? 28 : 16,
              overflow: 'hidden',
              backgroundColor: tokens.surfaceContainerLow,
              border: `1px solid ${tokens.outlineVariant}`,
            }}
          >
            {/* Banner */}
            <div
              onClick={() => bannerInputRef.current?.click()}
              style={{
                position: 'relative',
                height: 140,
                cursor: 'pointer',
                background: bannerUrl
                  ? `url(${bannerUrl}) center/cover no-repeat`
                  : `linear-gradient(135deg, ${tokens.primaryContainer}, ${tokens.surfaceContainerHighest})`,
              }}
            >
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  bannerInputRef.current?.click();
                }}
                style={{
                  position: 'absolute',
                  right: 12,
                  bottom: 10,
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 6,
                  padding: '6px 12px',
                  borderRadius: 999,
                  border: '1px solid rgba(255,255,255,0.2)',
                  backgroundColor: 'rgba(0,0,0,0.58)',
                  color: '#FFFFFF',
                  fontSize: '0.74rem',
                  fontWeight: 700,
                  cursor: 'pointer',
                }}
              >
                <ImageIcon size={13} />
                <span>Alterar banner</span>
              </button>
            </div>

            {/* Avatar + Name */}
            <div style={{ padding: '0 20px 20px', position: 'relative' }}>
              <div style={{ display: 'flex', alignItems: 'flex-end', justifyContent: 'space-between', marginTop: -42, marginBottom: 12 }}>
                <div
                  onClick={() => avatarInputRef.current?.click()}
                  style={{
                    position: 'relative',
                    width: 84,
                    height: 84,
                    borderRadius: 999,
                    border: `4px solid ${tokens.surfaceContainerLow}`,
                    backgroundColor: tokens.primaryContainer,
                    color: tokens.onPrimaryContainer,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    overflow: 'hidden',
                    cursor: 'pointer',
                    fontSize: '1.8rem',
                    fontWeight: 800,
                  }}
                >
                  {avatarUrl ? (
                    <img
                      src={avatarUrl}
                      alt={shownName}
                      style={{ width: '100%', height: '100%', objectFit: 'cover' }}
                    />
                  ) : (
                    shownName.charAt(0).toUpperCase()
                  )}
                  <div
                    style={{
                      position: 'absolute',
                      inset: 0,
                      backgroundColor: 'rgba(0,0,0,0.28)',
                      display: 'flex',
                      alignItems: 'center',
                      justifyContent: 'center',
                      opacity: 0.85,
                    }}
                  >
                    <Camera size={18} color="#FFFFFF" />
                  </div>
                </div>

                {detectedClassCode && (
                  <span
                    style={{
                      padding: '5px 12px',
                      borderRadius: 999,
                      backgroundColor: tokens.primaryContainer,
                      color: tokens.onPrimaryContainer,
                      fontSize: '0.76rem',
                      fontWeight: 800,
                    }}
                  >
                    {detectedClassCode} • CTI UNESP
                  </span>
                )}
              </div>

              <h1 style={{ margin: 0, fontSize: '1.45rem', fontWeight: 800 }}>
                {shownName}
              </h1>
              <div style={{ fontSize: '0.8rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                {accountProfile?.nsac_email ?? ''}
              </div>
              {bio.trim() && (
                <p style={{ margin: '10px 0 0', fontSize: '0.86rem', lineHeight: 1.5, color: tokens.onSurface }}>
                  {bio}
                </p>
              )}
            </div>
          </div>

          {/* Edit Profile Form */}
          <ExpressiveSegmentedCard index={0} total={1} style={{ padding: '20px 22px' }}>
            <form onSubmit={handleSave} style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
              <div>
                <label style={{ display: 'block', fontSize: '0.75rem', fontWeight: 700, color: tokens.onSurfaceVariant, marginBottom: 6, textTransform: 'uppercase' }}>
                  Nome de Exibição (padrão: {profile?.first_name ?? fallbackFirstName})
                </label>
                <input
                  type="text"
                  maxLength={48}
                  placeholder={profile?.first_name ?? fallbackFirstName}
                  value={displayName}
                  onChange={(e) => setDisplayName(e.target.value)}
                  style={{
                    width: '100%',
                    height: 42,
                    padding: '0 14px',
                    borderRadius: 12,
                    border: `1px solid ${tokens.outlineVariant}`,
                    backgroundColor: tokens.surfaceContainerLowest,
                    color: tokens.onSurface,
                    fontSize: '0.9rem',
                    boxSizing: 'border-box',
                  }}
                />
              </div>

              <div>
                <label style={{ display: 'block', fontSize: '0.75rem', fontWeight: 700, color: tokens.onSurfaceVariant, marginBottom: 6, textTransform: 'uppercase' }}>
                  Bio
                </label>
                <textarea
                  rows={3}
                  maxLength={240}
                  placeholder="Conte um pouco sobre você, seu curso ou metas no CTI..."
                  value={bio}
                  onChange={(e) => setBio(e.target.value)}
                  style={{
                    width: '100%',
                    padding: '10px 14px',
                    borderRadius: 12,
                    border: `1px solid ${tokens.outlineVariant}`,
                    backgroundColor: tokens.surfaceContainerLowest,
                    color: tokens.onSurface,
                    fontSize: '0.88rem',
                    resize: 'vertical',
                    boxSizing: 'border-box',
                  }}
                />
              </div>

              {statusMsg && (
                <div style={{ fontSize: '0.8rem', fontWeight: 600, color: tokens.primary }}>
                  {statusMsg}
                </div>
              )}

              <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
                <EnhancedMorphButton type="submit" variant="filled" size="md" disabled={saving}>
                  <Check size={15} />
                  <span>{saving ? 'Salvando...' : 'Salvar Perfil'}</span>
                </EnhancedMorphButton>
              </div>
            </form>
          </ExpressiveSegmentedCard>
        </div>
      )}
    </div>
  );
};
