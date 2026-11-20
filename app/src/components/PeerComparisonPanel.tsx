import React from 'react';
import { Equal, TrendingDown, TrendingUp } from 'lucide-react';
import type { PeerComparisonDetail, SubjectComparisonRow } from '../api/types';
import { useM3Theme } from '../theme/ThemeContext';
import {
  AnimatedNumber,
  ExpressiveGradeCompareBar,
  ExpressiveSegmentedCard,
  IconShapeContainer,
} from './expressive/ExpressivePrimitives';
import { MetricTile } from './expressive/ExpressiveModal';
import { renderSubjectIcon } from '../screens/BoletimScreen';

const plural = (n: number) => (n === 1 ? 'matéria' : 'matérias');

/** Backend treats |delta| <= 0.05 as a tie; mirror it so chips match the scoreboard. */
const TIE_EPSILON = 0.05;

const sectionLabelStyle = (color: string): React.CSSProperties => ({
  fontSize: '0.78rem',
  fontWeight: 800,
  color,
  margin: '20px 0 10px',
  textTransform: 'uppercase',
});

const DeltaChip: React.FC<{ delta: number }> = ({ delta }) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const tie = Math.abs(delta) <= TIE_EPSILON;
  const ahead = delta > 0;
  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: 4,
        padding: '4px 10px',
        borderRadius: isMaterialYou ? 999 : 8,
        fontSize: '0.76rem',
        fontWeight: 800,
        flexShrink: 0,
        fontVariantNumeric: 'tabular-nums',
        backgroundColor: tie ? tokens.surfaceContainerHighest : ahead ? tokens.gradeSuccessContainer : tokens.errorContainer,
        color: tie ? tokens.onSurfaceVariant : ahead ? tokens.onGradeSuccessContainer : tokens.onErrorContainer,
      }}
    >
      {tie ? <Equal size={13} /> : ahead ? <TrendingUp size={13} /> : <TrendingDown size={13} />}
      {tie ? 'Empate' : `${ahead ? '+' : ''}${delta.toFixed(2)}`}
    </span>
  );
};

/**
 * Head-to-head body shared by the comparison popup and its full-screen page:
 * scoreboard tiles, per-subject duel bars, and subjects without a comparable grade.
 */
