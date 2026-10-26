(() => {
'use strict';
const buttons=[...document.querySelectorAll('[data-command]')],tooltip=document.querySelector('#tray-tooltip'),invoke=window.__TAURI__?.core?.invoke;
let target;
function hide(){target?.removeAttribute('aria-describedby');target=null;tooltip.hidden=true}
function show(button){hide();if(!button.dataset.tooltip)return;target=button;tooltip.textContent=button.dataset.tooltip;tooltip.hidden=false;button.setAttribute('aria-describedby',tooltip.id)}
for(const button of buttons){
 button.addEventListener('focus',()=>show(button));button.addEventListener('pointerenter',()=>show(button));button.addEventListener('blur',hide);button.addEventListener('pointerleave',hide);
 button.addEventListener('click',async()=>{hide();if(typeof invoke!=='function'){button.dataset.tooltip='浏览器预览：未连接后端';show(button);return}button.disabled=true;try{await invoke(button.dataset.command);delete button.dataset.tooltip}catch(error){button.dataset.tooltip=`操作失败：${error}`;show(button)}finally{button.disabled=false}});
}
document.addEventListener('keydown',event=>{if(event.key==='Escape'){hide();return}if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;event.preventDefault();const available=buttons.filter(b=>!b.disabled),current=available.indexOf(document.activeElement);const index=event.key==='Home'?0:event.key==='End'?available.length-1:event.key==='ArrowDown'?(current+1)%available.length:current<0?available.length-1:(current-1+available.length)%available.length;available[index]?.focus()});
window.addEventListener('blur',hide);
})();
