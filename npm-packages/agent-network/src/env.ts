export interface Env {
  DB: D1Database;
  NOTIFICATION_CHANNELS: DurableObjectNamespace<import('./realtime/channel').NotificationChannel>;
  ENVIRONMENT: string;
  PUBLIC_ORIGIN: string;
}
