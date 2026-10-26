const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {JSDOM}=require('jsdom');
const root=path.resolve(__dirname,'..');
function boot(invoke){const html=fs.readFileSync(path.join(root,'tray.html'),'utf8');const dom=new JSDOM(html,{runScripts:'outside-only',pretendToBeVisual:true});if(invoke)dom.window.__TAURI__={core:{invoke}};dom.window.eval(fs.readFileSync(path.join(root,'tray.js'),'utf8'));return dom;}
test('dedicated tray has exactly two actions and no settings or React dependencies',()=>{
 const html=fs.readFileSync(path.join(root,'tray.html'),'utf8'),js=fs.readFileSync(path.join(root,'tray.js'),'utf8');
 assert.doesNotMatch(html+js,/icons.jsx|main.js|react|styles.css|get_config|get_logs/);
 const dom=boot();try{const d=dom.window.document;assert.equal(d.querySelector('strong').textContent,'Paxile');assert.equal(d.querySelectorAll('button').length,2);assert.deepEqual([...d.querySelectorAll('button')].map(b=>b.textContent.trim()),['打开设置页','退出 Paxile']);assert(d.querySelector('[role=separator]'));assert.equal(d.querySelectorAll('[title]').length,0);}finally{dom.window.close()}
});
test('tray commands invoke once and recover from failure with accessible custom tooltip',async()=>{
 const calls=[];const dom=boot(async command=>{calls.push(command);if(command==='quit_app')throw Error('denied')});const w=dom.window,d=w.document;
 try{const buttons=[...d.querySelectorAll('button')];buttons[0].click();await new Promise(r=>setTimeout(r,0));buttons[1].click();await new Promise(r=>setTimeout(r,0));assert.deepEqual(calls,['open_settings_window','quit_app']);assert.equal(buttons[1].disabled,false);buttons[1].focus();assert.match(d.querySelector('[role=tooltip]').textContent,/denied/);assert.equal(d.querySelector('[role=tooltip]').hidden,false);d.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Escape',bubbles:true}));assert.equal(d.querySelector('[role=tooltip]').hidden,true);}finally{w.close()}
});
test('tray keyboard navigates the two commands without adding native tooltips',()=>{const dom=boot();try{const w=dom.window,d=w.document;d.dispatchEvent(new w.KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));assert.equal(d.activeElement,d.querySelector('button'));d.dispatchEvent(new w.KeyboardEvent('keydown',{key:'ArrowUp',bubbles:true}));assert.equal(d.activeElement,d.querySelector('button:last-child'));}finally{dom.window.close()}});
