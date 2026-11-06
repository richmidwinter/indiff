#!/usr/bin/env bash
# Creates a small TypeScript repo with an agent-style change to review:
# webhook delivery gains retries with backoff (and a few problems).
# Usage: examples/webhook/setup.sh /path/to/new/repo
set -euo pipefail
dir="${1:?usage: setup.sh <dir>}"
rm -rf "$dir"; mkdir -p "$dir"; cd "$dir"
git init -q -b main
git config user.email demo@example.com; git config user.name Demo

mkdir -p src/webhooks src/jobs src/admin src/lib test/webhooks
cat > src/http.ts <<'EOF'
export interface Res { ok: boolean; status: number }
export async function post(url: string, body: unknown): Promise<Res> {
  const r = await fetch(url, { method: 'POST', body: JSON.stringify(body) });
  return { ok: r.ok, status: r.status };
}
EOF
cat > src/lib/logger.ts <<'EOF'
export const log = {
  info: (msg: string, data?: unknown) => console.log(msg, data ?? ''),
};
EOF
cat > src/webhooks/types.ts <<'EOF'
export interface Webhook { id: string; url: string; body: { email: string; orderId: string; total: number } }
export class DeliveryError extends Error {
  constructor(public status: number | string) { super(`delivery failed: ${status}`); }
}
EOF
cat > src/webhooks/log.ts <<'EOF'
import { Webhook } from './types';
import { log } from '../lib/logger';

export const logSent = (h: Webhook) => log.info(`webhook ${h.id} sent`);
EOF
cat > src/webhooks/deliver.ts <<'EOF'
import { post } from '../http';
import { Webhook, DeliveryError } from './types';
import { logSent } from './log';

export async function deliver(hook: Webhook) {
  const res = await post(hook.url, hook.body);
  if (!res.ok) throw new DeliveryError(res.status);
  logSent(hook);
  return res;
}
EOF
cat > src/jobs/worker.ts <<'EOF'
import { deliver } from '../webhooks/deliver';
import { Webhook } from '../webhooks/types';

export async function run(queue: Webhook[]) {
  for (const hook of queue) {
    try { await deliver(hook); } catch (e) { console.error(e); }
  }
}
EOF
cat > src/admin/server.ts <<'EOF'
export const REQUEST_TIMEOUT_MS = 10_000;
EOF
cat > src/admin/resend.ts <<'EOF'
import { deliver } from '../webhooks/deliver';
import { Webhook } from '../webhooks/types';

// POST /admin/webhooks/:id/resend, runs inside the HTTP request (REQUEST_TIMEOUT_MS)
export async function adminResend(hook: Webhook) {
  await deliver(hook);
  return { resent: hook.id };
}
EOF
cat > test/webhooks/deliver.test.ts <<'EOF'
import { deliver } from '../../src/webhooks/deliver';
import { mockPost, hook } from './helpers';

it('delivers a webhook', async () => {
  mockPost([200]);
  await expect(deliver(hook)).resolves.toBeOk();
});
EOF
git add -A; git commit -qm "Webhook delivery"

git checkout -q -b feature/webhook-retry
cat > src/webhooks/backoff.ts <<'EOF'
export function backoffDelay(attempt: number): number {
  const base = 500;
  return Math.min(base * 2 ** attempt, 30_000) + jitter();
}

function jitter() { return Math.floor(Math.random() * 250); }

export const sleep = (ms: number) =>
  new Promise((r) => setTimeout(r, ms));
EOF
cat > src/webhooks/deliver.ts <<'EOF'
import { post } from '../http';
import { backoffDelay, sleep } from './backoff';
import { Webhook, DeliveryError } from './types';
import { logSent } from './log';

const MAX_ATTEMPTS = 5;

export async function deliver(hook: Webhook) {
  let attempt = 0;
  while (attempt < MAX_ATTEMPTS) {
    await sleep(backoffDelay(attempt));
    const res = await post(hook.url, hook.body);
    if (res.ok) { logSent(hook); return res; }
    if (res.status < 500) throw new DeliveryError(res.status);
    attempt++;
  }
  throw new DeliveryError('exhausted');
}
EOF
cat > src/webhooks/log.ts <<'EOF'
import { Webhook } from './types';
import { log } from '../lib/logger';

export const logSent = (h: Webhook) =>
  log.info('webhook sent', { id: h.id, body: h.body });
EOF
cat >> test/webhooks/deliver.test.ts <<'EOF'

it('retries on 503 then succeeds', async () => {
  mockPost([503, 503, 200]);
  await expect(deliver(hook)).resolves.toBeOk();
});

it('does not retry on 400', async () => {
  mockPost([400]);
  await expect(deliver(hook)).rejects.toThrow();
});
EOF
git add -A; git commit -qm "Retry webhook delivery with exponential backoff"
echo "created $dir (review main..feature/webhook-retry)"
