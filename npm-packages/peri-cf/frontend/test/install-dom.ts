import { installDom } from './dom';

/** side-effect 模块：在 query-core 等库求值前安装最小 DOM，避免其把 Bun 测试环境判定为 server。 */
installDom();
