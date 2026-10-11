import { useState } from 'react';
import { Sparkles, X } from 'lucide-react';
import {
  MODEL_PROVIDERS, configProblems, isModelProvider,
  type BrowserConfig, type ModelConfig, type StorageConfig,
} from './config';

/**
 * 浏览器 BYOK 设置：模型 key、存储 URL/token 只保存在本机 localStorage，
 * 请求经同源 relay 按真实值上行；保存后由刷新后的宿主生效。
 */
export interface SettingsViewProps {
  config: BrowserConfig;
  onSave: (config: BrowserConfig) => void;
  /** 已有可用配置时的关闭入口；首次运行必须完成填写。 */
  onClose?: () => void;
}

export function SettingsView({ config, onSave, onClose }: SettingsViewProps) {
  const [draft, setDraft] = useState(config);
  const problems = configProblems(draft);
  const setModel = (patch: Partial<ModelConfig>) => setDraft((current) => ({ ...current, model: { ...current.model, ...patch } }));
  const setStorage = (patch: Partial<StorageConfig>) => setDraft((current) => ({ ...current, storage: { ...current.storage, ...patch } }));

  return (
    <div className="settings-shell">
      <form className="settings-card" onSubmit={(event) => { event.preventDefault(); if (problems.length === 0) onSave(draft); }}>
        <div className="settings-head">
          <span className="brand-mark"><Sparkles size={19} /></span>
          <h1>浏览器配置</h1>
          {onClose && <button type="button" className="icon-button" onClick={onClose} aria-label="关闭设置"><X size={17} /></button>}
        </div>
        <p className="settings-intro">密钥与令牌只保存在本机浏览器（localStorage），由浏览器经同源 relay 按真实值转发；模型与存储服务直连上游，服务端不保存配置。</p>
        <div className="settings-group">
          <h2>设备标识</h2>
          <label className="settings-field">
            <span>machineId（首次运行自动生成，本地长期保存）</span>
            <input readOnly value={draft.machineId} />
          </label>
        </div>
        <div className="settings-group">
          <h2>模型服务</h2>
          <label className="settings-field">
            <span>Provider（必填）</span>
            <select value={draft.model.provider} onChange={(event) => { if (isModelProvider(event.target.value)) setModel({ provider: event.target.value }); }}>
              {MODEL_PROVIDERS.map((provider) => <option key={provider} value={provider}>{provider}</option>)}
            </select>
          </label>
          <label className="settings-field">
            <span>模型 ID（必填）</span>
            <input value={draft.model.id} placeholder="gpt-4o-mini" autoComplete="off" spellCheck={false}
              onChange={(event) => setModel({ id: event.target.value })} />
          </label>
          <label className="settings-field">
            <span>模型 Base URL（必填）</span>
            <input value={draft.model.baseUrl} placeholder="https://api.openai.com/v1" autoComplete="off" spellCheck={false}
              onChange={(event) => setModel({ baseUrl: event.target.value })} />
          </label>
          <label className="settings-field">
            <span>模型 API Key（必填）</span>
            <input type="password" value={draft.model.apiKey} autoComplete="off"
              onChange={(event) => setModel({ apiKey: event.target.value })} />
          </label>
        </div>
        <div className="settings-group">
          <h2>存储服务</h2>
          <label className="settings-field">
            <span>存储 URL（必填）</span>
            <input value={draft.storage.url} placeholder="https://your-db.example" autoComplete="off" spellCheck={false}
              onChange={(event) => setStorage({ url: event.target.value })} />
          </label>
          <label className="settings-field">
            <span>存储 Token（必填）</span>
            <input type="password" value={draft.storage.token} autoComplete="off"
              onChange={(event) => setStorage({ token: event.target.value })} />
          </label>
        </div>
        <div className="settings-foot">
          <button type="submit" className="primary-button" disabled={problems.length > 0}>{onClose ? '保存并刷新' : '保存并进入'}</button>
          <span className="muted">{onClose ? '配置在宿主启动时读取，保存后刷新页面生效。' : '保存后进入聊天，宿主按新配置启动。'}</span>
        </div>
        {problems.length > 0 && <p className="settings-problems" role="status">还需填写：{problems.join('、')}</p>}
      </form>
    </div>
  );
}
