import { InteractionCard } from './InteractionCard';
import type { InteractionAnswerHandler } from '../runtime';
import type { Interaction } from '../runtime';

/** 待回答交互：composer 上方内联；未就绪时不提供回答通道。 */
export function PendingInteractions({
  interactions, onAnswer,
}: {
  interactions: readonly Interaction[];
  onAnswer?: InteractionAnswerHandler;
}) {
  if (interactions.length === 0) return null;
  return <section
    aria-label="等待回答的交互"
    data-slot="peri-pending-interactions"
    className="flex flex-col items-stretch gap-2 border-b px-3 py-2.5"
  >
    {interactions.map((interaction) => (
      <InteractionCard key={interaction.id} interaction={interaction} onAnswer={onAnswer} />
    ))}
  </section>;
}
