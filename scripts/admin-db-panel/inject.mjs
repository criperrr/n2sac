#!/usr/bin/env node
// Injects `scripts/dev-local/admin-db-panel.js` into the connected Android phone's WebView via CDP.
// Usage: node scripts/dev-local/inject-admin-panel.mjs [--reset]
import { execSync } from 'node:child_process';
import http from 'node:http';

const SERIAL = process.env.ANDROID_SERIAL || '7196e114';
const CDP_PORT = Number(process.env.CDP_PORT || 9336);
const CTRL_PORT = Number(process.env.DB_CONTROL_PORT || 18081);
const doReset = process.argv.includes('--reset');

execSync(`adb -s ${SERIAL} reverse tcp:${CTRL_PORT} tcp:${CTRL_PORT}`, { stdio: 'inherit' });
if (doReset) {
  execSync(`curl -sf -X POST http://127.0.0.1:${CTRL_PORT}/reset >/dev/null`, { stdio: 'inherit' });
  console.log('✓ Banco resetado: somente Você (#1) + 1 bot (#2) na turma INI3.');
}

const wpid = execSync(`adb -s ${SERIAL} shell pidof com.cti.n2sac`, { encoding: 'utf8' }).trim();
if (!wpid) {
  console.error('App com.cti.n2sac não está aberto no celular.');
  process.exit(1);
}
try {
  execSync(`adb -s ${SERIAL} forward --remove tcp:${CDP_PORT}`, { stdio: 'ignore' });
} catch {}
execSync(`adb -s ${SERIAL} forward tcp:${CDP_PORT} localabstract:webview_devtools_remote_${wpid}`);

http.get(`http://127.0.0.1:${CDP_PORT}/json`, (res) => {
  let body = '';
  res.on('data', (c) => (body += c));
  res.on('end', async () => {
    const pages = JSON.parse(body);
    const page = pages.find((p) => p.url && p.url.includes('localhost:4173')) || pages[0];
    if (!page?.webSocketDebuggerUrl) {
      console.error('Nenhuma aba WebView encontrada via CDP.');
      process.exit(1);
    }
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((r) => ws.addEventListener('open', r));
    let id = 1;
    const send = (method, params = {}) =>
      new Promise((resolve) => {
        const curId = id++;
        const handler = (ev) => {
          const d = JSON.parse(ev.data);
          if (d.id === curId) {
            ws.removeEventListener('message', handler);
            resolve(d);
          }
        };
        ws.addEventListener('message', handler);
        ws.send(JSON.stringify({ id: curId, method, params }));
      });

    const r = await send('Runtime.evaluate', {
      expression: `(async () => {
        const code = await (await fetch("http://localhost:${CTRL_PORT}/panel.js")).text();
        eval(code);
        return "ADMIN_PANEL_INJECTED";
      })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    console.log('✓', r.result?.result?.value || r);
    ws.close();
    process.exit(0);
  });
});
