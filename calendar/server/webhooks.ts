import { createHmac } from 'node:crypto';
import type { CalendarStore, Delivery } from './events';

export interface WebhookConfig {
  url: URL;
  bearer?: string;
  secret?: string;
}

export function startWebhookWorker(store: CalendarStore, config: WebhookConfig) {
  let running: Promise<void> | undefined;
  let stopped = false;

  async function send(delivery: Delivery) {
    const headers = new Headers({
      'Content-Type': 'application/json',
      'X-Calendar-Delivery': delivery.id,
      'X-Calendar-Event': delivery.type,
    });
    if (config.bearer) headers.set('Authorization', `Bearer ${config.bearer}`);
    if (config.secret) {
      const timestamp = String(Math.floor(Date.now() / 1000));
      const signature = createHmac('sha256', config.secret).update(`${timestamp}.${delivery.body}`).digest('hex');
      headers.set('X-Calendar-Timestamp', timestamp);
      headers.set('X-Calendar-Signature', `sha256=${signature}`);
    }

    let failure = 'network error or timeout';
    try {
      const response = await fetch(config.url, {
        method: 'POST',
        headers,
        body: delivery.body,
        redirect: 'manual',
        signal: AbortSignal.timeout(5000),
      });
      void response.body?.cancel().catch(() => {});
      if (response.ok) return null;
      failure = `HTTP ${response.status}`;
    } catch {
      // Retry connection failures without logging URLs, tokens or event data.
    }
    return failure;
  }

  async function drain() {
    while (!stopped) {
      const delivery = store.nextDelivery(Date.now());
      if (!delivery) return;
      const failure = await send(delivery);
      if (!failure) {
        store.delivered(delivery.id);
        continue;
      }
      const attempts = delivery.attempts + 1;
      const when = attempts >= 8 ? null : Date.now() + Math.min(5000 * 2 ** (attempts - 1), 300_000);
      store.retry(delivery.id, attempts, when);
      if (when === null) {
        console.error(`Webhook ${delivery.id} (${delivery.type}) failed after ${attempts} attempts: ${failure}`);
      }
    }
  }

  const timer = setInterval(() => {
    if (running) return;
    running = drain()
      .catch((error) => console.error('Webhook queue error:', error))
      .finally(() => { running = undefined; });
  }, 1000);

  return async () => {
    stopped = true;
    clearInterval(timer);
    await running;
  };
}
