const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { JSDOM } = require(process.env.JSDOM_PATH || 'jsdom');
const root = path.resolve(__dirname, '..');

test('slow secondary initialization never blocks rule controls', async () => {
 let release; const pending = new Promise(r => release = r);
 const app = await boot({delays:{get_core_status:pending,get_logs:pending,get_autostart:pending,get_silent_start:pending}});
 try { assert.equal(app.w.document.querySelector('#add-button').disabled,false); click(app.w,'#add-button'); assert.equal(app.w.document.querySelector('#rule-modal').hidden,false); }
 finally { release(); await tick(); app.close(); }
});
test('history batches once and hidden incoming logs defer all DOM work', async () => {
 const history=Array.from({length:500},(_,i)=>({time:String(i),level:'info',msg:`HISTORY ${i}`}));
 const app=await boot({history}),{w}=app;
 try { assert.equal(app.logPaints(),0); click(w,'[data-page=settings]'); assert.equal(app.logPaints(),1);
 for(let i=0;i<100;i++)app.emit({time:String(1000+i),msg:`LIVE ${i}`});
 assert.equal(app.logPaints(),1); await tick(); assert.equal(app.logPaints(),2); assert.match(w.document.querySelector('#log-list').textContent,/LIVE 99/);
 click(w,'[data-page=intercept]'); for(let i=0;i<20;i++)app.emit({time:String(2000+i),msg:`HIDDEN ${i}`}); await tick(); assert.equal(app.logPaints(),2); } finally {app.close();}
});

test('header settings navigation has no duplicate icon action', () => {
  const html = fs.readFileSync(path.join(root, 'index.html'), 'utf8');
  const dom = new JSDOM(html);
  assert.equal(dom.window.document.querySelectorAll('[data-page="settings"]').length, 1);
  assert.equal(dom.window.document.querySelectorAll('.top-actions [data-page="settings"]').length, 0);
  dom.window.close();
});
const preset = { name: 'CLIP Studio Paint', executables: ['CLIPStudioPaint.exe'], non_text_classes: ['742DEA58-ED6B-4402-BC11-20DFC6D08040*'] };
const tick = () => new Promise(resolve => setTimeout(resolve, 20));
async function boot({ update = {available:false,version:null,body:'',current_version:'0.1.0'}, checkError, installError, delays = {}, history, tray = false, preview = false, failSave = false, failSettings = false, initial = {}, available = [preset], picked = 'paint.com', status = { running: false, requested_enabled: false, error: null } } = {}) {
  const dom = new JSDOM(fs.readFileSync(path.join(root, 'index.html'), 'utf8'), { url: `http://localhost/${tray ? '?tray=1' : ''}`, runScripts: 'outside-only', pretendToBeVisual: true });
  const w = dom.window;
  w.structuredClone = structuredClone;
  let config = { enabled: false, profile: 'auto', presets: [], allow_exe: [], retained: 'keep', ...initial };
  const preferences = { autostart: false, silent_start: false };
  const calls = [], errors = [];
  let listener;
  w.addEventListener('error', e => errors.push(e.error));
  class FakeChannel { onmessage; }
  if (!preview) w.__TAURI__ = { core: { Channel: FakeChannel, invoke: async (command, args) => {
    calls.push({ command, args });
    if (delays[command]) await delays[command];
    if (command === 'get_config') return structuredClone(config);
    if (command === 'get_available_presets') return structuredClone(available);
    if (command === 'check_update') { if (checkError) throw Error(checkError); return structuredClone(update); }
    if (command === 'install_update') { if (installError) throw Error(installError); return true; }
    if (command === 'get_core_status') return status;
    if (command === 'pick_program') return picked;
    if (command === 'get_logs') return history || [{ time: '1793000000000', level: 'info', msg: 'REAL CORE ENTRY' }];
    if (command === 'save_config') { if (failSave) throw Error('disk denied'); config = structuredClone(args.config); return; }
    if (command === 'set_enabled') { throw Error('core refused'); }
    if (command.startsWith('set_')) { if (failSettings) throw Error('preference denied'); preferences[command.slice(4)] = args.on; }
    if (command.startsWith('get_')) return preferences[command.slice(4)] || false;
    return args?.on;
  } }, event: { listen: async (_, callback) => { listener = callback; return () => {}; } } };
  let logPaints = 0;
  const list = w.document.querySelector('#log-list'), replace = list.replaceChildren.bind(list);
  list.replaceChildren = (...args) => { logPaints++; return replace(...args); };
  w.eval(fs.readFileSync(path.join(root, 'main.js'), 'utf8'));
  w.document.dispatchEvent(new w.Event('DOMContentLoaded'));
  await tick();
  return { w, calls, errors, logPaints: () => logPaints, emit: entry => listener({ payload: entry }), getConfig: () => config, setFailure: value => { failSave = value; }, close: () => w.close() };
}
function click(w, selector) { w.document.querySelector(selector).click(); }
function selectType(w, type) { click(w, `input[name=rule-type][value=${type}]`); }

