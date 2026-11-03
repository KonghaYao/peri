import type { ComposerCommand } from './composerSlash';

interface ComposerSlashMenuProps {
  id: string;
  commands: readonly ComposerCommand[];
  activeIndex: number;
  onSelect: (command: ComposerCommand) => void;
}

/** 斜杠命令菜单：键盘高亮由 textarea 的 aria-activedescendant 指向 option。 */
export function ComposerSlashMenu({ id, commands, activeIndex, onSelect }: ComposerSlashMenuProps) {
  return (
    <div
      className="bg-popover text-popover-foreground absolute inset-x-0 bottom-full z-30 mb-2 max-h-64 overflow-y-auto rounded-xl border p-1 text-xs shadow-xl"
      data-slot="peri-composer-slash-menu"
    >
      <div id={id} role="listbox" aria-label="斜杠命令">
        {commands.length === 0 ? (
          <p className="text-muted-foreground px-2 py-1.5" role="status">没有匹配的命令</p>
        ) : commands.map((command, index) => (
          <div
            key={command.name}
            id={`${id}-option-${index}`}
            role="option"
            aria-selected={index === activeIndex}
            className={`cursor-pointer rounded-md px-2 py-1.5 ${
              index === activeIndex ? 'bg-accent text-accent-foreground' : 'hover:bg-muted'
            }`}
            // 保持 textarea 焦点：点击选项不触发 blur，选择后光标仍可继续输入。
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onSelect(command)}
          >
            <div className="flex items-baseline justify-between gap-3">
              <span className="font-medium">/{command.name}</span>
              {command.hint && <span className="text-muted-foreground truncate">{command.hint}</span>}
            </div>
            {command.description && (
              <div className="text-muted-foreground truncate">{command.description}</div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
