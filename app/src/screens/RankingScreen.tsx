import React, { useCallback, useEffect, useState } from 'react';
import {
  Trophy,
  Users,
  EyeOff,
  Swords,
  Search,
  ShieldCheck,
  X,
} from 'lucide-react';
import {
  fetchCompareLeaderboard,
  updateRankingPreferences,
} from '../api/nsacClient';
import type { ComparisonSummaryResponse, RankingPreferences } from '../api/types';
import { useM3Theme } from '../theme/ThemeContext';
import {
  AnimatedNumber,
  EnhancedConnectedButtonGroup,
  EnhancedMorphButton,
  ExpressiveSegmentedCard,
  ExpressiveSwitch,
  IconShapeContainer,
} from '../components/expressive/ExpressivePrimitives';

export const RankingScreen: React.FC<{
  token: string;
  classCode: string | null;
  preferences: RankingPreferences | null;
  onPreferencesUpdated: (prefs: RankingPreferences) => void;
}> = ({ token, classCode, preferences, onPreferencesUpdated }) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const [scope, setScope] = useState<'class' | 'course' | 'school'>('class');
  const [bimester, setBimester] = useState<number>(0); // 0 = all
  const [subjectSearch, setSubjectSearch] = useState('');
  const [selectedPeerId, setSelectedPeerId] = useState<number | null>(null);
  const [data, setData] = useState<ComparisonSummaryResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [savingPrefs, setSavingPrefs] = useState(false);

  const loadLeaderboard = useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetchCompareLeaderboard(token, {
        scope,
        bimester: bimester === 0 ? null : bimester,
        subject: subjectSearch.trim() || undefined,
        peerId: selectedPeerId,
      });
      setData(res);
      onPreferencesUpdated(res.my_preferences);
    } catch {
      // keep previous state
    } finally {
      setLoading(false);
    }
  }, [token, scope, bimester, subjectSearch, selectedPeerId, onPreferencesUpdated]);

  useEffect(() => {
    loadLeaderboard();
  }, [loadLeaderboard]);

  const effectivePrefs = data?.my_preferences ?? preferences;
  const optedIn = effectivePrefs?.opted_in ?? false;
  const hideName = effectivePrefs?.hide_name ?? false;

  const handleToggleOptIn = async (nextOptIn: boolean) => {
    setSavingPrefs(true);
    try {
      const updated = await updateRankingPreferences(token, {
        opted_in: nextOptIn,
        hide_name: hideName,
      });
      onPreferencesUpdated(updated);
      await loadLeaderboard();
    } finally {
      setSavingPrefs(false);
    }
  };

  const handleToggleHideName = async (nextHide: boolean) => {
    setSavingPrefs(true);
    try {
      const updated = await updateRankingPreferences(token, {
        opted_in: optedIn,
        hide_name: nextHide,
      });
      onPreferencesUpdated(updated);
      await loadLeaderboard();
    } finally {
      setSavingPrefs(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: isMaterialYou ? 18 : 16 }}>
      {/* 1. HERO HEADER & PRIVACY CONTROLS */}
      <section
        style={{
          borderRadius: isMaterialYou ? 32 : 16,
          padding: isMaterialYou ? '24px 22px' : '24px 26px',
          backgroundColor: isMaterialYou ? tokens.surfaceContainerLow : tokens.background,
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
            <IconShapeContainer size={isMaterialYou ? 52 : 42} active>
              <Trophy size={isMaterialYou ? 24 : 20} />
            </IconShapeContainer>
            <div>
              <div
                style={{
                  fontSize: '0.73rem',
                  fontWeight: 600,
                  color: tokens.onSurfaceVariant,
                  textTransform: 'uppercase',
                  letterSpacing: '0.04em',
                }}
              >
                Comparação entre Alunos • {classCode ?? 'CTI'}
              </div>
              <h1
                className={!isMaterialYou ? 'openai-serif-display' : undefined}
                style={{
                  margin: 0,
                  fontSize: isMaterialYou ? '1.65rem' : '2.15rem',
                  fontWeight: isMaterialYou ? 800 : 400,
                }}
              >
                Ranking & Comparação
              </h1>
            </div>
          </div>

          {data?.my_rank && (
            <div
              style={{
                padding: '8px 14px',
                borderRadius: isMaterialYou ? 999 : 10,
                backgroundColor: tokens.primaryContainer,
                color: tokens.onPrimaryContainer,
                fontWeight: 700,
                fontSize: '0.86rem',
                display: 'inline-flex',
                alignItems: 'center',
                gap: 8,
              }}
            >
              <span>Sua posição: #{data.my_rank}</span>
              {data.my_percentile !== null && (
                <span style={{ opacity: 0.85 }}>
                  (Top {(100 - data.my_percentile + 0.1).toFixed(0)}%)
                </span>
              )}
            </div>
          )}
        </div>

        {/* Privacy Settings Strip */}
        <div
          style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(240px, 1fr))',
            gap: 10,
            marginBottom: 18,
          }}
        >
          <div
            onClick={() => !savingPrefs && handleToggleOptIn(!optedIn)}
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 12,
              padding: '12px 14px',
              borderRadius: isMaterialYou ? 20 : 12,
              backgroundColor: tokens.surfaceContainer,
              border: `1px solid ${optedIn ? tokens.primary : tokens.outlineVariant}`,
              cursor: savingPrefs ? 'wait' : 'pointer',
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <ShieldCheck size={18} color={optedIn ? tokens.gradeSuccess : tokens.onSurfaceVariant} />
              <div>
                <div style={{ fontWeight: 600, fontSize: '0.86rem' }}>
                  Participar do Ranking
                </div>
                <div style={{ fontSize: '0.74rem', color: tokens.onSurfaceVariant }}>
                  {optedIn ? 'Você está visível no ranking' : 'Você está oculto (fora do ranking)'}
                </div>
              </div>
            </div>
            <ExpressiveSwitch checked={optedIn} onChange={handleToggleOptIn} disabled={savingPrefs} />
          </div>

          <div
            onClick={() => !savingPrefs && optedIn && handleToggleHideName(!hideName)}
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 12,
              padding: '12px 14px',
              borderRadius: isMaterialYou ? 20 : 12,
              backgroundColor: tokens.surfaceContainer,
              border: `1px solid ${hideName && optedIn ? tokens.primary : tokens.outlineVariant}`,
              opacity: optedIn ? 1 : 0.5,
              cursor: !optedIn || savingPrefs ? 'not-allowed' : 'pointer',
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <EyeOff size={18} color={hideName ? tokens.primary : tokens.onSurfaceVariant} />
              <div>
                <div style={{ fontWeight: 600, fontSize: '0.86rem' }}>
                  Ocultar Meu Nome
                </div>
                <div style={{ fontSize: '0.74rem', color: tokens.onSurfaceVariant }}>
                  {hideName ? 'Exibido como Aluno #XXX' : 'Exibindo seu nome'}
                </div>
              </div>
            </div>
            <ExpressiveSwitch
              checked={hideName}
              onChange={handleToggleHideName}
              disabled={!optedIn || savingPrefs}
            />
          </div>
        </div>

        {/* Scope & Bimester Filters */}
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 10, alignItems: 'center', justifyContent: 'space-between' }}>
          <EnhancedConnectedButtonGroup<'class' | 'course' | 'school'>
            wrap={true}
            value={scope}
            onChange={(s) => {
              setScope(s);
              setSelectedPeerId(null);
            }}
            options={[
              { value: 'class', label: `Minha Turma (${classCode ?? 'CTI'})`, icon: <Users size={14} /> },
              { value: 'course', label: 'Meu Curso', icon: <Trophy size={14} /> },
              { value: 'school', label: 'Todo o CTI', icon: <Trophy size={14} /> },
            ]}
          />

          <EnhancedConnectedButtonGroup<number>
            wrap={true}
            value={bimester}
            onChange={setBimester}
            options={[
              { value: 0, label: 'Ano Todo' },
              { value: 1, label: '1ºB' },
              { value: 2, label: '2ºB' },
              { value: 3, label: '3ºB' },
              { value: 4, label: '4ºB' },
            ]}
          />
        </div>

        {/* Subject filter input */}
        <div
          style={{
            marginTop: 12,
            display: 'flex',
            alignItems: 'center',
            gap: 10,
            height: 38,
            padding: '0 14px',
            borderRadius: isMaterialYou ? 999 : 12,
            backgroundColor: tokens.surfaceContainer,
            border: `1px solid ${tokens.outlineVariant}`,
          }}
        >
          <Search size={15} color={tokens.onSurfaceVariant} />
          <input
            type="text"
            placeholder="Filtrar ranking por disciplina específica (ex: Matematica, Fisica, Java...)"
            value={subjectSearch}
            onChange={(e) => setSubjectSearch(e.target.value)}
            style={{
              flex: 1,
              border: 'none',
              outline: 'none',
              background: 'transparent',
              color: tokens.onSurface,
              fontSize: '0.84rem',
            }}
          />
          {subjectSearch && (
            <button
              type="button"
              onClick={() => setSubjectSearch('')}
              style={{
                background: 'transparent',
                border: 'none',
                color: tokens.onSurfaceVariant,
                cursor: 'pointer',
                display: 'flex',
              }}
            >
              <X size={15} />
            </button>
          )}
        </div>
      </section>

      {/* 2. HEAD-TO-HEAD PEER COMPARISON (When user clicks a classmate) */}
      {data?.peer_comparison && (
        <section
          style={{
            borderRadius: isMaterialYou ? 28 : 16,
            padding: '20px 22px',
            backgroundColor: tokens.surfaceContainerLow,
            border: `1.5px solid ${tokens.primary}`,
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 14 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <Swords size={20} color={tokens.primary} />
              <div>
                <div style={{ fontWeight: 700, fontSize: '1rem' }}>
                  Comparação Direta: Você vs {data.peer_comparison.peer_entry.display_label}
                </div>
                <div style={{ fontSize: '0.78rem', color: tokens.onSurfaceVariant }}>
                  Vitórias: <strong style={{ color: tokens.gradeSuccess }}>{data.peer_comparison.my_wins}</strong> •
                  Empates: <strong>{data.peer_comparison.ties}</strong> •
                  Atrás: <strong style={{ color: tokens.error }}>{data.peer_comparison.peer_wins}</strong>
                </div>
              </div>
            </div>
            <EnhancedMorphButton
              size="sm"
              variant="outlined"
              icon={<X size={14} />}
              onClick={() => setSelectedPeerId(null)}
            >
              Fechar
            </EnhancedMorphButton>
          </div>

          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(260px, 1fr))', gap: 8 }}>
            {data.peer_comparison.subjects.map((row) => {
              const diff = row.delta_me_vs_peer;
              return (
                <div
                  key={row.subject_name}
                  style={{
                    padding: '10px 12px',
                    borderRadius: isMaterialYou ? 16 : 10,
                    backgroundColor: tokens.surfaceContainer,
                    border: `1px solid ${tokens.outlineVariant}`,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    gap: 8,
                  }}
                >
                  <div style={{ minWidth: 0 }}>
                    <div
                      style={{
                        fontWeight: 600,
                        fontSize: '0.84rem',
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap',
                      }}
                    >
                      {row.subject_name}
                    </div>
                    <div style={{ fontSize: '0.74rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                      Você: <strong>{row.my_average !== null ? row.my_average.toFixed(1) : '—'}</strong> •{' '}
                      Colega: <strong>{row.peer_average !== null ? row.peer_average.toFixed(1) : '—'}</strong>
                    </div>
                  </div>
                  {diff !== null && (
                    <span
                      style={{
                        padding: '3px 8px',
                        borderRadius: isMaterialYou ? 999 : 6,
                        fontSize: '0.75rem',
                        fontWeight: 700,
                        backgroundColor:
                          diff >= 0 ? tokens.gradeSuccessContainer : tokens.errorContainer,
                        color:
                          diff >= 0 ? tokens.onGradeSuccessContainer : tokens.onErrorContainer,
                        flexShrink: 0,
                      }}
                    >
                      {diff >= 0 ? '+' : ''}
                      {diff.toFixed(1)}
                    </span>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      )}

      {/* 3. LEADERBOARD LIST */}
      <section style={{ display: 'flex', flexDirection: 'column', gap: isMaterialYou ? 4 : 8 }}>
        {loading && !data ? (
          <div style={{ padding: 32, textAlign: 'center', color: tokens.onSurfaceVariant }}>
            Carregando ranking...
          </div>
        ) : !data || data.leaderboard.length === 0 ? (
          <div
            style={{
              padding: 36,
              textAlign: 'center',
              borderRadius: isMaterialYou ? 28 : 14,
              backgroundColor: tokens.surfaceContainerLow,
              border: `1px solid ${tokens.outlineVariant}`,
            }}
          >
            <div style={{ fontWeight: 700, fontSize: '1rem' }}>
              Nenhum participante encontrado neste filtro ainda
            </div>
            <div style={{ fontSize: '0.84rem', color: tokens.onSurfaceVariant, marginTop: 4 }}>
              Ative &quot;Participar do Ranking&quot; acima ou alterne para &quot;Todo o CTI&quot;.
            </div>
          </div>
        ) : (
          data.leaderboard.map((entry, idx) => (
            <ExpressiveSegmentedCard
              key={entry.account_id}
              index={idx}
              total={data.leaderboard.length}
              selected={entry.is_me || selectedPeerId === entry.account_id}
              onClick={
                !entry.is_me
                  ? () => setSelectedPeerId((prev) => (prev === entry.account_id ? null : entry.account_id))
                  : undefined
              }
              style={{ padding: '14px 18px' }}
            >
              <div
                style={{
                  display: 'flex',
                  flexWrap: 'wrap',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                  gap: 12,
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                  <div
                    style={{
                      width: 34,
                      height: 34,
                      borderRadius: isMaterialYou ? 999 : 8,
                      backgroundColor:
                        entry.rank === 1
                          ? tokens.gradeWarningContainer
                          : entry.is_me
                            ? tokens.primaryContainer
                            : tokens.surfaceContainerHighest,
                      color:
                        entry.rank === 1
                          ? tokens.onGradeWarningContainer
                          : entry.is_me
                            ? tokens.onPrimaryContainer
                            : tokens.onSurface,
                      fontWeight: 800,
                      fontSize: '0.88rem',
                      display: 'inline-flex',
                      alignItems: 'center',
                      justifyContent: 'center',
                      flexShrink: 0,
                    }}
                  >
                    #{entry.rank}
                  </div>
                  <div>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                      <span style={{ fontWeight: 700, fontSize: '0.96rem' }}>
                        {entry.display_label}
                      </span>
                      <span
                        style={{
                          padding: '2px 7px',
                          borderRadius: isMaterialYou ? 999 : 6,
                          fontSize: '0.7rem',
                          fontWeight: 700,
                          backgroundColor: tokens.surfaceContainerHighest,
                          color: tokens.onSurfaceVariant,
                        }}
                      >
                        {entry.class_code}
                      </span>
                      {entry.is_me && (
                        <span
                          style={{
                            padding: '2px 7px',
                            borderRadius: isMaterialYou ? 999 : 6,
                            fontSize: '0.7rem',
                            fontWeight: 700,
                            backgroundColor: tokens.primaryContainer,
                            color: tokens.onPrimaryContainer,
                          }}
                        >
                          Você
                        </span>
                      )}
                    </div>
                    <div style={{ fontSize: '0.76rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                      {entry.subjects_count} matérias • {entry.total_absences} faltas •{' '}
                      {entry.delta_vs_class >= 0 ? '+' : ''}
                      {entry.delta_vs_class.toFixed(2)} vs sala
                    </div>
                  </div>
                </div>

                <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                  <div style={{ textAlign: 'right' }}>
                    <div style={{ fontSize: '0.68rem', color: tokens.onSurfaceVariant, textTransform: 'uppercase' }}>
                      Média
                    </div>
                    <div style={{ fontWeight: 800, fontSize: '1.15rem', color: tokens.primary }}>
                      <AnimatedNumber value={entry.personal_average} decimals={2} />
                    </div>
                  </div>
                  {!entry.is_me && (
                    <EnhancedMorphButton
                      size="sm"
                      variant={selectedPeerId === entry.account_id ? 'filled' : 'tonal'}
                      icon={<Swords size={13} />}
                      onClick={() =>
                        setSelectedPeerId((prev) =>
                          prev === entry.account_id ? null : entry.account_id
                        )
                      }
                    >
                      Comparar
                    </EnhancedMorphButton>
                  )}
                </div>
              </div>
            </ExpressiveSegmentedCard>
          ))
        )}
      </section>
    </div>
  );
};