test('failed add keeps confirmed rows and modal open; retry persists', async () => {
 const app=await boot({failSave:true}),{w}=app;
 click(w,'#add-button');click(w,'#confirm-modal');await tick();
 assert.equal(w.document.querySelectorAll('.rule-row').length,0);
 assert.equal(w.document.querySelector('#rule-modal').hidden,false);
 assert.match(w.document.querySelector('#modal-error').textContent,/disk denied/);
 app.setFailure(false);click(w,'#confirm-modal');await tick();assert.equal(app.getConfig().presets.length,1);app.close();
});
test('custom whitelist CRUD, cancel and duplicate validation',async()=>{
 const app=await boot(),{w}=app;
 click(w,'#add-button');selectType(w,'custom');w.document.querySelector('#custom-executable').value='game.exe';click(w,'#confirm-modal');await tick();
 assert.deepEqual(app.getConfig().allow_exe,['game.exe']);assert.equal(app.getConfig().presets.length,0);
 assert.match(w.document.querySelector('.row-actions button').dataset.tooltip,/白名单/);
 click(w,'[data-edit]');w.document.querySelector('#custom-executable').value='other.exe';click(w,'#cancel-modal');assert.deepEqual(app.getConfig().allow_exe,['game.exe']);
 click(w,'[data-edit]');w.document.querySelector('#custom-executable').value='other.exe';click(w,'#confirm-modal');await tick();assert.deepEqual(app.getConfig().allow_exe,['other.exe']);
 click(w,'#add-button');selectType(w,'custom');w.document.querySelector('#custom-executable').value='OTHER.exe';click(w,'#confirm-modal');await tick();assert.match(w.document.querySelector('#modal-error').textContent,/已存在/);click(w,'#cancel-modal');
 click(w,'[data-delete]');assert.equal(app.getConfig().allow_exe.length,1);click(w,'#cancel-modal');
 click(w,'[data-delete]');click(w,'#confirm-modal');await tick();assert.equal(app.getConfig().allow_exe.length,0);app.close();
});
test('edit switches preset from catalog and preserves unchanged effective rules',async()=>{
 const second={name:'Second',display_name:'Second app',executables:['two.exe'],non_text_classes:['Canvas*']};
 const effective={...preset,non_text_classes:['CUSTOM*']};
 const app=await boot({initial:{presets:[effective],profile:preset.name},available:[preset,second]}),{w}=app;
 click(w,'[data-edit]');click(w,'#confirm-modal');await tick();assert.deepEqual(app.getConfig().presets,[effective]);
 click(w,'[data-edit]');w.document.querySelector('#software-select').value='Second';click(w,'#confirm-modal');await tick();
 assert.deepEqual(app.getConfig().presets,[{name:'Second',executables:['two.exe'],non_text_classes:['Canvas*']}]);assert.equal(app.getConfig().profile,'auto');app.close();
});
test('failed settings restore switches, report errors, never fake core logs',async()=>{
 const app=await boot({failSettings:true}),{w}=app;
 for(const id of ['global-toggle','autostart','silent-start']){click(w,`#${id}`);await tick();assert.equal(w.document.querySelector(`#${id}`).checked,false);assert.equal(w.document.querySelector(`#${id}`).disabled,false)}
 assert.match(w.document.querySelector('#ui-status').textContent,/失败/);
 click(w,'[data-page=settings]');assert.equal(w.document.querySelector('[data-page-panel=settings]').hidden,false);click(w,'[data-page=intercept]');
 assert.equal(w.document.querySelectorAll('.log-row').length,1);assert.match(w.document.querySelector('#log-list').textContent,/REAL CORE ENTRY/);assert.doesNotMatch(w.document.querySelector('#log-list').textContent,/已连接|设置已更新/);app.close();
});
test('failed edit and delete preserve confirmed rules',async()=>{
 const app=await boot({failSave:true,initial:{presets:[preset]}}),{w}=app;
 click(w,'[data-edit]');selectType(w,'custom');w.document.querySelector('#custom-executable').value='test.exe';click(w,'#confirm-modal');await tick();assert.deepEqual(app.getConfig().presets,[preset]);click(w,'#cancel-modal');
 click(w,'[data-delete]');click(w,'#confirm-modal');await tick();assert.equal(w.document.querySelectorAll('.rule-row').length,1);assert.equal(w.document.querySelector('#rule-modal').hidden,false);app.close();
});
test('tray route contains title and two commands without main-page initialization',async()=>{
 const app=await boot({tray:true}),{w}=app;
 assert.equal(w.document.querySelector('.tray-menu strong').textContent,'Paxile');assert.equal(w.document.querySelectorAll('.tray-menu button').length,2);
 assert(w.document.documentElement.classList.contains('tray-root'));click(w,'.tray-menu button');await tick();click(w,'.tray-menu button:last-child');await tick();
 assert.deepEqual(app.calls.map(c=>c.command),['open_settings_window','quit_app']);assert.equal(app.errors.length,0);app.close();
});
test('bounded logs render actual entries and virtualize scrolling',async()=>{
 const app=await boot(),{w}=app;
 click(w,'[data-page=settings]');
 for(let i=0;i<5100;i++)app.emit({time:String(1793000000001+i),level:'warn',msg:`CORE ${i}`});
 await tick();
 assert(w.document.querySelectorAll('.log-row').length<=14);assert.match(w.document.querySelector('#log-list').textContent,/CORE 5099/);
 const list=w.document.querySelector('#log-list');list.scrollTop=149700;list.dispatchEvent(new w.Event('scroll'));
 assert.doesNotMatch(list.textContent,/CORE 0\b/);assert.equal(app.errors.length,0);app.close();
});
test('preview has no invented catalog or logs; keyboard modal focus returns',async()=>{
 const app=await boot({preview:true}),{w}=app;assert.equal(w.document.querySelectorAll('.rule-row').length,0);assert.equal(w.document.querySelectorAll('.log-row').length,0);
 click(w,'#add-button');assert.equal(w.document.querySelectorAll('#software-select option').length,0);w.document.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Escape',bubbles:true}));assert.equal(w.document.querySelector('#rule-modal').hidden,true);assert.equal(app.errors.length,0);app.close();
});



