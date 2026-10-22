import { ArrowRight, BookOpen, Code2, Feather, Sparkles } from 'lucide-react';
import '../../styles/chat-welcome.css';

const suggestions = [
  { icon: Feather, title: '让文字更有力量', description: '写作、润色，让表达恰到好处', prompt: '帮我写一封简洁、真诚的工作邮件。请先问我收件人和想表达的内容。' },
  { icon: Code2, title: '一起解决代码难题', description: '读懂代码，找到更好的解法', prompt: '我想请你帮我分析一段代码。请先问我使用的语言和遇到的问题。' },
  { icon: BookOpen, title: '把复杂的事讲明白', description: '拆解知识，收获新的理解', prompt: '请用通俗的语言解释一个复杂概念。先问我想了解什么，以及我的知识背景。' },
  { icon: Sparkles, title: '给想法一点新灵感', description: '打开思路，发现更多可能', prompt: '我想和你一起头脑风暴。请先问我目标和限制条件，再帮我拓展思路。' },
];

export interface WelcomeProps {
  disabled?: boolean;
  onSelectPrompt: (prompt: string) => void;
}

/**
 * 空会话欢迎页：居中列 + 极简排版。
 * 建议项只填入 composer 草稿，不直接发送；禁用条件由调用方给出。
 */
export function Welcome({ disabled = false, onSelectPrompt }: WelcomeProps) {
  return (
    <section className="chat-welcome" aria-labelledby="chat-welcome-title" data-slot="peri-welcome">
      <span className="chat-welcome-mark" aria-hidden="true"><Sparkles size={20} strokeWidth={1.5} /></span>
      <h1 id="chat-welcome-title" className="chat-welcome-title">
        今天，我们一起<br className="chat-welcome-break" />探索什么？
      </h1>
      <p className="chat-welcome-description">
        一个问题、一闪灵感，或一件想完成的事。<br />从这里开始，把想法变成下一步。
      </p>
      <div className="chat-welcome-suggestions">
        {suggestions.map(item => (
          <button type="button" className="chat-welcome-suggestion" key={item.title}
            disabled={disabled} onClick={() => onSelectPrompt(item.prompt)}>
            <span className="chat-welcome-suggestion-icon" aria-hidden="true"><item.icon size={16} strokeWidth={1.7} /></span>
            <span className="chat-welcome-suggestion-text">
              <strong>{item.title}</strong>
              <span className="chat-welcome-suggestion-description">{item.description}</span>
            </span>
            <ArrowRight className="chat-welcome-suggestion-arrow" size={15} strokeWidth={1.7} aria-hidden="true" />
          </button>
        ))}
      </div>
    </section>
  );
}
