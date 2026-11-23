// Standalone administrative DB & Election Simulator UI overlay for local n²sac testing.
// Talks to `scripts/dev-local/db-control-server.mjs` on http://localhost:18081 (via `adb reverse`).
// NEVER shipped in production builds.
(() => {
  const CTRL = window.__N2SAC_DB_CTRL_URL__ || 'http://localhost:18081';

  const oldWrap = document.getElementById('n2sac-db-ctrl-root');
  if (oldWrap) oldWrap.remove();
  const oldQa = document.getElementById('n2sac-qa-showcase-bar');
  if (oldQa) oldQa.remove();

  const root = document.createElement('div');
  root.id = 'n2sac-db-ctrl-root';
  document.body.appendChild(root);

  const fab = document.createElement('button');
  fab.textContent = '🗄️ Admin DB';
  Object.assign(fab.style, {
    position: 'fixed',
    right: '12px',
    bottom: '88px',
    zIndex: '99998',
    padding: '10px 14px',
    borderRadius: '999px',
    border: '1px solid rgba(250, 204, 21, 0.72)',
    background: 'rgba(14, 16, 22, 0.94)',
    color: '#FDE047',
    fontWeight: '800',
    fontSize: '12px',
    boxShadow: '0 8px 24px rgba(0, 0, 0, 0.5)',
    cursor: 'pointer',
    fontFamily: 'system-ui, -apple-system, sans-serif',
  });
  root.appendChild(fab);

  const drawer = document.createElement('div');
  Object.assign(drawer.style, {
    position: 'fixed',
    inset: '0',
    zIndex: '99999',
    background: 'rgba(10, 11, 15, 0.98)',
    color: '#F3F4F6',
    display: 'none',
    flexDirection: 'column',
    fontFamily: 'system-ui, -apple-system, sans-serif',
    paddingTop: 'env(safe-area-inset-top, 28px)',
    paddingBottom: 'env(safe-area-inset-bottom, 16px)',
  });
  root.appendChild(drawer);

  let state = null;
  let busy = false;
  let statusMsg = 'Pronto';
  let searchFilter = '';

  const req = async (method, path, body) => {
    busy = true;
    render();
    try {
      const r = await fetch(CTRL + path, {
        method,
        headers: body ? { 'Content-Type': 'application/json' } : {},
        body: body ? JSON.stringify(body) : undefined,
      });
      const data = await r.json();
      if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
      state = data.state || data;
      statusMsg = `Sincronizado • ${new Date().toLocaleTimeString()}`;
    } catch (e) {
      statusMsg = `Erro: ${String(e?.message || e)}`;
    } finally {
      busy = false;
      render();
    }
  };

  const render = () => {
    const accounts = state?.accounts || [];
    const votes = state?.votes || [];
    const reps = state?.reps || [];
    const repIds = new Set(reps.map((r) => r.account_id));
    const voteCounts = {};
    for (const v of votes) {
      voteCounts[v.candidate_id] = (voteCounts[v.candidate_id] || 0) + 1;
    }

    const minVotes =
      state?.overview?.room?.quorum ??
      state?.overview?.election?.min_votes_to_elect ??
      Math.max(2, Math.ceil(accounts.length * 0.35));

    const prevScroll = drawer.querySelector('[data-admin-scroll]')?.scrollTop ?? 0;
    drawer.innerHTML = '';

    // Header
    const header = document.createElement('div');
    Object.assign(header.style, {
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'space-between',
      padding: '12px 16px',
      borderBottom: '1px solid rgba(255, 255, 255, 0.1)',
      gap: '8px',
      flexShrink: '0',
    });

    const titleWrap = document.createElement('div');
    titleWrap.innerHTML = `
      <div style="font-size:10px;font-weight:800;color:#FACC15;letter-spacing:.06em;text-transform:uppercase">
        Painel Administrativo Local • Postgres + API Real
      </div>
      <div style="font-size:16px;font-weight:900;margin-top:2px">
        Controle do Banco (${state?.class_code || 'INI3'})
      </div>
    `;
    header.appendChild(titleWrap);

    const closeBtn = document.createElement('button');
    closeBtn.textContent = 'Ver App ✕';
    Object.assign(closeBtn.style, {
      padding: '8px 12px',
      borderRadius: '999px',
      border: '1px solid rgba(255, 255, 255, 0.22)',
      background: 'rgba(255, 255, 255, 0.08)',
      color: '#fff',
      fontWeight: '800',
      fontSize: '12px',
      cursor: 'pointer',
      flexShrink: '0',
    });
    closeBtn.onclick = () => {
      drawer.style.display = 'none';
    };
    header.appendChild(closeBtn);
    drawer.appendChild(header);

    // Scrollable Body
    const body = document.createElement('div');
    body.setAttribute('data-admin-scroll', '1');
    Object.assign(body.style, {
      flex: '1',
      overflowY: 'auto',
      padding: '12px 14px 28px',
      display: 'flex',
      flexDirection: 'column',
      gap: '12px',
    });
    drawer.appendChild(body);

    // KPI Strip
    const kpi = document.createElement('div');
    Object.assign(kpi.style, {
      display: 'grid',
      gridTemplateColumns: 'repeat(4, minmax(0, 1fr))',
      gap: '6px',
    });
    kpi.innerHTML = `
      <div style="background:rgba(255,255,255,0.06);border-radius:12px;padding:8px 10px">
        <div style="font-size:9px;opacity:.7;font-weight:700">ALUNOS</div>
        <div style="font-size:18px;font-weight:900">${accounts.length}</div>
      </div>
      <div style="background:rgba(255,255,255,0.06);border-radius:12px;padding:8px 10px">
        <div style="font-size:9px;opacity:.7;font-weight:700">QUÓRUM</div>
        <div style="font-size:18px;font-weight:900;color:#60A5FA">${minVotes}</div>
      </div>
      <div style="background:rgba(245,158,11,0.14);border:1px solid rgba(250,204,21,0.35);border-radius:12px;padding:8px 10px">
        <div style="font-size:9px;color:#FDE047;font-weight:700">ELEITOS</div>
        <div style="font-size:18px;font-weight:900;color:#FDE047">${reps.length}/3</div>
      </div>
      <div style="background:rgba(255,255,255,0.06);border-radius:12px;padding:8px 10px">
        <div style="font-size:9px;opacity:.7;font-weight:700">VOTOS</div>
        <div style="font-size:18px;font-weight:900;color:#34D399">${votes.length}</div>
      </div>
    `;
    body.appendChild(kpi);

    // Bot Creation & Bulk Actions
    const actions = document.createElement('div');
    Object.assign(actions.style, { display: 'flex', flexWrap: 'wrap', gap: '6px' });
    const mkAct = (txt, bg, fn) => {
      const b = document.createElement('button');
      b.textContent = txt;
      b.disabled = busy;
      Object.assign(b.style, {
        padding: '8px 10px',
        borderRadius: '10px',
        border: '1px solid rgba(255, 255, 255, 0.14)',
        background: bg,
        color: '#fff',
        fontSize: '12px',
        fontWeight: '800',
        cursor: 'pointer',
        opacity: busy ? '0.55' : '1',
      });
      b.onclick = fn;
      actions.appendChild(b);
    };
    mkAct('+ 1 Bot Hash', '#1D4ED8', () => req('POST', '/bots/add', { count: 1 }));
    mkAct('+ 3 Bots Hash', '#1E40AF', () => req('POST', '/bots/add', { count: 3 }));
    mkAct('+ 5 Bots Hash', '#1E3A8A', () => req('POST', '/bots/add', { count: 5 }));
    mkAct('⚡ Todos votam em Mim', '#047857', () => req('POST', '/bulk', { action: 'all_vote_me' }));
    mkAct('🎲 Votos Aleatórios', '#6D28D9', () => req('POST', '/bulk', { action: 'random_votes' }));
    mkAct('🚫 Limpar Votos', '#9A3412', () => req('POST', '/bulk', { action: 'clear_votes' }));
    mkAct('🧹 Reset (Eu + 1 Bot)', '#991B1B', () => req('POST', '/reset'));
    mkAct('🔄 Atualizar', 'rgba(255,255,255,0.1)', () => req('GET', '/state'));
    body.appendChild(actions);

    const statusLine = document.createElement('div');
    statusLine.textContent = busy ? '⏳ Executando na API real do nsac-server...' : statusMsg;
    Object.assign(statusLine.style, { fontSize: '11px', color: '#9CA3AF', fontWeight: '600' });
    body.appendChild(statusLine);

    // Filter input when there are many bots
    if (accounts.length > 6) {
      const filterInput = document.createElement('input');
      filterInput.type = 'search';
      filterInput.placeholder = 'Filtrar bot por #id ou hash...';
      filterInput.value = searchFilter;
      Object.assign(filterInput.style, {
        width: '100%',
        padding: '8px 12px',
        borderRadius: '10px',
        border: '1px solid rgba(255,255,255,0.15)',
        background: 'rgba(255,255,255,0.06)',
        color: '#fff',
        fontSize: '12px',
        boxSizing: 'border-box',
      });
      filterInput.oninput = (e) => {
        searchFilter = e.target.value;
        render();
      };
      body.appendChild(filterInput);
    }

    const listTitle = document.createElement('div');
    listTitle.textContent = 'Membros & Controle Individual de Votos (toque no candidato para votar ou remover voto):';
    Object.assign(listTitle.style, { fontSize: '12px', fontWeight: '800', color: '#93C5FD' });
    body.appendChild(listTitle);

    const q = searchFilter.trim().toLowerCase();
    const visibleAccounts = q
      ? accounts.filter(
          (a) =>
            String(a.id).includes(q) ||
            String(a.label).toLowerCase().includes(q) ||
            String(a.nsac_email).toLowerCase().includes(q)
        )
      : accounts;

    for (const acc of visibleAccounts) {
      const myVotes = new Set(votes.filter((v) => v.voter_id === acc.id).map((v) => v.candidate_id));
      const received = voteCounts[acc.id] || 0;
      const isRep = repIds.has(acc.id);
      const isMe = acc.id === 1;

      const card = document.createElement('div');
      Object.assign(card.style, {
        background: isRep ? 'rgba(245, 158, 11, 0.10)' : 'rgba(255, 255, 255, 0.04)',
        border: isRep ? '1px solid rgba(250, 204, 21, 0.65)' : '1px solid rgba(255, 255, 255, 0.1)',
        borderRadius: '14px',
        padding: '10px 12px',
        display: 'flex',
        flexDirection: 'column',
        gap: '8px',
      });

      const top = document.createElement('div');
      Object.assign(top.style, {
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: '8px',
      });

      const info = document.createElement('div');
      info.innerHTML = `
        <div style="font-weight:800;font-size:13px;display:flex;align-items:center;gap:6px;flex-wrap:wrap">
          <span>#${acc.id} ${isMe ? 'Você' : acc.label}</span>
          ${
            isRep
              ? '<span style="background:#F59E0B;color:#111;padding:1px 6px;border-radius:999px;font-size:10px;font-weight:900">👑 REP</span>'
              : ''
          }
          <span style="background:rgba(96,165,250,0.2);color:#93C5FD;padding:1px 6px;border-radius:999px;font-size:10px;font-weight:800">${received} voto(s)</span>
        </div>
        <div style="font-size:11px;opacity:.65;margin-top:2px;word-break:break-all">${acc.nsac_email}</div>
      `;
      top.appendChild(info);

      if (!isMe) {
        const del = document.createElement('button');
        del.textContent = '🗑️';
        del.title = 'Remover bot do banco';
        del.disabled = busy;
        Object.assign(del.style, {
          padding: '6px 9px',
          borderRadius: '8px',
          border: '1px solid rgba(239, 68, 68, 0.45)',
          background: 'rgba(239, 68, 68, 0.15)',
          color: '#FCA5A5',
          cursor: 'pointer',
          fontSize: '12px',
          flexShrink: '0',
        });
        del.onclick = () => req('DELETE', `/bots/${acc.id}`);
        top.appendChild(del);
      }
      card.appendChild(top);

      // Vote chips for this voter
      const chipsWrap = document.createElement('div');
      Object.assign(chipsWrap.style, {
        display: 'flex',
        flexWrap: 'wrap',
        gap: '5px',
        alignItems: 'center',
      });
      const lbl = document.createElement('span');
      lbl.textContent = `Votos (${myVotes.size}/3):`;
      Object.assign(lbl.style, {
        fontSize: '11px',
        opacity: '0.7',
        fontWeight: '700',
        marginRight: '2px',
      });
      chipsWrap.appendChild(lbl);

      for (const cand of accounts) {
        if (cand.id === acc.id) continue;
        const voted = myVotes.has(cand.id);
        const chip = document.createElement('button');
        chip.disabled = busy;
        chip.textContent = (voted ? '✓ ' : '+ ') + (cand.id === 1 ? 'Você' : cand.label);
        Object.assign(chip.style, {
          padding: '5px 9px',
          borderRadius: '999px',
          fontSize: '11px',
          fontWeight: '800',
          cursor: 'pointer',
          border: voted ? '1px solid #34D399' : '1px solid rgba(255, 255, 255, 0.16)',
          background: voted ? 'rgba(16, 185, 129, 0.22)' : 'rgba(255, 255, 255, 0.05)',
          color: voted ? '#6EE7B7' : '#D1D5DB',
        });
        chip.onclick = () =>
          req('POST', '/vote', {
            voter_id: acc.id,
            candidate_id: cand.id,
            action: voted ? 'revoke' : 'cast',
          });
        chipsWrap.appendChild(chip);
      }

      card.appendChild(chipsWrap);
      body.appendChild(card);
    }

    body.scrollTop = prevScroll;
  };

  fab.onclick = () => {
    drawer.style.display = 'flex';
    req('GET', '/state');
  };

  // Auto-open on first inject so the admin controls are immediately ready
  drawer.style.display = 'flex';
  req('GET', '/state');
})();