test('native picker fills a safe basename and permits all-files extensions', async () => {
 const app=await boot(),{w}=app;click(w,'#add-button');selectType(w,'custom');click(w,'#pick-program');await tick();
 assert.equal(w.document.querySelector('#custom-executable').value,'paint.com');click(w,'#confirm-modal');await tick();assert.deepEqual(app.getConfig().allow_exe,['paint.com']);app.close();
});
test('picker cancellation preserves manual input', async () => {
 const app=await boot({picked:null}),{w}=app;click(w,'#add-button');selectType(w,'custom');w.document.querySelector('#custom-executable').value='manual.exe';click(w,'#pick-program');await tick();assert.equal(w.document.querySelector('#custom-executable').value,'manual.exe');app.close();
});
test('header drags noninteractive descendants exactly once and never controls',async()=>{
 const app=await boot(),{w}=app;
 const down=(node,options={})=>node.dispatchEvent(new w.MouseEvent('mousedown',{bubbles:true,button:0,detail:1,...options}));
 const header=w.document.querySelector('.topbar');
 for(const selector of ['.topbar','.brand','.brand img','.brand span','.tabs','.top-actions','#core-status']){
  const before=app.calls.filter(c=>c.command==='start_settings_drag').length;
  down(w.document.querySelector(selector));await tick();
  assert.equal(app.calls.filter(c=>c.command==='start_settings_drag').length,before+1,selector);
 }
 const before=app.calls.filter(c=>c.command==='start_settings_drag').length;
 for(const selector of ['.tab','#add-button','#global-toggle','.toggle-track','.toggle-track span','#minimize-settings','#close-settings']) down(w.document.querySelector(selector));
 down(header,{button:2});down(header,{detail:2});await tick();
 assert.equal(app.calls.filter(c=>c.command==='start_settings_drag').length,before);
 click(w,'[data-page=settings]');assert.equal(w.document.querySelector('[data-page-panel=settings]').hidden,false);
 click(w,'#minimize-settings');await tick();assert(app.calls.some(c=>c.command==='minimize_settings'));
 assert.equal(app.errors.length,0);app.close();
});

