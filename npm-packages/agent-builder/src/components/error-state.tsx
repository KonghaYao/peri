import { AlertTriangle } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { describeApiError } from '@/lib/api-client';

export interface ErrorStateProps {
  readonly title?: string;
  readonly error: unknown;
  readonly onRetry?: () => void;
  readonly retryLabel?: string;
}

export function ErrorState({ title = '加载失败', error, onRetry, retryLabel = '重试' }: ErrorStateProps) {
  return (
    <Alert variant="destructive">
      <AlertTriangle />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>
        <p>{describeApiError(error)}</p>
        {onRetry ? (
          <Button className="mt-3" size="sm" variant="outline" onClick={onRetry}>
            {retryLabel}
          </Button>
        ) : null}
      </AlertDescription>
    </Alert>
  );
}
