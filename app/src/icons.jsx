import React from 'react';
import { createRoot } from 'react-dom/client';
import { IconPlus, IconSettings, IconX, IconPencil, IconTrash, IconShieldCheck, IconInfoCircle, IconFolderOpen, IconMinus } from '@tabler/icons-react';
import './main.js';
const icons = { plus: IconPlus, settings: IconSettings, close: IconX, edit: IconPencil, trash: IconTrash, shield: IconShieldCheck, info: IconInfoCircle, folder: IconFolderOpen, minus: IconMinus };
const roots = new Map();
function mountIcons(scope = document) {
  const nodes = [...(scope.querySelectorAll?.('[data-icon]') || [])];
  if (scope.matches?.('[data-icon]')) nodes.unshift(scope);
  nodes.forEach(node => {
    if (roots.has(node)) return;
    const Icon = icons[node.dataset.icon];
    if (!Icon) return;
    const root = createRoot(node); roots.set(node, root);
    root.render(React.createElement(Icon, { size: 20, stroke: 1.7, 'aria-hidden': true }));
  });
}
const observer = new MutationObserver(records => {
  for (const record of records) {
    for (const node of record.addedNodes) {
      if (node.nodeType === 1 && !node.closest('.tabler-icon-root svg')) mountIcons(node);
    }
  }
  for (const [node, root] of roots) if (!node.isConnected) { root.unmount(); roots.delete(node); }
});
observer.observe(document.body, { childList: true, subtree: true });
mountIcons();
window.addEventListener('beforeunload', () => { observer.disconnect(); roots.forEach(root => root.unmount()); roots.clear(); }, { once: true });
