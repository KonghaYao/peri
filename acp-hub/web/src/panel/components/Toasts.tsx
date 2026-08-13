// Store owns expiry; the UI primitive owns live-region and visual behavior.
import { toasts } from '../store';
import { ToastViewport } from '../../ui';

export function Toasts() {
  return <ToastViewport items={toasts().map((toast) => ({ id: toast.id, content: toast.msg }))} />;
}
