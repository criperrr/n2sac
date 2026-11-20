import React, { useState } from 'react';
import {
  ClipboardList,
  Clock,
  DoorOpen,
  Globe,
  ExternalLink,
  CheckCircle2,
  Lock,
  ChevronDown,
  ChevronUp,
  Info,
  Calendar,
  UserCheck,
} from 'lucide-react';
import { fetchFichaDetail } from '../api/nsacClient';
import type {
  AtrasoRecord,
  FichaAvaliacaoDetail,
  FichaAvaliacaoEntry,
  HomeInfo,
  SaidaRecord,
} from '../api/types';
import { useM3Theme } from '../theme/ThemeContext';
import {
  EnhancedConnectedButtonGroup,
  EnhancedMorphButton,
  ExpressiveSegmentedCard,
  IconShapeContainer,
} from '../components/expressive/ExpressivePrimitives';

type AuxSection = 'fichas' | 'atrasos' | 'saidas' | 'portal';

export const FrequenciaScreen: React.FC<{
  token: string;
  accountId: number;
  fichas: FichaAvaliacaoEntry[];
  atrasos: AtrasoRecord[];
  saidas: SaidaRecord[];
  homeInfo: HomeInfo | null;
}> = ({ token, accountId, fichas, atrasos, saidas, homeInfo }) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const [section, setSection] = useState<AuxSection>('fichas');
  const [expandedFichaId, setExpandedFichaId] = useState<number | null>(null);
  const [fichaDetailsMap, setFichaDetailsMap] = useState<Record<number, FichaAvaliacaoDetail>>({});
  const [loadingFichaId, setLoadingFichaId] = useState<number | null>(null);
  const [fichaError, setFichaError] = useState<string | null>(null);

  const handleToggleFichaDetail = async (bimester: number, detailId: number | null) => {
    const idToFetch = detailId ?? bimester;
    if (expandedFichaId === idToFetch) {
      setExpandedFichaId(null);
      return;
    }
    setExpandedFichaId(idToFetch);
    setFichaError(null);

    if (fichaDetailsMap[idToFetch]) return;

    setLoadingFichaId(idToFetch);
    try {
      const res = await fetchFichaDetail(token, accountId, idToFetch);
      setFichaDetailsMap((prev) => ({ ...prev, [idToFetch]: res.data }));
    } catch (err) {
      setFichaError(
        err instanceof Error ? err.message : 'Não foi possível carregar os detalhes da ficha.'
      );
    } finally {
      setLoadingFichaId(null);
    }
  };

  const displayFichas: FichaAvaliacaoEntry[] =
    fichas.length > 0
      ? fichas
      : [1, 2, 3, 4].map((b) => ({
          schoolYear: new Date().getFullYear(),
          bimester: b,
          lastViewed: null,
          statusText: 'Consultar ficha conceitual no portal NSAC',
          available: true,
          detailId: b,
          detailUrl: `/nsac/aluno/ficha-avaliacao/${b}`,
        }));

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 18 }}>
      {/* Hero Banner */}
      <section
        style={{
          borderRadius: isMaterialYou ? 32 : 12,
          padding: isMaterialYou ? '22px 22px' : '18px 20px',
          backgroundColor: tokens.surfaceContainerLow,
          border: `1px solid ${tokens.outlineVariant}`,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 14, marginBottom: 16 }}>
          <IconShapeContainer size={isMaterialYou ? 50 : 42} active>
            <ClipboardList size={isMaterialYou ? 24 : 20} />
          </IconShapeContainer>
          <div>
            <div style={{ fontSize: '0.73rem', fontWeight: 700, color: tokens.primary, textTransform: 'uppercase' }}>
              Registros Oficiais • NSAC Online
            </div>
            <h1 style={{ margin: 0, fontSize: isMaterialYou ? '1.55rem' : '1.4rem', fontWeight: 800, letterSpacing: '-0.03em' }}>
              Fichas, Frequência & Portal CTI
            </h1>
          </div>
        </div>

        <EnhancedConnectedButtonGroup<AuxSection>
          value={section}
          onChange={setSection}
          options={[
            {
              value: 'fichas',
              label: 'Fichas de Avaliação',
              badge: displayFichas.length,
              icon: <ClipboardList size={15} />,
            },
            {
              value: 'atrasos',
              label: 'Atrasos',
              badge: atrasos.length,
              icon: <Clock size={15} />,
            },
            {
              value: 'saidas',
              label: 'Saídas Antecipadas',
              badge: saidas.length,
              icon: <DoorOpen size={15} />,
            },
            {
              value: 'portal',
              label: 'Portal NSAC',
              badge: homeInfo?.infoLinks?.length ?? 0,
              icon: <Globe size={15} />,
            },
          ]}
        />
      </section>

      {/* SECTION 1: FICHAS DE AVALIAÇÃO */}
      {section === 'fichas' && (
        <section style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          {displayFichas.map((entry, idx) => {
            const targetId = entry.detailId ?? entry.bimester;
            const isExpanded = expandedFichaId === targetId;
            const detail = fichaDetailsMap[targetId];
            const isLoading = loadingFichaId === targetId;

            return (
              <ExpressiveSegmentedCard
                key={`${entry.bimester}-${idx}`}
                index={idx}
                total={displayFichas.length}
                selected={isExpanded}
                onClick={() => handleToggleFichaDetail(entry.bimester, entry.detailId)}
                style={{ padding: '18px 20px' }}
              >
                <div
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    gap: 12,
                  }}
                >
                  <div style={{ display: 'flex', alignItems: 'center', gap: 14 }}>
                    <IconShapeContainer
                      size={44}
                      containerColor={
                        entry.available ? tokens.primaryContainer : tokens.surfaceContainerHighest
                      }
                      contentColor={
                        entry.available ? tokens.onPrimaryContainer : tokens.onSurfaceVariant
                      }
                      active={isExpanded}
                    >
                      {entry.available ? <CheckCircle2 size={19} /> : <Lock size={19} />}
                    </IconShapeContainer>

                    <div>
                      <div style={{ fontFamily: 'Outfit, sans-serif', fontWeight: 800, fontSize: '1.05rem' }}>
                        Ficha de Avaliação • {entry.bimester}º Bimestre
                        {entry.schoolYear ? ` (${entry.schoolYear})` : ''}
                      </div>
                      <div style={{ fontSize: '0.82rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                        {entry.statusText || 'Ficha conceitual do conselho de classe'}
                        {entry.lastViewed ? ` • Visualizado em ${entry.lastViewed}` : ''}
                      </div>
                    </div>
                  </div>

                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    <span
                      style={{
                        padding: '4px 10px',
                        borderRadius: 999,
                        fontSize: '0.73rem',
                        fontWeight: 700,
                        backgroundColor: entry.available
                          ? tokens.gradeSuccessContainer
                          : tokens.surfaceContainerHighest,
                        color: entry.available
                          ? tokens.onGradeSuccessContainer
                          : tokens.onSurfaceVariant,
                      }}
                    >
                      {entry.available ? 'Disponível' : 'Bloqueada'}
                    </span>
                    {isExpanded ? <ChevronUp size={18} /> : <ChevronDown size={18} />}
                  </div>
                </div>

                {/* Expanded Conceptual Ficha Details (T8RIN ExpandableItem pattern) */}
                {isExpanded && (
                  <div
                    onClick={(e) => e.stopPropagation()}
                    style={{
                      marginTop: 16,
                      paddingTop: 16,
                      borderTop: `1px solid ${tokens.outlineVariant}`,
                      display: 'flex',
                      flexDirection: 'column',
                      gap: 12,
                    }}
                  >
                    {isLoading && (
                      <div style={{ fontSize: '0.86rem', color: tokens.primary, fontWeight: 600 }}>
                        Carregando ficha de avaliação...
                      </div>
                    )}

                    {fichaError && (
                      <div
                        style={{
                          padding: '12px 14px',
                          borderRadius: 16,
                          backgroundColor: tokens.errorContainer,
                          color: tokens.onErrorContainer,
                          fontSize: '0.83rem',
                        }}
                      >
                        {fichaError}
                      </div>
                    )}

                    {detail && (
                      <>
                        {detail.title && (
                          <div style={{ fontWeight: 700, fontSize: '0.94rem', color: tokens.primary }}>
                            {detail.title}
                          </div>
                        )}

                        {detail.tables.length > 0 ? (
                          detail.tables.map((table, tIdx) => (
                            <div
                              key={tIdx}
                              style={{
                                overflowX: 'auto',
                                borderRadius: 16,
                                border: `1px solid ${tokens.outlineVariant}`,
                                backgroundColor: tokens.surfaceContainerLowest,
                              }}
                            >
                              <table
                                style={{
                                  width: '100%',
                                  borderCollapse: 'collapse',
                                  fontSize: '0.82rem',
                                }}
                              >
                                <tbody>
                                  {table.map((row, rIdx) => (
                                    <tr
                                      key={rIdx}
                                      style={{
                                        backgroundColor:
                                          rIdx === 0 ? tokens.surfaceContainer : 'transparent',
                                        fontWeight: rIdx === 0 ? 700 : 400,
                                        borderBottom:
                                          rIdx < table.length - 1
                                            ? `1px solid ${tokens.outlineVariant}`
                                            : 'none',
                                      }}
                                    >
                                      {row.map((cell, cIdx) => (
                                        <td key={cIdx} style={{ padding: '10px 12px' }}>
                                          {cell}
                                        </td>
                                      ))}
                                    </tr>
                                  ))}
                                </tbody>
                              </table>
                            </div>
                          ))
                        ) : (
                          <div style={{ fontSize: '0.84rem', color: tokens.onSurfaceVariant }}>
                            Nenhuma tabela conceitual publicada para este bimestre ainda.
                          </div>
                        )}

                        {detail.notes.length > 0 && (
                          <div
                            style={{
                              padding: '12px 14px',
                              borderRadius: 16,
                              backgroundColor: tokens.secondaryContainer,
                              color: tokens.onSecondaryContainer,
                              fontSize: '0.82rem',
                            }}
                          >
                            <div style={{ fontWeight: 700, marginBottom: 4 }}>
                              Observações do Conselho:
                            </div>
                            {detail.notes.map((n, nIdx) => (
                              <div key={nIdx}>• {n}</div>
                            ))}
                          </div>
                        )}
                      </>
                    )}
                  </div>
                )}
              </ExpressiveSegmentedCard>
            );
          })}
        </section>
      )}

      {/* SECTION 2: ATRASOS */}
      {section === 'atrasos' && (
        <section style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <div
            style={{
              padding: '14px 18px',
              borderRadius: 22,
              backgroundColor: tokens.secondaryContainer,
              color: tokens.onSecondaryContainer,
              display: 'flex',
              alignItems: 'center',
              gap: 12,
              fontSize: '0.84rem',
            }}
          >
            <Info size={18} style={{ flexShrink: 0 }} />
            <span>
              Regimento Disciplinar CTI/UNESP: o controle de pontualidade registra entradas após o
              início do primeiro tempo de aula.
            </span>
          </div>

          {atrasos.length === 0 ? (
            <div
              style={{
                padding: 36,
                textAlign: 'center',
                borderRadius: 28,
                backgroundColor: tokens.surfaceContainerLow,
                border: `1px solid ${tokens.outlineVariant}`,
              }}
            >
              <CheckCircle2 size={32} color={tokens.gradeSuccess} style={{ marginBottom: 8 }} />
              <div style={{ fontWeight: 800, fontSize: '1.05rem' }}>
                Zero atrasos registrados!
              </div>
              <div style={{ fontSize: '0.84rem', color: tokens.onSurfaceVariant, marginTop: 4 }}>
                Sua ficha de pontualidade no NSAC Online está limpa.
              </div>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {atrasos.map((item, idx) => (
                <ExpressiveSegmentedCard
                  key={`${item.date}-${idx}`}
                  index={idx}
                  total={atrasos.length}
                  style={{ padding: '16px 18px' }}
                >
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                      <IconShapeContainer
                        size={42}
                        containerColor={tokens.gradeWarningContainer}
                        contentColor={tokens.onGradeWarningContainer}
                      >
                        <Clock size={18} />
                      </IconShapeContainer>
                      <div>
                        <div style={{ fontWeight: 700, fontSize: '0.98rem' }}>
                          {item.date} {item.time ? `• ${item.time}` : ''}
                        </div>
                        <div style={{ fontSize: '0.82rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                          {item.subject ? `Disciplina: ${item.subject}` : ''}
                          {item.reason ? ` • Motivo: ${item.reason}` : ''}
                          {!item.subject && !item.reason && item.rawColumns.length > 0
                            ? item.rawColumns.join(' • ')
                            : ''}
                        </div>
                      </div>
                    </div>
                  </div>
                </ExpressiveSegmentedCard>
              ))}
            </div>
          )}
        </section>
      )}

      {/* SECTION 3: SAÍDAS ANTECIPADAS */}
      {section === 'saidas' && (
        <section style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          {saidas.length === 0 ? (
            <div
              style={{
                padding: 36,
                textAlign: 'center',
                borderRadius: 28,
                backgroundColor: tokens.surfaceContainerLow,
                border: `1px solid ${tokens.outlineVariant}`,
              }}
            >
              <DoorOpen size={32} color={tokens.primary} style={{ marginBottom: 8 }} />
              <div style={{ fontWeight: 800, fontSize: '1.05rem' }}>
                Nenhuma saída antecipada registrada
              </div>
              <div style={{ fontSize: '0.84rem', color: tokens.onSurfaceVariant, marginTop: 4 }}>
                Histórico de autorizações da portaria/coordenação sem ocorrências.
              </div>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {saidas.map((item, idx) => (
                <ExpressiveSegmentedCard
                  key={`${item.date}-${idx}`}
                  index={idx}
                  total={saidas.length}
                  style={{ padding: '16px 18px' }}
                >
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                      <IconShapeContainer size={42}>
                        <UserCheck size={18} />
                      </IconShapeContainer>
                      <div>
                        <div style={{ fontWeight: 700, fontSize: '0.98rem' }}>
                          {item.date} {item.time ? `• ${item.time}` : ''}
                        </div>
                        <div style={{ fontSize: '0.82rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                          {item.reason ? `Motivo: ${item.reason}` : ''}
                          {item.authorizedBy ? ` • Autorizado por: ${item.authorizedBy}` : ''}
                          {!item.reason && !item.authorizedBy && item.rawColumns.length > 0
                            ? item.rawColumns.join(' • ')
                            : ''}
                        </div>
                      </div>
                    </div>
                  </div>
                </ExpressiveSegmentedCard>
              ))}
            </div>
          )}
        </section>
      )}

      {/* SECTION 4: PORTAL NSAC HOME & LINKS INSTITUCIONAIS */}
      {section === 'portal' && (
        <section style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
          <div
            style={{
              padding: '16px 20px',
              borderRadius: 24,
              backgroundColor: tokens.primaryContainer,
              color: tokens.onPrimaryContainer,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              flexWrap: 'wrap',
              gap: 12,
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
              <Calendar size={20} />
              <div>
                <div style={{ fontSize: '0.72rem', fontWeight: 700, textTransform: 'uppercase' }}>
                  Data Oficial do Servidor NSAC
                </div>
                <div style={{ fontFamily: 'Outfit, sans-serif', fontWeight: 800, fontSize: '1.1rem' }}>
                  {homeInfo?.portalDate ?? 'Conectado ao Portal CTI UNESP Bauru'}
                </div>
              </div>
            </div>
            <span
              style={{
                padding: '4px 12px',
                borderRadius: 999,
                fontSize: '0.75rem',
                fontWeight: 700,
                backgroundColor: tokens.onPrimaryContainer,
                color: tokens.primaryContainer,
              }}
            >
              {homeInfo?.availableRoutes?.length ?? 5} rotas ativas
            </span>
          </div>

          {homeInfo?.infoLinks && homeInfo.infoLinks.length > 0 ? (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {homeInfo.infoLinks.map((link, idx) => (
                <ExpressiveSegmentedCard
                  key={`${link.url}-${idx}`}
                  index={idx}
                  total={homeInfo.infoLinks.length}
                  onClick={() => window.open(link.url, '_blank', 'noopener,noreferrer')}
                  style={{ padding: '16px 18px' }}
                >
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
                    <div>
                      <div style={{ fontWeight: 700, fontSize: '0.98rem' }}>{link.title}</div>
                      {link.description && (
                        <div style={{ fontSize: '0.82rem', color: tokens.onSurfaceVariant, marginTop: 2 }}>
                          {link.description}
                        </div>
                      )}
                      <div style={{ fontSize: '0.75rem', color: tokens.primary, marginTop: 4 }}>
                        {link.url}
                      </div>
                    </div>
                    <EnhancedMorphButton size="sm" variant="tonal" icon={<ExternalLink size={14} />}>
                      Abrir
                    </EnhancedMorphButton>
                  </div>
                </ExpressiveSegmentedCard>
              ))}
            </div>
          ) : (
            <div
              style={{
                padding: 28,
                borderRadius: 24,
                backgroundColor: tokens.surfaceContainerLow,
                border: `1px solid ${tokens.outlineVariant}`,
                fontSize: '0.86rem',
                color: tokens.onSurfaceVariant,
              }}
            >
              Nenhum link institucional adicional disponível no momento.
            </div>
          )}
        </section>
      )}
    </div>
  );
};
