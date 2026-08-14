import { Badge as UiBadge, type BadgeTone } from '../../ui';

const OK = ['online', 'healthy', 'completed', 'accepting', 'allow'];
const WARN = ['degraded', 'active', 'streaming', 'pending', 'awaitingPermission', 'running', 'deny'];
const ERR = ['offline', 'crashed', 'error', 'restarting', 'failed'];

export function badgeKind(status: string | null | undefined): BadgeTone {
  if (!status) return 'neutral';
  if (OK.includes(status)) return 'ok';
  if (WARN.includes(status)) return 'warn';
  if (ERR.includes(status)) return 'err';
  return 'neutral';
}

export function MessageStatusBadge(props: { status: string | null | undefined; tone?: BadgeTone }) {
  return <UiBadge tone={props.tone ?? badgeKind(props.status)}>{props.status}</UiBadge>;
}
