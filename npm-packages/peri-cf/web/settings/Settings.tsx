import { useEffect, useRef, useState } from 'react';
import { Check, Eye, EyeOff, KeyRound, ShieldCheck, X } from 'lucide-react';

interface SettingsProps {
  token: string;
  error: string;
  onSave: (token: string) => void;
  onClose: () => void;
}

export function Settings({ token, error, onSave, onClose }: SettingsProps) {
  const [value, setValue] = useState(token);
  const [visible, setVisible] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const previous = document.activeElement;
    dialog.current?.showModal();
    input.current?.focus();
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  return (
    <dialog className="settings-dialog" ref={dialog} onCancel={onClose}
      onClick={event => { if (event.target === dialog.current) onClose(); }} aria-labelledby="settings-title">
      <form onSubmit={event => { event.preventDefault(); onSave(value.trim()); }}>
        <div className="dialog-heading">
          <div className="setting-symbol"><KeyRound size={22} /></div>
          <button className="icon-button" type="button" aria-label="关闭设置" onClick={onClose}><X size={20} /></button>
        </div>
        <h2 id="settings-title">连接你的 Peri</h2>
        <p className="muted">设置访问令牌，开启一个专注的对话空间。</p>
        <label className="field-label" htmlFor="access-token">访问令牌 <span>Bearer token</span></label>
        <div className="token-field">
          <input ref={input} id="access-token" type={visible ? 'text' : 'password'} value={value}
            autoComplete="off" spellCheck={false} placeholder="输入服务端提供的访问令牌"
            onChange={event => setValue(event.target.value)} />
          <button type="button" className="icon-button" aria-label={visible ? '隐藏令牌' : '显示令牌'}
            onClick={() => setVisible(previous => !previous)}>{visible ? <EyeOff size={18} /> : <Eye size={18} />}</button>
        </div>
        {error && <p className="settings-error" role="alert">{error}</p>}
        <div className="privacy-note"><ShieldCheck size={18} /><p>保存在此浏览器的 localStorage 中，关闭标签页或重启浏览器后仍然保留。令牌会用于同源 API 的身份验证，请勿在共享设备上保留，不再需要时可在设置中保存空值清除。</p></div>
        <div className="dialog-footer">
          <button className="text-button" type="button" onClick={onClose}>取消</button>
          <button className="primary-button" type="submit"><Check size={17} />保存设置</button>
        </div>
      </form>
    </dialog>
  );
}