export const PeerComparisonContent: React.FC<{ comparison: PeerComparisonDetail; wide?: boolean }> = ({
  comparison,
  wide = false,
}) => {
  const { tokens, isMaterialYou } = useM3Theme();
  const comparable = comparison.subjects.filter((r) => r.my_average !== null && r.peer_average !== null);
  const unmatched = comparison.subjects.filter((r) => r.my_average === null || r.peer_average === null);
  const avg = (pick: (r: SubjectComparisonRow) => number) =>
    comparable.length ? comparable.reduce((s, r) => s + pick(r), 0) / comparable.length : null;
  const myAvg = avg((r) => r.my_average!);
  const peerAvg = avg((r) => r.peer_average!);

  if (comparison.subjects.length === 0) {
    return (
      <div style={{ marginTop: 18, padding: 28, textAlign: 'center', borderRadius: isMaterialYou ? 24 : 12, backgroundColor: tokens.surfaceContainer }}>
        <div style={{ fontWeight: 700 }}>Nenhuma nota publicada neste período</div>
        <div style={{ fontSize: '0.82rem', color: tokens.onSurfaceVariant, marginTop: 4 }}>
          Troque o bimestre para comparar outro período.
        </div>
      </div>
    );
  }

  return (
    <>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(140px, 1fr))', gap: 10, marginTop: 18 }}>
        <MetricTile
          label="Média em comum"
          value={myAvg !== null ? <AnimatedNumber value={myAvg} decimals={2} /> : '—'}
          caption={`Colega: ${peerAvg !== null ? peerAvg.toFixed(2) : '—'}`}
          background={tokens.primaryContainer}
          color={tokens.onPrimaryContainer}
        />
        <MetricTile
          label={<><TrendingUp size={13} /><span>Você à frente</span></>}
          value={<AnimatedNumber value={comparison.my_wins} decimals={0} />}
          caption={plural(comparison.my_wins)}
          background={tokens.gradeSuccessContainer}
          color={tokens.onGradeSuccessContainer}
        />
        <MetricTile
          label={<><Equal size={13} /><span>Empates</span></>}
          value={<AnimatedNumber value={comparison.ties} decimals={0} />}
          caption={plural(comparison.ties)}
          background={tokens.surfaceContainerHigh}
          color={tokens.onSurface}
          borderColor={tokens.outlineVariant}
          labelColor={tokens.onSurfaceVariant}
          captionColor={tokens.onSurfaceVariant}
        />
        <MetricTile
          label={<><TrendingDown size={13} /><span>Colega à frente</span></>}
          value={<AnimatedNumber value={comparison.peer_wins} decimals={0} />}
          caption={plural(comparison.peer_wins)}
          background={tokens.errorContainer}
          color={tokens.onErrorContainer}
        />
      </div>

      {comparable.length > 0 && (
        <>
          <div style={sectionLabelStyle(tokens.primary)}>Matéria por matéria</div>
          <div
            style={{
              display: wide ? 'grid' : 'flex',
              flexDirection: 'column',
              gridTemplateColumns: 'repeat(auto-fill, minmax(min(100%, 340px), 1fr))',
              gap: wide ? 8 : 4,
            }}
          >
            {comparable.map((row, idx) => (
              <ExpressiveSegmentedCard
                key={row.subject_name}
                index={wide ? 0 : idx}
                total={wide ? 1 : comparable.length}
                staggerIndex={idx}
                style={{ padding: '14px 16px' }}
              >
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 12 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0 }}>
                    <IconShapeContainer size={34}>{renderSubjectIcon(row.subject_name, 16)}</IconShapeContainer>
                    <div style={{ minWidth: 0 }}>
                      <div
                        style={{
                          fontWeight: 700,
                          fontSize: '0.92rem',
                          overflow: 'hidden',
                          textOverflow: 'ellipsis',
                          whiteSpace: 'nowrap',
                        }}
                      >
                        {row.subject_name}
                      </div>
                      <div style={{ fontSize: '0.72rem', color: tokens.onSurfaceVariant }}>
                        Faltas {row.my_absences} × {row.peer_absences}
                        {row.class_average !== null && ` • Sala ${row.class_average.toFixed(1)}`}
                      </div>
                    </div>
                  </div>
                  <DeltaChip delta={row.delta_me_vs_peer ?? row.my_average! - row.peer_average!} />
                </div>
                <ExpressiveGradeCompareBar
                  personalGrade={row.my_average}
                  classAverage={row.peer_average}
                  labels={['Você', 'Colega']}
                  showValues
                />
              </ExpressiveSegmentedCard>
            ))}
          </div>
        </>
      )}

      {unmatched.length > 0 && (
        <>
          <div style={sectionLabelStyle(tokens.onSurfaceVariant)}>Sem comparação ({unmatched.length})</div>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
            {unmatched.map((row) => (
              <span
                key={row.subject_name}
                style={{
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 6,
                  maxWidth: '100%',
                  padding: '6px 12px',
                  borderRadius: isMaterialYou ? 999 : 8,
                  fontSize: '0.78rem',
                  backgroundColor: tokens.surfaceContainer,
                  border: `1px solid ${tokens.outlineVariant}`,
                  color: tokens.onSurfaceVariant,
                }}
              >
                {renderSubjectIcon(row.subject_name, 13)}
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: tokens.onSurface, fontWeight: 600 }}>
                  {row.subject_name}
                </span>
                <span style={{ flexShrink: 0 }}>
                  {row.my_average !== null ? '· só você' : row.peer_average !== null ? '· só colega' : '· sem notas'}
                </span>
              </span>
            ))}
          </div>
          <div style={{ fontSize: '0.74rem', color: tokens.onSurfaceVariant, marginTop: 8 }}>
            Matérias sem nota de um dos dois ficam fora do placar.
          </div>
        </>
      )}
    </>
  );
};
