#!/usr/bin/env node
// Local-only n²sac Dev DB & Election Simulator Server (127.0.0.1:18081).
// NEVER used in production. Manages synthetic hashed bots in `n2sac_dev` Postgres
// and casts/revokes votes through the REAL `nsac-server` HTTP API (127.0.0.1:18080)
// so SSE broadcasts, quorum math, and representative elections react in real time.

import http from 'node:http';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PANEL_JS_PATH = path.join(__dirname, 'panel.js');

const PORT = Number(process.env.DB_CONTROL_PORT || 18081);
const API_BASE = process.env.N2SAC_DEV_API || 'http://127.0.0.1:18080';
const PG_CONTAINER = process.env.PG_CONTAINER || 'n2sac-dev-pg';
const DB_NAME = process.env.PG_DB || 'n2sac_dev';

const sha256 = (s) => crypto.createHash('sha256').update(s).digest('hex');

function tokenForAccount(id) {
  if (id === 1) return `nsac_usr_${'a'.repeat(64)}`;
  if (id === 2) return `nsac_usr_${'b'.repeat(64)}`;
  return `nsac_usr_${sha256(`n2sac-dev-bot-token-${id}`)}`;
}

function psql(sql) {
  return execFileSync(
    'docker',
    ['exec', '-i', PG_CONTAINER, 'psql', '-q', '-U', 'postgres', '-d', DB_NAME, '-v', 'ON_ERROR_STOP=1', '-t', '-A'],
    { input: sql, encoding: 'utf8' }
  ).trim();
}

function psqlJson(sql) {
  const out = psql(`SELECT COALESCE(json_agg(t), '[]'::json) FROM (${sql}) t;`);
  return JSON.parse(out || '[]');
}

function ensureBotSession(id) {
  if (id <= 2) return;
  const tokHash = sha256(tokenForAccount(id));
  psql(`
    INSERT INTO user_sessions (account_id, token_hash, expires_at)
    VALUES (${id}, '${tokHash}', NOW() + INTERVAL '365 days')
    ON CONFLICT DO NOTHING;

    INSERT INTO account_ranking_preferences (account_id, opted_in, hide_name, has_decided, public_profile, discoverable)
    VALUES (${id}, TRUE, FALSE, TRUE, TRUE, TRUE)
    ON CONFLICT DO NOTHING;

    INSERT INTO accounts_school_years (account_id, school_year_id, status)
    SELECT ${id}, sy.id, CASE WHEN sy.year = 2025 THEN 'Aprovado' ELSE 'Em Curso' END
    FROM school_years sy
    ON CONFLICT DO NOTHING;
  `);
}

async function nsacApi(accountId, method, path) {
  ensureBotSession(accountId);
  const res = await fetch(`${API_BASE}/api/v1${path}`, {
    method,
    headers: { Authorization: `Bearer ${tokenForAccount(accountId)}` },
  });
  const json = await res.json().catch(() => ({}));
  return { ok: res.ok, status: res.status, json };
}

async function resetToMeAndOneBot() {
  psql(`
    DELETE FROM class_representative_votes;
    DELETE FROM class_representatives;
    DELETE FROM accounts WHERE id > 2;
    SELECT setval('accounts_id_seq', GREATEST(2, COALESCE((SELECT MAX(id) FROM accounts), 2)));
  `);
  // Ensure account 2 has a clean hashed bot profile if desired, keeping its session working
  return getState();
}