test('header controls invoke native commands; brand is noninteractive and tooltips are custom',async()=>{
 const app=await boot(),{w}=app;assert.equal(w.document.querySelector('.brand').tagName,'DIV');assert.equal(w.document.querySelector('.brand img').getAttribute('src'),'/icon.png');
 click(w,'#minimize-settings');click(w,'#close-settings');await tick();assert(app.calls.some(c=>c.command==='minimize_settings'));assert(app.calls.some(c=>c.command==='close_settings'));assert.equal(w.document.querySelectorAll('[title]').length,0);assert(w.document.querySelector('.topbar[data-tauri-drag-region]'));app.close();
});
test('live status keeps unknown requested state, polls, and cleans up on unload',async()=>{
 const status={running:true,requested_enabled:null,error:null};const app=await boot({initial:{enabled:true},status}),{w}=app;
 assert.equal(w.document.querySelector('#core-status').textContent,'运行中');assert.equal(w.document.querySelector('#global-toggle').checked,true);
 status.running=false;await new Promise(r=>setTimeout(r,1100));assert.equal(w.document.querySelector('#core-status').textContent,'未运行');assert.equal(w.document.querySelector('#global-toggle').checked,true);
 status.requested_enabled=false;await new Promise(r=>setTimeout(r,1100));assert.equal(w.document.querySelector('#global-toggle').checked,false);
 w.dispatchEvent(new w.Event('beforeunload'));const before=app.calls.length;await new Promise(r=>setTimeout(r,1100));assert.equal(app.calls.length,before);app.close();
});

test('picker failures and unsafe basenames never save a rule',async()=>{
 const app=await boot({picked:'C:\\unsafe.exe'}),{w}=app;click(w,'#add-button');selectType(w,'custom');click(w,'#pick-program');await tick();click(w,'#confirm-modal');await tick();assert.equal(app.getConfig().allow_exe.length,0);assert.equal(w.document.querySelector('#custom-executable').checkValidity(),false);app.close();
});
test('custom tooltips expose full log text and dismiss with Escape',async()=>{
 const app=await boot(),{w}=app;click(w,'[data-page=settings]');const row=w.document.querySelector('.log-row');row.dispatchEvent(new w.Event('pointerover',{bubbles:true}));assert.equal(w.document.querySelector('#arc-tooltip').textContent,'REAL CORE ENTRY');assert.equal(w.document.querySelector('#arc-tooltip').hidden,false);w.document.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Escape',bubbles:true}));assert.equal(w.document.querySelector('#arc-tooltip').hidden,true);assert.equal(w.document.querySelectorAll('[title]').length,0);app.close();
});

