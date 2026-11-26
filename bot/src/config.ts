function env(name: string, fallback?: string): string {
  const v = process.env[name]?.trim();
  if (v) return v;
  if (fallback !== undefined) return fallback;
  console.error(`Missing environment variable ${name}`);
  process.exit(1);
}

export const config = {
  token: env('DISCORD_TOKEN'),
  guildId: env('DISCORD_GUILD_ID', ''),
  apiUrl: env('PDFSIGN_URL', 'http://localhost:8080').replace(/\/$/, ''),
  apiKey: env('PDFSIGN_API_KEY'),
  eventTemplate: env('EVENT_TEMPLATE', 'event-waiver'),
  equipmentTemplate: env('EQUIPMENT_TEMPLATE', 'equipment'),
  adminRoleIds: env('ADMIN_ROLE_IDS', '').split(',').map((s) => s.trim()).filter(Boolean),
  watchScheduledEvents: env('WATCH_SCHEDULED_EVENTS', 'true') !== 'false',
  notifyChannelId: env('NOTIFY_CHANNEL_ID', ''),
};