async function addRandomBots(count) {
  const n = Math.max(1, Math.min(25, Number(count) || 1));
  const created = [];
  for (let i = 0; i < n; i++) {
    const h = crypto.randomBytes(8).toString('hex');
    const shortHash = h.slice(0, 6);
    const emailHash = h.slice(0, 8);
    const passHash = sha256(`pass-${h}`);
    const bioHash = sha256(`bio-${h}`).slice(0, 12);

    const newIdStr = psql(`
      INSERT INTO accounts (nsac_email, nsac_pass_enc, class_code)
      VALUES ('bot.${emailHash}@aluno.example', '${passHash}', 'INI3')
      RETURNING id;
    `);
    const id = Number.parseInt(newIdStr.split(/\s+/)[0], 10);
    if (!Number.isInteger(id) || id <= 0) {
      throw new Error(`Failed to parse new bot id from psql output: ${JSON.stringify(newIdStr)}`);
    }
    const tok = tokenForAccount(id);
    const tokHash = sha256(tok);

    psql(`
      INSERT INTO user_sessions (account_id, token_hash, expires_at)
      VALUES (${id}, '${tokHash}', NOW() + INTERVAL '365 days')
      ON CONFLICT DO NOTHING;

      INSERT INTO account_ranking_preferences (account_id, opted_in, hide_name, has_decided, public_profile, discoverable)
      VALUES (${id}, TRUE, FALSE, TRUE, TRUE, TRUE)
      ON CONFLICT DO NOTHING;

      INSERT INTO account_profiles (account_id, display_name, bio)
      VALUES (${id}, 'Bot #${shortHash}', 'hash:${bioHash}')
      ON CONFLICT (account_id) DO UPDATE SET display_name = EXCLUDED.display_name, bio = EXCLUDED.bio;

      INSERT INTO accounts_school_years (account_id, school_year_id, status)
      SELECT ${id}, sy.id, CASE WHEN sy.year = 2025 THEN 'Aprovado' ELSE 'Em Curso' END
      FROM school_years sy
      ON CONFLICT DO NOTHING;
    `);
    created.push({ id, display_name: `Bot #${shortHash}`, email: `bot.${emailHash}@aluno.example` });
  }
  return { created, state: await getState() };
}

async function deleteBot(id) {
  const numId = Number(id);
  if (!Number.isInteger(numId) || numId <= 1) {
    throw new Error('Cannot delete account id <= 1');
  }
  psql(`DELETE FROM accounts WHERE id = ${numId};`);
  // Trigger a harmless read on /classrooms/me so server state stays fresh
  return getState();
}

async function getState() {
  const accounts = psqlJson(`
    SELECT
      a.id,
      a.nsac_email,
      a.class_code,
      COALESCE(p.display_name, split_part(a.nsac_email, '@', 1)) AS label,
      COALESCE(p.bio, '') AS bio
    FROM accounts a
    LEFT JOIN account_profiles p ON p.account_id = a.id
    WHERE UPPER(a.class_code) = 'INI3'
    ORDER BY a.id ASC
  `);

  const votes = psqlJson(`
    SELECT voter_id, candidate_id
    FROM class_representative_votes
    ORDER BY created_at ASC
  `);

  const reps = psqlJson(`
    SELECT account_id, vote_count
    FROM class_representatives
    ORDER BY vote_count DESC, reached_threshold_at ASC
  `);

  const overviewRes = await nsacApi(1, 'GET', '/classrooms/me');
  const overview = overviewRes.ok ? overviewRes.json?.data : null;

  return {
    class_code: 'INI3',
    accounts,
    votes,
    reps,
    overview,
  };
}

async function handleBulk(action) {
  const st = await getState();
  const bots = st.accounts.filter((a) => a.id > 1);
  const allIds = st.accounts.map((a) => a.id);

  if (action === 'clear_votes') {
    for (const v of st.votes) {
      await nsacApi(v.voter_id, 'DELETE', `/classrooms/me/representatives/votes/${v.candidate_id}`);
    }
    return getState();
  }

  if (action === 'all_vote_me') {
    for (const b of bots) {
      await nsacApi(b.id, 'POST', `/classrooms/me/representatives/votes/1`);
    }
    return getState();
  }

  if (action === 'random_votes') {
    for (const b of bots) {
      const eligible = allIds.filter((cid) => cid !== b.id);
      if (eligible.length === 0) continue;
      const pick = eligible[Math.floor(Math.random() * eligible.length)];
      await nsacApi(b.id, 'POST', `/classrooms/me/representatives/votes/${pick}`);
    }
    return getState();
  }

  throw new Error(`Unknown bulk action: ${action}`);
}