test('update auto-check is nonblocking and release notes render as literal text',async()=>{
 let release;const pending=new Promise(r=>release=r);
 const app=await boot({delays:{check_update:pending},update:{available:true,update_id:'checked-update-42',version:'0.2.0',current_version:'0.1.0',body:'<img src=x onerror=alert(1)>\nFix input'}}),{w}=app;
 try {
 assert.equal(w.document.querySelector('#add-button').disabled,false);
 assert.equal(app.calls.filter(c=>c.command==='check_update').length,1);
 release();await tick();click(w,'[data-page=settings]');
 assert.match(w.document.querySelector('#update-version').textContent,/0.1.0.*0.2.0/);
 assert.equal(w.document.querySelector('#release-notes').textContent,'<img src=x onerror=alert(1)>\nFix input');
 assert.equal(w.document.querySelector('#release-notes img'),null);
 assert.equal(app.calls.some(c=>c.command==='install_update'),false);
 }finally{release();app.close()}
});

test('update install requires confirmation; cancel and Escape never download',async()=>{
 const app=await boot({update:{available:true,update_id:'checked-update-42',version:'0.2.0',current_version:'0.1.0',body:'Fix'}}),{w}=app;
 try{click(w,'[data-page=settings]');w.document.querySelector('#install-update').focus();click(w,'#install-update');
 assert.equal(w.document.querySelector('#update-modal').hidden,false);
 assert.equal(w.document.activeElement.id,'cancel-update');
 assert.equal(w.document.querySelector('#app-shell').inert,true);
 click(w,'#cancel-update');assert.equal(w.document.activeElement.id,'install-update');
 click(w,'#install-update');w.document.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Escape',bubbles:true}));
 assert.equal(w.document.querySelector('#update-modal').hidden,true);
 assert.equal(app.calls.some(c=>c.command==='install_update'),false);
 click(w,'#install-update');click(w,'#confirm-update');click(w,'#confirm-update');await tick();
 const installs=app.calls.filter(c=>c.command==='install_update');assert.equal(installs.length,1);
 assert.deepEqual(Object.keys(installs[0].args).sort(),['confirmed','onEvent','updateId']);
 assert.equal(installs[0].args.updateId,'checked-update-42');assert.equal(installs[0].args.confirmed,true);
 assert(installs[0].args.onEvent instanceof w.__TAURI__.core.Channel);assert.equal(typeof installs[0].args.onEvent.onmessage,'function');
 assert.match(w.document.querySelector('#update-status').textContent,/安装程序/);
 assert.equal(w.document.querySelector('#install-update').disabled,true);
 assert.equal(w.document.querySelector('#check-update').disabled,true);
 }finally{app.close()}
});

