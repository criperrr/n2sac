#!/usr/bin/env node
// Local n²sac test helpers (synthetic users only).
//   node dev.mjs history          seed chat history through the real API
//   node dev.mjs bots             friends that read, type, reply, react and send GIFs
//   node dev.mjs login <user>     log the phone's WebView in as a test user (via adb + CDP)
//   node dev.mjs say <from> <to> <text…>   send one message as any test user
//   node dev.mjs photo <user>     give a test user a generated profile photo + banner
//   node dev.mjs report <from> <target> <media|text>   report as any test user
import { execSync } from 'node:child_process';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
import { readFileSync } from 'node:fs';

const API = process.env.N2SAC_DEV_API ?? 'http://127.0.0.1:18080';
const USERS = {
  voce: { id: 1, letter: 'a', email: 'voce.teste@aluno.example' },
  bruno: { id: 2, letter: 'b', email: 'bruno.lima@aluno.example' },
  carla: { id: 3, letter: 'c', email: 'carla.mendes@aluno.example' },
  diego: { id: 4, letter: 'd', email: 'diego.souza@aluno.example' },
  elisa: { id: 5, letter: 'e', email: 'elisa.rocha@aluno.example' },
  felipe: { id: 6, letter: 'f', email: 'felipe.costa@aluno.example' },
};
const tokenOf = (u) => `nsac_usr_${u.letter.repeat(64)}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const pick = (arr) => arr[Math.floor(Math.random() * arr.length)];

function giphyKey() {
  if (process.env.VITE_GIPHY_API_KEY) return process.env.VITE_GIPHY_API_KEY;
  try {
    const env = readFileSync(new URL('../../../front/.env.local', import.meta.url), 'utf8');
    return /VITE_GIPHY_API_KEY=(\S+)/.exec(env)?.[1] ?? '';
  } catch {
    return '';
  }
}

async function api(user, method, path, body) {
  const res = await fetch(`${API}/api/v1${path}`, {
    method,
    headers: { Authorization: `Bearer ${tokenOf(user)}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const json = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(`${method} ${path} → ${res.status} ${JSON.stringify(json.errors ?? json)}`);
  return json.data;
}

const send = (from, to, text, replyTo) =>
  api(from, 'POST', `/social/conversations/${to.id}/messages`, replyTo ? { text, reply_to_id: replyTo } : { text });

async function randomGif(query) {
  const key = giphyKey();
  if (!key) return null;
  const res = await fetch(
    `https://api.giphy.com/v1/gifs/search?api_key=${key}&q=${encodeURIComponent(query)}&limit=25&rating=pg-13&lang=pt`
  );
  const json = await res.json();
  const g = pick(json.data ?? []);
  if (!g) return null;
  return `giphy:g:${g.id}:${g.images.original.width}x${g.images.original.height}`;
}

async function history() {
  const { voce, bruno, carla } = USERS;
  const m1 = await send(bruno, voce, 'Oi! Já viu a nota de física? 👀');
  await send(bruno, voce, 'Saiu hoje cedo no NSAC');
  const r = await send(voce, bruno, 'Vi sim, tirei 8,5 😅', m1.message.id);
  await api(bruno, 'PUT', `/social/conversations/${voce.id}/messages/${r.message.id}/reaction`, { emoji: '🎉' });
  const gif = await randomGif('celebration');
  if (gif) await send(bruno, voce, gif);
  await send(bruno, voce, 'Bora estudar amanhã depois da aula?');
  await send(carla, voce, 'Oii, você tem o resumo de química?');
  await send(carla, voce, 'Prova é sexta 😭');
  console.log('Histórico criado: Bruno (respondido, reação, GIF) e Carla (2 mensagens não lidas).');
}

const REPLIES = [
  'Kkkkk verdade',
  'Sério?? 😮',
  'Bora!',
  'Pode crer 👍',
  'Agora não dá, tô no ônibus 🚌',
  'Me manda o resumo depois?',
  'Hahaha',
  'Faz sentido',
  'Vamos ver no que dá 🤞',
  'Tá bom, combinado!',
];
const GIF_TERMS = ['lol', 'yes', 'shocked', 'happy dance', 'thumbs up', 'study', 'tired', 'cat'];