const PANEL_JS = `(() => {
  const CTRL = "http://localhost:${PORT}";
  const oldWrap = document.getElementById("n2sac-db-ctrl-root");
  if (oldWrap) oldWrap.remove();
  const oldQa = document.getElementById("n2sac-qa-showcase-bar");
  if (oldQa) oldQa.remove();

  const root = document.createElement("div");
  root.id = "n2sac-db-ctrl-root";
  document.body.appendChild(root);

  const fab = document.createElement("button");
  fab.textContent = "🗄️ Banco & Bots";
  Object.assign(fab.style, {
    position: "fixed",
    right: "12px",
    bottom: "88px",
    zIndex: "99998",
    padding: "10px 14px",
    borderRadius: "999px",
    border: "1px solid rgba(250, 204, 21, 0.7)",
    background: "rgba(18, 18, 24, 0.94)",
    color: "#FDE047",
    fontWeight: "800",
    fontSize: "12px",
    boxShadow: "0 8px 24px rgba(0,0,0,0.5)",
    cursor: "pointer",
    fontFamily: "system-ui, sans-serif",
  });
  root.appendChild(fab);

  const drawer = document.createElement("div");
  Object.assign(drawer.style, {
    position: "fixed",
    inset: "0",
    zIndex: "99999",
    background: "rgba(10, 11, 15, 0.97)",
    color: "#F3F4F6",
    display: "none",
    flexDirection: "column",
    fontFamily: "system-ui, -apple-system, sans-serif",
    paddingTop: "env(safe-area-inset-top, 28px)",
    paddingBottom: "env(safe-area-inset-bottom, 16px)",
  });
  root.appendChild(drawer);

  let state = null;
  let busy = false;
  let statusMsg = "Pronto";

  const req = async (method, path, body) => {
    busy = true;
    render();
    try {
      const r = await fetch(CTRL + path, {
        method,
        headers: body ? { "Content-Type": "application/json" } : {},
        body: body ? JSON.stringify(body) : undefined,
      });
      const data = await r.json();
      if (!r.ok) throw new Error(data.error || ("HTTP " + r.status));
      state = data.state || data;
      statusMsg = "Atualizado • " + new Date().toLocaleTimeString();
    } catch (e) {
      statusMsg = "Erro: " + String(e.message || e);
    } finally {
      busy = false;
      render();
    }
  };

  const render = () => {
    const accounts = state?.accounts || [];
    const votes = state?.votes || [];
    const reps = state?.reps || [];
    const repIds = new Set(reps.map(r => r.account_id));
    const voteCounts = {};
    for (const v of votes) voteCounts[v.candidate_id] = (voteCounts[v.candidate_id] || 0) + 1;
    const minVotes = state?.overview?.election?.min_votes_to_elect ?? state?.overview?.min_votes_to_elect ?? Math.max(2, Math.ceil(accounts.length * 0.35));

    drawer.innerHTML = "";

    // Header
    const header = document.createElement("div");
    Object.assign(header.style, {
      display: "flex", alignItems: "center", justifyContent: "space-between",
      padding: "14px 16px", borderBottom: "1px solid rgba(255,255,255,0.1)", gap: "8px"
    });
    header.innerHTML = \`
      <div>
        <div style="font-size:11px;font-weight:800;color:#FACC15;letter-spacing:.06em;text-transform:uppercase">Simulador Real • Postgres Local</div>
        <div style="font-size:17px;font-weight:900;margin-top:2px">Controle do Banco (INI3)</div>
      </div>
    \`;
    const closeBtn = document.createElement("button");
    closeBtn.textContent = "Ver App ✕";
    Object.assign(closeBtn.style, {
      padding: "8px 12px", borderRadius: "999px", border: "1px solid rgba(255,255,255,0.2)",
      background: "rgba(255,255,255,0.08)", color: "#fff", fontWeight: "800", fontSize: "12px", cursor: "pointer"
    });
    closeBtn.onclick = () => { drawer.style.display = "none"; };
    header.appendChild(closeBtn);
    drawer.appendChild(header);

    // Body scroll area
    const body = document.createElement("div");
    Object.assign(body.style, { flex: "1", overflowY: "auto", padding: "12px 14px", display: "flex", flexDirection: "column", gap: "12px" });
    drawer.appendChild(body);

    // KPI Strip
    const kpi = document.createElement("div");
    Object.assign(kpi.style, { display: "grid", gridTemplateColumns: "repeat(3, 1fr)", gap: "8px" });
    kpi.innerHTML = \`
      <div style="background:rgba(255,255,255,0.06);border-radius:12px;padding:10px">
        <div style="font-size:10px;opacity:.7;font-weight:700">ALUNOS INI3</div>
        <div style="font-size:20px;font-weight:900">\${accounts.length}</div>
      </div>
      <div style="background:rgba(255,255,255,0.06);border-radius:12px;padding:10px">
        <div style="font-size:10px;opacity:.7;font-weight:700">QUÓRUM MÍN.</div>
        <div style="font-size:20px;font-weight:900;color:#60A5FA">\${minVotes}</div>
      </div>
      <div style="background:rgba(245,158,11,0.14);border:1px solid rgba(250,204,21,0.35);border-radius:12px;padding:10px">
        <div style="font-size:10px;color:#FDE047;font-weight:700">ELEITOS</div>
        <div style="font-size:20px;font-weight:900;color:#FDE047">\${reps.length}/3</div>
      </div>
    \`;
    body.appendChild(kpi);

    // Action Buttons
    const actions = document.createElement("div");
    Object.assign(actions.style, { display: "flex", flexWrap: "wrap", gap: "6px" });
    const mkAct = (txt, bg, fn) => {
      const b = document.createElement("button");
      b.textContent = txt;
      b.disabled = busy;
      Object.assign(b.style, {
        padding: "8px 11px", borderRadius: "10px", border: "1px solid rgba(255,255,255,0.14)",
        background: bg, color: "#fff", fontSize: "12px", fontWeight: "800", cursor: "pointer",
        opacity: busy ? "0.55" : "1"
      });
      b.onclick = fn;
      actions.appendChild(b);
    };
    mkAct("+ 1 Bot Hash", "#1D4ED8", () => req("POST", "/bots/add", { count: 1 }));
    mkAct("+ 5 Bots Hash", "#1E40AF", () => req("POST", "/bots/add", { count: 5 }));
    mkAct("⚡ Bots votam em Mim", "#047857", () => req("POST", "/bulk", { action: "all_vote_me" }));
    mkAct("🎲 Votos Aleatórios", "#6D28D9", () => req("POST", "/bulk", { action: "random_votes" }));
    mkAct("🚫 Limpar Votos", "#9A3412", () => req("POST", "/bulk", { action: "clear_votes" }));
    mkAct("🧹 Reset (Eu + 1 Bot)", "#991B1B", () => req("POST", "/reset"));
    mkAct("🔄 Recarregar", "rgba(255,255,255,0.1)", () => req("GET", "/state"));
    body.appendChild(actions);

    const statusLine = document.createElement("div");
    statusLine.textContent = busy ? "⏳ Executando na API real..." : statusMsg;
    Object.assign(statusLine.style, { fontSize: "11px", color: "#9CA3AF", fontWeight: "600" });
    body.appendChild(statusLine);

    // Roster & Vote Matrix
    const listTitle = document.createElement("div");
    listTitle.textContent = "Membros da Turma & Controle de Votos (clique no candidato para votar/retirar):";
    Object.assign(listTitle.style, { fontSize: "12px", fontWeight: "800", color: "#93C5FD", marginTop: "4px" });
    body.appendChild(listTitle);

    for (const acc of accounts) {
      const myVotes = new Set(votes.filter(v => v.voter_id === acc.id).map(v => v.candidate_id));
      const received = voteCounts[acc.id] || 0;
      const isRep = repIds.has(acc.id);
      const isMe = acc.id === 1;

      const card = document.createElement("div");
      Object.assign(card.style, {
        background: isRep ? "rgba(245,158,11,0.10)" : "rgba(255,255,255,0.04)",
        border: isRep ? "1px solid rgba(250,204,21,0.6)" : "1px solid rgba(255,255,255,0.1)",
        borderRadius: "14px",
        padding: "10px 12px",
        display: "flex",
        flexDirection: "column",
        gap: "8px",
      });

      const top = document.createElement("div");
      Object.assign(top.style, { display: "flex", alignItems: "center", justifyContent: "space-between", gap: "8px" });
      const info = document.createElement("div");
      info.innerHTML = \`
        <div style="font-weight:800;font-size:13px;display:flex;align-items:center;gap:6px;flex-wrap:wrap">
          <span>#\${acc.id} \${isMe ? "Você" : acc.label}</span>
          \${isRep ? '<span style="background:#F59E0B;color:#111;padding:1px 6px;border-radius:999px;font-size:10px;font-weight:900">👑 REP</span>' : ''}
          <span style="background:rgba(96,165,250,0.2);color:#93C5FD;padding:1px 6px;border-radius:999px;font-size:10px;font-weight:800">\${received} voto(s)</span>
        </div>
        <div style="font-size:11px;opacity:.65;margin-top:2px;word-break:break-all">\${acc.nsac_email}</div>
      \`;
      top.appendChild(info);

      if (!isMe) {
        const del = document.createElement("button");
        del.textContent = "🗑️";
        del.disabled = busy;
        Object.assign(del.style, {
          padding: "6px 9px", borderRadius: "8px", border: "1px solid rgba(239,68,68,0.4)",
          background: "rgba(239,68,68,0.15)", color: "#FCA5A5", cursor: "pointer", fontSize: "12px"
        });
        del.onclick = () => req("DELETE", "/bots/" + acc.id);
        top.appendChild(del);
      }
      card.appendChild(top);

      // Vote chips for this voter (bots or you can cast/revoke up to 3 votes on any other student)
      const chipsWrap = document.createElement("div");
      Object.assign(chipsWrap.style, { display: "flex", flexWrap: "wrap", gap: "5px", alignItems: "center" });
      const lbl = document.createElement("span");
      lbl.textContent = \`Votos (\${myVotes.size}/3):\`;
      Object.assign(lbl.style, { fontSize: "11px", opacity: "0.7", fontWeight: "700", marginRight: "2px" });
      chipsWrap.appendChild(lbl);

      for (const cand of accounts) {
        if (cand.id === acc.id) continue;
        const voted = myVotes.has(cand.id);
        const chip = document.createElement("button");
        chip.disabled = busy;
        chip.textContent = (voted ? "✓ " : "+ ") + (cand.id === 1 ? "Você" : cand.label);
        Object.assign(chip.style, {
          padding: "5px 9px",
          borderRadius: "999px",
          fontSize: "11px",
          fontWeight: "800",
          cursor: "pointer",
          border: voted ? "1px solid #34D399" : "1px solid rgba(255,255,255,0.16)",
          background: voted ? "rgba(16,185,129,0.22)" : "rgba(255,255,255,0.05)",
          color: voted ? "#6EE7B7" : "#D1D5DB",
        });
        chip.onclick = () => req("POST", "/vote", {
          voter_id: acc.id,
          candidate_id: cand.id,
          action: voted ? "revoke" : "cast"
        });
        chipsWrap.appendChild(chip);
      }

      card.appendChild(chipsWrap);
      body.appendChild(card);
    }
  };

  fab.onclick = () => {
    drawer.style.display = "flex";
    req("GET", "/state");
  };

  // Auto-open immediately so the user sees the full DB control panel right away!
  drawer.style.display = "flex";
  req("GET", "/state");
})();`;