test('failed install consumes checked handle and requires recheck plus new confirmation; pending install locks actions',async()=>{
 let release;const pending=new Promise(r=>release=r);
 const app=await boot({update:{available:true,update_id:'checked-update-42',version:'0.2.0',current_version:'0.1.0',body:null},delays:{install_update:pending},installError:'download refused'}),{w}=app;
 try{click(w,'[data-page=settings]');click(w,'#install-update');click(w,'#confirm-update');
 assert.equal(w.document.querySelector('#check-update').disabled,true);assert.equal(w.document.querySelector('#install-update').disabled,true);
 app.emit({time:'1793000001234',level:'info',msg:'下载进度 50%'});await tick();assert.match(w.document.querySelector('#log-list').textContent,/下载进度 50%/);
 release();await tick();assert.match(w.document.querySelector('#update-status').textContent,/安装失败.*download refused/);
 assert.equal(w.document.querySelector('#install-update').disabled,true);assert.equal(w.document.querySelector('#check-update').disabled,false);
 click(w,'#install-update');assert.equal(w.document.querySelector('#update-modal').hidden,true);
 click(w,'#check-update');await tick();click(w,'#install-update');
 assert.equal(w.document.querySelector('#update-modal').hidden,false);
 assert.equal(app.calls.filter(c=>c.command==='install_update').length,1);
 click(w,'#confirm-update');await tick();
 const installs=app.calls.filter(c=>c.command==='install_update');assert.equal(installs.length,2);
 assert.equal(installs[1].args.updateId,'checked-update-42');assert.equal(installs[1].args.confirmed,true);
 assert(installs[1].args.onEvent instanceof w.__TAURI__.core.Channel);
 }finally{release();app.close()}
});
test('no release, current release and failed check are nonfatal and can be checked again',async()=>{
 for(const options of [{},{update:{available:false,version:'0.1.0',current_version:'0.1.0',body:'Stable'}},{checkError:'network unavailable'}]){
 const app=await boot(options),{w}=app;
 try{assert.equal(w.document.querySelector('#add-button').disabled,false);assert.equal(w.document.querySelector('#install-update').disabled,true);
 assert.match(w.document.querySelector('#update-status').textContent,options.checkError?/检查更新失败.*network/:options.update?/已是最新版本/:/暂无可用更新/);
 click(w,'#check-update');await tick();assert.equal(app.calls.filter(c=>c.command==='check_update').length,2);assert.equal(app.calls.some(c=>c.command==='install_update'),false);assert.equal(app.errors.length,0);
 }finally{app.close()}
 }
});

test('updater refuses missing checked id or unavailable update even via direct handlers',async()=>{
 for(const update of [{available:true},{available:true,update_id:null},{available:true,update_id:''},{available:false,update_id:'stale'}]){
  const app=await boot({update:{version:'0.2.0',current_version:'0.1.0',...update}}),{w}=app;
  try{assert.equal(w.document.querySelector('#install-update').disabled,true);
   w.document.querySelector('#install-update').onclick();assert.equal(w.document.querySelector('#update-modal').hidden,true);
   w.document.querySelector('#update-modal').hidden=false;await w.document.querySelector('#confirm-update').onclick();
   assert.equal(app.calls.some(c=>c.command==='install_update'),false);
  }finally{app.close()}
 }
});

test('download channel progress never claims installation before command success',async()=>{
 let release;const pending=new Promise(r=>release=r);
 const app=await boot({update:{available:true,update_id:'channel-check',version:'0.2.0',current_version:'0.1.0'},delays:{install_update:pending}}),{w}=app;
 try{click(w,'#install-update');click(w,'#confirm-update');
 const {onEvent}=app.calls.find(c=>c.command==='install_update').args;
 onEvent.onmessage({event:'started',data:{content_length:100}});
 onEvent.onmessage({event:'progress',data:{chunk_length:25}});
 assert.match(w.document.querySelector('#update-status').textContent,/25%/);
 onEvent.onmessage({event:'progress',data:{chunk_length:25}});
 assert.match(w.document.querySelector('#update-status').textContent,/50%/);
 onEvent.onmessage({event:'finished'});
 assert.match(w.document.querySelector('#update-status').textContent,/下载完成/);
 assert.doesNotMatch(w.document.querySelector('#update-status').textContent,/安装程序已启动/);
 assert.equal(w.document.querySelector('#check-update').disabled,true);
 release();await tick();assert.match(w.document.querySelector('#update-status').textContent,/安装程序已启动/);
 }finally{release();app.close()}
});

test('preset catalog retains effective class rules on add', async () => {
  const app = await boot(); const { w } = app;
  click(w, '#add-button');
  assert.equal(w.document.querySelector('#software-select').selectedOptions[0].textContent, preset.name);
  click(w, '#confirm-modal'); await tick();
  assert.deepEqual(app.getConfig().presets, [preset]);
  assert.equal(app.getConfig().retained, 'keep');
  assert.equal(w.document.querySelectorAll('.rule-row').length, 1);
  app.close();
});
