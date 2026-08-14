// Browser-run assertion contract for `visual-fixture.html`. The in-app Browser
// executes the same checks at each viewport; keeping them here makes the matrix
// reviewable and reusable without a brittle pixel baseline.
export const visualContract = () => {
  const root = document.documentElement;
  const rail = document.querySelector('.visual-fixture-rail');
  const shell = document.querySelector('.app-shell');
  const dialog = document.querySelector('.ui-dialog-backdrop');
  const toast = document.querySelector('.ui-toast-viewport');
  const rect = shell?.getBoundingClientRect();
  return {
    fixture: document.title === 'acp-hub UI 状态验收台',
    railVisible: !!rail && getComputedStyle(rail).display !== 'none',
    shellVisible: !!rect && rect.width > 0 && rect.height > 0,
    horizontalOverflow: root.scrollWidth > root.clientWidth,
    bodyMargin: getComputedStyle(document.body).margin,
    selectedScenario: document.querySelector('.visual-fixture-rail a[aria-current="page"]')?.textContent?.trim() || null,
    projectCount: document.querySelectorAll('.project-group').length,
    sessionCount: document.querySelectorAll('.session-row').length,
    messageCount: document.querySelectorAll('.conversation-message').length,
    permissionCount: document.querySelectorAll('.permission-request').length,
    markdown: {
      headings: document.querySelectorAll('.markdown-body h2,.markdown-body h3').length,
      lists: document.querySelectorAll('.markdown-body ul,.markdown-body ol').length,
      codeBlocks: document.querySelectorAll('.md-code-block').length,
    },
    uncertainOutbox: document.querySelectorAll('.message-outbox--uncertain').length,
    readonly: document.body.textContent?.includes('只读模式') || false,
    overlays: {
      railZ: rail ? Number(getComputedStyle(rail).zIndex) : null,
      dialogZ: dialog ? Number(getComputedStyle(dialog).zIndex) : null,
      dialogCoversViewport: !!dialog && dialog.getBoundingClientRect().width === innerWidth && dialog.getBoundingClientRect().height === innerHeight,
      appInert: document.getElementById('app')?.inert || false,
      toastTop: toast ? toast.getBoundingClientRect().top : null,
    },
    viewport: [innerWidth, innerHeight],
    locale: navigator.language,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  };
};

export const assertVisualContract = (result) => {
  if (!result.fixture || !result.railVisible || !result.shellVisible) throw new Error('fixture shell is not visible');
  if (result.horizontalOverflow) throw new Error(`horizontal overflow at ${result.viewport.join('x')}`);
  if (result.bodyMargin !== '0px') throw new Error(`unexpected body margin: ${result.bodyMargin}`);
  if (!result.locale || !result.timezone) throw new Error('browser locale/timezone evidence missing');
  return result;
};