function readBody(req) {
  return new Promise((resolve, reject) => {
    let buf = '';
    req.on('data', (c) => (buf += c));
    req.on('end', () => {
      if (!buf) return resolve({});
      try {
        resolve(JSON.parse(buf));
      } catch (e) {
        reject(e);
      }
    });
    req.on('error', reject);
  });
}

const server = http.createServer(async (req, res) => {
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, DELETE, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');

  if (req.method === 'OPTIONS') {
    res.writeHead(204);
    res.end();
    return;
  }

  try {
    const url = new URL(req.url || '/', `http://127.0.0.1:${PORT}`);
    if (req.method === 'GET' && url.pathname === '/panel.js') {
      res.writeHead(200, { 'Content-Type': 'application/javascript; charset=utf-8' });
      res.end(readFileSync(PANEL_JS_PATH, 'utf8'));
      return;
    }

    if (req.method === 'GET' && url.pathname === '/state') {
      const data = await getState();
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(data));
      return;
    }

    if (req.method === 'POST' && url.pathname === '/reset') {
      const data = await resetToMeAndOneBot();
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(data));
      return;
    }

    if (req.method === 'POST' && url.pathname === '/bots/add') {
      const body = await readBody(req);
      const data = await addRandomBots(body.count);
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(data));
      return;
    }

    if (req.method === 'DELETE' && url.pathname.startsWith('/bots/')) {
      const id = Number(url.pathname.split('/').pop());
      const data = await deleteBot(id);
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(data));
      return;
    }

    if (req.method === 'POST' && url.pathname === '/vote') {
      const body = await readBody(req);
      const voterId = Number(body.voter_id);
      const candidateId = Number(body.candidate_id);
      const method = body.action === 'revoke' ? 'DELETE' : 'POST';
      const apiRes = await nsacApi(voterId, method, `/classrooms/me/representatives/votes/${candidateId}`);
      if (!apiRes.ok) {
        const msg = apiRes.json?.errors?.[0]?.user_message || apiRes.json?.errors?.[0]?.message || `HTTP ${apiRes.status}`;
        res.writeHead(apiRes.status, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: msg }));
        return;
      }
      const data = await getState();
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(data));
      return;
    }

    if (req.method === 'POST' && url.pathname === '/bulk') {
      const body = await readBody(req);
      const data = await handleBulk(body.action);
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(data));
      return;
    }

    res.writeHead(404, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'Not found' }));
  } catch (err) {
    res.writeHead(500, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: String(err?.message || err) }));
  }
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`n2sac Dev DB Control Server listening on http://127.0.0.1:${PORT}`);
});
