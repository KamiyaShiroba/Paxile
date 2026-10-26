(() => {
'use strict';
const $=(s,r=document)=>r.querySelector(s), $$=(s,r=document)=>[...r.querySelectorAll(s)];
const api=window.__TAURI__, invoke=api?.core?.invoke, preview=typeof invoke!=='function';
const clone=x=>JSON.parse(JSON.stringify(x)), help='IME 白名单：允许此程序使用输入法，不创建拦截规则。';
let config,catalog=[],rules=[],busy=false,started=false,editing,deleting,focus,unlisten,statusTimer,statusPending=false,disposed=false;
const logs=[],logKeys=new Set();
let logFrame=0,logsDirty=false,autoReady=false,silentReady=false;
function logsVisible(){return !document.hidden&&!$('[data-page-panel="settings"]').hidden}
function scheduleLogs(){if(logFrame||!logsVisible())return;logFrame=requestAnimationFrame(()=>{logFrame=0;if(logsDirty&&logsVisible())paintLogs()})}
const name=p=>catalog.find(x=>x.name===p.name)?.display_name||p.name;
function report(text){$('#ui-status').textContent=String(text)}
function paintLogs(){
 if(!logsVisible())return;logsDirty=false;
 const list=$('#log-list'),start=Math.min(logs.length,Math.max(0,Math.floor(list.scrollTop/30)-2)),count=Math.ceil((list.clientHeight||300)/30)+4;
 const spacer=h=>{const n=document.createElement('div');n.className='log-spacer';n.style.height=`${h}px`;return n};
 const rows=logs.slice(start,start+count).map(e=>{const row=document.createElement('div');row.className='log-row';row.dataset.tooltip=e.msg;row.tabIndex=0;const date=new Date(Number(e.time));
 for(const text of [Number.isFinite(date.getTime())?date.toLocaleTimeString('zh-CN',{hour12:false}):e.time,e.level.toUpperCase(),e.msg]){const cell=document.createElement('span');cell.textContent=text;row.append(cell)}return row});
 list.replaceChildren(spacer(start*30),...rows,spacer(Math.max(0,logs.length-start-count)*30));
}
function receiveBatch(entries){
 let changed=false;
 for(const e of entries){if(!e||typeof e.msg!=='string')continue;const item={time:String(e.time),level:String(e.level||'info'),msg:e.msg},key=JSON.stringify([item.time,item.level,item.msg]);if(logKeys.has(key))continue;logKeys.add(key);logs.push(item);changed=true}
 if(!changed)return;logs.sort((a,b)=>Number(b.time)-Number(a.time));for(const item of logs.splice(5000))logKeys.delete(JSON.stringify([item.time,item.level,item.msg]));logsDirty=true;scheduleLogs();
}
function receive(e){receiveBatch([e])}
function page(name){$$('[data-page]').forEach(b=>{const on=b.dataset.page===name;b.classList.toggle('is-active',on);if(on)b.setAttribute('aria-current','page');else b.removeAttribute('aria-current')});$$('[data-page-panel]').forEach(p=>{p.hidden=p.dataset.pagePanel!==name});if(name==='settings')paintLogs()}
function button(symbol,title,action){const b=document.createElement('button');b.type='button';b.className='icon-button';b.dataset.tooltip=title;b.setAttribute('aria-label',title);b.innerHTML=`<span class="tabler-icon-root" data-icon="${symbol}" aria-hidden="true"></span>`;b.onclick=action||(()=>{});return b}
function lock(){
 $$('#add-button,[data-open-modal],#rule-list button,#global-toggle,#autostart,#silent-start').forEach(n=>{n.disabled=busy||!config});
 $('#autostart').disabled=busy||!autoReady;$('#silent-start').disabled=busy||!silentReady;
 $$('#rule-modal button,#rule-modal input,#rule-modal select').forEach(n=>{n.disabled=busy});
 $('#confirm-modal').disabled=busy||(!deleting&&$('input[name=rule-type]:checked').value==='preset'&&!$('#software-select').value);
}
function render(){
 rules=[...(config?.presets||[]).map((preset,index)=>({type:'preset',index,preset,name:name(preset)})),...(config?.allow_exe||[]).map((value,index)=>({type:'custom',index,value,name:value}))];
 $('#rule-count').textContent=`${rules.length} 条规则`;$('#empty-state').hidden=!!rules.length;
 $('#rule-list').replaceChildren(...rules.map(rule=>{
 const row=document.createElement('div');row.className='rule-row';const icon=document.createElement('div');icon.className='rule-icon';icon.innerHTML=`<span class="tabler-icon-root" data-icon="${rule.type==='preset'?'shield':'edit'}" aria-hidden="true"></span>`;
 const copy=document.createElement('div');copy.className='rule-copy';const strong=document.createElement('strong');strong.textContent=rule.name;copy.append(strong);
 const actions=document.createElement('div');actions.className='row-actions';const edit=button('edit',`编辑 ${rule.name}`,()=>open(rule));edit.dataset.edit=`${rule.type}-${rule.index}`;const remove=button('trash',`删除 ${rule.name}`,()=>open(rule,true));remove.dataset.delete=`${rule.type}-${rule.index}`;remove.classList.add('danger');
 actions.append(button('info',rule.type==='custom'?help:`程序：${rule.preset.executables.join(', ')}；非文本窗口：${rule.preset.non_text_classes.join(', ')}`),edit,remove);row.append(icon,copy,actions);return row}));lock();
}
function choice(){const custom=$('input[name=rule-type]:checked').value==='custom';$$('.choice').forEach(n=>n.classList.toggle('is-selected',$('input',n).checked));$('#software-select').hidden=custom||deleting;$('#custom-executable').hidden=!custom||deleting;$('#custom-program-field').hidden=!custom||deleting;$('#software-field-label').textContent=custom?'IME 白名单程序':'软件';$('#software-field-label').htmlFor=custom?'custom-executable':'software-select';$('#software-field-label').hidden=deleting;lock()}
function open(rule=null,remove=false){
 if(busy||!config)return;focus=document.activeElement;editing=rule;deleting=remove;$('#rule-modal').hidden=false;$('#app-shell').inert=true;$('#modal-error').textContent='';$('#modal-title').textContent=remove?`删除 ${rule.name}？`:rule?'编辑规则':'添加规则';$('#confirm-modal').textContent=remove?'删除':rule?'保存':'添加';$('.choice-grid').hidden=remove;
 const type=rule?.type||(catalog.length?'preset':'custom');$(`input[name=rule-type][value=${type}]`).checked=true;const options=[...catalog];if(rule?.type==='preset'&&!options.some(p=>p.name===rule.preset.name))options.push(rule.preset);
 $('#software-select').replaceChildren(...options.map(p=>{const o=document.createElement('option');o.value=p.name;o.textContent=name(p);return o}));if(rule?.type==='preset')$('#software-select').value=rule.preset.name;$('#custom-executable').value=rule?.type==='custom'?rule.value:'';$('#custom-executable').setCustomValidity('');choice();(remove?$('#cancel-modal'):type==='custom'?$('#custom-executable'):$('#software-select')).focus();
}
function close(){if(busy)return;$('#rule-modal').hidden=true;$('#app-shell').inert=false;(focus?.isConnected?focus:$('#add-button')).focus()}
async function save(){
 if(busy||!config)return;const next=clone(config),type=$('input[name=rule-type]:checked').value;next.presets||=[];next.allow_exe||=[];let value,preset;
 if(!deleting){if(type==='custom'){value=$('#custom-executable').value.trim();if(!/^[^\\/:*?"<>|\u0000-\u001f]+$/.test(value)||value==='.'||value==='..'||/[. ]$/.test(value)){$('#custom-executable').setCustomValidity('请输入不含路径的有效文件名');$('#custom-executable').reportValidity();return}}
 else{const selected=$('#software-select').value;preset=editing?.type==='preset'&&editing.preset.name===selected?editing.preset:catalog.find(p=>p.name===selected);if(!preset)return;preset={name:preset.name,executables:clone(preset.executables),non_text_classes:clone(preset.non_text_classes)}}}
 if(editing)next[editing.type==='preset'?'presets':'allow_exe'].splice(editing.index,1);
 if(!deleting){const duplicate=type==='preset'?next.presets.some(p=>p.name.toLowerCase()===preset.name.toLowerCase()):next.allow_exe.some(v=>v.toLowerCase()===value.toLowerCase());if(duplicate){$('#modal-error').textContent='该规则已存在';return}const key=type==='preset'?'presets':'allow_exe';next[key].splice(editing?.type===type?editing.index:next[key].length,0,type==='preset'?preset:value)}
 if(next.profile!=='auto'&&!next.presets.some(p=>p.name===next.profile))next.profile='auto';busy=true;lock();
 try{if(!preview)await invoke('save_config',{config:next});config=preview?next:await invoke('get_config');render();busy=false;close();report(preview?'浏览器预览：修改未写入系统':'')}
 catch(error){$('#modal-error').textContent=`保存失败：${error}`;report(`保存失败：${error}`)}finally{busy=false;lock()}
}
function core(s){const node=$('#core-status');node.textContent=s.running?'运行中':'未运行';node.dataset.running=String(!!s.running);node.setAttribute('aria-label',s.error?`未运行，核心错误：${s.error}`:node.textContent);if(typeof s.requested_enabled==='boolean'&&!busy&&config){config.enabled=s.requested_enabled;$('#global-toggle').checked=s.requested_enabled}}
async function refreshStatus(){if(preview||disposed||statusPending||busy)return;statusPending=true;try{const status=await invoke('get_core_status');if(!disposed)core(status)}catch(error){if(!disposed){core({running:false,error:String(error),requested_enabled:null});report(`状态读取失败：${error}`)}}finally{statusPending=false}}
function cleanup(){disposed=true;cancelAnimationFrame(logFrame);clearInterval(statusTimer);unlisten?.();hideTooltip()}
async function pickProgram(){if(busy)return;if(preview){$('#modal-error').textContent='浏览器预览无法打开系统文件选择器';return}busy=true;lock();try{const selected=await invoke('pick_program');if(typeof selected==='string'){$('#custom-executable').value=selected;$('#custom-executable').setCustomValidity('')}}catch(error){$('#modal-error').textContent=`选择失败：${error}`}finally{busy=false;lock();$('#custom-executable').focus()}}
async function toggle(input,command,arg){if(busy)return;const wanted=input.checked,previous=!wanted;busy=true;lock();try{if(!preview)await invoke(command,{[arg]:wanted});const actual=preview?wanted:command==='set_enabled'?(await invoke('get_config')).enabled:await invoke(command==='set_autostart'?'get_autostart':'get_silent_start');input.checked=!!actual;if(command==='set_enabled'){config.enabled=!!actual;if(!preview)core(await invoke('get_core_status'))}report(preview?'浏览器预览：修改未写入系统':'')}catch(error){input.checked=previous;if(command==='set_enabled')config.enabled=previous;report(`设置失败：${error}`)}finally{busy=false;lock()}}
let updateInfo,updateChecking=false,updateInstalling=false,updateInstalled=false;
function updateControls(){
 $('#check-update').disabled=preview||updateChecking||updateInstalling||updateInstalled;
 $('#install-update').disabled=preview||updateChecking||updateInstalling||updateInstalled||!updateInfo?.available||!updateInfo.update_id;
}
async function checkUpdate(){
 if(preview||disposed||updateChecking||updateInstalling||updateInstalled)return;
 updateChecking=true;updateInfo=null;$('#update-details').hidden=true;$('#update-status').textContent='正在检查更新…';updateControls();
 try{const result=await invoke('check_update');if(disposed)return;
  updateInfo=result;$('#update-version').textContent=`当前版本：${result.current_version} · 最新版本：${result.version||'暂无发布'}`;
  $('#update-status').textContent=result.available?'发现新版本':result.version?'已是最新版本':'暂无可用更新';
  $('#release-notes').textContent=result.body||'暂无发布说明';$('#update-details').hidden=!result.version;$('#install-update').hidden=!result.available;
 }catch(error){if(!disposed){$('#update-status').textContent=`检查更新失败：${error}`;if($('#update-version').textContent.includes('检查中'))$('#update-version').textContent='版本信息暂不可用'}}
 finally{updateChecking=false;if(!disposed)updateControls()}
}
async function installUpdate(){if(!updateInfo?.available||!updateInfo.update_id||updateInstalling)return;const modal=$('#update-modal');modal.hidden=false;$('#app-shell').inert=true;$('#cancel-update').focus()}
async function confirmUpdate(){
 if(!updateInfo?.available||!updateInfo.update_id||updateInstalling||updateInstalled||$('#update-modal').hidden)return;
 updateInstalling=true;$('#update-modal').hidden=true;$('#app-shell').inert=false;$('#update-status').textContent='正在下载安装更新…';updateControls();
 try{const onEvent=new api.core.Channel();let total=null,downloaded=0;
  onEvent.onmessage=message=>{if(disposed||!updateInstalling)return;
   if(message.event==='started'){total=message.data?.content_length;downloaded=0;$('#update-status').textContent='正在下载更新…'}
   else if(message.event==='progress'){downloaded+=message.data.chunk_length;$('#update-status').textContent=total>0?`正在下载更新… ${Math.min(100,Math.floor(downloaded/total*100))}%`:`正在下载更新… ${downloaded} 字节`}
   else if(message.event==='finished')$('#update-status').textContent='下载完成，正在校验并准备安装…';
  };
  const updateId=updateInfo.update_id;updateInfo=null;
  await invoke('install_update',{updateId,confirmed:true,onEvent});if(disposed)return;updateInstalled=true;$('#update-status').textContent='安装程序已启动，正在静默更新'}
 catch(error){if(!disposed)$('#update-status').textContent=`安装失败：${error}`}
 finally{updateInstalling=false;if(!disposed){updateControls();$('#update-status').tabIndex=-1;$('#update-status').focus()}}
}
function closeUpdate(){if(updateInstalling)return;$('#update-modal').hidden=true;$('#app-shell').inert=false;$('#install-update').focus()}
async function load(){if(preview){config={enabled:false,profile:'auto',presets:[],allow_exe:[]};autoReady=silentReady=true;report('浏览器预览：未连接后端');render();return}
 const secondary=(task,label)=>task.catch(error=>{if(!disposed)report(`${label}失败：${error}`)});
 if(api.event?.listen)secondary(api.event.listen('paxile-log',event=>{if(!disposed)receive(event.payload)}).then(stop=>{if(disposed)stop();else unlisten=stop}),'日志订阅');
 secondary(invoke('get_logs').then(history=>{if(!disposed)receiveBatch(history)}),'日志读取');
 for(const [id,command]of [['autostart','get_autostart'],['silent-start','get_silent_start']])secondary(invoke(command).then(value=>{if(disposed)return;$(`#${id}`).checked=!!value;if(id==='autostart')autoReady=true;else silentReady=true;lock()}),'偏好读取');
 try{const [loaded,available]=await Promise.all([invoke('get_config'),invoke('get_available_presets')]);if(disposed)return;config=loaded;catalog=available;$('#global-toggle').checked=!!config.enabled;render();refreshStatus();statusTimer=setInterval(refreshStatus,1000)}catch(error){if(!disposed){report(`读取失败：${error}`);lock()}}
}
function tray(){setupTooltips();document.documentElement.classList.add('tray-root');document.body.classList.add('tray-root');$('#rule-modal').remove();const shell=$('#app-shell');shell.className='tray-menu';const title=document.createElement('strong');title.textContent='Paxile';shell.replaceChildren(title);
 for(const [text,command]of [['打开设置页','open_settings_window'],['退出 Paxile','quit_app']]){const b=document.createElement('button');b.type='button';b.className='button';b.textContent=text;b.onclick=async()=>{if(preview){b.dataset.tooltip='浏览器预览：未连接后端';return}b.disabled=true;try{await invoke(command)}catch(error){b.dataset.tooltip=String(error);b.setAttribute('aria-label',`${text}：${error}`)}finally{b.disabled=false}};shell.append(b)}}
let tooltip,tooltipTarget;
function hideTooltip(){if(tooltipTarget)tooltipTarget.removeAttribute('aria-describedby');tooltipTarget=null;if(tooltip)tooltip.hidden=true}
function setupTooltips(){tooltip=document.createElement('div');tooltip.id='arc-tooltip';tooltip.className='arc-tooltip';tooltip.setAttribute('role','tooltip');tooltip.hidden=true;document.body.append(tooltip);
 const show=e=>{const target=e.target.closest?.('[data-tooltip]');if(!target||target.closest('[hidden]'))return;hideTooltip();tooltipTarget=target;tooltip.textContent=target.dataset.tooltip;tooltip.hidden=false;target.setAttribute('aria-describedby',tooltip.id);const r=target.getBoundingClientRect(),t=tooltip.getBoundingClientRect();tooltip.style.left=`${Math.max(8,Math.min(innerWidth-t.width-8,r.left+r.width/2-t.width/2))}px`;tooltip.style.top=`${r.bottom+t.height+16<innerHeight?r.bottom+8:Math.max(8,r.top-t.height-8)}px`};
 document.addEventListener('pointerover',show);document.addEventListener('focusin',show);document.addEventListener('pointerout',e=>{if(tooltipTarget&&!tooltipTarget.contains(e.relatedTarget))hideTooltip()});document.addEventListener('focusout',hideTooltip);document.addEventListener('pointerdown',hideTooltip);document.addEventListener('keydown',e=>{if(e.key==='Escape')hideTooltip()});window.addEventListener('scroll',hideTooltip,true);
}
function setupHeaderDrag(){
 const header=$('.topbar'),interactive='button,input,select,textarea,a,label,[role="button"],[role="switch"],[contenteditable="true"]';
 // Tauri's built-in drag listener checks only the exact event target. Mark
 // noninteractive descendants, but route the gesture through one IPC handler.
 for(const node of [header,...$$('*',header)])if(!node.closest(interactive))node.setAttribute('data-tauri-drag-region','');
 header.addEventListener('mousedown',e=>{
  if(e.button!==0||e.detail>1||e.target.closest(interactive))return;
  e.preventDefault();e.stopPropagation(); // Do not also run Tauri's document listener.
  if(!preview)invoke('start_settings_drag').catch(error=>report(`窗口拖动失败：${error}`));
 });
}
function init(){if(started)return;started=true;if(new URLSearchParams(location.search).get('tray')==='1'){tray();return}
 const status=document.createElement('div');status.id='ui-status';status.setAttribute('role','status');$('.topbar').after(status);const error=document.createElement('div');error.id='modal-error';error.setAttribute('role','alert');$('.modal-actions').before(error);
 const info=document.createElement('span');info.id='core-status';info.setAttribute('role','status');info.dataset.running='false';info.textContent='未运行';$('.top-actions').prepend(info);
 for(const [id,symbol,label,command]of [['minimize-settings','minus','最小化','minimize_settings'],['close-settings','close','关闭窗口','close_settings']]){const control=button(symbol,label,async()=>{try{if(!preview)await invoke(command)}catch(error){report(`窗口操作失败：${error}`)}});control.id=id;control.classList.add('window-control');$('.top-actions').append(control)}
 const input=$('#custom-executable'),field=document.createElement('div');field.id='custom-program-field';field.className='custom-program-field';input.before(field);field.append(input);const picker=button('folder','选择程序文件',pickProgram);picker.id='pick-program';field.append(picker);
 $$('[title]').forEach(n=>{n.dataset.tooltip=n.getAttribute('title');n.removeAttribute('title')});$$('svg use').forEach(n=>{const host=document.createElement('span');host.className='tabler-icon-root';host.dataset.icon=n.getAttribute('href').replace('#icon-','');host.setAttribute('aria-hidden','true');n.closest('svg').replaceWith(host)});$('.svg-defs')?.remove();setupTooltips();
 $$('.eyebrow,.setting-row p,.empty-state p,.choice small').forEach(n=>n.remove());$$('.setting-row h2').forEach(n=>n.append(button('info',n.textContent.includes('静默')?'启动时不打开设置窗口。':'登录 Windows 后自动运行 Paxile。')));
 const custom=$('input[value=custom]').closest('label');$('strong',custom).textContent='IME 白名单';custom.dataset.tooltip=help;$('#custom-executable').dataset.tooltip=help;$('#custom-executable').setAttribute('aria-label','IME 白名单可执行文件名');$('.live-dot').remove();
 $$('[data-page]').forEach(b=>{b.onclick=()=>page(b.dataset.page)});$('#add-button').onclick=()=>open();$('[data-open-modal]').onclick=()=>open();$('#close-modal').onclick=close;$('#cancel-modal').onclick=close;$('#confirm-modal').onclick=save;
 $$('input[name=rule-type]').forEach(n=>{n.onchange=choice});$('#custom-executable').oninput=e=>e.target.setCustomValidity('');$('#rule-modal').onclick=e=>{if(e.target===e.currentTarget)close()};
 document.addEventListener('keydown',e=>{if($('#rule-modal').hidden)return;if(e.key==='Escape'){e.preventDefault();close()}if(e.key==='Enter'&&!e.target.matches('button')){e.preventDefault();save()}if(e.key!=='Tab')return;const nodes=$$('button,input,select',$('#rule-modal')).filter(n=>!n.disabled&&!n.hidden&&!n.closest('[hidden]')),first=nodes[0],last=nodes.at(-1);if(e.shiftKey&&document.activeElement===first){e.preventDefault();last?.focus()}else if(!e.shiftKey&&document.activeElement===last){e.preventDefault();first?.focus()}});
 for(const[id,command,arg]of [['global-toggle','set_enabled','enabled'],['autostart','set_autostart','on'],['silent-start','set_silent_start','on']])$(`#${id}`).onchange=e=>toggle(e.target,command,arg);
 $('#install-update').onclick=installUpdate;$('#confirm-update').onclick=confirmUpdate;$('#cancel-update').onclick=closeUpdate;$('#close-update').onclick=closeUpdate;$('#update-modal').onclick=e=>{if(e.target===e.currentTarget)closeUpdate()};
 document.addEventListener('keydown',e=>{if($('#update-modal').hidden)return;if(e.key==='Escape'){e.preventDefault();closeUpdate()}if(e.key==='Tab'){const nodes=$$('button',$('#update-modal')),first=nodes[0],last=nodes.at(-1);if(e.shiftKey&&document.activeElement===first){e.preventDefault();last.focus()}else if(!e.shiftKey&&document.activeElement===last){e.preventDefault();first.focus()}}});
 $('#check-update').onclick=checkUpdate;updateControls();if(preview){$('#update-version').textContent='版本信息暂不可用';$('#update-status').textContent='浏览器预览无法检查更新'}else checkUpdate();
 setupHeaderDrag();$('#log-list').addEventListener('scroll',paintLogs);window.addEventListener('resize',scheduleLogs);document.addEventListener('visibilitychange',scheduleLogs);window.addEventListener('beforeunload',cleanup,{once:true});lock();load();
}
document.addEventListener('DOMContentLoaded',init);if(document.readyState!=='loading')init();
})();