/** Friends that react to what you send: read → "digitando…" → reply / reaction / GIF. */
async function bots() {
  const me = USERS.voce;
  const personas = [
    { user: USERS.bruno, typingMs: [1200, 2500], mode: 'chatty' },
    { user: USERS.carla, typingMs: [3500, 6000], mode: 'slow' },
    { user: USERS.diego, typingMs: [600, 900], mode: 'reactor' },
  ];
  const cursors = new Map();
  const handled = new Set();
  for (const p of personas) {
    const first = await api(p.user, 'GET', `/social/conversations/${me.id}/messages?limit=1`);
    cursors.set(p.user.id, first.next_rev ?? 0);
  }
  console.log('Robôs ativos: Bruno (responde, às vezes GIF ou citação), Carla (digita devagar), Diego (só reage). Ctrl+C para parar.');

  const handle = async (p, msg) => {
    if (msg.deleted) return;
    await sleep(700);
    if (p.mode === 'reactor') {
      await api(p.user, 'PUT', `/social/conversations/${me.id}/messages/${msg.id}/reaction`, { emoji: pick(['❤️', '😂', '🔥', '👍', '😮']) });
      console.log(`Diego reagiu a "${msg.text.slice(0, 30)}"`);
      return;
    }
    const typing = setInterval(() => void api(p.user, 'POST', `/social/conversations/${me.id}/typing`).catch(() => {}), 2500);
    await api(p.user, 'POST', `/social/conversations/${me.id}/typing`).catch(() => {});
    await sleep(p.typingMs[0] + Math.random() * (p.typingMs[1] - p.typingMs[0]));
    clearInterval(typing);
    const roll = Math.random();
    if (msg.text.startsWith('giphy:') || roll < 0.2) {
      const gif = await randomGif(pick(GIF_TERMS));
      if (gif) {
        await send(p.user, me, gif);
        console.log(`${p.user.email.split('.')[0]} mandou um GIF`);
        return;
      }
    }
    if (roll < 0.4) {
      await api(p.user, 'PUT', `/social/conversations/${me.id}/messages/${msg.id}/reaction`, { emoji: pick(['❤️', '😂', '👍']) });
    }
    const quote = roll > 0.75 ? msg.id : undefined;
    await send(p.user, me, pick(REPLIES), quote);
    console.log(`${p.user.email.split('.')[0]} respondeu${quote ? ' (citando)' : ''}`);
  };

  for (;;) {
    for (const p of personas) {
      try {
        const page = await api(p.user, 'GET', `/social/conversations/${me.id}/messages?after_rev=${cursors.get(p.user.id)}&limit=50`);
        const known = cursors.get(p.user.id);
        cursors.set(p.user.id, page.next_rev ?? known);
        // Only brand-new messages from you: reactions/edits bump `rev` on old ones too.
        for (const m of page.messages) {
          if (m.is_me || handled.has(m.id) || Date.now() - new Date(m.created_at).getTime() > 20000) continue;
          handled.add(m.id);
          void handle(p, m).catch((e) => console.error(String(e)));
        }
      } catch (e) {
        console.error(String(e));
      }
    }
    await sleep(1500);
  }
}

/** Writes the session into the WebView's localStorage through Chrome DevTools Protocol. */
async function login(name) {
  const user = USERS[name];
  if (!user) throw new Error(`Usuário desconhecido: ${name}. Opções: ${Object.keys(USERS).join(', ')}`);
  const sockets = execSync('adb shell cat /proc/net/unix').toString().match(/webview_devtools_remote_\d+/g);
  if (!sockets?.length) throw new Error('WebView do n²sac não encontrado. Abra o app no celular primeiro.');
  execSync(`adb forward tcp:9333 localabstract:${sockets[sockets.length - 1]}`);
  const pages = await (await fetch('http://127.0.0.1:9333/json')).json();
  const page = pages.find((p) => p.type === 'page' && p.url.includes('localhost:4173')) ?? pages.find((p) => p.type === 'page');
  if (!page) throw new Error('Nenhuma página no WebView.');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener('open', r));
  const session = {
    account_id: user.id,
    nsac_email: user.email,
    class_code: 'INI3',
    token: tokenOf(user),
    expires_at: new Date(Date.now() + 365 * 86400000).toISOString(),
  };
  const expression = `localStorage.setItem('nsac_expressive_session_v2', ${JSON.stringify(JSON.stringify(session))}); location.reload(); 'ok'`;
  ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression } }));
  await new Promise((r) => ws.addEventListener('message', r));
  ws.close();
  console.log(`Celular logado como ${name} (${user.email}).`);
}

/** Tiny valid PNG (solid color) as a data URL, built without dependencies. */
function solidPng(w, h, [r, g, b]) {
  const { deflateSync } = require('node:zlib');
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const byte of buf) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: w }, () => [r, g, b]).flat())]);
  const raw = Buffer.concat(Array.from({ length: h }, () => row));
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
  return `data:image/png;base64,${png.toString('base64')}`;
}

async function photo(name) {
  const user = USERS[name];
  const color = [Math.random() * 255, Math.random() * 255, Math.random() * 255].map(Math.round);
  const res = await api(user, 'PUT', '/account/profile', {
    display_name: null,
    bio: 'Perfil de teste',
    avatar_data_url: solidPng(400, 400, color),
    banner_data_url: solidPng(1600, 500, color.map((c) => 255 - c)),
  });
  console.log(`${name}: foto ${res.avatar_data_url?.length ?? 0} chars, banner ${res.banner_data_url?.length ?? 0} chars (re-encodados em JPEG)`);
}

const [cmd, ...args] = process.argv.slice(2);
const commands = {
  history,
  bots,
  login: () => login(args[0] ?? 'voce'),
  photo: () => photo(args[0] ?? 'bruno'),
  report: async () => {
    const out = await api(USERS[args[0]], 'POST', `/profiles/${USERS[args[1]].id}/report`, { kind: args[2] ?? 'media' });
    console.log(`${args[0]} denunciou ${args[1]} (${args[2] ?? 'media'}): ${out.outcome}`);
  },
  say: () => send(USERS[args[0]], USERS[args[1]], args.slice(2).join(' ')).then(() => console.log('enviado')),
};
if (!commands[cmd]) {
  console.log('Uso: node dev.mjs history | bots | login <usuario> | say <de> <para> <texto> | photo <usuario> | report <de> <alvo> <media|text>');
  process.exit(1);
}
await commands[cmd]();
