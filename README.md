# n²sac — backend

servidor e api oficial do **n²sac** pro NSAC Online (CTI/UNESP Bauru), escrito inteiro em Rust (`nsac-core` + `nsac-server`) com PostgreSQL, cache em memória (`moka`), sentinela de notas por turma e push via Firebase (FCM HTTP v1).

o frontend (React + Vite + Capacitor/Android) fica separado em [`criperrr/n2sac-front`](https://github.com/criperrr/n2sac-front) e o bot do Discord em [`criperrr/gemilona-discord-bot`](https://github.com/criperrr/gemilona-discord-bot) (branch `rust-v2`).

---

## o que tem aqui dentro

- **`nsac-core/`** — motor puro do NSAC: login com CSRF duplo, parser HTML das 5 rotas do NSAC (`home`, `boletim`, `atrasos`, `saidas`, `ficha-avaliacao`), criptografia AES-256-GCM + `zeroize`, filtros imutáveis sem acento (`NFD`) e hash SHA-256 da média da turma (`M.T.`) pro Sentinel. nao depende de axum, banco nem discord.
- **`nsac-server/`** — servidor HTTP em `axum 0.8`: auth dupla (sessão de usuário `nsac_usr_*` + API key de serviço `nsac_srv_*`), repositórios `sqlx`, semáforo pra nao derrubar o NSAC (`NsacSemaphore`), Sentinel `O(21)` com heurística histórica de postagem de notas, envio de push FCM v1 e realtime SSE pros canais de turma.
- **`migrations/`** — migrations SQL (`0001` até `0008`), rodam automaticamente no startup. nada de `CREATE TABLE` em runtime.
- **`e2e/`** — suite caixa-preta em Python (`pytest`) que sobe o binário real + Postgres + mocks do NSAC/FCM.
- **`scripts/`** — deploy image-only (`deploy-server.sh`), logs remotos (`server-logs.sh`), status (`server-status.sh`), telemetria (`extract-telemetry.sh`) e ambiente de teste local (`dev-local/`).

---

## pré-requisitos

pra subir em produção (jeito mais fácil):
- Docker + Docker Compose
- (opcional) conta no Firebase se quiser push no Android
- (opcional) bot no Discord Developer Portal se quiser o bot integrado
- (opcional) Tailscale Funnel ou Cloudflare Tunnel pra expor em HTTPS

pra rodar local/compilar na mão:
- Rust (Edition 2024 / toolchain estável recente) + `clang`/`lld`
- PostgreSQL 16
- Python 3 + `pytest` (só se for rodar o `e2e/`)

---

## como subir o servidor do zero (Docker)

### 1. clona e cria o `.env`

```bash
git clone https://github.com/criperrr/n2sac.git
cd n2sac
cp .env.example .env
```

### 2. gera a chave de criptografia (obrigatório)

toda senha do NSAC, cookie PHP e mensagem de chat vai criptografada em AES-256-GCM no banco. gera uma chave de 32 bytes (64 chars hex) e coloca no `ENCRYPTION_KEY` do `.env`:

```bash
openssl rand -hex 32
```

> nao perde essa chave depois que o banco tiver dados, senao o servidor nao consegue descriptografar as contas/mensagens ja salvas.

### 3. configura o `.env`

o mínimo que você precisa ajustar no `.env`:

```env
# banco (bate com o container postgres do docker-compose.yml)
POSTGRES_USER=n2sac
POSTGRES_PASSWORD=troca-essa-senha
POSTGRES_DB=nsac_server

# chave gerada no passo 2 (64 hex)
ENCRYPTION_KEY=<sua-chave-64-hex>

# opcional: chaves de serviço/admin no formato nome:segredo separadas por vírgula
SERVICE_API_KEYS=admin:um-segredo-forte

# cache (em prod deixa auth=60 e snapshot=900 pra aliviar o banco)
CACHE_AUTH_TTL_SECS=60
CACHE_BOLETIM_TTL_SECS=300
CACHE_SNAPSHOT_TTL_SECS=900

# push no Android (deixa false se ainda nao tiver firebase)
FCM_ENABLED=false
FCM_PROJECT_ID=seu-project-id

# bot do discord embutido (opcional — se nao preencher, o bot simplesmente nao sobe)
DISCORD_APPLICATION_ID=
DISCORD_BOT_TOKEN=
```

### 4. (opcional) Firebase FCM pra notificações push

se colocou `FCM_ENABLED=true`:
1. baixa o JSON da conta de serviço no Firebase Console (*Configurações do Projeto -> Contas de serviço -> Gerar nova chave privada*).
2. salva em `secrets/firebase-service-account.json` (essa pasta ja ta no `.gitignore`).
3. o `deploy/docker-entrypoint.sh` lê esse arquivo automaticamente e joga em `FCM_CREDENTIALS_JSON` antes de iniciar o `nsac-server`.

### 5. sobe tudo

```bash
docker compose up -d --build
```

o `docker-compose.yml` sobe o `n2sac-postgres` (porta `5432`) e o `n2sac-server` (porta `3000`, `network_mode: host`), já aplicando todas as migrations automaticamente no primeiro boot.

### 6. testa se subiu direito

```bash
curl -fsS http://127.0.0.1:3000/api/v1/health
# esperado: {"success":true,"data":{"status":"ok"}}
```

pra ver os logs:

```bash
docker logs -f --tail 100 n2sac-server
```

---

## expondo pra internet (Nginx + HTTPS)

o `nsac-server` escuta em `0.0.0.0:3000`. em produção o Nginx (que fica no repo `n2sac-front`) serve o app na porta `8080` e faz proxy reverso de:
- `/api/` -> `http://127.0.0.1:3000/api/`
- `/api/v1/classrooms/me/stream` -> `http://127.0.0.1:3000/api/v1/classrooms/me/stream` (**importante:** precisa de `proxy_buffering off;` e timeout alto porque é SSE em tempo real)

na frente do `8080`, dá pra expor com **Tailscale Funnel** (`tailscale funnel 8080`) ou Cloudflare Tunnel.

---

## rodando local (desenvolvimento)

se quiser compilar e rodar direto fora do Docker:

```bash
# sobe só o postgres
docker compose up -d postgres

# exporta o .env e roda
set -a && source .env && set +a
cargo run -p nsac-server
```

se tiver o repo `front/` do lado (`../front`), tem o script de ambiente local completo com banco de teste isolado e usuários sintéticos:

```bash
./scripts/dev-local/dev-local.sh up
./scripts/dev-local/dev-local.sh status
./scripts/dev-local/dev-local.sh down
```

---

## variáveis de ambiente principais

| Variável | Padrão | Pra que serve |
|---|---|---|
| `HTTP_BIND` | `0.0.0.0:3000` | endereço/porta do servidor HTTP |
| `DATABASE_URL` | — | string de conexão do PostgreSQL |
| `ENCRYPTION_KEY` | — | chave AES-256-GCM (64 chars hex, obrigatória) |
| `SERVICE_API_KEYS` | vazio | chaves `nome:segredo` pra integrações/admin |
| `USER_SESSION_TTL_SECS` | `2592000` | validade da sessão de usuário (30 dias) |
| `NSAC_BASE_URL` | `http://200.145.153.1/nsac` | URL base do NSAC Online |
| `NSAC_HTTP_TIMEOUT_SECS` | `12` | timeout por request no NSAC (protege contra travadas de madrugada) |
| `NSAC_MAX_CONCURRENT_REQUESTS` | `4` | limite do `NsacSemaphore` pra nao derrubar o NSAC |
| `SENTINEL_ENABLED` | `true` | liga/desliga o polling `O(21)` de notas por turma |
| `FCM_ENABLED` / `FCM_PROJECT_ID` | `false` | ativa push HTTP v1 do Firebase |
| `DISCORD_APPLICATION_ID` / `DISCORD_BOT_TOKEN` | vazio | ativa o bot do Discord embutido |

---

## o que a API implementa (`/api/v1/*`)

- **saúde e conta:** `/health`, `/auth/login`, `/auth/logout`, `/account`, `/account/profile`, `/account/fcm-token`, `/account/notification-preferences`, `/account/discord-link`, `/account/privacy`, `/account/ranking-preferences`
- **NSAC (com `ETag`/`304` + `next_poll_hint_secs`):** `/nsac/home`, `/nsac/boletim`, `/nsac/atrasos`, `/nsac/saidas`, `/nsac/ficha-avaliacao`, `/nsac/ficha-avaliacao/{id}`, links temporários de PDF
- **ranking e social:** `/compare`, `/students/search`, `/profiles/{id}` (`/avatar`, `/banner`, `/report`), `/social/overview`, follows/requests/blocks e chat privado criptografado (`/social/conversations/{peer_id}/*`)
- **canais da turma + realtime:** `/classrooms/me`, `/classrooms/me/stream` (SSE), eleições de representante, anúncios com anexo/reação, canais de chat e off-topic
- **feedback e telemetria:** `/feedback`, `/feedback/mine`, `/admin/feedback/*`, `/telemetry`

---

## testes (Rust + E2E)

### 1. suite Rust (`cargo test --workspace`) — 98 testes

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- **`nsac-core` (35 testes):**
  - `crypto_tests` (6): AES-256-GCM, nonce único, detecção de adulteração, redação de segredos no `Debug`.
  - `parser_tests` (7): parsers HTML de todas as 5 rotas do NSAC, diurno/noturno, notas não lançadas (`PF-05`), reset de célula (`PF-EXTRA-02`), detecção de sessão expirada.
  - `filter_tests` (7): filtros de boletim sem acento/case (`PF-03`), imutabilidade do cache (`PF-02`), média da turma (`PF-01`), booleanos e poda de anos vazios.
  - `http_client_tests` (10): login com CSRF, cookies fora de ordem (`PF-06`), timeout explícito (`PF-04`), rotação de cookie, PDF de ficha.
  - `sentinel_tests` (3) + unit (2): hash canônico `M.T.` igual pra alunos da mesma turma com notas individuais diferentes.
- **`nsac-server` (63 testes):**
  - `http_integration_tests` (8): login/logout, dual auth, 5 rotas NSAC + `ETag`/`304`, rotação de cookie sem deadlock, rate limit de telemetria.
  - `db_integration_tests` (5): migrations idempotentes, zero segredo em texto puro no banco, CTE do leaderboard sem TOAST no `GROUP BY`, 21 turmas do CTI.
  - `heuristic_simulation_tests` (6): simulações multi-ano do Sentinel contra ataques/anomalias (envenenamento estatístico, flood de API/DDoS, NSAC instável/503 de madrugada, thundering herd, failover automático de sentinela).
  - `fcm_tests` (9): cache de token OAuth2, canais prioritários Android, acúmulo de notas (`1`, `2-3`, `4+`), limpeza de `404 UNREGISTERED`, isolamento em `503`.
  - `cache_semaphore_tests` (2), `config_error_tests` (4), `chat_features_tests` (1), `classrooms_tests` (1), `feedback_tests` (1), `moderation_tests` (2) + 24 testes unitários internos.

### 2. suite E2E caixa-preta (`e2e/`) — 92 testes em 4 tiers

sobe o binário real compilado do `nsac-server` contra um Postgres real + servidores mock de NSAC e FCM:

```bash
pytest e2e/tests
```

- **Tier 1 (35 testes):** caminhos felizes de auth, conta/FCM/Discord, 5 rotas NSAC, filtros, `ETag`/`304`/cache e Sentinel/DB.
- **Tier 2 (40 testes):** bordas, falhas e segurança (credenciais erradas, timeout `PF-04`, nota nula `PF-05`, cookies invertidos `PF-06`, limpeza de token FCM `UNREGISTERED`, limites de validação).
- **Tier 3 (10 testes):** combinações cruzadas entre features (ex.: cache + filtros + auth + rotação).
- **Tier 4 (7 testes):** cenários reais simulando as 21 turmas do CTI sob carga e cenários adversariais.

---

## deploy em produção (image-only)

em produção o servidor remoto só precisa de `/opt/n2sac/docker-compose.yml`, `.env` e `secrets/` (nenhum código-fonte é enviado via `rsync`):

```bash
# valida antes sem reiniciar nada
./scripts/deploy-server.sh --dry-run

# builda n2sac-server:latest local, envia via SSH e reinicia só o nsac-server
./scripts/deploy-server.sh
```

---

## Licença

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

Este projeto é licenciado sob a **GNU Affero General Public License v3.0 (AGPL-3.0)** — veja o arquivo [LICENSE](./LICENSE) para o texto completo.

Qualquer pessoa pode estudar, modificar e redistribuir o código, mas **qualquer versão modificada distribuída ou executada em servidor/rede (SaaS) deve manter 100% do código-fonte aberto sob a mesma licença AGPL-3.0**, impedindo apropriação comercial em código fechado.
